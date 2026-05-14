use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};
use eyre::{bail, Context, ContextCompat, Result};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "vesper", about = "Simple freedesktop .desktop entry runner")]
struct Cli {
    #[command(subcommand)]
    command: Command_,
}

#[derive(Subcommand)]
enum Command_ {
    /// List desktop entries
    List {
        /// Output only entry IDs
        #[arg(long)]
        ids: bool,
        /// Include NoDisplay entries
        #[arg(long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include icon field in tab-separated output
        #[arg(long)]
        icons: bool,
    },
    /// Run a desktop entry
    Run {
        /// Desktop entry ID or path to .desktop file
        id_or_path: String,
        /// Terminal command (e.g. "foot -e")
        #[arg(long, default_value = "x-terminal-emulator -e")]
        terminal: String,
        /// Extra file/URL arguments passed to the Exec command
        #[arg(long, num_args = 1..)]
        args: Vec<String>,
    },
    /// Inspect a parsed desktop entry
    Inspect {
        /// Desktop entry ID or path to .desktop file
        id_or_path: String,
    },
    /// List autostart entries for a given desktop
    Autostart {
        /// Desktop name (e.g. "niri", "sway", "GNOME")
        #[arg(long)]
        desktop: String,
        /// Run autostart entries instead of listing them
        #[arg(long)]
        run: bool,
        /// Terminal command (e.g. "foot -e")
        #[arg(long, default_value = "x-terminal-emulator -e")]
        terminal: String,
    },
}

// ---------------------------------------------------------------------------
// Desktop entry representation
// ---------------------------------------------------------------------------

macro_rules! desktop_entry_accessors {
    ($($kind:ident $name:ident $key:literal $($default:literal)?);* $(;)?) => {
        $(
            desktop_entry_accessors!(@arm $kind $name $key $($default)?);
        )*
    };
    (@arm str  $name:ident $key:literal $default:literal) => {
        fn $name(&self) -> &str { self.get($key).unwrap_or($default) }
    };
    (@arm opt  $name:ident $key:literal) => {
        fn $name(&self) -> Option<&str> { self.get($key) }
    };
    (@arm bool $name:ident $key:literal) => {
        fn $name(&self) -> bool { self.get($key) == Some("true") }
    };
}

#[derive(Debug, Clone)]
struct DesktopEntry {
    /// Path to the .desktop file on disk
    path: PathBuf,
    /// The keys we care about, raw from the file
    raw: BTreeMap<String, String>,
    /// Computed entry ID (relative path without .desktop, with dirs joined by '-')
    id: String,
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
    }

    /// Returns true if this entry should appear in listings for `desktop`.
    fn visible_for(&self, desktop: Option<&str>) -> bool {
        if self.hidden() {
            return false;
        }

        let desktops: Vec<&str> = desktop
            .unwrap_or("")
            .split(':')
            .filter(|s| !s.is_empty())
            .collect();

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

    /// Check TryExec; returns false if TryExec is set and the binary cannot be found.
    fn try_exec_ok(&self) -> bool {
        match self.try_exec() {
            None | Some("") => true,
            Some(cmd) => find_binary(cmd).is_some(),
        }
    }
}

// ---------------------------------------------------------------------------
// Desktop file parsing
// ---------------------------------------------------------------------------

/// Compute the desktop entry ID for a file.
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

/// Parse a .desktop file, extracting only keys from `[Desktop Entry]`.
fn parse_desktop_file(path: &Path) -> Result<DesktopEntry> {
    parse_desktop_file_with_base(path, None)
}

/// Parse a .desktop file with an optional base dir for computing its entry ID.
fn parse_desktop_file_with_base(path: &Path, base_dir: Option<&Path>) -> Result<DesktopEntry> {
    let f = fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let reader = BufReader::new(f);

    let mut in_entry = false;
    let mut raw = BTreeMap::new();
    let wanted: &[&str] = &[
        "Type",
        "Name",
        "Exec",
        "URL",
        "Terminal",
        "TryExec",
        "Hidden",
        "NoDisplay",
        "OnlyShowIn",
        "NotShowIn",
        "Path",
        "Icon",
    ];

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Group header
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let group = &trimmed[1..trimmed.len() - 1];
            in_entry = group == "Desktop Entry";
            continue;
        }

        if !in_entry {
            continue;
        }

        // Key=Value
        if let Some(eq) = trimmed.find('=') {
            let key = &trimmed[..eq];
            if wanted.contains(&key) {
                let value = &trimmed[eq + 1..];
                raw.insert(key.to_string(), value.to_string());
            }
        }
    }

    let id = desktop_entry_id(path, base_dir);

    Ok(DesktopEntry {
        path: path.to_path_buf(),
        raw,
        id,
    })
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Return all .desktop entry paths from the given directory (recursive).
fn discover_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.extend(discover_dir(&p));
            } else if p.extension().is_some_and(|e| e == "desktop") {
                out.push(p);
            }
        }
    }
    out
}

