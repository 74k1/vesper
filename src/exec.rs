// ---------------------------------------------------------------------------
// Exec tokenizing, field-code expansion, and argument routing
// ---------------------------------------------------------------------------

use eyre::{bail, ContextCompat, Result};

use crate::desktop::DesktopEntry;

/// Split an Exec value into tokens, respecting double-quotes and backslash escapes.
pub fn tokenize_exec(exec: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = exec.chars();
    let mut in_quotes = false;
    let mut in_token = false;

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                in_token = true;
                current.push(chars.next().unwrap_or('\\'));
            }
            '"' => {
                in_quotes = !in_quotes;
                in_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            _ => {
                in_token = true;
                current.push(c);
            }
        }
    }

    if in_quotes {
        bail!("unterminated quote in {:?}", exec);
    }
    if in_token {
        tokens.push(current);
    }

    Ok(tokens)
}

/// Expand the field codes in a single Exec token into zero or more arguments.
/// Substituted values are never re-scanned for field codes.
fn expand_token(
    token: &str,
    entry: &DesktopEntry,
    file_args: &[String],
    url_args: &[String],
) -> Vec<String> {
    match token {
        "%f" => return file_args.iter().take(1).cloned().collect(),
        "%F" => return file_args.to_vec(),
        "%u" => return url_args.iter().take(1).cloned().collect(),
        "%U" => return url_args.to_vec(),
        "%i" => {
            return match entry.icon() {
                Some(icon) if !icon.is_empty() => vec!["--icon".into(), icon.into()],
                _ => Vec::new(),
            }
        }
        _ => {}
    }

    let first = |args: &[String]| args.first().cloned().unwrap_or_default();
    let mut out = String::new();
    let mut had_code = false;
    let mut chars = token.chars();

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(code) = chars.next() else {
            out.push('%');
            break;
        };
        had_code = true;
        match code {
            '%' => out.push('%'),
            'f' => out.push_str(&first(file_args)),
            'F' => out.push_str(&file_args.join(" ")),
            'u' => out.push_str(&first(url_args)),
            'U' => out.push_str(&url_args.join(" ")),
            'c' => out.push_str(entry.name()),
            'k' => out.push_str(&entry.path.to_string_lossy()),
            // %i only makes sense as its own token; the rest are deprecated
            'i' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }

    if out.is_empty() && had_code {
        Vec::new()
    } else {
        vec![out]
    }
}

pub fn build_command(
    entry: &DesktopEntry,
    file_args: &[String],
    url_args: &[String],
) -> Result<(String, Vec<String>)> {
    let exec = entry.exec().context("entry has no Exec key")?;

    let mut expanded: Vec<String> = tokenize_exec(exec)?
        .iter()
        .flat_map(|token| expand_token(token, entry, file_args, url_args))
        .collect();

    if expanded.is_empty() {
        bail!("Exec expands to nothing (missing file/URL args?)");
    }

    let program = expanded.remove(0);
    Ok((program, expanded))
}

fn has_field_code(exec: &str, code: char) -> bool {
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') => continue,
                Some(next) if next == code => return true,
                Some(_) | None => {}
            }
        }
    }
    false
}

/// Arguments sorted by the field codes that will receive them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Routed {
    pub files: Vec<String>,
    pub urls: Vec<String>,
    /// Arguments that no field code takes.
    pub dropped: Vec<String>,
}

