// ---------------------------------------------------------------------------
// CLI entry point and argv[0] dispatch
// ---------------------------------------------------------------------------

mod desktop;
mod exec;
mod failure;
mod launch;
mod mime;
mod open;
mod settings;
#[cfg(test)]
mod test_support;
mod xdg;

use std::path::Path;

use clap::{Parser, Subcommand};
use eyre::{eyre, ContextCompat};

use crate::desktop::DesktopEntry;
use crate::failure::Failure;
use crate::launch::LaunchMode;
use crate::mime::Resolver;
use crate::settings::Setting;

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
                return report(
                    serde_json::to_string_pretty(&values)
                        .map(|text| println!("{text}"))
                        .map_err(Into::into),
                );
            }

            for entry in &filtered {
                if ids {
                    println!("{}", entry.id);
                } else if icons {
                    println!(
                        "{}\t{}\t{}",
                        tsv_field(entry.name()),
                        entry.id,
                        tsv_field(entry.icon().unwrap_or(""))
                    );
                } else {
                    println!("{}\t{}", tsv_field(entry.name()), entry.id);
                }
            }
            0
        }

        Commands::Run {
            id_or_path,
            terminal,
            detach,
            args,
        } => report(run_entry(
            &id_or_path,
            terminal.as_deref(),
            &args,
            launch_mode(detach),
        )),

        Commands::Open {
            target,
            terminal,
            detach,
        } => exit_code(
            "vesper",
            open::open_target(
                &target,
                terminal.as_deref(),
                launch_mode(detach),
                &Resolver::load(),
            ),
        ),

        Commands::Inspect {
            id_or_path,
            terminal,
        } => report(
            desktop::find_entry(&id_or_path)
                .map(|entry| inspect_entry(&entry, terminal.as_deref())),
        ),

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
                for entry in &entries {
                    eprintln!("Running: {} ({})", entry.name(), entry.path.display());
                    if let Err(err) =
                        launch::launch(entry, terminal.as_deref(), &[], false, LaunchMode::Spawn)
                    {
                        eprintln!("  error: {:#}", err);
                    }
                }
            } else {
                for entry in &entries {
                    println!("{}\t{}", tsv_field(entry.name()), entry.id);
                }
            }
            0
        }

        Commands::Mime { command } => match command {
            MimeCommands::Query { query } => match query {
                MimeQuery::Filetype { file } => query_filetype(&file, "vesper"),
                MimeQuery::Default { mime } => query_default(&mime),
            },
            MimeCommands::Default { app, mimes } => {
                let code = exit_code("vesper", mime::set_default(&app, &mimes));
                if code == 0 && desktop::find_by_id(&app).is_none() {
                    eprintln!(
                        "vesper: warning: {} is not a discoverable desktop entry",
                        app
                    );
                }
                code
            }
        },

        Commands::Settings { command } => {
            let (verb, property, rest) = match command {
                SettingsCommands::Get { property, rest } => (SettingsVerb::Get, property, rest),
                SettingsCommands::Set { property, rest } => (SettingsVerb::Set, property, rest),
                SettingsCommands::Check { property, rest } => (SettingsVerb::Check, property, rest),
            };
            settings(verb, &property, &rest, "vesper")
        }
    }
}

// ---------------------------------------------------------------------------
// Shared command bodies
// ---------------------------------------------------------------------------

fn launch_mode(detach: bool) -> LaunchMode {
    if detach {
        LaunchMode::Detach
    } else {
        LaunchMode::Replace
    }
}

fn print_version() {
    println!("vesper {}", env!("CARGO_PKG_VERSION"));
}

/// Replace characters that would break the tab-separated output format.
fn tsv_field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn report(result: eyre::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("vesper: {:#}", err);
            1
        }
    }
}

fn exit_code(prefix: &str, result: Result<(), Failure>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(failure) => {
            eprintln!("{}: {}", prefix, failure.message);
            failure.code
        }
    }
}

fn run_entry(
    id_or_path: &str,
    terminal: Option<&str>,
    args: &[String],
    mode: LaunchMode,
) -> eyre::Result<()> {
    let entry = desktop::find_entry(id_or_path)?;
    if entry.type_() == "Link" {
        let url = entry.url().context("Link entry has no URL key")?;
        return open::open_target(url, terminal, mode, &Resolver::load())
            .map_err(|failure| eyre!(failure.message));
    }
    launch::launch(&entry, terminal, args, false, mode)
}

fn query_filetype(file: &str, prefix: &str) -> i32 {
    let path = xdg::expand_tilde(file);
    let result = if path.exists() {
        println!("{}", mime::MimeDatabase::load().detect(&path));
        Ok(())
    } else {
        Err(Failure::new(
            Failure::MISSING,
            format!("file '{}' does not exist", file),
        ))
    };
    exit_code(prefix, result)
}

fn query_default(mime: &str) -> i32 {
    print_entry_id(Resolver::load().resolve(mime))
}

/// Print the entry's `.desktop` ID; no entry prints nothing and still
/// succeeds, as xdg-mime and xdg-settings do.
fn print_entry_id(entry: Option<&DesktopEntry>) -> i32 {
    if let Some(entry) = entry {
        println!("{}", entry.desktop_file_id());
    }
    0
}

