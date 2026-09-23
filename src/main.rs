use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::{Parser, Subcommand};
use eyre::{bail, Context, ContextCompat, Result};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "vesper", about = "Simple freedesktop .desktop entry runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List desktop entries
    List {
        /// Output only entry IDs
        #[arg(long, conflicts_with_all = ["json", "icons"])]
        ids: bool,
        /// Include NoDisplay entries and entries whose TryExec is missing
        #[arg(long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include icon field in tab-separated output
        #[arg(long, conflicts_with = "json")]
        icons: bool,
    },
    /// Run a desktop entry
    Run {
        /// Desktop entry ID or path to .desktop file
        id_or_path: String,
        /// Terminal command (e.g. "foot -e") [default: "$TERMINAL -e", xdg-terminal-exec, "x-terminal-emulator -e"]
        #[arg(long)]
        terminal: Option<String>,
        /// Detach from the current terminal (new session, stdio to /dev/null)
        #[arg(long)]
        detach: bool,
        /// Extra file/URL arguments passed to the Exec command (after --)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Inspect a parsed desktop entry
    Inspect {
        /// Desktop entry ID or path to .desktop file
        id_or_path: String,
        /// Terminal command used to show the resulting action
        #[arg(long)]
        terminal: Option<String>,
    },
    /// List autostart entries for a given desktop
    Autostart {
        /// Desktop name (e.g. "niri", "sway", "GNOME") [default: $XDG_CURRENT_DESKTOP]
        #[arg(long)]
        desktop: Option<String>,
        /// Run autostart entries instead of listing them
        #[arg(long)]
        run: bool,
        /// Terminal command (e.g. "foot -e")
        #[arg(long)]
        terminal: Option<String>,
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
    /// All `[Desktop Entry]` keys, with string escapes decoded
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

    /// Returns true if the entry has the keys required to launch it.
    fn is_launchable(&self) -> bool {
        let non_empty = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
        non_empty(self.get("Name"))
            && match self.type_() {
                "Application" => non_empty(self.exec()),
                "Link" => non_empty(self.url()),
                _ => false,
            }
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

/// Parse a .desktop file with an optional base dir for computing its entry ID.
fn parse_desktop_file(path: &Path, base_dir: Option<&Path>) -> Result<DesktopEntry> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;

    Ok(DesktopEntry {
        path: path.to_path_buf(),
        raw: parse_desktop_entry_group(&String::from_utf8_lossy(&bytes)),
        id: desktop_entry_id(path, base_dir),
    })
}

/// Extract the keys of the `[Desktop Entry]` group.
fn parse_desktop_entry_group(content: &str) -> BTreeMap<String, String> {
    let mut in_entry = false;
    let mut raw = BTreeMap::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Group header
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_entry {
                break;
            }
            in_entry = &trimmed[1..trimmed.len() - 1] == "Desktop Entry";
            continue;
        }

        if !in_entry {
            continue;
        }

        // Key=Value, whitespace around '=' is ignored
        if let Some((key, value)) = trimmed.split_once('=') {
            raw.insert(
                key.trim_end().to_string(),
                unescape_value(value.trim_start()),
            );
        }
    }

    raw
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
    // Guard against symlink loops
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
fn application_dirs() -> Vec<PathBuf> {
    xdg_data_home()
        .into_iter()
        .chain(xdg_data_dirs())
        .map(|d| d.join("applications"))
        .collect()
}

fn discover_all() -> Vec<DesktopEntry> {
    collect_entries(&application_dirs())
}

/// Entries shown by `list`, sorted case-insensitively by name.
fn list_entries<'a>(
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
    out.sort_by_cached_key(|e| (e.name().to_lowercase(), e.id.clone()));
    out
}

/// Find a specific entry by ID or path.
fn find_entry(id_or_path: &str) -> Result<DesktopEntry> {
    let p = Path::new(id_or_path);

    // Direct path
    if (p.is_absolute() || p.exists()) && p.extension().is_some_and(|e| e == "desktop") {
        return parse_desktop_file(p, None);
    }

    // Look up by ID, accepting both "foo" and the full "foo.desktop" form.
    let id = id_or_path.strip_suffix(".desktop").unwrap_or(id_or_path);
    for dir in application_dirs() {
        if let Some(path) = resolve_id(&dir, id) {
            return parse_desktop_file(&path, Some(&dir));
        }
    }

    bail!("desktop entry not found: {}", id_or_path)
}

