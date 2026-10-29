use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("vesper-cli-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vesper"))
}

fn base_command() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env("XDG_DATA_HOME", root().join("fixtures"))
        .env("XDG_DATA_DIRS", "/nonexistent")
        .env("XDG_CONFIG_HOME", root().join("fixtures"))
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env("HOME", "/tmp")
        .env("XDG_CURRENT_DESKTOP", "niri")
        .env_remove("BROWSER")
        .env_remove("VESPER_FALLBACKS")
        .env_remove("TERMINAL");
    cmd
}

fn run(args: &[&str]) -> Output {
    base_command().args(args).output().unwrap()
}

fn run_with_xdg(args: &[&str], data_home: &Path, config_home: &Path) -> Output {
    base_command()
        .env("XDG_DATA_HOME", data_home)
        .env("XDG_CONFIG_HOME", config_home)
        .args(args)
        .output()
        .unwrap()
}

fn run_linked(link: &Path, args: &[&str]) -> Output {
    Command::new(link)
        .env("XDG_DATA_HOME", root().join("fixtures"))
        .env("XDG_DATA_DIRS", "/nonexistent")
        .env("XDG_CONFIG_HOME", root().join("fixtures"))
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env("HOME", "/tmp")
        .env("XDG_CURRENT_DESKTOP", "niri")
        .env_remove("BROWSER")
        .env_remove("VESPER_FALLBACKS")
        .env_remove("TERMINAL")
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

#[test]
fn list_reads_fixture_entries() {
    let output = run(&["list"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains("Test Browser\t"));
    assert!(text.contains("\ttest-browser"));
}

#[test]
fn mime_query_filetype_uses_globs_and_magic() {
    let svg = root().join("fixtures/test.svg");
    let magic = root().join("fixtures/test.vesper");

    let output = run(&["mime", "query", "filetype", svg.to_str().unwrap()]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "image/svg+xml");

    let output = run(&["mime", "query", "filetype", magic.to_str().unwrap()]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "application/x-vesper-magic");
}

#[test]
fn mime_query_default_reads_mimeapps() {
    let output = run(&["mime", "query", "default", "image/svg+xml"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "test-browser.desktop");

    let output = run(&["mime", "query", "default", "application/x-nothing"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output), "");
}

#[test]
fn open_reports_missing_and_unassociated_targets() {
    let output = run(&["open", "/nonexistent/thing.svg"]);
    assert_eq!(code(&output), 2);

    let binary = root().join("fixtures/test.bin");
    let output = run(&["open", binary.to_str().unwrap()]);
    assert_eq!(code(&output), 3);
}

#[test]
fn open_launches_the_resolved_entry() {
    let dir = TempDir::new("open-success");
    dir.write(
        "data/applications/run.desktop",
        "[Desktop Entry]\nType=Application\nName=Run\nExec=true %f\n",
    );
    dir.write(
        "config/mimeapps.list",
        "[Default Applications]\ntext/plain=run.desktop;\n",
    );
    let target = dir.write("note.txt", "hello\n");

    let output = run_with_xdg(
        &["open", target.to_str().unwrap()],
        &dir.0.join("data"),
        &dir.0.join("config"),
    );
    assert_eq!(code(&output), 0);
}

#[test]
fn settings_read_the_browser_association() {
    let output = run(&["settings", "get", "default-web-browser"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "test-browser.desktop");
}

#[test]
fn writer_updates_the_configured_file() {
    let dir = TempDir::new("writer");
    dir.write("config/mimeapps.list", "# keep\n[Default Applications]\n");

    let output = run_with_xdg(
        &[
            "mime",
            "default",
            "test-editor.desktop",
            "image/png",
            "image/jpeg",
        ],
        &root().join("fixtures"),
        &dir.0.join("config"),
    );
    assert_eq!(code(&output), 0);

    let content = fs::read_to_string(dir.0.join("config/mimeapps.list")).unwrap();
    assert!(content.contains("# keep"));
    assert!(content.contains("image/png=test-editor.desktop;"));
    assert!(content.contains("image/jpeg=test-editor.desktop;"));
}

#[test]
fn writer_fails_on_read_only_config() {
    let dir = TempDir::new("writer-readonly");
    let path = dir.write("config/mimeapps.list", "[Default Applications]\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

    let output = run_with_xdg(
        &["mime", "default", "test-editor.desktop", "image/png"],
        &root().join("fixtures"),
        &dir.0.join("config"),
    );
    assert_eq!(code(&output), 6);
}

#[test]
fn xdg_open_name_is_a_drop_in() {
    let dir = TempDir::new("xdg-open");
    let link = dir.0.join("xdg-open");
    std::os::unix::fs::symlink(bin(), &link).unwrap();

    let output = run_linked(&link, &["/nonexistent/thing.svg"]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("xdg-open:"));

    let output = run_linked(&link, &["--help"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("xdg-open"));

    dir.write(
        "data/applications/run.desktop",
        "[Desktop Entry]\nType=Application\nName=Run\nExec=true %f\n",
    );
    dir.write("data/mime/globs2", "50:image/svg+xml:*.svg\n");
    dir.write(
        "config/mimeapps.list",
        "[Default Applications]\nimage/svg+xml=run.desktop;\n",
    );
    let svg = dir.write("icon.svg", "<svg/>\n");

    let output = Command::new(&link)
        .env("XDG_DATA_HOME", dir.0.join("data"))
        .env("XDG_DATA_DIRS", "/nonexistent")
        .env("XDG_CONFIG_HOME", dir.0.join("config"))
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env("HOME", "/tmp")
        .env("XDG_CURRENT_DESKTOP", "niri")
        .env_remove("BROWSER")
        .env_remove("VESPER_FALLBACKS")
        .arg(svg.to_str().unwrap())
        .output()
        .unwrap();

    assert_eq!(code(&output), 0);
}

#[test]
fn xdg_mime_name_maps_to_mime_queries() {
    let dir = TempDir::new("xdg-mime");
    let link = dir.0.join("xdg-mime");
    std::os::unix::fs::symlink(bin(), &link).unwrap();

    let output = run_linked(&link, &["query", "default", "image/svg+xml"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "test-browser.desktop");

    let output = run_linked(&link, &["query", "filetype", "/nonexistent"]);
    assert_eq!(code(&output), 2);
}

#[test]
fn xdg_settings_name_reads_the_browser() {
    let dir = TempDir::new("xdg-settings");
    let link = dir.0.join("xdg-settings");
    std::os::unix::fs::symlink(bin(), &link).unwrap();

    let output = run_linked(&link, &["get", "default-web-browser"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), "test-browser.desktop");
}

#[test]
fn v_name_opens_multiple_targets() {
    let dir = TempDir::new("v-name");
    dir.write(
        "data/applications/run.desktop",
        "[Desktop Entry]\nType=Application\nName=Run\nExec=true %f\n",
    );
    dir.write(
        "config/mimeapps.list",
        "[Default Applications]\ntext/plain=run.desktop;\n",
    );
    let first = dir.write("one.txt", "one\n");
    let second = dir.write("two.txt", "two\n");

    let link = dir.0.join("v");
    std::os::unix::fs::symlink(bin(), &link).unwrap();

    let output = Command::new(&link)
        .env("XDG_DATA_HOME", dir.0.join("data"))
        .env("XDG_DATA_DIRS", "/nonexistent")
        .env("XDG_CONFIG_HOME", dir.0.join("config"))
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env("HOME", "/tmp")
        .env("XDG_CURRENT_DESKTOP", "niri")
        .env_remove("BROWSER")
        .env_remove("VESPER_FALLBACKS")
        .args([first.to_str().unwrap(), second.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(code(&output), 0);
}