/// Discover all desktop entries, user dir takes precedence over system dir.
fn discover_all() -> Vec<DesktopEntry> {
    let user_dir = xdg_data_home().join("applications");
    let system_dirs: Vec<PathBuf> = xdg_data_dirs()
        .into_iter()
        .map(|d| d.join("applications"))
        .collect();

    let mut seen: BTreeMap<String, DesktopEntry> = BTreeMap::new();

    // System dirs first
    for dir in &system_dirs {
        for f in discover_dir(dir) {
            if let Ok(e) = parse_desktop_file_with_base(&f, Some(dir)) {
                seen.entry(e.id.clone()).or_insert(e);
            }
        }
    }

    // User dir overrides
    for f in discover_dir(&user_dir) {
        if let Ok(e) = parse_desktop_file_with_base(&f, Some(&user_dir)) {
            seen.insert(e.id.clone(), e);
        }
    }

    seen.into_values().collect()
}

/// Find a specific entry by ID or path.
fn find_entry(id_or_path: &str) -> Result<DesktopEntry> {
    let p = Path::new(id_or_path);

    // Direct path
    if (p.is_absolute() || p.exists()) && p.extension().is_some_and(|e| e == "desktop") {
        return parse_desktop_file(p);
    }

    // Look up by ID, accepting both "foo" and the full "foo.desktop" form.
    let lookup_id = id_or_path.strip_suffix(".desktop").unwrap_or(id_or_path);
    for e in discover_all() {
        if e.id == lookup_id {
            return Ok(e);
        }
    }

    bail!("desktop entry not found: {}", id_or_path)
}

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------

fn autostart_dirs() -> Vec<PathBuf> {
    // Lower-priority dirs first, higher-priority dirs later, so insert() lets
    // user autostart files override system files with the same ID.
    let mut dirs: Vec<PathBuf> = xdg_config_dirs()
        .into_iter()
        .map(|d| d.join("autostart"))
        .collect();
    dirs.reverse();
    dirs.push(xdg_config_home().join("autostart"));
    dirs
}

fn autostart_entries(desktop: &str) -> Vec<DesktopEntry> {
    collect_autostart_entries(&autostart_dirs(), desktop)
}

fn collect_autostart_entries(dirs: &[PathBuf], desktop: &str) -> Vec<DesktopEntry> {
    let mut seen: BTreeMap<String, DesktopEntry> = BTreeMap::new();
    for dir in dirs {
        for f in discover_dir(dir) {
            if let Ok(e) = parse_desktop_file_with_base(&f, Some(dir)) {
                seen.insert(e.id.clone(), e);
            }
        }
    }

    seen.into_values()
        .filter(|e| e.visible_for(Some(desktop)))
        .filter(|e| e.type_() == "Application")
        .collect()
}

// ---------------------------------------------------------------------------
// Exec field-code expansion
// ---------------------------------------------------------------------------

/// Split an Exec value into tokens, respecting double-quotes and backslash escapes.
fn tokenize_exec(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = exec.chars().peekable();
    let mut in_quotes = false;
    let mut in_token = false;

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                in_token = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push('\\');
                }
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

    if in_token {
        tokens.push(current);
    }

    tokens
}

