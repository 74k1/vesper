// ---------------------------------------------------------------------------
// Desktop entry representation, parsing, discovery, and autostart
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result};

use crate::xdg;

// ---------------------------------------------------------------------------
// Representation
// ---------------------------------------------------------------------------

macro_rules! desktop_entry_accessors {
    ($($kind:ident $name:ident $key:literal $($default:literal)?);* $(;)?) => {
        $(
            desktop_entry_accessors!(@arm $kind $name $key $($default)?);
        )*
    };
    (@arm str  $name:ident $key:literal $default:literal) => {
        pub fn $name(&self) -> &str { self.get($key).unwrap_or($default) }
    };
    (@arm opt  $name:ident $key:literal) => {
        pub fn $name(&self) -> Option<&str> { self.get($key) }
    };
    (@arm bool $name:ident $key:literal) => {
        pub fn $name(&self) -> bool { self.get($key) == Some("true") }
    };
}

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub path: PathBuf,
    /// All `[Desktop Entry]` keys, with string escapes decoded
    pub raw: BTreeMap<String, String>,
    /// Computed entry ID (relative path without .desktop, with dirs joined by '-')
    pub id: String,
}

impl DesktopEntry {
    fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(|s| s.as_str())
    }

    desktop_entry_accessors! {
        str  type_        "Type"        "Application";
        str  name         "Name"        "(unnamed)";
        opt  exec         "Exec";
        opt  url          "URL";
        bool terminal     "Terminal";
        opt  try_exec     "TryExec";
        bool hidden       "Hidden";
        bool no_display   "NoDisplay";
        str  only_show_in "OnlyShowIn"  "";
        str  not_show_in  "NotShowIn"   "";
        opt  path_        "Path";
        opt  icon         "Icon";
        opt  mime_type    "MimeType";
    }

    /// The ID as `mimeapps.list` and xdg-utils spell it.
    pub fn desktop_file_id(&self) -> String {
        format!("{}.desktop", self.id)
    }

    pub fn mime_types(&self) -> impl Iterator<Item = &str> {
        split_list(self.mime_type().unwrap_or(""))
    }

    pub fn is_launchable(&self) -> bool {
        let non_empty = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
        non_empty(self.get("Name"))
            && match self.type_() {
                "Application" => non_empty(self.exec()),
                "Link" => non_empty(self.url()),
                _ => false,
            }
    }

    pub fn visible_for(&self, desktop: Option<&str>) -> bool {
        if self.hidden() {
            return false;
        }

        let desktops: Vec<&str> = xdg::desktop_names(desktop.unwrap_or("")).collect();

        let only = self.only_show_in();
        if !only.is_empty() && (desktops.is_empty() || !desktop_list_matches(only, &desktops)) {
            return false;
        }

        let not = self.not_show_in();
        if !not.is_empty() && desktop_list_matches(not, &desktops) {
            return false;
        }

        true
    }

    pub fn try_exec_ok(&self) -> bool {
        match self.try_exec() {
            None | Some("") => true,
            Some(cmd) => xdg::find_binary(cmd).is_some(),
        }
    }
}

fn desktop_list_matches(list: &str, desktops: &[&str]) -> bool {
    split_list(list).any(|candidate| desktops.contains(&candidate))
}

pub fn split_list(value: &str) -> impl Iterator<Item = &str> {
    value.split(';').map(str::trim).filter(|s| !s.is_empty())
}

fn name_key(entry: &DesktopEntry) -> (String, String) {
    (entry.name().to_lowercase(), entry.id.clone())
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// The group a `[Group Name]` header line opens.
pub fn group_name(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

/// Parse an INI-style document into its groups, preserving group order.
/// Keys before the first group are ignored, as the spec requires. Values are
/// returned as written, without escape decoding.
pub fn parse_ini_groups(content: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut groups: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    let mut current: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(name) = group_name(trimmed) {
            let name = name.to_string();
            let index = groups
                .iter()
                .position(|(existing, _)| *existing == name)
                .unwrap_or_else(|| {
                    groups.push((name, BTreeMap::new()));
                    groups.len() - 1
                });
            current = Some(index);
            continue;
        }

        let Some(index) = current else {
            continue;
        };

        if let Some((key, value)) = trimmed.split_once('=') {
            groups[index]
                .1
                .insert(key.trim_end().to_string(), value.trim_start().to_string());
        }
    }

    groups
}

fn desktop_entry_group(content: &str) -> BTreeMap<String, String> {
    parse_ini_groups(content)
        .into_iter()
        .find(|(name, _)| name == "Desktop Entry")
        .map(|(_, keys)| keys)
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| (key, unescape_value(&value)))
        .collect()
}

