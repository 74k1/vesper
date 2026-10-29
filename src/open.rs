// ---------------------------------------------------------------------------
// Open dispatch: path or URL to MIME type to desktop entry
// ---------------------------------------------------------------------------

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::desktop;
use crate::exec;
use crate::launch::{self, LaunchMode, Target};
use crate::mime::{self, Resolver};
use crate::xdg;

/// Failure of an `open` request, carrying the xdg-open exit code.
#[derive(Debug)]
pub enum OpenError {
    Usage(String),
    Missing(String),
    NoAssociation(String),
    Launch(String),
    ReadPermission(String),
}

impl OpenError {
    pub fn code(&self) -> i32 {
        match self {
            OpenError::Usage(_) => 1,
            OpenError::Missing(_) => 2,
            OpenError::NoAssociation(_) => 3,
            OpenError::Launch(_) => 4,
            OpenError::ReadPermission(_) => 5,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            OpenError::Usage(message)
            | OpenError::Missing(message)
            | OpenError::NoAssociation(message)
            | OpenError::Launch(message)
            | OpenError::ReadPermission(message) => message,
        }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message())
    }
}

impl std::error::Error for OpenError {}

/// Open a file path or URL with the resolved desktop entry.
pub fn open_target(
    target: &str,
    terminal: Option<&str>,
    mode: LaunchMode,
    resolver: &Resolver,
) -> Result<(), OpenError> {
    open_depth(target, terminal, mode, resolver, 0)
}

fn open_depth(
    target: &str,
    terminal: Option<&str>,
    mode: LaunchMode,
    resolver: &Resolver,
    depth: u8,
) -> Result<(), OpenError> {
    if depth > 8 {
        return Err(OpenError::Launch("link loop".into()));
    }

    let parsed = parse_target(target)?;
    let parsed = match parsed {
        Target::File(path) => {
            if !path.exists() {
                return Err(OpenError::Missing(format!(
                    "{} does not exist",
                    path.display()
                )));
            }
            if path.is_file() && fs::File::open(&path).is_err() {
                return Err(OpenError::ReadPermission(format!(
                    "no permission to read {}",
                    path.display()
                )));
            }
            Target::File(fs::canonicalize(&path).unwrap_or(path))
        }
        Target::Url(url) => Target::Url(url),
    };

    let mime = match &parsed {
        Target::File(path) => resolver.detect(path),
        Target::Url(url) => scheme_mime(url)?,
    };

    let id = match resolver.resolve(&mime) {
        Some(id) => id,
        None => {
            if run_fallbacks(&mime, target, mode)? {
                return Ok(());
            }
            return Err(OpenError::NoAssociation(format!(
                "no application for {}",
                mime
            )));
        }
    };

    let entry = desktop::find_entry(&id)
        .map_err(|_| OpenError::NoAssociation(format!("no desktop entry for {}", id)))?;

    match entry.type_() {
        "Application" => {
            let terminal_cmd = launch::resolve_terminal(terminal.map(str::to_string));
            launch::launch_target(&entry, &terminal_cmd, Some(&parsed), mode)
                .map_err(|err| OpenError::Launch(err.to_string()))
        }
        "Link" => {
            let url = entry.url().ok_or_else(|| {
                OpenError::NoAssociation(format!("{} is a Link without a URL", entry.id))
            })?;
            open_depth(url, terminal, mode, resolver, depth + 1)
        }
        other => Err(OpenError::NoAssociation(format!(
            "{} entries are not runnable",
            other
        ))),
    }
}

