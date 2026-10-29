// ---------------------------------------------------------------------------
// CLI entry point and argv[0] dispatch
// ---------------------------------------------------------------------------

mod desktop;
mod exec;
mod launch;
mod mime;
mod open;
#[cfg(test)]
mod test_support;
mod xdg;

use std::path::Path;

use clap::{Parser, Subcommand};

use crate::desktop::DesktopEntry;
use crate::launch::LaunchMode;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "vesper",
    version,
    about = "Simple freedesktop .desktop entry runner"
)]
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
    /// Open a file, directory, or URL with its default application
    Open {
        /// File path, directory, file URI, or URL
        target: String,
        /// Terminal command (e.g. "foot -e")
        #[arg(long)]
        terminal: Option<String>,
        /// Detach from the current terminal (new session, stdio to /dev/null)
        #[arg(long)]
        detach: bool,
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
    /// Query MIME types and set default applications
    Mime {
        #[command(subcommand)]
        command: MimeCommands,
    },
    /// Read and write desktop settings
    Settings {
        #[command(subcommand)]
        command: SettingsCommands,
    },
}

#[derive(Subcommand)]
enum MimeCommands {
    /// Query a file type or a default application
    Query {
        #[command(subcommand)]
        query: MimeQuery,
    },
    /// Set the default application for one or more MIME types
    Default {
        /// Desktop entry ID ending in .desktop
        app: String,
        /// MIME types
        mimes: Vec<String>,
    },
}

#[derive(Subcommand)]
enum MimeQuery {
    /// Print the MIME type of a file
    Filetype {
        /// File path
        file: String,
    },
    /// Print the default desktop entry for a MIME type
    Default {
        /// MIME type
        mime: String,
    },
}

#[derive(Subcommand)]
enum SettingsCommands {
    /// Print the current value of a setting
    Get {
        /// Setting name
        property: String,
        /// Extra argument (scheme for default-url-scheme-handler)
        rest: Vec<String>,
    },
    /// Write a new value for a setting
    Set {
        /// Setting name
        property: String,
        /// Extra argument and value
        rest: Vec<String>,
    },
    /// Check whether a setting has a given value
    Check {
        /// Setting name
        property: String,
        /// Extra argument and expected value
        rest: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let mut argv = std::env::args_os();
    let program = argv.next().unwrap_or_default();
    let name = Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vesper".into());
    let args: Vec<String> = argv.map(|arg| arg.to_string_lossy().into_owned()).collect();

    match name.as_str() {
        "v" => run_v(&args),
        "xdg-open" => run_xdg_open(&args),
        "xdg-mime" => run_xdg_mime(&args),
        "xdg-settings" => run_xdg_settings(&args),
        _ => run_vesper(args),
    }
}

fn run_vesper(args: Vec<String>) -> i32 {
    let cli = Cli::parse_from(std::iter::once("vesper".to_string()).chain(args));

    match cli.command {
        Commands::List {
            ids,
            all,
            json,
            icons,
        } => {
            let entries = desktop::discover_all();
            let filtered = desktop::list_entries(&entries, xdg::current_desktop().as_deref(), all);

            if json {
                let values: Vec<serde_json::Value> = filtered
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "id": entry.id,
                            "name": entry.name(),
                            "type": entry.type_(),
                            "exec": entry.exec(),
                            "url": entry.url(),
                            "icon": entry.icon(),
                            "path": entry.path.display().to_string(),
                            "terminal": entry.terminal(),
                            "no_display": entry.no_display(),
                        })
                    })
                    .collect();
                match serde_json::to_string_pretty(&values) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("vesper: {err}");
                        return 1;
                    }
                }
            } else if ids {
                for entry in &filtered {
                    println!("{}", entry.id);
                }
            } else if icons {
                for entry in &filtered {
                    println!(
                        "{}\t{}\t{}",
                        xdg::tsv_field(entry.name()),
                        entry.id,
                        xdg::tsv_field(entry.icon().unwrap_or(""))
                    );
                }
            } else {
                for entry in &filtered {
                    println!("{}\t{}", xdg::tsv_field(entry.name()), entry.id);
                }
            }
            0
        }

        Commands::Run {
            id_or_path,
            terminal,
            detach,
            args,
        } => match desktop::find_entry(&id_or_path) {
            Ok(entry) => {
                let terminal_cmd = launch::resolve_terminal(terminal);
                match launch::run_entry(&entry, &terminal_cmd, &args, detach) {
                    Ok(()) => 0,
                    Err(err) => {
                        eprintln!("vesper: {:#}", err);
                        1
                    }
                }
            }
            Err(err) => {
                eprintln!("vesper: {:#}", err);
                1
            }
        },

        Commands::Open {
            target,
            terminal,
            detach,
        } => {
            let resolver = mime::Resolver::load();
            let mode = if detach {
                LaunchMode::Detach
            } else {
                LaunchMode::Replace
            };
            match open::open_target(&target, terminal.as_deref(), mode, &resolver) {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("vesper: {}", err.message());
                    err.code()
                }
            }
        }

        Commands::Inspect {
            id_or_path,
            terminal,
        } => match desktop::find_entry(&id_or_path) {
            Ok(entry) => {
                inspect_entry(&entry, &launch::resolve_terminal(terminal));
                0
            }
            Err(err) => {
                eprintln!("vesper: {:#}", err);
                1
            }
        },

        Commands::Autostart {
            desktop,
            run,
            terminal,
        } => {
            let Some(desktop) = desktop.or_else(xdg::current_desktop) else {
                eprintln!("vesper: no --desktop given and $XDG_CURRENT_DESKTOP is not set");
                return 1;
            };
            let entries = desktop::autostart_entries(&desktop::autostart_dirs(), &desktop);

            if run {
                if entries.is_empty() {
                    eprintln!("No autostart entries for desktop '{}'", desktop);
                }
                let terminal = launch::resolve_terminal(terminal);
                for entry in &entries {
                    eprintln!("Running: {} ({})", entry.name(), entry.path.display());
                    if let Err(err) = launch::spawn_entry(entry, &terminal) {
                        eprintln!("  error: {:#}", err);
                    }
                }
            } else {
                for entry in &entries {
                    println!("{}\t{}", xdg::tsv_field(entry.name()), entry.id);
                }
            }
            0
        }

        Commands::Mime { command } => match command {
            MimeCommands::Query { query } => match query {
                MimeQuery::Filetype { file } => filetype_exit(&file, "vesper"),
                MimeQuery::Default { mime } => {
                    let resolver = mime::Resolver::load();
                    if let Some(id) = resolver.resolve(&mime) {
                        println!("{id}");
                    }
                    0
                }
            },
            MimeCommands::Default { app, mimes } => {
                if mimes.is_empty() {
                    eprintln!("vesper: no MIME type given");
                    return 1;
                }
                set_default_exit(&app, &mimes, "vesper", true)
            }
        },

        Commands::Settings { command } => match command {
            SettingsCommands::Get { property, rest } => settings_get(&property, &rest, "vesper"),
            SettingsCommands::Set { property, rest } => settings_set(&property, &rest, "vesper"),
            SettingsCommands::Check { property, rest } => settings_check(&property, &rest),
        },
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
    println!("MimeType:   {}", entry.mime_type().unwrap_or("(none)"));

    let current_desktop = xdg::current_desktop();
    println!(
        "Visible (current desktop {}): {}",
        current_desktop.as_deref().unwrap_or("?"),
        entry.visible_for(current_desktop.as_deref())
    );
    println!("Launchable: {}", entry.is_launchable());
    println!("TryExec OK: {}", entry.try_exec_ok());

    if entry.type_() == "Link" {
        println!("Action:     (Link -> {})", entry.url().unwrap_or("(none)"));
        return;
    }

    match launch::command_for_entry(entry, terminal_cmd, &[], &[]) {
        Ok(cmd) => println!("Action:     {:?}", cmd),
        Err(err) => println!("Action:     (cannot run: {:#})", err),
    }
}