/// Decode the string escapes `\s`, `\n`, `\t`, `\r` and `\\`. Other escapes
/// (like `\;` in lists or `\"` in Exec) are left for later stages.
fn unescape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}

fn desktop_entry_id(path: &Path, base_dir: Option<&Path>) -> String {
    let mut id = if let Some(base) = base_dir {
        path.strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('/', "-")
    } else {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };

    if let Some(stripped) = id.strip_suffix(".desktop") {
        id = stripped.to_string();
    }

    id
}

fn parse_desktop_file(path: &Path, base_dir: Option<&Path>) -> Result<DesktopEntry> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;

    Ok(DesktopEntry {
        path: path.to_path_buf(),
        raw: desktop_entry_group(&String::from_utf8_lossy(&bytes)),
        id: desktop_entry_id(path, base_dir),
    })
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Return all .desktop entry paths under `dir` (recursive), shallowest first.
fn discover_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_dir(dir, &mut HashSet::new(), &mut out);
    out.sort_by_cached_key(|p| (p.components().count(), p.clone()));
    out
}

fn walk_dir(dir: &Path, visited: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
    let Ok(canonical) = fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_dir(&p, visited, out);
        } else if p.extension().is_some_and(|e| e == "desktop") {
            out.push(p);
        }
    }
}

/// Collect entries from `dirs`, highest priority first: the first dir
/// providing an ID wins.
fn collect_entries(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut seen: BTreeMap<String, DesktopEntry> = BTreeMap::new();
    for dir in dirs {
        for f in discover_dir(dir) {
            if let Ok(e) = parse_desktop_file(&f, Some(dir)) {
                seen.entry(e.id.clone()).or_insert(e);
            }
        }
    }
    seen.into_values().collect()
}

/// `$XDG_DATA_HOME/applications` followed by `$XDG_DATA_DIRS/applications`.
pub fn application_dirs() -> Vec<PathBuf> {
    xdg::data_dirs()
        .into_iter()
        .map(|d| d.join("applications"))
        .collect()
}

pub fn discover_all() -> Vec<DesktopEntry> {
    collect_entries(&application_dirs())
}

/// Entries shown by `list`, sorted case-insensitively by name.
pub fn list_entries<'a>(
    entries: &'a [DesktopEntry],
    desktop: Option<&str>,
    all: bool,
) -> Vec<&'a DesktopEntry> {
    let mut out: Vec<&DesktopEntry> = entries
        .iter()
        .filter(|e| e.is_launchable())
        .filter(|e| e.visible_for(desktop))
        .filter(|e| all || (!e.no_display() && e.try_exec_ok()))
        .collect();
    out.sort_by_cached_key(|e| name_key(e));
    out
}

/// Find an entry by path, or by ID with or without the `.desktop` suffix.
pub fn find_entry(id_or_path: &str) -> Result<DesktopEntry> {
    let p = Path::new(id_or_path);

    if (p.is_absolute() || p.exists()) && p.extension().is_some_and(|e| e == "desktop") {
        return parse_desktop_file(p, None);
    }

    find_by_id(id_or_path).with_context(|| format!("desktop entry not found: {}", id_or_path))
}

/// Find the entry that discovery assigns `id` to. Never reads outside the
/// application directories.
pub fn find_by_id(id: &str) -> Option<DesktopEntry> {
    find_in(&application_dirs(), id)
}

fn find_in(dirs: &[PathBuf], id: &str) -> Option<DesktopEntry> {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    dirs.iter().find_map(|dir| {
        discover_dir(dir)
            .into_iter()
            .filter(|path| desktop_entry_id(path, Some(dir)) == id)
            .find_map(|path| parse_desktop_file(&path, Some(dir)).ok())
    })
}

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------

