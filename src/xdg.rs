// ---------------------------------------------------------------------------
// XDG base directories and environment lookups
// ---------------------------------------------------------------------------

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// An environment value as a path, if set, non-empty and absolute. The XDG
/// basedir spec says relative paths must be ignored.
fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// A colon-separated path list; unset or empty falls back to `default`.
fn path_list(value: Option<OsString>, default: &str) -> Vec<PathBuf> {
    let value = value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into());
    env::split_paths(&value)
        .filter(|p| p.is_absolute())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    absolute_path(env::var_os("HOME"))
}

fn home_or(var: &str, fallback: &str) -> Option<PathBuf> {
    absolute_path(env::var_os(var)).or_else(|| home_dir().map(|home| home.join(fallback)))
}

pub fn config_home() -> Option<PathBuf> {
    home_or("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME` followed by `$XDG_DATA_DIRS`, highest priority first.
pub fn data_dirs() -> Vec<PathBuf> {
    home_or("XDG_DATA_HOME", ".local/share")
        .into_iter()
        .chain(path_list(
            env::var_os("XDG_DATA_DIRS"),
            "/usr/local/share:/usr/share",
        ))
        .collect()
}

/// `$XDG_CONFIG_HOME` followed by `$XDG_CONFIG_DIRS`, highest priority first.
pub fn config_dirs() -> Vec<PathBuf> {
    config_home()
        .into_iter()
        .chain(path_list(env::var_os("XDG_CONFIG_DIRS"), "/etc/xdg"))
        .collect()
}

pub fn current_desktop() -> Option<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Each desktop name in an `$XDG_CURRENT_DESKTOP` value, highest priority first.
pub fn desktop_names(value: &str) -> impl Iterator<Item = &str> {
    value.split(':').filter(|s| !s.is_empty())
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if let (Some(rest), Some(home)) = (s.strip_prefix("~/"), home_dir()) {
        return home.join(rest);
    }
    PathBuf::from(s)
}

fn is_executable(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Find an executable: in PATH for a bare name, otherwise at the given
/// path (with `~/` resolved).
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
}
