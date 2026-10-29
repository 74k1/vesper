// ---------------------------------------------------------------------------
// MIME detection, mimeapps resolution, writer, and settings helpers
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::desktop::{self, DesktopEntry};
use crate::xdg;

const OCTET_STREAM: &str = "application/octet-stream";
const TEXT_PLAIN: &str = "text/plain";
const X_DESKTOP: &str = "application/x-desktop";

/// MIME types written by `xdg-settings set default-web-browser`.
pub const WEB_BROWSER_MIMES: [&str; 5] = [
    "text/html",
    "x-scheme-handler/http",
    "x-scheme-handler/https",
    "x-scheme-handler/about",
    "x-scheme-handler/unknown",
];

// ---------------------------------------------------------------------------
// Glob matching
// ---------------------------------------------------------------------------

/// Match a shared-mime-info glob against a file name.
pub fn glob_match(pattern: &str, name: &str, case_sensitive: bool) -> bool {
    let pattern: Vec<char> = if case_sensitive {
        pattern.chars().collect()
    } else {
        pattern.chars().flat_map(|c| c.to_lowercase()).collect()
    };
    let name: Vec<char> = if case_sensitive {
        name.chars().collect()
    } else {
        name.chars().flat_map(|c| c.to_lowercase()).collect()
    };

    match_glob(&pattern, 0, &name, 0)
}

fn match_glob(pattern: &[char], mut pi: usize, name: &[char], mut ni: usize) -> bool {
    while pi < pattern.len() {
        match pattern[pi] {
            '*' => {
                while pi < pattern.len() && pattern[pi] == '*' {
                    pi += 1;
                }
                if pi == pattern.len() {
                    return true;
                }
                for skip in ni..=name.len() {
                    if match_glob(pattern, pi, name, skip) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ni >= name.len() {
                    return false;
                }
                pi += 1;
                ni += 1;
            }
            '[' => {
                let (matched, next) = match_class(pattern, pi, name.get(ni).copied());
                if !matched {
                    return false;
                }
                pi = next;
                ni += 1;
            }
            '\\' => {
                pi += 1;
                if pi >= pattern.len() || ni >= name.len() || pattern[pi] != name[ni] {
                    return false;
                }
                pi += 1;
                ni += 1;
            }
            literal => {
                if ni >= name.len() || literal != name[ni] {
                    return false;
                }
                pi += 1;
                ni += 1;
            }
        }
    }
    ni == name.len()
}

fn match_class(pattern: &[char], start: usize, candidate: Option<char>) -> (bool, usize) {
    let mut i = start + 1;
    let negated = matches!(pattern.get(i), Some('!') | Some('^'));
    if negated {
        i += 1;
    }

    let mut matched = false;
    let mut first = true;
    let mut closed = false;

    while i < pattern.len() {
        if pattern[i] == ']' && !first {
            closed = true;
            break;
        }
        first = false;

        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            let low = pattern[i];
            let high = pattern[i + 2];
            if let Some(c) = candidate {
                if low <= c && c <= high {
                    matched = true;
                }
            }
            i += 3;
        } else {
            if candidate == Some(pattern[i]) {
                matched = true;
            }
            i += 1;
        }
    }

    if !closed {
        return (candidate == Some('['), start + 1);
    }

    (matched != negated, i + 1)
}

// ---------------------------------------------------------------------------
// MIME database
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct GlobRule {
    weight: u32,
    mime: String,
    pattern: String,
    case_sensitive: bool,
}

#[derive(Debug, Clone)]
struct MagicRule {
    indent: u32,
    offset: usize,
    value: Vec<u8>,
    mask: Option<Vec<u8>>,
    range: usize,
}

#[derive(Debug, Clone)]
struct MagicEntry {
    priority: u32,
    mime: String,
    rules: Vec<MagicRule>,
}

#[derive(Debug, Default)]
pub struct MimeDatabase {
    globs: Vec<GlobRule>,
    magic: Vec<MagicEntry>,
    aliases: BTreeMap<String, String>,
    parents: BTreeMap<String, Vec<String>>,
}

impl MimeDatabase {
    /// Load the shared-mime-info database from the XDG data directories.
    pub fn load() -> Self {
        let mut db = MimeDatabase::default();
        for dir in mime_dirs() {
            db.load_mime_dir(&dir.join("mime"));
        }
        db
    }