// ---------------------------------------------------------------------------
// MIME and settings helpers
// ---------------------------------------------------------------------------

fn filetype_exit(file: &str, prefix: &str) -> i32 {
    let path = xdg::expand_tilde(file);
    if !path.exists() {
        eprintln!("{}: file '{}' does not exist", prefix, file);
        return 2;
    }
    let database = mime::MimeDatabase::load();
    println!("{}", database.detect(&path));
    0
}

fn set_default_exit(app: &str, mimes: &[String], prefix: &str, warn: bool) -> i32 {
    match mime::set_default(app, mimes, warn) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{}: {}", prefix, err.message);
            err.code
        }
    }
}

fn settings_get(property: &str, rest: &[String], prefix: &str) -> i32 {
    let resolver = mime::Resolver::load();
    match property {
        "default-web-browser" => {
            if let Some(id) = mime::web_browser(&resolver) {
                println!("{id}");
            }
            0
        }
        "default-url-scheme-handler" => {
            let Some(scheme) = rest.first() else {
                eprintln!("{}: default-url-scheme-handler needs a scheme", prefix);
                return 1;
            };
            if let Some(id) = resolver.resolve(&mime::scheme_mime(scheme)) {
                println!("{id}");
            }
            0
        }
        other => {
            eprintln!("{}: unknown property '{}'", prefix, other);
            1
        }
    }
}

fn settings_set(property: &str, rest: &[String], prefix: &str) -> i32 {
    match property {
        "default-web-browser" => {
            let Some(app) = rest.first() else {
                eprintln!("{}: default-web-browser needs an application", prefix);
                return 1;
            };
            set_default_exit(app, &mime::web_browser_mimes(), prefix, false)
        }
        "default-url-scheme-handler" => {
            let (Some(scheme), Some(app)) = (rest.first(), rest.get(1)) else {
                eprintln!(
                    "{}: default-url-scheme-handler needs a scheme and an application",
                    prefix
                );
                return 1;
            };
            set_default_exit(app, &[mime::scheme_mime(scheme)], prefix, false)
        }
        other => {
            eprintln!("{}: unknown property '{}'", prefix, other);
            1
        }
    }
}

