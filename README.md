# vesper

Simple freedesktop `.desktop` entry runner and MIME dispatcher. No GUI, no
xdg-utils, no bloat.

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

# Open a file, directory, file URI, or URL with its default application
vesper open document.pdf
vesper open https://example.com
vesper open file:///tmp/report.pdf
vesper open ~/Pictures

# Shorter form: v opens one or more targets
v document.pdf
v *.pdf

# MIME queries
vesper mime query filetype document.pdf
vesper mime query default image/svg+xml

# Set a default application
vesper mime default firefox.desktop image/png image/jpeg

# Settings
vesper settings get default-web-browser
vesper settings set default-web-browser firefox.desktop
vesper settings check default-web-browser firefox.desktop
vesper settings get default-url-scheme-handler mailto

# Inspect an entry
vesper inspect firefox

# List autostart entries for a desktop
vesper autostart --desktop niri

# Run autostart entries
vesper autostart --desktop niri --run
```

## Drop-in command names

`vesper` dispatches on `argv[0]`. A symlink named `v` means `open`. Symlinks
named `xdg-open`, `xdg-mime`, and `xdg-settings` provide the xdg-utils
interface without xdg-utils:

```bash
ln -s vesper ~/.local/bin/v
ln -s vesper ~/.local/bin/xdg-open
ln -s vesper ~/.local/bin/xdg-mime
ln -s vesper ~/.local/bin/xdg-settings
```

The flake installs `v` in `bin/`. Enable the xdg names with the wrapper
package:

```bash
nix build .#xdgWrappers
```

The wrapper package puts `xdg-open`, `xdg-mime`, and `xdg-settings` next to
`vesper` and `v` in `bin/`. Install it in the user profile to shadow the
system copies. Programs that call `xdg-open` by absolute path still bypass it.

`xdg-mime install` and `xdg-mime uninstall` are not implemented. Use
`update-mime-database`.

## How it works

- Discovers `.desktop` files from `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/applications`; empty, unset or relative XDG values fall back to the spec defaults
- Computes desktop IDs from relative paths (`foo/bar.desktop` → `foo-bar`), and accepts optional `.desktop` suffixes for lookup
- Parses the `[Desktop Entry]` group and keeps every key, including `X-*`
- Supports `Type=Application`, `Type=Link`, ignores `Type=Directory`; entries without a `Name` and a non-empty `Exec` (or `URL`) are dropped
- Decodes the `\s \n \t \r \\` value escapes, then tokenizes `Exec` with quotes/backslash escapes
- Handles field codes `%%`, `%f`, `%F`, `%u`, `%U`, `%c`, `%k`, `%i`; the deprecated `%d %D %n %N %v %m` are removed
- Arguments after `--` are used as file args for `%f`/`%F` entries and URL args for `%u`/`%U` entries; substituted values are never rescanned for field codes
- Extra arguments that match no placeholder are dropped with a warning
- `Terminal=true` runs through `$TERMINAL -e`, `xdg-terminal-exec`, then `x-terminal-emulator -e` (`--terminal` overrides)
- `TryExec` must resolve to an executable file before running
- `list` respects `Hidden`, `OnlyShowIn`, `NotShowIn`, and hides `NoDisplay=true` and entries whose `TryExec` is missing unless `--all` is passed
- Autostart filtering respects XDG precedence/user overrides plus `Hidden`, `OnlyShowIn`, `NotShowIn`, `TryExec`; `--run` spawns all matching entries, `--desktop` defaults to `$XDG_CURRENT_DESKTOP`

### MIME detection

`vesper open` and `vesper mime query filetype` read the shared-mime-info
database from `$XDG_DATA_HOME/mime` and `$XDG_DATA_DIRS/mime`:

- `globs2` (or the older `globs`), weighted; ties keep the longest pattern
- the compiled `magic` file, with nested rules and masks
- `aliases` and `subclasses`

Globs are checked first. Magic is checked when no glob matches or the glob
result is generic. `stat` decides `inode/directory` and `inode/symlink`.
Empty files report `text/plain`.

### Association resolution

The resolver reads `mimeapps.list` files in this order, per desktop name in
`$XDG_CURRENT_DESKTOP`:

1. `$XDG_CONFIG_HOME/$desktop-mimeapps.list`, then `$XDG_CONFIG_HOME/mimeapps.list`
2. `$XDG_CONFIG_DIRS/$desktop-mimeapps.list`, then `$XDG_CONFIG_DIRS/mimeapps.list`
3. `$XDG_DATA_HOME/applications/$desktop-mimeapps.list`, then the generic file
4. `$XDG_DATA_DIRS/applications/$desktop-mimeapps.list`, then the generic file
5. the deprecated `defaults.list` files

The first `[Default Applications]` entry wins. `[Added Associations]` builds
the fallback list. `[Removed Associations]` in a higher-priority file removes
associations from lower-priority files. `mimeinfo.cache` is not read: the
resolver scans `MimeType=` on discovered entries instead, so stale caches
cannot affect results.

If no association exists, the resolver tries parent types from `subclasses`,
then `$BROWSER` for URL schemes, then `$VESPER_FALLBACKS`.

### Fallbacks

`$BROWSER` is a colon-separated list of commands. `$VESPER_FALLBACKS` is a
colon-separated list of commands used for any MIME type. Each command is
tokenized with the same quoting rules as `Exec`, and `%s` is replaced with the
target in every token. If no token contains `%s`, the target is appended.
Commands run directly, never through a shell. Mailcap, `mimeopen`, `gio`,
`kde-open`, `exo-open`, and `gnome-open` are not used.

### Writing defaults

`vesper mime default` and `xdg-mime default` write
`$XDG_CONFIG_HOME/mimeapps.list`:

- The application must end in `.desktop`
- Existing comments, blank lines, and section order are preserved
- The file is written through a temporary file and renamed into place, so a
  symlink survives and a crash cannot truncate the file
- A read-only or otherwise unwritable target exits 6 and names the resolved
  path
- There is no file locking. Two concurrent writers lose one update

A home-manager managed `mimeapps.list` is a symlink into the Nix store and is
read-only. `vesper mime default` reports exit 6 in that case. Change the
default through home-manager instead.

## Output format

| Command | Format |
|---------|--------|
| `vesper list` | `Name\tID` |
| `vesper list --ids` | `ID` |
| `vesper list --icons` | `Name\tID\tIcon` |
| `vesper list --json` | JSON array |

`--ids`, `--json` and `--icons` are mutually exclusive.

## Exit codes

`open`, `v`, and `xdg-open` follow the xdg-open codes:

| Code | Meaning |
|------|---------|
| 1 | bad usage |
| 2 | target does not exist |
| 3 | no application for the MIME type |
| 4 | launch failed |
| 5 | read permission denied |
| 6 | write permission denied |

## Limits

- Desktop portals do not call `xdg-open`. GTK, Qt, and Flatpak use
  `org.freedesktop.portal.OpenURI`, and the portal backend resolves the same
  `mimeapps.list` data through GIO. The wrapper catches programs that exec
  `xdg-open`, not portals.
- `xdg-mime install` and `xdg-mime uninstall` are not implemented.
- Mailcap entries are not read.
- `mimeapps.list` writes are not locked.

## Build

```bash
# With cargo
cargo build --release

# With nix
nix build
nix build .#xdgWrappers
```

## Companion tools

- **aurora** — TUI frontend for browsing and launching desktop entries
