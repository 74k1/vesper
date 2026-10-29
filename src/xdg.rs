// ---------------------------------------------------------------------------
// XDG base directories, environment, and small string helpers
// ---------------------------------------------------------------------------

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// An environment value as a path, if set, non-empty and absolute. The XDG
/// basedir spec says relative paths must be ignored.
pub fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// A colon-separated path list; unset or empty falls back to `default`.
pub fn path_list(value: Option<OsString>, default: &str) -> Vec<PathBuf> {
    let value = value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into());
    env::split_paths(&value)
        .filter(|p| p.is_absolute())
        .collect()
}

pub fn home_dir() -> Option<PathBuf> {
    absolute_path(env::var_os("HOME"))
}

pub fn current_desktop() -> Option<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Each desktop name in `$XDG_CURRENT_DESKTOP`, highest priority first.
pub fn desktop_names() -> Vec<String> {
    current_desktop()
        .map(|d| {
            d.split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn xdg_data_home() -> Option<PathBuf> {
    absolute_path(env::var_os("XDG_DATA_HOME"))
        .or_else(|| home_dir().map(|h| h.join(".local/share")))
}

pub fn xdg_config_home() -> Option<PathBuf> {
    absolute_path(env::var_os("XDG_CONFIG_HOME")).or_else(|| home_dir().map(|h| h.join(".config")))
}

pub fn xdg_data_dirs() -> Vec<PathBuf> {
    path_list(env::var_os("XDG_DATA_DIRS"), "/usr/local/share:/usr/share")
}

pub fn xdg_config_dirs() -> Vec<PathBuf> {
    path_list(env::var_os("XDG_CONFIG_DIRS"), "/etc/xdg")
}

/// Resolve a `~/` prefix.
pub fn expand_tilde(s: &str) -> PathBuf {
    if let (Some(rest), Some(home)) = (s.strip_prefix("~/"), home_dir()) {
        return home.join(rest);
    }
    PathBuf::from(s)
}

pub fn is_executable(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Find an executable in PATH, resolving `~`.
pub fn find_binary(cmd: &str) -> Option<PathBuf> {
    if cmd.is_empty() {
        return None;
    }

    if cmd.contains('/') {
        let p = expand_tilde(cmd);
        return is_executable(&p).then_some(p);
    }

    env::split_paths(&env::var_os("PATH")?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(cmd))
        .find(|p| is_executable(p))
}

/// Replace characters that would break the tab-separated output format.
pub fn tsv_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Decode `%XX` escapes in a URI component.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
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

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `$BROWSER` as a list of commands, in order of preference.
pub fn browser_commands() -> Vec<String> {
    env::var("BROWSER")
        .ok()
        .map(|v| {
            v.split(':')
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `$VESPER_FALLBACKS` as a list of commands, in order of preference.
pub fn fallback_commands() -> Vec<String> {
    env::var("VESPER_FALLBACKS")
        .ok()
        .map(|v| {
            v.split(':')
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_ignore_empty_and_relative_values() {
        assert_eq!(absolute_path(Some("".into())), None);
        assert_eq!(absolute_path(Some("./rel".into())), None);
        assert_eq!(
            absolute_path(Some("/abs".into())),
            Some(PathBuf::from("/abs"))
        );
        assert_eq!(
            path_list(Some("".into()), "/a:/b"),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert_eq!(
            path_list(Some("rel:/c::".into()), "/a"),
            vec![PathBuf::from("/c")]
        );
    }

    #[test]
    fn percent_decoding_handles_escapes_and_literals() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("no-escapes"), "no-escapes");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_decode("%2Ftmp%2Ffile"), "/tmp/file");
    }

    #[test]
    fn tsv_fields_lose_separators() {
        assert_eq!(tsv_field("a\tb\nc\rd"), "a b c d");
    }
}
