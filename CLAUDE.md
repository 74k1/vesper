# CLAUDE.md

## Build / Lint
```bash
nix-shell -p cargo rustc clippy --command "cargo build"
nix-shell -p cargo rustc clippy --command "cargo clippy"
```

## Test with fixtures
```bash
# List
XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list
XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --all
XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --icons
XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --ids
XDG_DATA_HOME=./fixtures XDG_DATA_DIRS= cargo run -- list --json

# Inspect
cargo run -- inspect ./fixtures/applications/test-fieldcodes.desktop

# Autostart
XDG_CONFIG_HOME=./fixtures HOME=/tmp XDG_CONFIG_DIRS= cargo run -- autostart --desktop niri
```

## Architecture
Single-file (`src/main.rs`, ~750 lines). No modules. No traits. No overengineering.

### Macro
`desktop_entry_accessors!` — data-driven DSL in the `impl DesktopEntry` block:
- `str NAME KEY DEFAULT` → `&str` accessor with fallback
- `opt NAME KEY` → `Option<&str>` accessor
- `bool NAME KEY` → `bool` accessor (checks `== "true"`)

### Key functions
- `parse_desktop_file` — INI-style parser, extracts only `[Desktop Entry]` keys
- `discover_all` — scans `$XDG_DATA_HOME/applications` + `$XDG_DATA_DIRS/applications`
- `build_command` — tokenizes `Exec`, expands field codes (`%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`)
- `run_entry` — exec's directly (no shell), handles `Terminal=true` via `--terminal` flag
- `autostart_entries` — reads from `$XDG_CONFIG_HOME/autostart` + `$XDG_CONFIG_DIRS/autostart`

### Dependencies
- `clap` (derive) — CLI
- `eyre` + `color-eyre` — errors
- `serde` + `serde_json` — `--json` output
