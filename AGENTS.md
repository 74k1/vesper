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
- `src/main.rs` — clap CLI, `argv[0]` dispatch, xdg-utils argument parsing, inspect output
- `src/desktop.rs` — `DesktopEntry`, shared INI group parser, discovery, IDs, autostart
- `src/exec.rs` — Exec tokenizer, field codes, `route_extra_args`
- `src/launch.rs` — terminal resolution, command construction, `LaunchMode`, setsid
- `src/mime.rs` — globs2/magic/aliases/subclasses parsing, detection, mimeapps resolution, writer, settings helpers
- `src/open.rs` — target parsing (`file:` URIs, URLs), MIME resolution, fallbacks, `OpenError` exit codes
- `src/xdg.rs` — XDG base dirs, `$BROWSER`/`$VESPER_FALLBACKS`, `find_binary`, percent decoding
- `src/test_support.rs` — `#[cfg(test)]` temp dir helper

### Macro
`desktop_entry_accessors!` — data-driven DSL in the `impl DesktopEntry` block:
- `str NAME KEY DEFAULT` → `pub fn` returning `&str` with fallback
- `opt NAME KEY` → `pub fn` returning `Option<&str>`
- `bool NAME KEY` → `pub fn` returning `bool` (checks `== "true"`)

### Key functions
- `desktop::parse_ini_groups` — one parser for `.desktop` and `mimeapps.list`; group order preserved
- `desktop_entry_id` — computes IDs from relative paths (`foo/bar.desktop` → `foo-bar`)
- `desktop::discover_all` / `collect_entries` — scan `$XDG_DATA_HOME/applications` + `$XDG_DATA_DIRS/applications`, user entries override system entries
- `desktop::walk_dir` — recursive discovery, each directory visited once (symlink-loop guard)
- `exec::tokenize_exec` / `expand_token` / `build_command` — tokenize `Exec`, expand field codes in one pass; substituted values are never rescanned
- `exec::route_extra_args` — routes `--` args to file vs URL field codes and reports dropped args
- `launch::command_for_entry` — builds `std::process::Command` for Application entries; `Terminal=true` via `--terminal` or `$TERMINAL -e` → `xdg-terminal-exec` → `x-terminal-emulator -e`
- `launch::execute` — `Replace` (exec), `Detach` (setsid + null stdio), `Spawn` (setsid, inherited stdio)
- `mime::MimeDatabase::load_mime_dir` — parse `aliases`, `subclasses`, `globs2`/`globs`, compiled `magic`
- `mime::MimeDatabase::detect` — glob first, magic when needed, `stat` for `inode/*`
- `mime::Resolver::resolve` — defaults → added minus removed → `MimeType=` claims → parent types
- `mime::set_default_in` — atomic mimeapps.list writer; testable without process env
- `open::open_target` — parse target, detect MIME, resolve entry, launch; Link entries recurse with a depth guard
- `xdg::absolute_path` / `path_list` — ignore relative and empty XDG values
- `xdg::find_binary` — PATH lookup, requires a regular executable file

### Conventions
- No `unsafe` except the `setsid` FFI declaration in `launch.rs`
- Never call `xdg-open`, `xdg-mime`, `gio`, or `perl`; the binary must stand alone
- MIME data is read from XDG dirs; never vendor a static MIME table
- Do not read `mimeinfo.cache`; scan `MimeType=` on entries
- Fallback commands run tokenized, never through a shell
- `mimeapps.list` writes preserve comments and use temp+rename; symlinks survive

### Dependencies
- `clap` (derive) — CLI
- `eyre` — errors
- `serde_json` — `--json` output
