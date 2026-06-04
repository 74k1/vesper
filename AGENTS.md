# CLAUDE.md

## Build / Lint
```bash
nix-shell -p cargo rustc rustfmt clippy --command "cargo fmt -- --check"
nix-shell -p cargo rustc clippy --command "cargo build"
nix-shell -p cargo rustc clippy --command "cargo test"
nix-shell -p cargo rustc clippy --command "cargo clippy -- -D warnings"
```

## Test with fixtures
```bash
# List
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --all
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --icons
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --ids
XDG_CURRENT_DESKTOP=niri XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --json

# Inspect
cargo run -- inspect ./fixtures/applications/test-fieldcodes.desktop

# Autostart
XDG_CONFIG_HOME=./fixtures HOME=/tmp XDG_CONFIG_DIRS= cargo run -- autostart --desktop niri
```

## Architecture
Single-file (`src/main.rs`, ~1000 lines). No production modules. No traits. No overengineering.

### Macro
`desktop_entry_accessors!` — data-driven DSL in the `impl DesktopEntry` block:
- `str NAME KEY DEFAULT` → `&str` accessor with fallback
- `opt NAME KEY` → `Option<&str>` accessor
- `bool NAME KEY` → `bool` accessor (checks `== "true"`)

### Key functions
- `desktop_entry_id` — computes IDs from relative paths (`foo/bar.desktop` → `foo-bar`)
- `parse_desktop_file_with_base` — INI-style parser, extracts only `[Desktop Entry]` keys
- `discover_all` — scans `$XDG_DATA_HOME/applications` + `$XDG_DATA_DIRS/applications`, user entries override system entries
- `build_command` — tokenizes `Exec` with quotes/escapes, expands field codes (`%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`)
- `split_extra_args_for_exec` — routes `--args` to file vs URL field-code expansion
- `command_for_entry` — builds `std::process::Command`, handles `Terminal=true` via `--terminal` flag
- `run_entry` — exec's directly (no shell)
- `spawn_entry` — spawns without replacing the process, used by `autostart --run`
- `autostart_entries` / `collect_autostart_entries` — read XDG autostart dirs, dedupe by priority before filtering

### Dependencies
- `clap` (derive) — CLI
- `eyre` + `color-eyre` — errors
- `serde` + `serde_json` — `--json` output
