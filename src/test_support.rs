// ---------------------------------------------------------------------------
// Test-only helpers
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::desktop::DesktopEntry;

/// An Application entry with the ID `test` and the given Exec value.
pub fn test_entry(exec: &str) -> DesktopEntry {
    let mut raw = BTreeMap::new();
    raw.insert("Type".into(), "Application".into());
    raw.insert("Name".into(), "Test App".into());
    raw.insert("Exec".into(), exec.into());

    DesktopEntry {
        path: PathBuf::from("/tmp/test.desktop"),
        raw,
        id: "test".into(),
    }
}

/// Temporary directory removed on drop, even if the test panics.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("vesper-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn write(&self, relative: &str, content: &str) -> PathBuf {
        self.write_bytes(relative, content.as_bytes())
    }

    pub fn write_bytes(&self, relative: &str, content: &[u8]) -> PathBuf {
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