    /// Load `aliases`, `subclasses`, `globs2` (or `globs`) and `magic` from
    /// one MIME data directory.
    pub fn load_mime_dir(&mut self, dir: &Path) {
        if let Ok(content) = fs::read_to_string(dir.join("aliases")) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(alias), Some(target)) = (parts.next(), parts.next()) {
                    if !alias.starts_with('#') {
                        self.aliases
                            .entry(alias.to_string())
                            .or_insert_with(|| target.to_string());
                    }
                }
            }
        }

        if let Ok(content) = fs::read_to_string(dir.join("subclasses")) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(child), Some(parent)) = (parts.next(), parts.next()) {
                    self.parents
                        .entry(child.to_string())
                        .or_default()
                        .push(parent.to_string());
                }
            }
        }

        if let Ok(content) = fs::read_to_string(dir.join("globs2")) {
            self.globs.extend(parse_globs2(&content));
        } else if let Ok(content) = fs::read_to_string(dir.join("globs")) {
            self.globs.extend(parse_globs1(&content));
        }

        if let Ok(bytes) = fs::read(dir.join("magic")) {
            self.magic.extend(parse_magic(&bytes));
        }
    }

    /// Follow aliases until a canonical type is found.
    pub fn unalias(&self, mime: &str) -> String {
        let mut current = mime.to_string();
        for _ in 0..8 {
            match self.aliases.get(&current) {
                Some(next) if *next != current => current = next.clone(),
                _ => break,
            }
        }
        current
    }

    /// All parent types of `mime`, transitively, excluding `mime` itself.
    pub fn ancestors(&self, mime: &str) -> Vec<String> {
        let canonical = self.unalias(mime);
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        seen.insert(canonical.clone());
        let mut queue = vec![canonical];

        while let Some(current) = queue.pop() {
            if let Some(parents) = self.parents.get(&current) {
                for parent in parents {
                    let parent = self.unalias(parent);
                    if seen.insert(parent.clone()) {
                        out.push(parent.clone());
                        queue.push(parent);
                    }
                }
            }
        }

        out
    }

    pub fn mime_type_subclass(&self, mime: &str, base: &str) -> bool {
        let mime = self.unalias(mime);
        let base = self.unalias(base);
        mime == base || self.ancestors(&mime).contains(&base)
    }

    /// Matching MIME types for a file name, highest weight first, ties broken
    /// by the longest pattern.
    fn glob_types(&self, file_name: &str) -> Vec<String> {
        let mut matching: Vec<&GlobRule> = self
            .globs
            .iter()
            .filter(|rule| glob_match(&rule.pattern, file_name, rule.case_sensitive))
            .collect();

        let Some(max_weight) = matching.iter().map(|rule| rule.weight).max() else {
            return Vec::new();
        };
        matching.retain(|rule| rule.weight == max_weight);

        let Some(max_length) = matching
            .iter()
            .map(|rule| rule.pattern.chars().count())
            .max()
        else {
            return Vec::new();
        };
        matching.retain(|rule| rule.pattern.chars().count() == max_length);

        let mut out: Vec<String> = Vec::new();
        for rule in matching {
            if !out.contains(&rule.mime) {
                out.push(rule.mime.clone());
            }
        }
        out
    }

    fn magic_extent(&self) -> usize {
        self.magic
            .iter()
            .flat_map(|entry| entry.rules.iter())
            .map(|rule| rule.offset + rule.value.len() + rule.range)
            .max()
            .unwrap_or(0)
    }

    fn sniff(&self, data: &[u8]) -> Option<(String, u32)> {
        self.magic
            .iter()
            .find(|entry| magic_entry_matches(entry, data))
            .map(|entry| (entry.mime.clone(), entry.priority))
    }

    /// Detect the MIME type of a path, following symlinks.
    pub fn detect(&self, path: &Path) -> String {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => return "inode/symlink".into(),
        };

        if metadata.is_dir() {
            return "inode/directory".into();
        }

        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let glob_types = self.glob_types(&file_name);

        if glob_types.len() == 1 && glob_types[0] != OCTET_STREAM {
            return glob_types[0].clone();
        }

        if metadata.len() == 0 {
            return glob_types
                .first()
                .cloned()
                .unwrap_or_else(|| TEXT_PLAIN.to_string());
        }

        let limit = self.magic_extent().clamp(128, 65536);
        let data = read_prefix(path, limit);
        let sniffed = self.sniff(&data);

        if glob_types.is_empty() {
            return match sniffed {
                Some((mut mime, _)) => {
                    if mime == OCTET_STREAM && looks_like_text(&data) {
                        mime = TEXT_PLAIN.into();
                    }
                    if mime == X_DESKTOP && !file_name.ends_with(".desktop") {
                        mime = TEXT_PLAIN.into();
                    }
                    mime
                }
                None => {
                    if looks_like_text(&data) {
                        TEXT_PLAIN.into()
                    } else {
                        OCTET_STREAM.into()
                    }
                }
            };
        }

        let (mut mime, priority) = sniffed.unwrap_or((OCTET_STREAM.to_string(), 80));

        if mime == OCTET_STREAM && looks_like_text(&data) {
            mime = TEXT_PLAIN.into();
        }

        // Do not let a file pretend to be a desktop entry without the suffix.
        if mime == X_DESKTOP && !file_name.ends_with(".desktop") {
            mime = TEXT_PLAIN.into();
        }

        if mime != OCTET_STREAM {
            if priority >= 80 {
                return mime;
            }
            if glob_types
                .iter()
                .any(|glob| self.mime_type_subclass(&mime, glob))
            {
                return mime;
            }
        }

        glob_types[0].clone()
    }
}