/// Expand field codes in a single argument.
/// Returns None if the argument should be removed entirely.
/// For %F and %U, if the token is exactly the field code, returns
/// a special marker that build_command expands into multiple args.
fn expand_field_code(
    arg: &str,
    name: &str,
    desktop_path: &Path,
    file_args: &[String],
    url_args: &[String],
) -> Option<String> {
    if !arg.contains('%') {
        return Some(arg.to_string());
    }

    let mut result = arg.to_string();

    // Handle %% first — use placeholder to avoid double-processing
    result = result.replace("%%", "\x00");

    // Single file
    if result.contains("%f") {
        if file_args.is_empty() {
            if result == "%f" {
                return None;
            }
            result = result.replace("%f", "");
        } else {
            result = result.replace("%f", &file_args[0]);
        }
    }
    // Multiple files
    if result.contains("%F") {
        if file_args.is_empty() {
            if result == "%F" {
                return None;
            }
            result = result.replace("%F", "");
        } else {
            result = result.replace("%F", &file_args.join(" "));
        }
    }

    // Single URL
    if result.contains("%u") {
        if url_args.is_empty() {
            if result == "%u" {
                return None;
            }
            result = result.replace("%u", "");
        } else {
            result = result.replace("%u", &url_args[0]);
        }
    }
    // Multiple URLs
    if result.contains("%U") {
        if url_args.is_empty() {
            if result == "%U" {
                return None;
            }
            result = result.replace("%U", "");
        } else {
            result = result.replace("%U", &url_args.join(" "));
        }
    }

    // Translated name
    result = result.replace("%c", name);

    // Desktop file path
    result = result.replace("%k", &desktop_path.to_string_lossy());

    // Restore %%
    result = result.replace('\x00', "%");

    // Remove if it became empty
    if result.is_empty() {
        return None;
    }

    Some(result)
}

