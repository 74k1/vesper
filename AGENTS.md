# AGENTS.md

## Build / Lint
`cargo` needs a linker on NixOS; include `gcc` in the shell.
```bash
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo build
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo test
nix shell nixpkgs#cargo nixpkgs#rustfmt -c cargo fmt -- --check
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc nixpkgs#clippy -c cargo clippy --all-targets -- -D warnings
```

`nix flake check` runs the tests, fmt and clippy as flake checks. The devShell
includes `gcc`.

Flakes read the Git tree. This is a jj-colocated repository, so uncommitted
working-copy files are absent from the Git index and `nix build` fails until the
change is committed. Use `nix build "path:$PWD"` while iterating.

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

# MIME and open
XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent \
XDG_CONFIG_HOME=$PWD/fixtures XDG_CONFIG_DIRS=/nonexistent \
  cargo run -- mime query filetype fixtures/test.svg
XDG_DATA_HOME=$PWD/fixtures XDG_DATA_DIRS=/nonexistent \
XDG_CONFIG_HOME=$PWD/fixtures XDG_CONFIG_DIRS=/nonexistent \
  cargo run -- mime query default image/svg+xml
```

## Architecture
One binary, modules under `src/`. No lib target. `argv[0]` basename dispatch
selects the interface: `v` means `open`, `xdg-open`/`xdg-mime`/`xdg-settings`
map to the same internals with xdg-utils argument rules. No new dependencies
without a reason.

### Modules
Imports point down this list only; nothing imports a module below itself.
- `src/failure.rs` — `Failure { code, message }` and the xdg-open exit codes
- `src/xdg.rs` — XDG base dirs (`data_dirs`, `config_dirs`, `config_home`), `$XDG_CURRENT_DESKTOP`, `find_binary`, `expand_tilde`
- `src/desktop.rs` — `DesktopEntry`, shared INI group parser, discovery, IDs, autostart
- `src/exec.rs` — Exec tokenizer, field codes, `route_args`
- `src/launch.rs` — terminal resolution, command construction, `LaunchMode`, setsid; the only place a process is started
- `src/mime.rs` — globs2/magic/aliases/subclasses parsing, detection, mimeapps resolution, writer
- `src/settings.rs` — `Setting`: xdg-settings properties as MIME associations
- `src/open.rs` — target parsing (`file:` URIs, URLs), MIME resolution, `$BROWSER`/`$VESPER_FALLBACKS` fallbacks
- `src/main.rs` — clap CLI, `argv[0]` dispatch, xdg-utils argument parsing, inspect output
- `src/test_support.rs` — `#[cfg(test)]` temp dir and `test_entry` helpers

### Macro
`desktop_entry_accessors!` — data-driven DSL in the `impl DesktopEntry` block:
- `str NAME KEY DEFAULT` → `pub fn` returning `&str` with fallback
- `opt NAME KEY` → `pub fn` returning `Option<&str>`
- `bool NAME KEY` → `pub fn` returning `bool` (checks `== "true"`)

### Key functions
- `desktop::parse_ini_groups` — one parser for `.desktop` and `mimeapps.list`; group order preserved
- `desktop_entry_id` — computes IDs from relative paths (`foo/bar.desktop` → `foo-bar`)
- `desktop::find_entry` / `find_by_id` — lookup by path or ID; IDs resolve through the same discovery as `list`
- `desktop::group_name` — the one definition of a `[Group]` header, shared by the parser and the mimeapps writer
- `desktop::discover_all` / `collect_entries` — scan `$XDG_DATA_HOME/applications` + `$XDG_DATA_DIRS/applications`, user entries override system entries
- `desktop::walk_dir` — recursive discovery, each directory visited once (symlink-loop guard)
- `exec::tokenize_exec` / `expand_token` / `build_command` — tokenize `Exec`, expand field codes in one pass; substituted values are never rescanned
- `exec::route_args` — routes arguments to file vs URL field codes and reports dropped args
- `launch::command_for_entry` — routes args and builds `std::process::Command` for Application entries; `Terminal=true` via `--terminal` or `$TERMINAL -e` → `xdg-terminal-exec` → `x-terminal-emulator -e`
- `launch::launch` — route args, build the command, `execute` it
- `launch::execute` — `Replace` (exec), `Detach` (setsid + null stdio), `Spawn` (setsid, inherited stdio); fallback commands use it too
- `mime::MimeDatabase::load_mime_dir` — parse `aliases`, `subclasses`, `globs2`/`globs`, compiled `magic`
- `mime::MimeDatabase::detect` — glob first, magic when needed, `stat` for `inode/*`
- `mime::Resolver::resolve` — returns the `DesktopEntry`: defaults → added minus removed → `MimeType=` claims → parent types
- `mime::set_default_in` — atomic mimeapps.list writer; testable without process env
- `settings::Setting::parse` / `get` / `set` — one place per xdg-settings property
- `open::open_target` — parse target, detect MIME, resolve entry, launch; Link entries recurse with a depth guard
- `xdg::data_dirs` / `config_dirs` — home dir first, then the system list; relative and empty XDG values are ignored
- `xdg::find_binary` — PATH lookup, requires a regular executable file

### Conventions
- No `unsafe` except the `setsid` FFI declaration in `launch.rs`
- Errors that decide the exit code are `Failure`; everything else is `eyre` and exits 1
- Desktop entry IDs carry no `.desktop` suffix in code; `DesktopEntry::desktop_file_id` adds it for output
- Never call `xdg-open`, `xdg-mime`, `gio`, or `perl`; the binary must stand alone
- MIME data is read from XDG dirs; never vendor a static MIME table
- Do not read `mimeinfo.cache`; scan `MimeType=` on entries
- Fallback commands run tokenized, never through a shell
- `mimeapps.list` writes preserve comments and use temp+rename; symlinks survive

### Dependencies
- `clap` (derive) — CLI
- `eyre` — errors
- `serde_json` — `--json` output
