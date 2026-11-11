// ---------------------------------------------------------------------------
// Open dispatch: path or URL to MIME type to desktop entry
// ---------------------------------------------------------------------------

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::exec;
use crate::failure::Failure;
use crate::launch::{self, LaunchMode};
use crate::mime::{self, Resolver};
use crate::xdg;

#[derive(Debug, PartialEq, Eq)]
enum Target {
    File(PathBuf),
    Url { url: String, scheme: String },
}

/// Open a file path or URL with the resolved desktop entry. `terminal` is
/// the `--terminal` override for `Terminal=true` entries.
pub fn open_target(
    target: &str,
    terminal: Option<&str>,
    mode: LaunchMode,
    resolver: &Resolver,
) -> Result<(), Failure> {
    open_depth(target, terminal, mode, resolver, 0)
}

fn open_depth(
    target: &str,
    terminal: Option<&str>,
    mode: LaunchMode,
    resolver: &Resolver,
    depth: u8,
) -> Result<(), Failure> {
    if depth > 8 {
        return Err(Failure::new(Failure::LAUNCH, "link loop"));
    }

    let (mime, argument, is_url) = match parse_target(target)? {
        Target::File(path) => {
            if !path.exists() {
                return Err(Failure::new(
                    Failure::MISSING,
                    format!("{} does not exist", path.display()),
                ));
            }
            if path.is_file() && fs::File::open(&path).is_err() {
                return Err(Failure::new(
                    Failure::READ,
                    format!("no permission to read {}", path.display()),
                ));
            }
            let path = fs::canonicalize(&path).unwrap_or(path);
            (
                resolver.detect(&path),
                path.to_string_lossy().into_owned(),
                false,
            )
        }
        Target::Url { url, scheme } => (mime::scheme_mime(&scheme), url, true),
    };

    let Some(entry) = resolver.resolve(&mime) else {
        if run_fallbacks(&mime, target, mode) {
            return Ok(());
        }
        return Err(Failure::new(
            Failure::NO_ASSOCIATION,
            format!("no application for {}", mime),
        ));
    };

    match entry.type_() {
        "Application" => launch::launch(entry, terminal, &[argument], is_url, mode)
            .map_err(|err| Failure::new(Failure::LAUNCH, err.to_string())),
        "Link" => {
            let url = entry.url().ok_or_else(|| {
                Failure::new(
                    Failure::NO_ASSOCIATION,
                    format!("{} is a Link without a URL", entry.id),
                )
            })?;
            open_depth(url, terminal, mode, resolver, depth + 1)
        }
        other => Err(Failure::new(
            Failure::NO_ASSOCIATION,
            format!("{} entries are not runnable", other),
        )),
    }
}

/// Parse a file path, `file:` URI, or URL with a scheme.
fn parse_target(target: &str) -> Result<Target, Failure> {
    if target.is_empty() {
        return Err(Failure::new(Failure::USAGE, "empty target"));
    }

    if let Some(rest) = target.strip_prefix("file:") {
        return Ok(Target::File(decode_file_uri(rest)?));
    }

    if let Some(scheme) = url_scheme(target) {
        return Ok(Target::Url {
            url: target.to_string(),
            scheme: scheme.to_string(),
        });
    }

    Ok(Target::File(xdg::expand_tilde(target)))
}

fn url_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme)
}

fn decode_file_uri(rest: &str) -> Result<PathBuf, Failure> {
    let rest = rest.trim_start_matches("//");

    let Some(slash) = rest.find('/') else {
        return Ok(PathBuf::from(percent_decode(rest)));
    };
    let (authority, path) = rest.split_at(slash);

    if !authority.is_empty() && authority != "localhost" {
        return Err(Failure::new(
            Failure::USAGE,
            format!("unsupported file URI authority '{}'", authority),
        ));
    }

    Ok(PathBuf::from(percent_decode(path)))
}

fn percent_decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|digit| digit as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// A colon-separated list of commands from the environment, in order of
/// preference.
fn env_commands(var: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn run_fallbacks(mime: &str, target: &str, mode: LaunchMode) -> bool {
    let mut commands = Vec::new();
    if mime.starts_with("x-scheme-handler/") {
        commands.extend(env_commands("BROWSER"));
    }
    commands.extend(env_commands("VESPER_FALLBACKS"));

    commands
        .iter()
        .any(|command| run_fallback_command(command, target, mode))
}

fn run_fallback_command(command: &str, target: &str, mode: LaunchMode) -> bool {
    let mut tokens = match exec::tokenize_exec(command) {
        Ok(tokens) => tokens,
        Err(err) => {
            eprintln!("vesper: fallback '{}' is malformed: {}", command, err);
            return false;
        }
    };

    if tokens.iter().any(|token| token.contains("%s")) {
        for token in &mut tokens {
            *token = token.replace("%s", target);
        }
    } else {
        tokens.push(target.to_string());
    }

    let Some((program, args)) = tokens.split_first() else {
        return false;
    };
    if is_self(program) {
        return false;
    }

    let mut cmd = Command::new(program);
    cmd.args(args);

    match launch::execute(cmd, mode) {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "vesper: fallback '{}' failed: {}",
                command,
                err.root_cause()
            );
            false
        }
    }
}

/// A fallback naming this program would recurse forever.
fn is_self(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .is_some_and(|name| name == "vesper" || name == "v" || name == "xdg-open")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Target {
        Target::File(PathBuf::from(path))
    }

    fn url(url: &str, scheme: &str) -> Target {
        Target::Url {
            url: url.into(),
            scheme: scheme.into(),
        }
    }

    #[test]
    fn parses_paths_urls_and_file_uris() {
        let parse = |target| parse_target(target).unwrap();

        assert_eq!(parse("x.svg"), file("x.svg"));
        assert_eq!(
            parse("https://example.com"),
            url("https://example.com", "https")
        );
        assert_eq!(parse("mailto:a@b"), url("mailto:a@b", "mailto"));
        assert_eq!(parse("file:///tmp/a%20b.txt"), file("/tmp/a b.txt"));
        assert_eq!(parse("file://localhost/tmp/x"), file("/tmp/x"));
        assert_eq!(parse("file:/tmp/x"), file("/tmp/x"));
        assert_eq!(
            parse_target("file://remote/tmp/x").unwrap_err().code,
            Failure::USAGE
        );
        assert_eq!(parse_target("").unwrap_err().code, Failure::USAGE);
    }

    #[test]
    fn file_uris_decode_percent_escapes_and_keep_malformed_ones() {
        let parse = |target| parse_target(target).unwrap();

        assert_eq!(parse("file:///tmp/100%25"), file("/tmp/100%"));
        assert_eq!(parse("file:///tmp/bad%zz"), file("/tmp/bad%zz"));
        assert_eq!(parse("file:%2Ftmp%2Ffile"), file("/tmp/file"));
        assert_eq!(parse("file:///tmp/end%20"), file("/tmp/end "));
    }
}