/// Build the command to execute from an entry's Exec field.
fn build_command(
    entry: &DesktopEntry,
    file_args: &[String],
    url_args: &[String],
) -> Result<(String, Vec<String>)> {
    let exec = entry.exec().context("entry has no Exec key")?;
    let name = entry.name();

    let tokens = tokenize_exec(exec);
    let mut expanded: Vec<String> = Vec::new();

    for token in &tokens {
        // Standalone %F expands to multiple arguments
        if token == "%F" && !file_args.is_empty() {
            expanded.extend(file_args.iter().cloned());
            continue;
        }
        // Standalone %U expands to multiple arguments
        if token == "%U" && !url_args.is_empty() {
            expanded.extend(url_args.iter().cloned());
            continue;
        }

        if let Some(s) = expand_field_code(token, name, &entry.path, file_args, url_args) {
            expanded.push(s);
        }
    }

    if expanded.is_empty() {
        bail!("Exec expands to nothing (missing file/URL args?)");
    }

    let program = expanded.remove(0);
    Ok((program, expanded))
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

fn exec_has_field_code(exec: &str, code: char) -> bool {
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

fn split_extra_args_for_exec(exec: &str, extra_args: &[String]) -> (Vec<String>, Vec<String>) {
    let wants_files = exec_has_field_code(exec, 'f') || exec_has_field_code(exec, 'F');
    let wants_urls = exec_has_field_code(exec, 'u') || exec_has_field_code(exec, 'U');

    if wants_urls && !wants_files {
        (Vec::new(), extra_args.to_vec())
    } else {
        (extra_args.to_vec(), Vec::new())
    }
}

fn apply_entry_current_dir(cmd: &mut Command, entry: &DesktopEntry) {
    if let Some(p) = entry.path_() {
        let expanded = expand_tilde(p);
        cmd.current_dir(Path::new(&expanded));
    }
}

fn command_for_entry(
    entry: &DesktopEntry,
    terminal_cmd: &str,
    extra_args: &[String],
) -> Result<Command> {
    let type_ = entry.type_();

    match type_ {
        "Application" => {
            if !entry.try_exec_ok() {
                bail!("TryExec not found: {}", entry.try_exec().unwrap_or(""));
            }

            let exec = entry.exec().context("entry has no Exec key")?;
            let (file_args, url_args) = split_extra_args_for_exec(exec, extra_args);
            let (program, args) = build_command(entry, &file_args, &url_args)?;

            let terminal_tokens = tokenize_exec(terminal_cmd);
            let use_terminal = entry.terminal() && !terminal_tokens.is_empty();

            let mut cmd = if use_terminal {
                let mut cmd = Command::new(&terminal_tokens[0]);
                cmd.args(&terminal_tokens[1..]);
                cmd.arg(program);
                cmd.args(args);
                cmd
            } else {
                let mut cmd = Command::new(&program);
                cmd.args(args);
                cmd
            };

            apply_entry_current_dir(&mut cmd, entry);
            Ok(cmd)
        }

        "Link" => {
            let url = entry.url().context("Link entry has no URL key")?;
            let mut cmd = Command::new("xdg-open");
            cmd.arg(url);
            Ok(cmd)
        }

        "Directory" => {
            bail!("Directory entries are not runnable")
        }

        _ => {
            bail!("unknown Type: {}", type_)
        }
    }
}

fn run_entry(entry: &DesktopEntry, terminal_cmd: &str, extra_args: &[String]) -> Result<()> {
    let mut cmd = command_for_entry(entry, terminal_cmd, extra_args)?;
    Err(cmd.exec().into())
}

fn spawn_entry(entry: &DesktopEntry, terminal_cmd: &str, extra_args: &[String]) -> Result<()> {
    let mut cmd = command_for_entry(entry, terminal_cmd, extra_args)?;
    cmd.spawn()
        .with_context(|| format!("failed to spawn {}", entry.name()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Inspect output
// ---------------------------------------------------------------------------

fn inspect_entry(entry: &DesktopEntry) {
    println!("Path:       {}", entry.path.display());
    println!("ID:         {}", entry.id);
    println!("Type:       {}", entry.type_());
    println!("Name:       {}", entry.name());
    println!("Exec:       {}", entry.exec().unwrap_or("(none)"));
    println!("URL:        {}", entry.url().unwrap_or("(none)"));
    println!("Terminal:   {}", entry.terminal());
    println!("TryExec:    {}", entry.try_exec().unwrap_or("(none)"));
    println!("Hidden:     {}", entry.hidden());
    println!("NoDisplay:  {}", entry.no_display());
    println!("OnlyShowIn: {}", entry.only_show_in());
    println!("NotShowIn:  {}", entry.not_show_in());
    println!("Path:       {}", entry.path_().unwrap_or("(none)"));
    println!("Icon:       {}", entry.icon().unwrap_or("(none)"));

    let current_desktop = current_desktop();
    println!(
        "Visible (current desktop {}): {}",
        current_desktop.as_deref().unwrap_or("?"),
        entry.visible_for(current_desktop.as_deref())
    );
    println!("TryExec OK: {}", entry.try_exec_ok());

    match entry.type_() {
        "Application" => {
            if entry.exec().is_some() {
                if let Ok((prog, args)) = build_command(entry, &[], &[]) {
                    println!("Action:     exec {} {:?}", prog, args);
                    if entry.terminal() {
                        println!("            (in terminal)");
                    }
                }
            }
        }
        "Link" => {
            println!("Action:     xdg-open {}", entry.url().unwrap_or("?"));
        }
        _ => {
            println!("Action:     (cannot run type={})", entry.type_());
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve `~/` prefix
fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return Path::new(&home).join(rest).to_string_lossy().to_string();
        }
    }
    s.to_string()
}

fn home_dir() -> Option<String> {
    env::var("HOME").ok()
}

fn current_desktop() -> Option<String> {
    env::var("XDG_CURRENT_DESKTOP").ok()
}

fn xdg_data_home() -> PathBuf {
    env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            home_dir()
                .map(|h| PathBuf::from(h).join(".local/share"))
                .unwrap_or_default()
        })
}

fn xdg_config_home() -> PathBuf {
    env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            home_dir()
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_default()
        })
}

fn desktop_list_matches(list: &str, desktops: &[&str]) -> bool {
    list.split(';')
        .filter(|s| !s.is_empty())
        .any(|candidate| desktops.contains(&candidate))
}

// Minimal XDG helpers

fn xdg_data_dirs() -> Vec<PathBuf> {
    let raw = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn xdg_config_dirs() -> Vec<PathBuf> {
    let raw = env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Find a binary in PATH, resolving `~`.
fn find_binary(cmd: &str) -> Option<PathBuf> {
    if cmd.is_empty() {
        return None;
    }

    let expanded = expand_tilde(cmd);

    // If it contains a path separator, treat as a direct path
    if expanded.contains('/') {
        let p = Path::new(&expanded);
        if p.exists() {
            return Some(p.to_path_buf());
        }
        return None;
    }

    // Search PATH
    let path_env = env::var("PATH").ok()?;
    for dir in path_env.split(':') {
        let full = Path::new(dir).join(&expanded);
        if full.exists() {
            return Some(full);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_entry(exec: &str) -> DesktopEntry {
        let mut raw = BTreeMap::new();
        raw.insert("Type".into(), "Application".into());
        raw.insert("Name".into(), "Test App".into());
        raw.insert("Exec".into(), exec.into());

        DesktopEntry {
            path: PathBuf::from("/tmp/test.desktop"),
            raw,
            id: "test".into(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("vesper-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn tokenizes_quotes_and_escapes() {
        let tokens = tokenize_exec(
            r#"prog "arg with spaces" arg\ with\ spaces "quote \"inside\"" back\\slash"#,
        );
        assert_eq!(
            tokens,
            vec![
                "prog",
                "arg with spaces",
                "arg with spaces",
                "quote \"inside\"",
                "back\\slash",
            ]
        );
    }

    #[test]
    fn url_field_codes_receive_extra_args_as_urls() {
        let entry = test_entry("browser %u");
        let extra = vec!["https://example.com".to_string()];
        let (file_args, url_args) = split_extra_args_for_exec(entry.exec().unwrap(), &extra);

        assert!(file_args.is_empty());
        assert_eq!(url_args, extra);

        let (program, args) = build_command(&entry, &file_args, &url_args).unwrap();
        assert_eq!(program, "browser");
        assert_eq!(args, vec!["https://example.com"]);
    }

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
    fn autostart_user_hidden_override_suppresses_system_entry() {
        let root = temp_dir("autostart-override");
        let system = root.join("system/autostart");
        let user = root.join("user/autostart");
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&user).unwrap();

        fs::write(
            system.join("thing.desktop"),
            "[Desktop Entry]\nType=Application\nName=System Thing\nExec=thing\n",
        )
        .unwrap();
        fs::write(
            user.join("thing.desktop"),
            "[Desktop Entry]\nType=Application\nName=User Override\nExec=thing\nHidden=true\n",
        )
        .unwrap();

        let entries = collect_autostart_entries(&[system, user], "niri");
        assert!(entries.is_empty());

        let _ = fs::remove_dir_all(root);
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Command_::List {
            ids,
            all,
            json,
            icons,
        } => {
            let entries = discover_all();
            let current_desktop = current_desktop();
            let mut filtered: Vec<&DesktopEntry> = entries
                .iter()
                .filter(|e| e.visible_for(current_desktop.as_deref()))
                .filter(|e| all || !e.no_display())
                .filter(|e| e.type_() == "Application" || e.type_() == "Link")
                .collect();
            filtered.sort_by(|a, b| a.name().cmp(b.name()));

            if json {
                let v: Vec<serde_json::Value> = filtered
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id,
                            "name": e.name(),
                            "type": e.type_(),
                            "exec": e.exec(),
                            "url": e.url(),
                            "icon": e.icon(),
                            "path": e.path.display().to_string(),
                            "terminal": e.terminal(),
                            "no_display": e.no_display(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if ids {
                for e in &filtered {
                    println!("{}", e.id);
                }
            } else if icons {
                for e in &filtered {
                    println!("{}\t{}\t{}", e.name(), e.id, e.icon().unwrap_or(""));
                }
            } else {
                for e in &filtered {
                    println!("{}\t{}", e.name(), e.id);
                }
            }
        }

        Command_::Run {
            id_or_path,
            terminal,
            args,
        } => {
            let entry = find_entry(&id_or_path)?;
            run_entry(&entry, &terminal, &args)?;
        }

        Command_::Inspect { id_or_path } => {
            let entry = find_entry(&id_or_path)?;
            inspect_entry(&entry);
        }

        Command_::Autostart {
            desktop,
            run,
            terminal,
        } => {
            let mut entries = autostart_entries(&desktop);
            entries.sort_by(|a, b| a.name().cmp(b.name()));

            // TryExec filter
            entries.retain(|e| e.try_exec_ok());

            if run {
                if entries.is_empty() {
                    eprintln!("No autostart entries for desktop '{}'", desktop);
                }
                for e in &entries {
                    eprintln!("Running: {} ({})", e.name(), e.path.display());
                    if let Err(err) = spawn_entry(e, &terminal, &[]) {
                        eprintln!("  error: {}", err);
                    }
                }
            } else {
                for e in &entries {
                    println!("{}\t{}", e.name(), e.id);
                }
            }
        }
    }

    Ok(())
}
