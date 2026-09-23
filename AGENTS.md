# AGENTS.md

## Build / Lint
```bash
nix-shell -p cargo rustc rustfmt clippy --command "cargo fmt -- --check"
nix-shell -p cargo rustc clippy --command "cargo build"
nix-shell -p cargo rustc clippy --command "cargo test"
nix-shell -p cargo rustc clippy --command "cargo clippy --all-targets -- -D warnings"
```

`nix flake check` runs the tests, fmt and clippy as flake checks.

## Test with fixtures
XDG values must be absolute: relative paths are ignored and empty lists fall
back to the spec defaults, so use `$PWD` and a nonexistent dir to isolate.
```bash
# List
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent cargo run -- list
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent cargo run -- list --all
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent cargo run -- list --icons
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent cargo run -- list --ids
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent cargo run -- list --json

# Inspect
cargo run -- inspect ./fixtures/applications/test-fieldcodes.desktop

# Autostart
XDG_CONFIG_HOME=$PWD/fixtures XDG_CONFIG_DIRS=/nonexistent HOME=/tmp cargo run -- autostart --desktop niri
```

## Architecture
Single-file (`src/main.rs`, ~1300 lines). No production modules. No traits. No overengineering.

### Macro
`desktop_entry_accessors!` — data-driven DSL in the `impl DesktopEntry` block:
- `str NAME KEY DEFAULT` → `&str` accessor with fallback
- `opt NAME KEY` → `Option<&str>` accessor
- `bool NAME KEY` → `bool` accessor (checks `== "true"`)

### Key functions
- `desktop_entry_id` — computes IDs from relative paths (`foo/bar.desktop` → `foo-bar`)
- `parse_desktop_file` / `parse_desktop_entry_group` — INI-style parser, keeps every `[Desktop Entry]` key (including `X-*`)
- `unescape_value` — decodes the `\s \n \t \r \\` value escapes before Exec parsing
- `discover_all` / `collect_entries` — scan `$XDG_DATA_HOME/applications` + `$XDG_DATA_DIRS/applications`, user entries override system entries
- `walk_dir` — recursive discovery, each directory visited once (symlink-loop guard)
- `tokenize_exec` / `expand_token` / `build_command` — tokenizes `Exec` with quotes/escapes, expands field codes (`%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`, `%i`) in one character pass; substituted values are never rescanned
- `split_extra_args_for_exec` / `dropped_args` — routes `--` args to file vs URL field-code expansion, warns about extras with no placeholder
- `command_for_entry` / `resolve_terminal` — builds `std::process::Command`; `Terminal=true` via `--terminal` or `$TERMINAL -e` → `xdg-terminal-exec` → `x-terminal-emulator -e`
- `run_entry` — exec's directly (no shell); `--detach` calls `new_session` (setsid, stdio to /dev/null)
- `spawn_entry` — spawns without replacing the process, used by `autostart --run`
- `autostart_entries` — reads XDG autostart dirs, dedupes by priority before filtering
- `is_executable` / `find_binary` — `TryExec` lookup requires a regular, executable file

### Dependencies
- `clap` (derive) — CLI
- `eyre` — errors
- `serde_json` — `--json` output