fn read_prefix(path: &Path, limit: usize) -> Vec<u8> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut limited = file.take(limit as u64);
    let mut buf = Vec::with_capacity(limit);
    if limited.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

fn looks_like_text(data: &[u8]) -> bool {
    !data
        .iter()
        .take(128)
        .any(|c| c.is_ascii_control() && !c.is_ascii_whitespace())
}

pub fn mime_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = xdg::xdg_data_home() {
        dirs.push(home);
    }
    dirs.extend(xdg::xdg_data_dirs());
    dirs
}

// ---------------------------------------------------------------------------
// globs2 / globs / magic parsing
// ---------------------------------------------------------------------------

fn parse_globs2(content: &str) -> Vec<GlobRule> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(4, ':');
        let Some(weight) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(mime) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(pattern) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let case_sensitive = parts
            .next()
            .map(|flags| flags.split(',').any(|flag| flag == "cs"))
            .unwrap_or(false);
        out.push(GlobRule {
            weight,
            mime: mime.to_string(),
            pattern: pattern.to_string(),
            case_sensitive,
        });
    }
    out
}

fn parse_globs1(content: &str) -> Vec<GlobRule> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, ':');
        let Some(mime) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(pattern) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        out.push(GlobRule {
            weight: 50,
            mime: mime.to_string(),
            pattern: pattern.to_string(),
            case_sensitive: false,
        });
    }
    out
}

