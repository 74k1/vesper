use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{bail, Context, ContextCompat, Result};
use clap::{Parser, Subcommand};

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
    /// Computed entry ID (basename without .desktop)
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
        if let Some(d) = desktop {
            let only = self.only_show_in();
            if !only.is_empty() && !only.split(';').any(|s| s == d) {
                return false;
            }
            let not = self.not_show_in();
            if !not.is_empty() && not.split(';').any(|s| s == d) {
                return false;
            }
        } else if !self.only_show_in().is_empty() {
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

/// Parse a .desktop file, extracting only keys from `[Desktop Entry]`.
fn parse_desktop_file(path: &Path) -> Result<DesktopEntry> {
    let f = fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let reader = BufReader::new(f);

    let mut in_entry = false;
    let mut raw = BTreeMap::new();
    let wanted: &[&str] = &[
        "Type", "Name", "Exec", "URL", "Terminal", "TryExec", "Hidden",
        "NoDisplay", "OnlyShowIn", "NotShowIn", "Path", "Icon",
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

    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

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
            if let Ok(e) = parse_desktop_file(&f) {
                seen.entry(e.id.clone()).or_insert(e);
            }
        }
    }

    // User dir overrides
    for f in discover_dir(&user_dir) {
        if let Ok(e) = parse_desktop_file(&f) {
            seen.insert(e.id.clone(), e);
        }
    }

    seen.into_values().collect()
}

/// Find a specific entry by ID or path.
fn find_entry(id_or_path: &str) -> Result<DesktopEntry> {
    let p = Path::new(id_or_path);

    // Direct path
    if (p.is_absolute() || p.exists())
        && p.extension().is_some_and(|e| e == "desktop")
    {
        return parse_desktop_file(p);
    }

    // Look up by ID
    for e in discover_all() {
        if e.id == id_or_path {
            return Ok(e);
        }
    }

    // Try resolving by appending .desktop
    let with_ext = format!("{}.desktop", id_or_path);
    for e in discover_all() {
        if e.id == with_ext || e.id == id_or_path {
            return Ok(e);
        }
    }

    bail!("desktop entry not found: {}", id_or_path)
}

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------

fn autostart_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // System autostart
    for d in xdg_config_dirs() {
        dirs.push(d.join("autostart"));
    }

    // User autostart
    let user_autostart = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            home_dir()
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_default()
        })
        .join("autostart");
    dirs.push(user_autostart);

    dirs
}

fn autostart_entries(desktop: &str) -> Vec<DesktopEntry> {
    let mut seen: BTreeMap<String, DesktopEntry> = BTreeMap::new();
    for dir in autostart_dirs() {
        for f in discover_dir(&dir) {
            if let Ok(e) = parse_desktop_file(&f) {
                // Apply filters
                if !e.visible_for(Some(desktop)) {
                    continue;
                }
                // Skip non-Application entries (links have no Exec)
                if e.type_() != "Application" {
                    continue;
                }
                seen.entry(e.id.clone()).or_insert(e);
            }
        }
    }
    seen.into_values().collect()
}

// ---------------------------------------------------------------------------
// Exec field-code expansion
// ---------------------------------------------------------------------------

/// Split an Exec value into tokens, respecting double-quotes.
fn tokenize_exec(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = exec.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        if chars[i] == '"' {
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                s.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1; // closing quote
            }
            tokens.push(s);
        } else {
            let mut s = String::new();
            while i < chars.len() && !chars[i].is_whitespace() {
                s.push(chars[i]);
                i += 1;
            }
            tokens.push(s);
        }
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

fn run_entry(entry: &DesktopEntry, terminal_cmd: &str, extra_args: &[String]) -> Result<()> {
    let type_ = entry.type_();

    match type_ {
        "Application" => {
            if !entry.try_exec_ok() {
                bail!("TryExec not found: {}", entry.try_exec().unwrap_or(""));
            }

            let file_args = extra_args.to_vec();
            let url_args: Vec<String> = Vec::new();
            let (program, args) = build_command(entry, &file_args, &url_args)?;

            let terminal_tokens = tokenize_exec(terminal_cmd);
            let use_terminal = entry.terminal() && !terminal_tokens.is_empty();

            if use_terminal {
                let mut argv: Vec<String> = Vec::new();
                argv.extend(terminal_tokens.iter().cloned());
                argv.push(program.clone());
                argv.extend(args.clone());

                Err(Command::new(&argv[0]).args(&argv[1..]).exec().into())
            } else {
                let mut cmd = Command::new(&program);
                cmd.args(&args);

                if let Some(p) = entry.path_() {
                    let expanded = expand_tilde(p);
                    cmd.current_dir(Path::new(&expanded));
                }

                Err(cmd.exec().into())
            }
        }

        "Link" => {
            let url = entry.url().context("Link entry has no URL key")?;
            Err(Command::new("xdg-open").arg(url).exec().into())
        }

        "Directory" => {
            bail!("Directory entries are not runnable")
        }

        _ => {
            bail!("unknown Type: {}", type_)
        }
    }
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

// Minimal XDG helpers

fn xdg_data_dirs() -> Vec<PathBuf> {
    let raw = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn xdg_config_dirs() -> Vec<PathBuf> {
    let raw =
        env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
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
            let mut filtered: Vec<&DesktopEntry> = entries
                .iter()
                .filter(|e| !e.hidden())
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
                    if let Err(err) = run_entry(e, &terminal, &[]) {
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