/// Find the file for `id` under `dir`, where each '-' in the ID may stand
/// for a subdirectory separator. Flat files win, matching `discover_dir`.
fn resolve_id(dir: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') || id == "." || id == ".." {
        return None;
    }

    let flat = dir.join(format!("{id}.desktop"));
    if flat.is_file() {
        return Some(flat);
    }

    for (i, _) in id.match_indices('-') {
        let prefix = &id[..i];
        if prefix.is_empty() || prefix == "." || prefix == ".." {
            continue;
        }
        let sub = dir.join(prefix);
        if sub.is_dir() {
            if let Some(found) = resolve_id(&sub, &id[i + 1..]) {
                return Some(found);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------

/// `$XDG_CONFIG_HOME/autostart` followed by `$XDG_CONFIG_DIRS/autostart`.
fn autostart_dirs() -> Vec<PathBuf> {
    xdg_config_home()
        .into_iter()
        .chain(xdg_config_dirs())
        .map(|d| d.join("autostart"))
        .collect()
}

/// Dedupe by priority first, so a hidden user override suppresses the
/// system entry, then filter.
fn autostart_entries(dirs: &[PathBuf], desktop: &str) -> Vec<DesktopEntry> {
    let mut entries: Vec<DesktopEntry> = collect_entries(dirs)
        .into_iter()
        .filter(|e| e.visible_for(Some(desktop)))
        .filter(|e| e.type_() == "Application" && e.is_launchable())
        .filter(|e| e.try_exec_ok())
        .collect();
    entries.sort_by_cached_key(|e| (e.name().to_lowercase(), e.id.clone()));
    entries
}

// ---------------------------------------------------------------------------
// Exec field-code expansion
// ---------------------------------------------------------------------------

/// Split an Exec value into tokens, respecting double-quotes and backslash escapes.
fn tokenize_exec(exec: &str) -> Result<Vec<String>> {
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
fn build_command(
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

/// The extra args that the Exec line has no field code for.
fn dropped_args<'a>(exec: &str, extra_args: &'a [String]) -> &'a [String] {
    let has = |code| exec_has_field_code(exec, code);
    if has('F') || has('U') {
        &[]
    } else if has('f') || has('u') {
        extra_args.get(1..).unwrap_or(&[])
    } else {
        extra_args
    }
}

/// Resolve the terminal command: `--terminal`, then `$TERMINAL -e`, then
/// `xdg-terminal-exec`, then `x-terminal-emulator -e`.
fn resolve_terminal(flag: Option<String>) -> String {
    if let Some(t) = flag {
        return t;
    }
    if let Some(t) = env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty()) {
        return format!("{t} -e");
    }
    if find_binary("xdg-terminal-exec").is_some() {
        return "xdg-terminal-exec".into();
    }
    "x-terminal-emulator -e".into()
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
            let dropped = dropped_args(exec, extra_args);
            if !dropped.is_empty() {
                eprintln!(
                    "vesper: warning: {} does not take these arguments, ignoring: {}",
                    entry.id,
                    dropped.join(" ")
                );
            }
            let (file_args, url_args) = split_extra_args_for_exec(exec, extra_args);
            let (program, args) = build_command(entry, &file_args, &url_args)?;

            let mut cmd = if entry.terminal() {
                let terminal_tokens = tokenize_exec(terminal_cmd)?;
                let (terminal, terminal_args) = terminal_tokens
                    .split_first()
                    .context("Terminal=true but the terminal command is empty")?;
                let mut cmd = Command::new(terminal);
                cmd.args(terminal_args).arg(program).args(args);
                cmd
            } else {
                let mut cmd = Command::new(&program);
                cmd.args(args);
                cmd
            };

            if let Some(dir) = entry.path_().filter(|p| !p.is_empty()) {
                let dir = expand_tilde(dir);
                if !dir.is_dir() {
                    bail!("working directory does not exist: {}", dir.display());
                }
                cmd.current_dir(dir);
            }
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

extern "C" {
    fn setsid() -> i32;
}

/// Start the child in its own session so it survives the launching terminal.
fn new_session(cmd: &mut Command) {
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
}

fn launch_error(cmd: &Command, err: io::Error) -> eyre::Report {
    let mut msg = format!("failed to run {}", cmd.get_program().to_string_lossy());
    if let Some(dir) = cmd.get_current_dir() {
        msg.push_str(&format!(" (in {})", dir.display()));
    }
    eyre::Report::new(err).wrap_err(msg)
}

fn run_entry(
    entry: &DesktopEntry,
    terminal_cmd: &str,
    extra_args: &[String],
    detach: bool,
) -> Result<()> {
    let mut cmd = command_for_entry(entry, terminal_cmd, extra_args)?;
    if detach {
        new_session(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        return match cmd.spawn() {
            Ok(_) => Ok(()),
            Err(err) => Err(launch_error(&cmd, err)),
        };
    }
    let err = cmd.exec();
    Err(launch_error(&cmd, err))
}

fn spawn_entry(entry: &DesktopEntry, terminal_cmd: &str) -> Result<()> {
    let mut cmd = command_for_entry(entry, terminal_cmd, &[])?;
    new_session(&mut cmd);
    match cmd.spawn() {
        Ok(_) => Ok(()),
        Err(err) => Err(launch_error(&cmd, err)),
    }
}

// ---------------------------------------------------------------------------
// Inspect output
// ---------------------------------------------------------------------------

fn inspect_entry(entry: &DesktopEntry, terminal_cmd: &str) {
    println!("File:       {}", entry.path.display());
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
    println!("WorkDir:    {}", entry.path_().unwrap_or("(none)"));
    println!("Icon:       {}", entry.icon().unwrap_or("(none)"));

    let current_desktop = current_desktop();
    println!(
        "Visible (current desktop {}): {}",
        current_desktop.as_deref().unwrap_or("?"),
        entry.visible_for(current_desktop.as_deref())
    );
    println!("Launchable: {}", entry.is_launchable());
    println!("TryExec OK: {}", entry.try_exec_ok());

    match command_for_entry(entry, terminal_cmd, &[]) {
        Ok(cmd) => println!("Action:     {:?}", cmd),
        Err(err) => println!("Action:     (cannot run: {:#})", err),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Replace characters that would break the tab-separated output format.
fn tsv_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Resolve `~/` prefix
fn expand_tilde(s: &str) -> PathBuf {
    if let (Some(rest), Some(home)) = (s.strip_prefix("~/"), home_dir()) {
        return home.join(rest);
    }
    PathBuf::from(s)
}

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

fn current_desktop() -> Option<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|s| !s.is_empty())
}

fn xdg_data_home() -> Option<PathBuf> {
    absolute_path(env::var_os("XDG_DATA_HOME"))
        .or_else(|| home_dir().map(|h| h.join(".local/share")))
}

fn xdg_config_home() -> Option<PathBuf> {
    absolute_path(env::var_os("XDG_CONFIG_HOME")).or_else(|| home_dir().map(|h| h.join(".config")))
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    path_list(env::var_os("XDG_DATA_DIRS"), "/usr/local/share:/usr/share")
}

fn xdg_config_dirs() -> Vec<PathBuf> {
    path_list(env::var_os("XDG_CONFIG_DIRS"), "/etc/xdg")
}

fn desktop_list_matches(list: &str, desktops: &[&str]) -> bool {
    list.split(';')
        .filter(|s| !s.is_empty())
        .any(|candidate| desktops.contains(&candidate))
}

fn is_executable(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Find an executable in PATH, resolving `~`.
fn find_binary(cmd: &str) -> Option<PathBuf> {
    if cmd.is_empty() {
        return None;
    }

    // If it contains a path separator, treat as a direct path
    if cmd.contains('/') {
        let p = expand_tilde(cmd);
        return is_executable(&p).then_some(p);
    }

    env::split_paths(&env::var_os("PATH")?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(cmd))
        .find(|p| is_executable(p))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List {
            ids,
            all,
            json,
            icons,
        } => {
            let entries = discover_all();
            let filtered = list_entries(&entries, current_desktop().as_deref(), all);

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
                    println!(
                        "{}\t{}\t{}",
                        tsv_field(e.name()),
                        e.id,
                        tsv_field(e.icon().unwrap_or(""))
                    );
                }
            } else {
                for e in &filtered {
                    println!("{}\t{}", tsv_field(e.name()), e.id);
                }
            }
        }

        Commands::Run {
            id_or_path,
            terminal,
            detach,
            args,
        } => {
            let entry = find_entry(&id_or_path)?;
            run_entry(&entry, &resolve_terminal(terminal), &args, detach)?;
        }

        Commands::Inspect {
            id_or_path,
            terminal,
        } => {
            let entry = find_entry(&id_or_path)?;
            inspect_entry(&entry, &resolve_terminal(terminal));
        }

        Commands::Autostart {
            desktop,
            run,
            terminal,
        } => {
            let desktop = desktop
                .or_else(current_desktop)
                .context("no --desktop given and $XDG_CURRENT_DESKTOP is not set")?;
            let entries = autostart_entries(&autostart_dirs(), &desktop);

            if run {
                if entries.is_empty() {
                    eprintln!("No autostart entries for desktop '{}'", desktop);
                }
                let terminal = resolve_terminal(terminal);
                for e in &entries {
                    eprintln!("Running: {} ({})", e.name(), e.path.display());
                    if let Err(err) = spawn_entry(e, &terminal) {
                        eprintln!("  error: {:#}", err);
                    }
                }
            } else {
                for e in &entries {
                    println!("{}\t{}", tsv_field(e.name()), e.id);
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn argv(entry: &DesktopEntry, files: &[&str], urls: &[&str]) -> Vec<String> {
        let (program, args) = build_command(entry, &strings(files), &strings(urls)).unwrap();
        std::iter::once(program).chain(args).collect()
    }

    /// Temporary directory removed on drop, even if the test panics.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = env::temp_dir().join(format!("vesper-{name}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn app(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=prog\n")
    }

    // -- parsing ------------------------------------------------------------

    #[test]
    fn parses_only_desktop_entry_group() {
        let raw = parse_desktop_entry_group(
            "# comment\n[Other]\nName=Wrong\n[Desktop Entry]\nName = Right\r\nExec= prog  \n\
             X-Custom=yes\n[Desktop Action new]\nExec=other\n",
        );
        assert_eq!(raw.get("Name").unwrap(), "Right");
        assert_eq!(raw.get("Exec").unwrap(), "prog");
        assert_eq!(raw.get("X-Custom").unwrap(), "yes");
    }

    #[test]
    fn decodes_string_escapes() {
        assert_eq!(unescape_value(r"a\sb\tc\\d\;e"), "a b\tc\\d\\;e");
        // Exec: string escapes first, then quoting rules
        let raw = parse_desktop_entry_group("[Desktop Entry]\nExec=prog \"a\\\\\\\\b\"\n");
        assert_eq!(
            tokenize_exec(raw.get("Exec").unwrap()).unwrap(),
            vec!["prog", "a\\b"]
        );
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

    // -- field codes --------------------------------------------------------

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
        assert_eq!(dropped_args("prog", &args), args.as_slice());
        assert_eq!(dropped_args("prog %f", &args), &args[1..]);
        assert!(dropped_args("prog %U", &args).is_empty());
        assert_eq!(dropped_args("prog %%f", &args), args.as_slice());
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

        let resolved = |id| resolve_id(&dirs[1], id).map(|p| parse_desktop_file(&p, None).unwrap());
        assert_eq!(resolved("foo-bar").unwrap().name(), "Flat");
        fs::remove_file(dir.0.join("system/foo-bar.desktop")).unwrap();
        assert_eq!(resolved("foo-bar").unwrap().name(), "Nested");
        assert!(resolve_id(&dirs[1], "..-system-foo-bar").is_none());
        assert!(resolve_id(&dirs[1], "../user/thing").is_none());
    }

    #[test]
    fn symlink_loops_are_visited_once() {
        let dir = TempDir::new("symlink");
        dir.write("apps/a.desktop", &app("A"));
        std::os::unix::fs::symlink("..", dir.0.join("apps/loop")).unwrap();

        let found = discover_dir(&dir.0.join("apps"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn find_binary_requires_executable_file() {
        let dir = TempDir::new("binary");
        let exe = dir.write("exe", "");
        let plain = dir.write("plain", "");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(find_binary(exe.to_str().unwrap()).is_some());
        assert!(find_binary(plain.to_str().unwrap()).is_none());
        assert!(find_binary(dir.0.to_str().unwrap()).is_none());
    }

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
    }
}
