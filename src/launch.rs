// ---------------------------------------------------------------------------
// Command construction and process launching
// ---------------------------------------------------------------------------

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use eyre::{bail, ContextCompat, Result};

use crate::desktop::DesktopEntry;
use crate::exec::{build_command, route_args, tokenize_exec};
use crate::xdg;

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

fn resolve_terminal(flag: Option<&str>) -> String {
    if let Some(t) = flag {
        return t.to_string();
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

/// Build the command for an Application entry, routing `args` through its
/// field codes. `terminal` is the `--terminal` override used when the entry
/// sets `Terminal=true`. `are_urls` says the arguments are known to be URLs;
/// untyped arguments (as from `vesper run`) pass `false`.
pub fn command_for_entry(
    entry: &DesktopEntry,
    terminal: Option<&str>,
    args: &[String],
    are_urls: bool,
) -> Result<Command> {
    if entry.type_() != "Application" {
        bail!("{} entries are not runnable", entry.type_());
    }

    if !entry.try_exec_ok() {
        bail!("TryExec not found: {}", entry.try_exec().unwrap_or(""));
    }

    let exec = entry.exec().context("entry has no Exec key")?;
    let routed = route_args(exec, args, are_urls);
    if !routed.dropped.is_empty() {
        eprintln!(
            "vesper: warning: {} does not take these arguments, ignoring: {}",
            entry.id,
            routed.dropped.join(" ")
        );
    }

    let (program, args) = build_command(entry, &routed.files, &routed.urls)?;

    let mut cmd = if entry.terminal() {
        let terminal_tokens = tokenize_exec(&resolve_terminal(terminal))?;
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

extern "C" {
    fn setsid() -> i32;
}

fn launch_error(cmd: &Command, err: io::Error) -> eyre::Report {
    let mut msg = format!("failed to run {}", cmd.get_program().to_string_lossy());
    if let Some(dir) = cmd.get_current_dir() {
        msg.push_str(&format!(" (in {})", dir.display()));
    }
    eyre::Report::new(err).wrap_err(msg)
}

/// Run `cmd`. `Replace` only returns on failure.
pub fn execute(mut cmd: Command, mode: LaunchMode) -> Result<()> {
    if mode == LaunchMode::Replace {
        let err = cmd.exec();
        return Err(launch_error(&cmd, err));
    }

    // A new session lets the child survive the launching terminal.
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    if mode == LaunchMode::Detach {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    cmd.spawn().map(|_| ()).map_err(|e| launch_error(&cmd, e))
}

/// Launch an Application entry; see `command_for_entry` for the arguments.
pub fn launch(
    entry: &DesktopEntry,
    terminal: Option<&str>,
    args: &[String],
    are_urls: bool,
    mode: LaunchMode,
) -> Result<()> {
    execute(command_for_entry(entry, terminal, args, are_urls)?, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_entry;

    #[test]
    fn wraps_terminal_commands() {
        let mut entry = test_entry("htop");
        entry.raw.insert("Terminal".into(), "true".into());
        let cmd = command_for_entry(&entry, Some("foot -e"), &[], false).unwrap();

        assert_eq!(cmd.get_program(), "foot");
        assert_eq!(
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["-e", "htop"]
        );
    }
}
