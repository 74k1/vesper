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

    // Drop tokens that consisted only of field codes expanding to nothing
    if out.is_empty() && had_code {
        Vec::new()
    } else {
        vec![out]
    }
}

/// Build the command to execute from an entry's Exec field.
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

/// True if the Exec value contains the given field code.
pub fn has_field_code(exec: &str, code: char) -> bool {
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

/// Route extra arguments to file or URL field codes. Returns
/// `(file_args, url_args, dropped)`, where `dropped` are the arguments that
/// match no placeholder.
pub fn route_extra_args(
    exec: &str,
    extra_args: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let wants_files = has_field_code(exec, 'f') || has_field_code(exec, 'F');
    let wants_urls = has_field_code(exec, 'u') || has_field_code(exec, 'U');

    let (file_args, url_args) = if wants_urls && !wants_files {
        (Vec::new(), extra_args.to_vec())
    } else {
        (extra_args.to_vec(), Vec::new())
    };

    let dropped = if has_field_code(exec, 'F') || has_field_code(exec, 'U') {
        Vec::new()
    } else if wants_files || wants_urls {
        extra_args.get(1..).unwrap_or(&[]).to_vec()
    } else {
        extra_args.to_vec()
    };

    (file_args, url_args, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn test_entry(exec: &str) -> DesktopEntry {
        let mut raw = BTreeMap::new();
        raw.insert("Type".into(), "Application".into());
        raw.insert("Name".into(), "Test App".into());
        raw.insert("Exec".into(), exec.into());

        DesktopEntry::new(PathBuf::from("/tmp/test.desktop"), raw, "test".into())
    }

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
        let (file_args, url_args, dropped) = route_extra_args(entry.exec().unwrap(), &extra);

        assert!(file_args.is_empty());
        assert_eq!(url_args, extra);
        assert!(dropped.is_empty());

        let (program, args) = build_command(&entry, &file_args, &url_args).unwrap();
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
        assert_eq!(route_extra_args("prog", &args).2, args);
        assert_eq!(route_extra_args("prog %f", &args).2, strings(&["b"]));
        assert!(route_extra_args("prog %U", &args).2.is_empty());
        assert_eq!(route_extra_args("prog %%f", &args).2, args);
    }
}