fn settings_check(property: &str, rest: &[String]) -> i32 {
    let resolver = mime::Resolver::load();
    match property {
        "default-web-browser" => {
            let Some(app) = rest.first() else {
                return 1;
            };
            if mime::web_browser(&resolver).as_deref() == Some(app.as_str()) {
                0
            } else {
                1
            }
        }
        "default-url-scheme-handler" => {
            let (Some(scheme), Some(app)) = (rest.first(), rest.get(1)) else {
                return 1;
            };
            if resolver.resolve(&mime::scheme_mime(scheme)).as_deref() == Some(app.as_str()) {
                0
            } else {
                1
            }
        }
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Drop-in command names
// ---------------------------------------------------------------------------

fn print_xdg_open_usage() {
    println!("xdg-open - opens a file or URL in the user's preferred application");
    println!();
    println!("Synopsis");
    println!();
    println!("xdg-open {{ file | URL }}");
    println!("xdg-open {{ --help | --version }}");
}

fn run_xdg_open(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "--manual")
    {
        print_xdg_open_usage();
        return 0;
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("vesper {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if args.len() != 1 {
        eprintln!("xdg-open: must specify exactly one file or URL");
        print_xdg_open_usage();
        return 1;
    }

    let resolver = mime::Resolver::load();
    match open::open_target(&args[0], None, LaunchMode::Replace, &resolver) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("xdg-open: {}", err.message());
            err.code()
        }
    }
}

fn print_v_usage() {
    println!("usage: v FILE|URL...");
}

fn run_v(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_v_usage();
        return 0;
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("vesper {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    let targets: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    if targets.is_empty() {
        print_v_usage();
        return 1;
    }

    let resolver = mime::Resolver::load();
    let mode = if targets.len() == 1 {
        LaunchMode::Replace
    } else {
        LaunchMode::Spawn
    };
    let mut failed = false;

    for target in targets {
        if let Err(err) = open::open_target(target.as_str(), None, mode, &resolver) {
            eprintln!("v: {}", err.message());
            failed = true;
        }
    }

    if failed {
        1
    } else {
        0
    }
}

fn print_xdg_mime_usage() {
    println!("xdg-mime query {{ filetype FILE | default MIME }}");
    println!("xdg-mime default APPLICATION MIME...");
}

fn run_xdg_mime(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") => {
            print_xdg_mime_usage();
            0
        }
        Some("--version") => {
            println!("vesper {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("query") => match args.get(1).map(String::as_str) {
            Some("filetype") => match args.get(2) {
                Some(file) => filetype_exit(file, "xdg-mime"),
                None => {
                    eprintln!("xdg-mime: query filetype needs a file");
                    1
                }
            },
            Some("default") => match args.get(2) {
                Some(mime) => {
                    let resolver = mime::Resolver::load();
                    if let Some(id) = resolver.resolve(mime) {
                        println!("{id}");
                    }
                    0
                }
                None => {
                    eprintln!("xdg-mime: query default needs a MIME type");
                    1
                }
            },
            _ => {
                print_xdg_mime_usage();
                1
            }
        },
        Some("default") => {
            let Some(app) = args.get(1) else {
                eprintln!("xdg-mime: default needs an application");
                return 1;
            };
            let mimes: Vec<String> = args[2..].to_vec();
            if mimes.is_empty() {
                eprintln!("xdg-mime: default needs at least one MIME type");
                return 1;
            }
            set_default_exit(app, &mimes, "xdg-mime", false)
        }
        Some("install") | Some("uninstall") => {
            eprintln!("xdg-mime: install and uninstall are not supported");
            1
        }
        _ => {
            print_xdg_mime_usage();
            1
        }
    }
}

fn print_xdg_settings_usage() {
    println!("xdg-settings get PROPERTY [ARGUMENT]");
    println!("xdg-settings set PROPERTY [ARGUMENT] VALUE");
    println!("xdg-settings check PROPERTY [ARGUMENT] VALUE");
    println!();
    println!("Properties: default-web-browser, default-url-scheme-handler");
}

fn run_xdg_settings(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_xdg_settings_usage();
        return 0;
    }

    let verb = args.first().map(String::as_str);
    let property = args.get(1).map(String::as_str);
    let rest: Vec<String> = args.iter().skip(2).cloned().collect();

    match (verb, property) {
        (Some("get"), Some(property)) => settings_get(property, &rest, "xdg-settings"),
        (Some("set"), Some(property)) => settings_set(property, &rest, "xdg-settings"),
        (Some("check"), Some(property)) => settings_check(property, &rest),
        _ => {
            print_xdg_settings_usage();
            1
        }
    }
}
