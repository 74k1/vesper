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

# Run an entry by ID
vesper run firefox

# Run by path
vesper run /usr/share/applications/firefox.desktop

# Run with file arguments
vesper run gvim --args file1.txt file2.txt

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
- Parses the `[Desktop Entry]` group
- Supports `Type=Application`, `Type=Link`, ignores `Type=Directory`
- Handles Exec field codes: `%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`
- `Terminal=true` runs through a configurable terminal command
- `TryExec` is checked before running
- `NoDisplay=true` entries are hidden from `list` unless `--all` is passed
- Autostart filtering respects `Hidden`, `OnlyShowIn`, `NotShowIn`, `TryExec`

## Output format

| Command | Format |
|---------|--------|
| `vesper list` | `Name\tID` |
| `vesper list --ids` | `ID` |
| `vesper list --icons` | `Name\tID\tIcon` |
| `vesper list --json` | JSON array |

## Companion tools

- **aurora** — TUI frontend for browsing and launching desktop entries