fn parse_magic(bytes: &[u8]) -> Vec<MagicEntry> {
    const HEADER: &[u8] = b"MIME-Magic\0\n";
    if !bytes.starts_with(HEADER) {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let mut i = HEADER.len();

    while i < bytes.len() {
        if bytes[i] != b'[' {
            break;
        }

        let Some(colon) = bytes[i..].iter().position(|&b| b == b':') else {
            break;
        };
        let Some(close) = bytes[i + colon..].iter().position(|&b| b == b']') else {
            break;
        };

        let priority = std::str::from_utf8(&bytes[i + 1..i + colon])
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let mime_start = i + colon + 1;
        let mime_end = i + colon + close;
        let mime = String::from_utf8_lossy(&bytes[mime_start..mime_end]).into_owned();

        i = mime_end + 1;
        if i < bytes.len() && bytes[i] == b'\r' {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'\n' {
            i += 1;
        }

        let mut rules = Vec::new();
        while i < bytes.len() && bytes[i] != b'[' {
            let Some((rule, next)) = parse_magic_rule(bytes, i) else {
                break;
            };
            rules.push(rule);
            i = next;
        }

        if !rules.is_empty() {
            entries.push(MagicEntry {
                priority,
                mime,
                rules,
            });
        }
    }

    entries
}

fn parse_magic_rule(bytes: &[u8], start: usize) -> Option<(MagicRule, usize)> {
    let mut i = start;

    let indent_start = i;
    while i < bytes.len() && bytes[i] != b'>' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let indent = std::str::from_utf8(&bytes[indent_start..i])
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    i += 1; // '>'
    let offset_start = i;
    while i < bytes.len() && bytes[i] != b'=' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let offset = std::str::from_utf8(&bytes[offset_start..i])
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    i += 1; // '='
    if i + 2 > bytes.len() {
        return None;
    }
    let length = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
    i += 2;

    if i + length > bytes.len() {
        return None;
    }
    let value = bytes[i..i + length].to_vec();
    i += length;

    let mut mask = None;
    if i < bytes.len() && bytes[i] == b'&' {
        i += 1;
        if i + length > bytes.len() {
            return None;
        }
        mask = Some(bytes[i..i + length].to_vec());
        i += length;
    }

    if i < bytes.len() && bytes[i] == b'~' {
        i += 1;
        if i < bytes.len() {
            i += 1;
        }
    }

    let mut range = 1usize;
    if i < bytes.len() && bytes[i] == b'+' {
        i += 1;
        let number_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        range = std::str::from_utf8(&bytes[number_start..i])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
    }

    if i < bytes.len() && bytes[i] == b'\r' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    } else if i < bytes.len() {
        return None;
    }

    Some((
        MagicRule {
            indent,
            offset,
            value,
            mask,
            range,
        },
        i,
    ))
}

fn magic_rule_matches(rule: &MagicRule, data: &[u8]) -> bool {
    let length = rule.value.len();
    if length == 0 || data.len() < length || rule.offset > data.len() - length {
        return false;
    }

    let max_start = data.len() - length;
    let end = (rule.offset + rule.range).min(max_start + 1);

    for start in rule.offset..end {
        let window = &data[start..start + length];
        let matched = match &rule.mask {
            Some(mask) => window
                .iter()
                .zip(mask)
                .zip(&rule.value)
                .all(|((byte, mask), value)| (byte & mask) == (value & mask)),
            None => window == rule.value.as_slice(),
        };
        if matched {
            return true;
        }
    }

    false
}

fn magic_entry_matches(entry: &MagicEntry, data: &[u8]) -> bool {
    let mut level = 0u32;
    let mut iter = entry.rules.iter().peekable();

    while let Some(rule) = iter.next() {
        if rule.indent == level && magic_rule_matches(rule, data) {
            match iter.peek() {
                Some(next) if next.indent > level => level += 1,
                _ => return true,
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// mimeapps.list resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct MimeAppsFile {
    defaults: BTreeMap<String, Vec<String>>,
    added: BTreeMap<String, Vec<String>>,
    removed: BTreeMap<String, Vec<String>>,
}

fn parse_mimeapps(content: &str) -> MimeAppsFile {
    let mut file = MimeAppsFile::default();

    for (group, keys) in desktop::parse_ini_groups(content, false) {
        let target = match group.as_str() {
            "Default Applications" => &mut file.defaults,
            "Added Associations" => &mut file.added,
            "Removed Associations" => &mut file.removed,
            _ => continue,
        };

        for (mime, value) in keys {
            let ids: Vec<String> = value
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !ids.is_empty() {
                target.entry(mime).or_default().extend(ids);
            }
        }
    }

    file
}

fn read_mimeapps(path: &Path) -> Option<MimeAppsFile> {
    let content = fs::read_to_string(path).ok()?;
    Some(parse_mimeapps(&content))
}

/// mimeapps.list files in precedence order, including desktop-specific
/// variants and the deprecated defaults.list.
pub fn mimeapps_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let desktops = xdg::desktop_names();
    let desktop_options: Vec<Option<String>> = if desktops.is_empty() {
        vec![None]
    } else {
        desktops.into_iter().map(Some).collect()
    };

    for desktop in desktop_options {
        let prefix = desktop.map(|name| format!("{name}-"));

        let mut push_slot = |base: PathBuf| {
            if let Some(prefix) = &prefix {
                out.push(base.join(format!("{prefix}mimeapps.list")));
            }
            out.push(base.join("mimeapps.list"));
        };

        if let Some(home) = xdg::xdg_config_home() {
            push_slot(home);
        }
        for dir in xdg::xdg_config_dirs() {
            push_slot(dir);
        }
        if let Some(home) = xdg::xdg_data_home() {
            push_slot(home.join("applications"));
        }
        for dir in xdg::xdg_data_dirs() {
            push_slot(dir.join("applications"));
        }
    }

    if let Some(home) = xdg::xdg_data_home() {
        out.push(home.join("applications/defaults.list"));
    }
    for dir in xdg::xdg_data_dirs() {
        out.push(dir.join("applications/defaults.list"));
    }

    out
}

pub struct Resolver {
    db: MimeDatabase,
    files: Vec<MimeAppsFile>,
    entries: Vec<DesktopEntry>,
}

impl Resolver {
    pub fn load() -> Self {
        let db = MimeDatabase::load();
        let files = mimeapps_files()
            .into_iter()
            .filter_map(|path| read_mimeapps(&path))
            .collect();
        let entries = desktop::discover_all();
        Resolver { db, files, entries }
    }

    pub fn detect(&self, path: &Path) -> String {
        self.db.detect(path)
    }

    /// Resolve a MIME type to a desktop entry ID.
    pub fn resolve(&self, mime: &str) -> Option<String> {
        let canonical = self.db.unalias(mime);
        let mut order = vec![canonical.clone()];
        if canonical != mime {
            order.push(mime.to_string());
        }

        for candidate in &order {
            if let Some(id) = self.lookup_exact(candidate) {
                return Some(id);
            }
            if let Some(id) = self.claim(candidate) {
                return Some(id);
            }
        }

        for parent in self.db.ancestors(&canonical) {
            if order.contains(&parent) {
                continue;
            }
            if let Some(id) = self.lookup_exact(&parent) {
                return Some(id);
            }
            if let Some(id) = self.claim(&parent) {
                return Some(id);
            }
        }

        None
    }

    fn has_entry(&self, id: &str) -> bool {
        let id = normalize_id(id);
        self.entries
            .iter()
            .any(|entry| normalize_id(&entry.id) == id)
    }

    fn lookup_exact(&self, mime: &str) -> Option<String> {
        let mut removed: BTreeSet<String> = BTreeSet::new();
        let mut added: Vec<String> = Vec::new();
        let mut default: Option<String> = None;

        for file in &self.files {
            if default.is_none() {
                if let Some(ids) = file.defaults.get(mime) {
                    default = ids.iter().find(|id| !removed.contains(*id)).cloned();
                }
            }

            if let Some(ids) = file.added.get(mime) {
                for id in ids {
                    if !removed.contains(id) && !added.contains(id) {
                        added.push(id.clone());
                    }
                }
            }

            if let Some(ids) = file.removed.get(mime) {
                removed.extend(ids.iter().cloned());
            }
        }

        default
            .or_else(|| added.into_iter().next())
            .filter(|id| self.has_entry(id))
            .map(|id| with_desktop_suffix(&id))
    }

    fn claim(&self, mime: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|entry| {
                entry
                    .mime_types()
                    .iter()
                    .any(|claimed| claimed == mime || self.db.unalias(claimed) == mime)
            })
            .map(|entry| with_desktop_suffix(&entry.id))
    }

    #[cfg(test)]
    fn from_parts(db: MimeDatabase, files: Vec<MimeAppsFile>, entries: Vec<DesktopEntry>) -> Self {
        Resolver { db, files, entries }
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Resolver {
            db: MimeDatabase::default(),
            files: Vec::new(),
            entries: Vec::new(),
        }
    }
}

/// The default browser, read from `text/html` then `x-scheme-handler/http`.
pub fn web_browser(resolver: &Resolver) -> Option<String> {
    resolver
        .resolve("text/html")
        .or_else(|| resolver.resolve("x-scheme-handler/http"))
}

pub fn web_browser_mimes() -> Vec<String> {
    WEB_BROWSER_MIMES
        .iter()
        .map(|mime| mime.to_string())
        .collect()
}

pub fn scheme_mime(scheme: &str) -> String {
    format!("x-scheme-handler/{}", scheme.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// mimeapps.list writer
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct WriteError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

fn write_error(path: &Path, err: &io::Error) -> WriteError {
    WriteError {
        code: 6,
        message: format!("cannot write {} ({})", path.display(), err),
    }
}

/// Set the default application for each MIME type in
/// `$XDG_CONFIG_HOME/mimeapps.list`.
pub fn set_default(app: &str, mimes: &[String], warn_missing: bool) -> Result<(), WriteError> {
    let config_home = xdg::xdg_config_home().ok_or_else(|| WriteError {
        code: 1,
        message: "XDG_CONFIG_HOME and HOME are unset".into(),
    })?;
    set_default_in(&config_home, app, mimes, warn_missing)
}

/// Set defaults in `config_home/mimeapps.list`. Split out so tests do not
/// mutate process environment.
pub fn set_default_in(
    config_home: &Path,
    app: &str,
    mimes: &[String],
    warn_missing: bool,
) -> Result<(), WriteError> {
    if !app.ends_with(".desktop") {
        return Err(WriteError {
            code: 1,
            message: format!("malformed argument '{}', expected *.desktop", app),
        });
    }
    if mimes.is_empty() {
        return Err(WriteError {
            code: 1,
            message: "no MIME type given".into(),
        });
    }

    let target = config_home.join("mimeapps.list");
    let resolved = fs::canonicalize(&target).unwrap_or_else(|_| target.clone());

    let existing = match fs::read(&resolved) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| WriteError {
            code: 5,
            message: format!("{} is not valid UTF-8", resolved.display()),
        })?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(WriteError {
                code: 5,
                message: format!("cannot read {} ({})", resolved.display(), err),
            })
        }
    };

    if resolved.exists() {
        if let Err(err) = fs::OpenOptions::new().write(true).open(&resolved) {
            return Err(write_error(&resolved, &err));
        }
    }

    if warn_missing && desktop::find_entry(app).is_err() {
        eprintln!(
            "vesper: warning: {} is not a discoverable desktop entry",
            app
        );
    }

    let updated = edit_defaults(&existing, app, mimes);
    let parent = resolved.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| write_error(parent, &err))?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temporary = parent.join(format!(
        ".mimeapps.list.tmp-{}-{}",
        std::process::id(),
        nanos
    ));

    fs::write(&temporary, updated.as_bytes()).map_err(|err| write_error(&temporary, &err))?;
    if let Ok(metadata) = fs::metadata(&resolved) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }
    fs::rename(&temporary, &resolved).map_err(|err| {
        let _ = fs::remove_file(&temporary);
        write_error(&resolved, &err)
    })?;

    Ok(())
}