/// `$XDG_CONFIG_HOME/autostart` followed by `$XDG_CONFIG_DIRS/autostart`.
pub fn autostart_dirs() -> Vec<PathBuf> {
    xdg::config_dirs()
        .into_iter()
        .map(|d| d.join("autostart"))
        .collect()
}

/// Dedupe by priority first, so a hidden user override suppresses the
/// system entry, then filter.
pub fn autostart_entries(dirs: &[PathBuf], desktop: &str) -> Vec<DesktopEntry> {
    let mut entries: Vec<DesktopEntry> = collect_entries(dirs)
        .into_iter()
        .filter(|e| e.visible_for(Some(desktop)))
        .filter(|e| e.type_() == "Application" && e.is_launchable())
        .filter(|e| e.try_exec_ok())
        .collect();
    entries.sort_by_cached_key(name_key);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_entry, TempDir};

    fn app(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=prog\n")
    }

    // -- parsing ------------------------------------------------------------

    #[test]
    fn reads_only_the_desktop_entry_group() {
        let dir = TempDir::new("groups");
        let path = dir.write(
            "app.desktop",
            "Name=Before\n# comment\n[Other]\nName=Wrong\n[Desktop Entry]\nName = Right\r\n\
             Exec= prog  \nX-Custom=yes\n[Desktop Action new]\nExec=other\n",
        );

        let entry = parse_desktop_file(&path, None).unwrap();
        assert_eq!(entry.name(), "Right");
        assert_eq!(entry.exec(), Some("prog"));
        assert_eq!(entry.raw.get("X-Custom").unwrap(), "yes");
    }

    #[test]
    fn decodes_string_escapes() {
        let dir = TempDir::new("escapes");
        let path = dir.write(
            "app.desktop",
            "[Desktop Entry]\nName=a\\sb\\tc\\\\d\\;e\nExec=prog \"a\\\\\\\\b\"\n",
        );

        let entry = parse_desktop_file(&path, None).unwrap();
        assert_eq!(entry.name(), "a b\tc\\d\\;e");
        // One level of escaping is left for the Exec tokenizer.
        assert_eq!(entry.exec(), Some(r#"prog "a\\b""#));
    }

    #[test]
    fn invalid_utf8_elsewhere_does_not_drop_entry() {
        let dir = TempDir::new("utf8");
        let path = dir.0.join("app.desktop");
        let mut content = app("App").into_bytes();
        content.extend_from_slice(b"Name[de]=\xff\xfe\n");
        fs::write(&path, content).unwrap();

        let entry = parse_desktop_file(&path, None).unwrap();
        assert_eq!(entry.name(), "App");
        assert_eq!(entry.exec(), Some("prog"));
    }

    #[test]
    fn mime_types_split_on_semicolons() {
        let mut entry = test_entry("app");
        entry.raw.insert(
            "MimeType".into(),
            "text/html;x-scheme-handler/http;;".into(),
        );
        assert_eq!(
            entry.mime_types().collect::<Vec<_>>(),
            vec!["text/html", "x-scheme-handler/http"]
        );
    }

    // -- discovery ----------------------------------------------------------

    #[test]
    fn desktop_ids_include_relative_subdirs_without_extension() {
        assert_eq!(
            desktop_entry_id(
                Path::new("/base/applications/foo/bar.desktop"),
                Some(Path::new("/base/applications")),
            ),
            "foo-bar"
        );
        assert_eq!(
            desktop_entry_id(
                Path::new("/base/applications/bar.desktop"),
                Some(Path::new("/base/applications")),
            ),
            "bar"
        );
        assert_eq!(
            desktop_entry_id(Path::new("./fixtures/applications/test.desktop"), None),
            "test"
        );
    }

    #[test]
    fn earlier_dirs_and_flat_files_take_priority() {
        let dir = TempDir::new("priority");
        dir.write("user/thing.desktop", &app("User"));
        dir.write("system/thing.desktop", &app("System"));
        dir.write("system/foo-bar.desktop", &app("Flat"));
        dir.write("system/foo/bar.desktop", &app("Nested"));

        let dirs = [dir.0.join("user"), dir.0.join("system")];
        let entries = collect_entries(&dirs);
        let name = |id: &str| {
            entries
                .iter()
                .find(|e| e.id == id)
                .unwrap()
                .name()
                .to_string()
        };
        assert_eq!(name("thing"), "User");
        assert_eq!(name("foo-bar"), "Flat");

        // Lookup by ID follows the same rules as discovery.
        let found = |id: &str| find_in(&dirs, id).map(|e| e.name().to_string());
        assert_eq!(found("thing").unwrap(), "User");
        assert_eq!(found("thing.desktop").unwrap(), "User");
        assert_eq!(found("foo-bar").unwrap(), "Flat");
        fs::remove_file(dir.0.join("system/foo-bar.desktop")).unwrap();
        assert_eq!(found("foo-bar").unwrap(), "Nested");
        assert_eq!(found("../user/thing"), None);
    }

    #[test]
    fn symlink_loops_are_visited_once() {
        let dir = TempDir::new("symlink");
        dir.write("apps/a.desktop", &app("A"));
        std::os::unix::fs::symlink("..", dir.0.join("apps/loop")).unwrap();

        let found = discover_dir(&dir.0.join("apps"));
        assert_eq!(found.len(), 1);
    }

    // -- filtering ----------------------------------------------------------

    #[test]
    fn visibility_respects_current_desktop_lists() {
        let mut entry = test_entry("app");
        entry.raw.insert("OnlyShowIn".into(), "niri;sway;".into());
        assert!(entry.visible_for(Some("niri")));
        assert!(entry.visible_for(Some("GNOME:niri")));
        assert!(!entry.visible_for(Some("GNOME")));
        assert!(!entry.visible_for(None));

        entry.raw.remove("OnlyShowIn");
        entry.raw.insert("NotShowIn".into(), "GNOME;".into());
        assert!(!entry.visible_for(Some("GNOME:niri")));
        assert!(entry.visible_for(Some("niri")));
    }

    #[test]
    fn list_filters_fixtures() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/applications");
        let entries = collect_entries(&[fixtures]);
        let ids = |desktop, all| -> Vec<String> {
            list_entries(&entries, Some(desktop), all)
                .iter()
                .map(|e| e.id.clone())
                .collect()
        };

        assert_eq!(
            ids("niri", false),
            vec![
                "test-autostart",
                "test-browser",
                "test-quoted",
                "test-fieldcodes",
                "test-editor",
                "test-niri-only",
                "test-not-gnome",
                "test-terminal",
                "test-tryexec-ok",
                "test-link",
            ]
        );

        let gnome = ids("GNOME", true);
        assert!(gnome.contains(&"test-nodisplay".to_string()));
        assert!(gnome.contains(&"test-tryexec-missing".to_string()));
        assert!(!gnome.contains(&"test-niri-only".to_string()));
        assert!(!gnome.contains(&"test-not-gnome".to_string()));
        assert!(!gnome.contains(&"test-hidden".to_string()));
        assert!(!gnome.contains(&"test-directory".to_string()));
    }

    #[test]
    fn list_sorts_case_insensitively() {
        let mut a = test_entry("a");
        a.raw.insert("Name".into(), "zathura".into());
        let mut b = test_entry("b");
        b.raw.insert("Name".into(), "Alacritty".into());
        let mut c = test_entry("c");
        c.raw.insert("Name".into(), "btop".into());
        let entries = [a, b, c];
        let names: Vec<&str> = list_entries(&entries, None, true)
            .iter()
            .map(|e| e.name())
            .collect();
        assert_eq!(names, vec!["Alacritty", "btop", "zathura"]);
    }

    #[test]
    fn autostart_user_hidden_override_suppresses_system_entry() {
        let dir = TempDir::new("autostart-override");
        dir.write(
            "system/autostart/thing.desktop",
            "[Desktop Entry]\nType=Application\nName=System Thing\nExec=thing\n",
        );
        dir.write(
            "user/autostart/thing.desktop",
            "[Desktop Entry]\nType=Application\nName=User Override\nExec=thing\nHidden=true\n",
        );

        let dirs = [dir.0.join("user/autostart"), dir.0.join("system/autostart")];
        assert!(autostart_entries(&dirs, "niri").is_empty());
        let system_only = autostart_entries(&dirs[1..], "niri");
        assert_eq!(system_only.len(), 1);
        assert_eq!(system_only[0].id, "thing");
    }
}