enum SettingsVerb {
    Get,
    Set,
    Check,
}

fn settings(verb: SettingsVerb, property: &str, rest: &[String], prefix: &str) -> i32 {
    let parsed = Setting::parse(property, rest);
    match verb {
        SettingsVerb::Get => match parsed {
            Ok((setting, _)) => print_entry_id(setting.get(&Resolver::load())),
            Err(failure) => exit_code(prefix, Err(failure)),
        },
        SettingsVerb::Set => exit_code(
            prefix,
            parsed.and_then(|(setting, rest)| match rest.first() {
                Some(app) => setting.set(app),
                None => Err(Failure::new(
                    Failure::USAGE,
                    format!("{} needs an application", property),
                )),
            }),
        ),
        SettingsVerb::Check => {
            let Ok((setting, [app, ..])) = parsed else {
                return 1;
            };
            let current = setting
                .get(&Resolver::load())
                .map(DesktopEntry::desktop_file_id);
            i32::from(current.as_ref() != Some(app))
        }
    }
}

// ---------------------------------------------------------------------------
// Inspect output
// ---------------------------------------------------------------------------

fn inspect_entry(entry: &DesktopEntry, terminal: Option<&str>) {
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

    match launch::command_for_entry(entry, terminal, &[], false) {
        Ok(cmd) => println!("Action:     {:?}", cmd),
        Err(err) => println!("Action:     (cannot run: {:#})", err),
    }
}

// ---------------------------------------------------------------------------
// Drop-in command names
// ---------------------------------------------------------------------------

fn has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|arg| flags.contains(&arg.as_str()))
}

fn print_xdg_open_usage() {
    println!("xdg-open - opens a file or URL in the user's preferred application");
    println!();
    println!("Synopsis");
    println!();
    println!("xdg-open {{ file | URL }}");
    println!("xdg-open {{ --help | --version }}");
}

fn run_xdg_open(args: &[String]) -> i32 {
    if has_flag(args, &["--help", "-h", "--manual"]) {
        print_xdg_open_usage();
        return 0;
    }
    if has_flag(args, &["--version"]) {
        print_version();
        return 0;
    }
    let [target] = args else {
        eprintln!("xdg-open: must specify exactly one file or URL");
        print_xdg_open_usage();
        return 1;
    };

    exit_code(
        "xdg-open",
        open::open_target(target, None, LaunchMode::Replace, &Resolver::load()),
    )
}

fn print_v_usage() {
    println!("usage: v FILE|URL...");
}

fn run_v(args: &[String]) -> i32 {
    if has_flag(args, &["--help", "-h"]) {
        print_v_usage();
        return 0;
    }
    if has_flag(args, &["--version"]) {
        print_version();
        return 0;
    }

    let targets: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    if targets.is_empty() {
        print_v_usage();
        return 1;
    }

    let resolver = Resolver::load();
    // A single target can take over this process; several need it to stay.
    let mode = if targets.len() == 1 {
        LaunchMode::Replace
    } else {
        LaunchMode::Spawn
    };

    let mut failed = false;
    for target in targets {
        if let Err(failure) = open::open_target(target, None, mode, &resolver) {
            eprintln!("v: {}", failure.message);
            failed = true;
        }
    }
    i32::from(failed)
}

fn print_xdg_mime_usage() {
    println!("xdg-mime query {{ filetype FILE | default MIME }}");
    println!("xdg-mime default APPLICATION MIME...");
}

fn run_xdg_mime(args: &[String]) -> i32 {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--help" | "-h", ..] => {
            print_xdg_mime_usage();
            0
        }
        ["--version", ..] => {
            print_version();
            0
        }
        ["query", "filetype", file, ..] => query_filetype(file, "xdg-mime"),
        ["query", "filetype"] => {
            eprintln!("xdg-mime: query filetype needs a file");
            1
        }
        ["query", "default", mime, ..] => query_default(mime),
        ["query", "default"] => {
            eprintln!("xdg-mime: query default needs a MIME type");
            1
        }
        ["default"] => {
            eprintln!("xdg-mime: default needs an application");
            1
        }
        ["default", _] => {
            eprintln!("xdg-mime: default needs at least one MIME type");
            1
        }
        ["default", app, mimes @ ..] => {
            let mimes: Vec<String> = mimes.iter().map(|mime| mime.to_string()).collect();
            exit_code("xdg-mime", mime::set_default(app, &mimes))
        }
        ["install" | "uninstall", ..] => {
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
    if has_flag(args, &["--help", "-h"]) {
        print_xdg_settings_usage();
        return 0;
    }

    let verb = match args.first().map(String::as_str) {
        Some("get") => SettingsVerb::Get,
        Some("set") => SettingsVerb::Set,
        Some("check") => SettingsVerb::Check,
        _ => {
            print_xdg_settings_usage();
            return 1;
        }
    };
    let [_, property, rest @ ..] = args else {
        print_xdg_settings_usage();
        return 1;
    };
    settings(verb, property, rest, "xdg-settings")
}
