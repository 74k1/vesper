# vesper

Simple freedesktop `.desktop` entry runner. No GUI, no bloat.

## Usage

```bash
# List entries for fuzzy finding
vesper list
vesper list | fzf

# List with icons (for frontends like aurora)
vesper list --icons

# List only IDs
vesper list --ids | fzf | xargs -r -I{} vesper run {}

# Include NoDisplay entries
vesper list --all

# JSON output
vesper list --json

# Run an entry by ID (with or without .desktop suffix)
vesper run firefox
vesper run firefox.desktop

# Run by path
vesper run /usr/share/applications/firefox.desktop

# Run with file or URL arguments (everything after -- is forwarded)
vesper run gvim -- file1.txt file2.txt
vesper run firefox -- https://example.com

# Run with custom terminal
vesper run htop --terminal "foot -e"

# Launch detached from the terminal (new session, stdio on /dev/null)
vesper run firefox --detach

# Inspect an entry
vesper inspect firefox

# List autostart entries for a desktop
vesper autostart --desktop niri

# Run autostart entries
vesper autostart --desktop niri --run
```

## Build

```bash
# With cargo
cargo build --release

# With nix
nix build
```

## How it works

- Discovers `.desktop` files from `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/applications`; empty, unset or relative XDG values fall back to the spec defaults
- Computes desktop IDs from relative paths (`foo/bar.desktop` → `foo-bar`), and accepts optional `.desktop` suffixes for lookup
- Parses the `[Desktop Entry]` group
- Supports `Type=Application`, `Type=Link`, ignores `Type=Directory`; entries without a `Name` and a non-empty `Exec` (or `URL`) are dropped
- Decodes the `\s \n \t \r \\` value escapes, then tokenizes `Exec` with quotes/backslash escapes
- Handles field codes `%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`, `%i`; the deprecated `%d %D %n %N %v %m` are removed
- Arguments after `--` are used as file args for `%f`/`%F` entries and URL args for `%u`/`%U` entries; substituted values are never rescanned for field codes
- Extra arguments that match no placeholder are dropped with a warning
- `Terminal=true` runs through `$TERMINAL -e`, `xdg-terminal-exec`, then `x-terminal-emulator -e` (`--terminal` overrides)
- `TryExec` must resolve to an executable file before running
- `list` respects `Hidden`, `OnlyShowIn`, `NotShowIn`, and hides `NoDisplay=true` and entries whose `TryExec` is missing unless `--all` is passed
- Autostart filtering respects XDG precedence/user overrides plus `Hidden`, `OnlyShowIn`, `NotShowIn`, `TryExec`; `--run` spawns all matching entries, `--desktop` defaults to `$XDG_CURRENT_DESKTOP`

## Output format

| Command | Format |
|---------|--------|
| `vesper list` | `Name\tID` |
| `vesper list --ids` | `ID` |
| `vesper list --icons` | `Name\tID\tIcon` |
| `vesper list --json` | JSON array |

`--ids`, `--json` and `--icons` are mutually exclusive.

## Companion tools

- **aurora** — TUI frontend for browsing and launching desktop entries