/// Route arguments to the file or URL field codes of `exec`. An Exec with
/// only one kind of code receives the arguments either way; `are_urls`
/// decides when it has both.
pub fn route_args(exec: &str, args: &[String], are_urls: bool) -> Routed {
    let wants_files = has_field_code(exec, 'f') || has_field_code(exec, 'F');
    let wants_urls = has_field_code(exec, 'u') || has_field_code(exec, 'U');
    let takes_many = has_field_code(exec, 'F') || has_field_code(exec, 'U');

    let (files, urls) = if wants_urls && (are_urls || !wants_files) {
        (Vec::new(), args.to_vec())
    } else {
        (args.to_vec(), Vec::new())
    };

    let dropped = if takes_many {
        Vec::new()
    } else if wants_files || wants_urls {
        args.get(1..).unwrap_or(&[]).to_vec()
    } else {
        args.to_vec()
    };

    Routed {
        files,
        urls,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_entry;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn argv(entry: &DesktopEntry, files: &[&str], urls: &[&str]) -> Vec<String> {
        let (program, args) = build_command(entry, &strings(files), &strings(urls)).unwrap();
        std::iter::once(program).chain(args).collect()
    }

    #[test]
    fn tokenizes_quotes_and_escapes() {
        let tokens = tokenize_exec(
            r#"prog "arg with spaces" arg\ with\ spaces "quote \"inside\"" back\\slash """#,
        )
        .unwrap();
        assert_eq!(
            tokens,
            vec![
                "prog",
                "arg with spaces",
                "arg with spaces",
                "quote \"inside\"",
                "back\\slash",
                "",
            ]
        );
        assert!(tokenize_exec(r#"prog "unterminated"#).is_err());
    }

    #[test]
    fn url_field_codes_receive_extra_args_as_urls() {
        let entry = test_entry("browser %u");
        let extra = vec!["https://example.com".to_string()];
        let routed = route_args(entry.exec().unwrap(), &extra, false);

        assert!(routed.files.is_empty());
        assert_eq!(routed.urls, extra);
        assert!(routed.dropped.is_empty());

        let (program, args) = build_command(&entry, &routed.files, &routed.urls).unwrap();
        assert_eq!(program, "browser");
        assert_eq!(args, vec!["https://example.com"]);
    }

    #[test]
    fn substituted_args_are_not_expanded_again() {
        let entry = test_entry("ed %f --opt=%f");
        assert_eq!(
            argv(&entry, &["100%c%k%u.txt"], &[]),
            vec!["ed", "100%c%k%u.txt", "--opt=100%c%k%u.txt"]
        );
    }

    #[test]
    fn expands_embedded_and_literal_codes() {
        let entry = test_entry("prog --name=%c --desktop=%k 100%% %F");
        assert_eq!(
            argv(&entry, &["a", "b"], &[]),
            vec![
                "prog",
                "--name=Test App",
                "--desktop=/tmp/test.desktop",
                "100%",
                "a",
                "b"
            ]
        );
    }

    #[test]
    fn missing_args_remove_tokens_but_keep_other_empty_args() {
        let entry = test_entry(r#"prog %f %U "" %d %D %n %N %v %m"#);
        assert_eq!(argv(&entry, &[], &[]), vec!["prog", ""]);
    }

    #[test]
    fn icon_field_code_expands_to_two_args() {
        let mut entry = test_entry("prog %i %u");
        assert_eq!(argv(&entry, &[], &["x"]), vec!["prog", "x"]);
        entry.raw.insert("Icon".into(), "foo".into());
        assert_eq!(
            argv(&entry, &[], &["x"]),
            vec!["prog", "--icon", "foo", "x"]
        );
    }

    #[test]
    fn reports_dropped_args() {
        let args = strings(&["a", "b"]);
        assert_eq!(route_args("prog", &args, false).dropped, args);
        assert_eq!(route_args("prog %f", &args, false).dropped, strings(&["b"]));
        assert!(route_args("prog %U", &args, false).dropped.is_empty());
        assert_eq!(route_args("prog %%f", &args, false).dropped, args);
    }

    #[test]
    fn urls_prefer_url_codes_and_fall_back_to_file_codes() {
        let url = strings(&["https://example.com"]);
        assert_eq!(route_args("app %f %u", &url, true).urls, url);
        assert_eq!(route_args("app %f %u", &url, false).files, url);
        assert_eq!(route_args("app %f", &url, true).files, url);
        assert_eq!(route_args("app %U", &url, false).urls, url);
    }
}
