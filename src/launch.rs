// ---------------------------------------------------------------------------
// Command construction and process launching
// ---------------------------------------------------------------------------

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use eyre::{bail, ContextCompat, Result};

use crate::desktop::DesktopEntry;
use crate::exec::{build_command, has_field_code, route_extra_args, tokenize_exec};
use crate::xdg;

/// What the user asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    File(PathBuf),
    Url(String),
}

impl Target {
    pub fn as_argument(&self) -> String {
        match self {
            Target::File(path) => path.to_string_lossy().into_owned(),
            Target::Url(url) => url.clone(),
        }
    }
}

/// How the child process relates to the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Replace the current process (`exec`).
    Replace,
    /// Spawn in a new session with stdio on `/dev/null`.
    Detach,
    /// Spawn in a new session, inheriting stdio.
    Spawn,
}

/// Resolve the terminal command: `--terminal`, then `$TERMINAL -e`, then
/// `xdg-terminal-exec`, then `x-terminal-emulator -e`.
pub fn resolve_terminal(flag: Option<String>) -> String {
    if let Some(t) = flag {
        return t;
    }
    if let Some(t) = std::env::var("TERMINAL")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        return format!("{t} -e");
    }
    if xdg::find_binary("xdg-terminal-exec").is_some() {
        return "xdg-terminal-exec".into();
    }
    "x-terminal-emulator -e".into()
}

/// Build the command for an Application entry.
pub fn command_for_entry(
    entry: &DesktopEntry,
    terminal_cmd: &str,
    file_args: &[String],
    url_args: &[String],
) -> Result<Command> {
    match entry.type_() {
        "Application" => {}
        "Link" => bail!("Link entries dispatch through the MIME resolver"),
        "Directory" => bail!("Directory entries are not runnable"),
        other => bail!("unknown Type: {}", other),
    }

    if !entry.try_exec_ok() {
        bail!("TryExec not found: {}", entry.try_exec().unwrap_or(""));
    }

    let (program, args) = build_command(entry, file_args, url_args)?;

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
        let dir = xdg::expand_tilde(dir);
        if !dir.is_dir() {
            bail!("working directory does not exist: {}", dir.display());
        }
        cmd.current_dir(dir);
    }

    Ok(cmd)
}

/// Route a single open target to the field codes the entry declares.
pub fn route_target(entry: &DesktopEntry, target: Option<&Target>) -> (Vec<String>, Vec<String>) {
    let Some(target) = target else {
        return (Vec::new(), Vec::new());
    };

    let exec = entry.exec().unwrap_or("");
    let wants_files = has_field_code(exec, 'f') || has_field_code(exec, 'F');
    let wants_urls = has_field_code(exec, 'u') || has_field_code(exec, 'U');
    let argument = target.as_argument();

    match target {
        Target::File(_) => {
            if wants_files {
                (vec![argument], Vec::new())
            } else if wants_urls {
                (Vec::new(), vec![argument])
            } else {
                eprintln!(
                    "vesper: warning: {} has no file field code, ignoring {}",
                    entry.id, argument
                );
                (Vec::new(), Vec::new())
            }
        }
        Target::Url(_) => {
            if wants_urls {
                (Vec::new(), vec![argument])
            } else if wants_files {
                (vec![argument], Vec::new())
            } else {
                eprintln!(
                    "vesper: warning: {} has no URL field code, ignoring {}",
                    entry.id, argument
                );
                (Vec::new(), Vec::new())
            }
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

fn execute(mut cmd: Command, mode: LaunchMode) -> Result<()> {
    match mode {
        LaunchMode::Replace => {
            let err = cmd.exec();
            Err(launch_error(&cmd, err))
        }
        LaunchMode::Detach => {
            new_session(&mut cmd);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.spawn().map(|_| ()).map_err(|e| launch_error(&cmd, e))
        }
        LaunchMode::Spawn => {
            new_session(&mut cmd);
            cmd.spawn().map(|_| ()).map_err(|e| launch_error(&cmd, e))
        }
    }
}

/// Launch an Application entry with an optional open target.
pub fn launch_target(
    entry: &DesktopEntry,
    terminal_cmd: &str,
    target: Option<&Target>,
    mode: LaunchMode,
) -> Result<()> {
    let (file_args, url_args) = route_target(entry, target);
    let cmd = command_for_entry(entry, terminal_cmd, &file_args, &url_args)?;
    execute(cmd, mode)
}

/// `vesper run`: route `--` arguments through the entry's field codes.
pub fn run_entry(
    entry: &DesktopEntry,
    terminal_cmd: &str,
    extra_args: &[String],
    detach: bool,
) -> Result<()> {
    if entry.type_() == "Link" {
        let url = entry.url().context("Link entry has no URL key")?;
        let resolver = crate::mime::Resolver::load();
        let mode = if detach {
            LaunchMode::Detach
        } else {
            LaunchMode::Replace
        };
        return crate::open::open_target(url, Some(terminal_cmd), mode, &resolver)
            .map_err(|err| eyre::eyre!("{}", err));
    }

    let exec = entry.exec().context("entry has no Exec key")?;
    let (file_args, url_args, dropped) = route_extra_args(exec, extra_args);
    if !dropped.is_empty() {
        eprintln!(
            "vesper: warning: {} does not take these arguments, ignoring: {}",
            entry.id,
            dropped.join(" ")
        );
    }

    let cmd = command_for_entry(entry, terminal_cmd, &file_args, &url_args)?;
    execute(
        cmd,
        if detach {
            LaunchMode::Detach
        } else {
            LaunchMode::Replace
        },
    )
}

/// `autostart --run`: spawn without replacing the current process.
pub fn spawn_entry(entry: &DesktopEntry, terminal_cmd: &str) -> Result<()> {
    let cmd = command_for_entry(entry, terminal_cmd, &[], &[])?;
    execute(cmd, LaunchMode::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn test_entry(exec: &str) -> DesktopEntry {
        let mut raw = BTreeMap::new();
        raw.insert("Type".into(), "Application".into());
        raw.insert("Name".into(), "Test App".into());
        raw.insert("Exec".into(), exec.into());
        DesktopEntry::new(PathBuf::from("/tmp/test.desktop"), raw, "test".into())
    }

    #[test]
    fn routes_files_and_urls_to_matching_field_codes() {
        let entry = test_entry("app %f");
        let file = Target::File(PathBuf::from("/tmp/a.txt"));
        let url = Target::Url("https://example.com".into());

        assert_eq!(
            route_target(&entry, Some(&file)),
            (vec!["/tmp/a.txt".to_string()], Vec::new())
        );
        assert_eq!(
            route_target(&entry, Some(&url)),
            (vec!["https://example.com".to_string()], Vec::new())
        );
    }

    #[test]
    fn routes_url_targets_to_url_field_codes() {
        let entry = test_entry("app %U");
        let file = Target::File(PathBuf::from("/tmp/a.txt"));
        let url = Target::Url("https://example.com".into());

        assert_eq!(
            route_target(&entry, Some(&file)),
            (Vec::new(), vec!["/tmp/a.txt".to_string()])
        );
        assert_eq!(
            route_target(&entry, Some(&url)),
            (Vec::new(), vec!["https://example.com".to_string()])
        );
    }

    #[test]
    fn wraps_terminal_commands() {
        let mut entry = test_entry("htop");
        entry.raw.insert("Terminal".into(), "true".into());
        let cmd = command_for_entry(&entry, "foot -e", &[], &[]).unwrap();

        assert_eq!(cmd.get_program(), "foot");
        assert_eq!(
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["-e", "htop"]
        );
    }
}
