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

- Discovers `.desktop` files from `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/applications`
- Computes desktop IDs from relative paths (`foo/bar.desktop` → `foo-bar`), and accepts optional `.desktop` suffixes for lookup
- Parses the `[Desktop Entry]` group
- Supports `Type=Application`, `Type=Link`, ignores `Type=Directory`
- Tokenizes `Exec` values with quotes/backslash escapes and handles field codes: `%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`
- Extra `--args` are used as file args for `%f`/`%F` entries and URL args for `%u`/`%U` entries
- `Terminal=true` runs through a configurable terminal command
- `TryExec` is checked before running
- `list` respects `Hidden`, `OnlyShowIn`, `NotShowIn`, and hides `NoDisplay=true` unless `--all` is passed
- Autostart filtering respects XDG precedence/user overrides plus `Hidden`, `OnlyShowIn`, `NotShowIn`, `TryExec`; `--run` spawns all matching entries

## Output format

| Command | Format |
|---------|--------|
| `vesper list` | `Name\tID` |
| `vesper list --ids` | `ID` |
| `vesper list --icons` | `Name\tID\tIcon` |
| `vesper list --json` | JSON array |

## Companion tools

- **aurora** — TUI frontend for browsing and launching desktop entries