fn edit_defaults(content: &str, app: &str, mimes: &[String]) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let header = "[Default Applications]";

    let mut section_start = None;
    let mut section_end = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            if name == "Default Applications" {
                section_start = Some(index);
            } else if section_start.is_some() {
                section_end = Some(index);
                break;
            }
        }
    }

    match section_start {
        Some(start) => {
            let end = section_end.unwrap_or(lines.len());
            let mut kept: Vec<String> = lines[..=start].to_vec();

            for line in &lines[start + 1..end] {
                let trimmed = line.trim();
                if let Some((key, _)) = trimmed.split_once('=') {
                    if mimes.iter().any(|mime| mime == key.trim()) {
                        continue;
                    }
                }
                kept.push(line.clone());
            }

            for mime in mimes {
                kept.push(format!("{mime}={app};"));
            }

            kept.extend(lines[end..].iter().cloned());
            lines = kept;
        }
        None => {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header.to_string());
            for mime in mimes {
                lines.push(format!("{mime}={app};"));
            }
        }
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn normalize_id(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

fn with_desktop_suffix(id: &str) -> String {
    if id.ends_with(".desktop") {
        id.to_string()
    } else {
        format!("{id}.desktop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn entry(id: &str, mime: Option<&str>) -> DesktopEntry {
        let mut raw = BTreeMap::new();
        raw.insert("Type".into(), "Application".into());
        raw.insert("Name".into(), id.into());
        raw.insert("Exec".into(), "prog".into());
        if let Some(mime) = mime {
            raw.insert("MimeType".into(), mime.into());
        }
        DesktopEntry::new(PathBuf::from(format!("/tmp/{id}.desktop")), raw, id.into())
    }

    fn magic_rule_bytes(indent: &str, offset: usize, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{indent}>{offset}=").as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value);
        out.push(b'\n');
        out
    }

    fn magic_file(entries: &[(&str, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut out = b"MIME-Magic\0\n".to_vec();
        for (mime, rules) in entries {
            out.extend_from_slice(format!("[50:{mime}]\n").as_bytes());
            for rule in rules {
                out.extend_from_slice(rule);
            }
        }
        out
    }

    // -- globs --------------------------------------------------------------

    #[test]
    fn globs_match_literals_patterns_and_classes() {
        assert!(glob_match("*.svg", "a.svg", false));
        assert!(glob_match("*.SVG", "a.svg", false));
        assert!(!glob_match("*.SVG", "a.svg", true));
        assert!(glob_match("Makefile", "Makefile", true));
        assert!(glob_match("file[0-9].txt", "file7.txt", false));
        assert!(!glob_match("file[!0-9].txt", "file7.txt", false));
        assert!(glob_match("a?c", "abc", false));
        assert!(!glob_match("a?c", "ac", false));
    }

    #[test]
    fn parses_globs2_weights_and_flags() {
        let rules = parse_globs2("50:text/plain:*.txt\n80:image/svg+xml:*.svg:cs\n# c\n");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].weight, 50);
        assert_eq!(rules[1].mime, "image/svg+xml");
        assert!(rules[1].case_sensitive);
    }

    #[test]
    fn glob_types_keep_biggest_weight_then_longest_pattern() {
        let db = MimeDatabase {
            globs: parse_globs2(
                "50:text/plain:*.txt\n80:text/x-short:*.txt\n80:text/x-long:*.long.txt\n",
            ),
            ..Default::default()
        };
        assert_eq!(db.glob_types("a.long.txt"), vec!["text/x-long"]);
        assert_eq!(db.glob_types("a.txt"), vec!["text/x-short"]);
    }

    // -- magic --------------------------------------------------------------

    #[test]
    fn parses_and_matches_magic_rules() {
        let bytes = magic_file(&[(
            "application/x-test",
            vec![magic_rule_bytes("", 0, b"MAGIC")],
        )]);
        let entries = parse_magic(&bytes);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].priority, 50);
        assert!(magic_entry_matches(&entries[0], b"MAGIC file"));
        assert!(!magic_entry_matches(&entries[0], b"not magic"));

        let shifted = magic_file(&[(
            "application/x-shifted",
            vec![magic_rule_bytes("", 4, b"MAGIC")],
        )]);
        let entries = parse_magic(&shifted);
        assert!(magic_entry_matches(&entries[0], b"headMAGIC"));
    }

    #[test]
    fn magic_nested_rules_require_parent_match() {
        let nested = magic_file(&[(
            "application/x-nested",
            vec![
                magic_rule_bytes("", 0, b"PK"),
                magic_rule_bytes("1", 2, b"mimetype"),
            ],
        )]);
        let entries = parse_magic(&nested);
        assert!(magic_entry_matches(&entries[0], b"PKmimetype"));
        assert!(!magic_entry_matches(&entries[0], b"PKother"));
    }

    #[test]
    fn magic_masks_ignore_compared_bits() {
        let mut bytes = b"MIME-Magic\0\n[50:application/x-mask]\n".to_vec();
        bytes.extend_from_slice(b">0=");
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(b"\x00\x42");
        bytes.extend_from_slice(b"&\xf0\xff");
        bytes.push(b'\n');

        let entries = parse_magic(&bytes);
        assert!(magic_entry_matches(&entries[0], b"\x0a\x42"));
        assert!(magic_entry_matches(&entries[0], b"\x0b\x42"));
        assert!(!magic_entry_matches(&entries[0], b"\x1a\x42"));
        assert!(!magic_entry_matches(&entries[0], b"\x0a\x43"));
    }

    // -- detection ----------------------------------------------------------

    #[test]
    fn detects_directories_globs_magic_and_text() {
        let dir = TempDir::new("detect");
        dir.write(
            "mime/globs2",
            "50:image/svg+xml:*.svg\n50:application/x-magic:*.magic\n",
        );
        let magic = magic_file(&[(
            "application/x-magic",
            vec![magic_rule_bytes("", 0, b"MAGIC")],
        )]);
        dir.write_bytes("mime/magic", &magic);

        let mut db = MimeDatabase::default();
        db.load_mime_dir(&dir.0.join("mime"));

        dir.write("a.svg", "<svg/>");
        dir.write("b.magic", "MAGIC");
        dir.write("c.unknown", "MAGIC");
        dir.write("d.unknown", "just text\n");
        fs::create_dir_all(dir.0.join("sub")).unwrap();

        assert_eq!(db.detect(&dir.0.join("a.svg")), "image/svg+xml");
        assert_eq!(db.detect(&dir.0.join("b.magic")), "application/x-magic");
        assert_eq!(db.detect(&dir.0.join("c.unknown")), "application/x-magic");
        assert_eq!(db.detect(&dir.0.join("d.unknown")), "text/plain");
        assert_eq!(db.detect(&dir.0.join("sub")), "inode/directory");
    }

    #[test]
    fn aliases_and_parents_are_transitive() {
        let mut db = MimeDatabase::default();
        db.aliases.insert("image/jpg".into(), "image/jpeg".into());
        db.parents
            .insert("image/svg+xml".into(), vec!["application/xml".into()]);
        db.parents
            .insert("application/xml".into(), vec!["text/plain".into()]);

        assert_eq!(db.unalias("image/jpg"), "image/jpeg");
        assert_eq!(
            db.ancestors("image/svg+xml"),
            vec!["application/xml", "text/plain"]
        );
        assert!(db.mime_type_subclass("image/svg+xml", "text/plain"));
        assert!(!db.mime_type_subclass("text/plain", "image/svg+xml"));
    }

    // -- mimeapps -----------------------------------------------------------

    #[test]
    fn mimeapps_precedence_and_removals() {
        let first = parse_mimeapps(
            "[Default Applications]\ntext/html=first.desktop;\n\
             [Added Associations]\napplication/x-a=first.desktop;\n\
             [Removed Associations]\napplication/x-b=second.desktop;\n",
        );
        let second = parse_mimeapps(
            "[Default Applications]\ntext/html=third.desktop;\n\
             [Added Associations]\napplication/x-a=first.desktop;\n\
             application/x-b=second.desktop;third.desktop;\n\
             [Removed Associations]\napplication/x-a=first.desktop;\n",
        );

        let entries = vec![
            entry("first.desktop", None),
            entry("second.desktop", None),
            entry("third.desktop", None),
        ];
        let resolver = Resolver::from_parts(MimeDatabase::default(), vec![first, second], entries);

        assert_eq!(resolver.resolve("text/html"), Some("first.desktop".into()));
        // A removal in a lower-priority file must not remove a higher one.
        assert_eq!(
            resolver.resolve("application/x-a"),
            Some("first.desktop".into())
        );
        // A higher-priority removal filters a lower-priority addition.
        assert_eq!(
            resolver.resolve("application/x-b"),
            Some("third.desktop".into())
        );
    }

    #[test]
    fn mimeapps_falls_back_to_claims_and_parents() {
        let mut db = MimeDatabase::default();
        db.parents
            .insert("text/x-python".into(), vec!["text/plain".into()]);
        db.parents
            .insert("text/x-markdown".into(), vec!["text/plain".into()]);

        let entries = vec![
            entry("python.desktop", Some("text/x-python;")),
            entry("plain.desktop", Some("text/plain;")),
        ];
        let resolver = Resolver::from_parts(db, Vec::new(), entries);

        assert_eq!(
            resolver.resolve("text/x-python"),
            Some("python.desktop".into())
        );
        assert_eq!(
            resolver.resolve("text/x-markdown"),
            Some("plain.desktop".into())
        );
    }

    #[test]
    fn mimeapps_ignores_missing_desktop_entries() {
        let file = parse_mimeapps("[Default Applications]\ntext/html=missing.desktop;\n");
        let resolver = Resolver::from_parts(MimeDatabase::default(), vec![file], Vec::new());
        assert_eq!(resolver.resolve("text/html"), None);
    }

    #[test]
    fn web_browser_uses_text_html_then_http() {
        let file =
            parse_mimeapps("[Default Applications]\nx-scheme-handler/http=browser.desktop;\n");
        let resolver = Resolver::from_parts(
            MimeDatabase::default(),
            vec![file],
            vec![entry("browser.desktop", None)],
        );
        assert_eq!(web_browser(&resolver), Some("browser.desktop".into()));
    }

    // -- writer -------------------------------------------------------------

    #[test]
    fn writer_creates_section_and_preserves_other_content() {
        let dir = TempDir::new("writer");
        dir.write(
            "mimeapps.list",
            "# keep me\n[Default Applications]\nimage/png=old.desktop;\n# tail\n\
             [Added Associations]\ntext/html=x.desktop;\n",
        );

        set_default_in(
            &dir.0,
            "new.desktop",
            &["image/png".into(), "image/jpeg".into()],
            false,
        )
        .unwrap();

        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert!(content.contains("# keep me"));
        assert!(content.contains("image/png=new.desktop;"));
        assert!(content.contains("image/jpeg=new.desktop;"));
        assert!(!content.contains("image/png=old.desktop;"));
        assert!(content.contains("# tail"));
        assert!(content.contains("[Added Associations]"));
        assert!(content.contains("text/html=x.desktop;"));
    }

    #[test]
    fn writer_creates_missing_file() {
        let dir = TempDir::new("writer-new");
        set_default_in(&dir.0, "app.desktop", &["text/plain".into()], false).unwrap();
        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert!(content.contains("[Default Applications]"));
        assert!(content.contains("text/plain=app.desktop;"));
    }

    #[test]
    fn writer_preserves_symlinks() {
        let dir = TempDir::new("writer-link");
        dir.write("real.list", "[Default Applications]\n");
        std::os::unix::fs::symlink("real.list", dir.0.join("mimeapps.list")).unwrap();

        set_default_in(&dir.0, "app.desktop", &["text/plain".into()], false).unwrap();

        let link = dir.0.join("mimeapps.list");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let content = fs::read_to_string(dir.0.join("real.list")).unwrap();
        assert!(content.contains("text/plain=app.desktop;"));
    }

    #[test]
    fn writer_rejects_read_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("writer-readonly");
        let path = dir.write("mimeapps.list", "[Default Applications]\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

        let error =
            set_default_in(&dir.0, "app.desktop", &["text/plain".into()], false).unwrap_err();
        assert_eq!(error.code, 6);
    }

    #[test]
    fn writer_requires_desktop_suffix() {
        let dir = TempDir::new("writer-suffix");
        let error = set_default_in(&dir.0, "firefox", &["text/plain".into()], false).unwrap_err();
        assert_eq!(error.code, 1);
    }

    #[test]
    fn writer_updates_existing_key_in_place_of_duplicates() {
        let dir = TempDir::new("writer-dupes");
        dir.write(
            "mimeapps.list",
            "[Default Applications]\ntext/plain=a.desktop;\ntext/plain=b.desktop;\n",
        );
        set_default_in(&dir.0, "c.desktop", &["text/plain".into()], false).unwrap();
        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert_eq!(content.matches("text/plain=").count(), 1);
        assert!(content.contains("text/plain=c.desktop;"));
    }
}