/// Parse a file path, `file:` URI, or URL with a scheme.
pub fn parse_target(target: &str) -> Result<Target, OpenError> {
    if target.is_empty() {
        return Err(OpenError::Usage("empty target".into()));
    }

    if let Some(rest) = target.strip_prefix("file:") {
        return Ok(Target::File(decode_file_uri(rest)?));
    }

    if url_scheme(target).is_some() {
        return Ok(Target::Url(target.to_string()));
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

fn decode_file_uri(rest: &str) -> Result<PathBuf, OpenError> {
    let rest = rest.trim_start_matches("//");

    if !rest.contains('/') {
        return Ok(PathBuf::from(xdg::percent_decode(rest)));
    }

    let (authority, path) = if let Some(stripped) = rest.strip_prefix('/') {
        ("", format!("/{stripped}"))
    } else {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        (authority, format!("/{path}"))
    };

    if !authority.is_empty() && authority != "localhost" {
        return Err(OpenError::Usage(format!(
            "unsupported file URI authority '{}'",
            authority
        )));
    }

    Ok(PathBuf::from(xdg::percent_decode(&path)))
}

fn scheme_mime(url: &str) -> Result<String, OpenError> {
    let scheme = url_scheme(url)
        .ok_or_else(|| OpenError::Usage(format!("not a URL with a scheme: {}", url)))?;
    Ok(mime::scheme_mime(scheme))
}

/// Try `$BROWSER` for URL schemes and `$VESPER_FALLBACKS` for any type.
fn run_fallbacks(mime: &str, target: &str, mode: LaunchMode) -> Result<bool, OpenError> {
    let mut commands = Vec::new();
    if mime.starts_with("x-scheme-handler/") {
        commands.extend(xdg::browser_commands());
    }
    commands.extend(xdg::fallback_commands());

    for command in commands {
        if run_fallback_command(&command, target, mode) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn run_fallback_command(command: &str, target: &str, mode: LaunchMode) -> bool {
    let mut tokens = match exec::tokenize_exec(command) {
        Ok(tokens) => tokens,
        Err(err) => {
            eprintln!("vesper: fallback '{}' is malformed: {}", command, err);
            return false;
        }
    };

    if tokens.is_empty() {
        return false;
    }

    let substituted = tokens.iter().any(|token| token.contains("%s"));
    if substituted {
        for token in &mut tokens {
            *token = token.replace("%s", target);
        }
    } else {
        tokens.push(target.to_string());
    }

    let program = tokens.remove(0);
    if is_self(&program) {
        return false;
    }

    let mut cmd = Command::new(&program);
    cmd.args(&tokens);

    match mode {
        LaunchMode::Replace => {
            use std::os::unix::process::CommandExt;
            let err = cmd.exec();
            eprintln!("vesper: fallback '{}' failed: {}", command, err);
            false
        }
        _ => match cmd.spawn() {
            Ok(_) => true,
            Err(err) => {
                eprintln!("vesper: fallback '{}' failed: {}", command, err);
                false
            }
        },
    }
}

fn is_self(program: &str) -> bool {
    let name = std::path::Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    matches!(name.as_str(), "vesper" | "v" | "xdg-open")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn parses_paths_urls_and_file_uris() {
        assert_eq!(
            parse_target("x.svg").unwrap(),
            Target::File(PathBuf::from("x.svg"))
        );
        assert_eq!(
            parse_target("https://example.com").unwrap(),
            Target::Url("https://example.com".into())
        );
        assert_eq!(
            parse_target("mailto:a@b").unwrap(),
            Target::Url("mailto:a@b".into())
        );
        assert_eq!(
            parse_target("file:///tmp/a%20b.txt").unwrap(),
            Target::File(PathBuf::from("/tmp/a b.txt"))
        );
        assert_eq!(
            parse_target("file://localhost/tmp/x").unwrap(),
            Target::File(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            parse_target("file:/tmp/x").unwrap(),
            Target::File(PathBuf::from("/tmp/x"))
        );
        assert!(matches!(
            parse_target("file://remote/tmp/x"),
            Err(OpenError::Usage(_))
        ));
    }

    #[test]
    fn maps_schemes_to_mime_types() {
        assert_eq!(
            scheme_mime("HTTPS://example.com").unwrap(),
            "x-scheme-handler/https"
        );
        assert_eq!(
            scheme_mime("mailto:a@b").unwrap(),
            "x-scheme-handler/mailto"
        );
        assert!(matches!(scheme_mime("not a url"), Err(OpenError::Usage(_))));
    }

    #[test]
    fn missing_targets_exit_two() {
        let dir = TempDir::new("open-missing");
        let resolver = Resolver::empty();
        let error = open_target(
            dir.0.join("nope.svg").to_str().unwrap(),
            None,
            LaunchMode::Spawn,
            &resolver,
        )
        .unwrap_err();
        assert_eq!(error.code(), 2);
    }

    #[test]
    fn unassociated_targets_exit_three() {
        let dir = TempDir::new("open-noassoc");
        let file = dir.write("note.txt", "text\n");
        let resolver = Resolver::empty();
        let error =
            open_target(file.to_str().unwrap(), None, LaunchMode::Spawn, &resolver).unwrap_err();
        assert_eq!(error.code(), 3);
    }
}
