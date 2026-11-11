// ---------------------------------------------------------------------------
// MIME detection, mimeapps resolution, and the mimeapps.list writer
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::desktop::{self, DesktopEntry};
use crate::failure::Failure;
use crate::xdg;

const OCTET_STREAM: &str = "application/octet-stream";
const TEXT_PLAIN: &str = "text/plain";
const X_DESKTOP: &str = "application/x-desktop";
const DEFAULT_APPLICATIONS: &str = "Default Applications";

/// Magic matches at or above this priority beat a glob match, as
/// shared-mime-info specifies.
const MAGIC_WINS: u32 = 80;

// ---------------------------------------------------------------------------
// Glob matching
// ---------------------------------------------------------------------------

/// Match a shared-mime-info glob against a file name.
fn glob_match(pattern: &str, name: &str, case_sensitive: bool) -> bool {
    let chars = |text: &str| -> Vec<char> {
        if case_sensitive {
            text.chars().collect()
        } else {
            text.chars().flat_map(char::to_lowercase).collect()
        }
    };

    match_glob(&chars(pattern), 0, &chars(name), 0)
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
    indent: usize,
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
    pub fn load() -> Self {
        let mut db = MimeDatabase::default();
        for dir in xdg::data_dirs() {
            db.load_mime_dir(&dir.join("mime"));
        }
        db
    }

    /// Load `aliases`, `subclasses`, `globs2` (or `globs`) and `magic` from
    /// one MIME data directory.
    fn load_mime_dir(&mut self, dir: &Path) {
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

    /// The canonical name of a type. The loop bound guards alias cycles.
    fn unalias(&self, mime: &str) -> String {
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
    fn ancestors(&self, mime: &str) -> Vec<String> {
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

    fn mime_type_subclass(&self, mime: &str, base: &str) -> bool {
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

        let rank = |rule: &GlobRule| (rule.weight, rule.pattern.chars().count());
        let Some(best) = matching.iter().map(|rule| rank(rule)).max() else {
            return Vec::new();
        };
        matching.retain(|rule| rank(rule) == best);

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

        let (mut mime, priority) = sniffed.unwrap_or((OCTET_STREAM.to_string(), MAGIC_WINS));

        if mime == OCTET_STREAM && looks_like_text(&data) {
            mime = TEXT_PLAIN.into();
        }

        // Do not let a file pretend to be a desktop entry without the suffix.
        if mime == X_DESKTOP && !file_name.ends_with(".desktop") {
            mime = TEXT_PLAIN.into();
        }

        if glob_types.is_empty() {
            return mime;
        }

        if mime != OCTET_STREAM {
            if priority >= MAGIC_WINS {
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

/// Parse the decimal number that ends at the next `stop` byte. Returns the
/// number (0 if absent) and the index after `stop`.
fn number_until(bytes: &[u8], start: usize, stop: u8) -> Option<(usize, usize)> {
    let end = start + bytes[start..].iter().position(|&b| b == stop)?;
    let number = std::str::from_utf8(&bytes[start..end])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((number, end + 1))
}

fn parse_magic_rule(bytes: &[u8], start: usize) -> Option<(MagicRule, usize)> {
    let (indent, i) = number_until(bytes, start, b'>')?;
    let (offset, mut i) = number_until(bytes, i, b'=')?;

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
    let mut level = 0usize;
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

    for (group, keys) in desktop::parse_ini_groups(content) {
        let target = match group.as_str() {
            DEFAULT_APPLICATIONS => &mut file.defaults,
            "Added Associations" => &mut file.added,
            "Removed Associations" => &mut file.removed,
            _ => continue,
        };

        for (mime, value) in keys {
            let ids: Vec<String> = desktop::split_list(&value).map(str::to_string).collect();
            if !ids.is_empty() {
                target.entry(mime).or_default().extend(ids);
            }
        }
    }

    file
}

fn section_ids<'a>(
    section: &'a BTreeMap<String, Vec<String>>,
    mime: &str,
) -> impl Iterator<Item = &'a str> {
    section.get(mime).into_iter().flatten().map(String::as_str)
}

/// mimeapps.list files in precedence order, including desktop-specific
/// variants and the deprecated defaults.list.
fn mimeapps_files() -> Vec<PathBuf> {
    let application_dirs = desktop::application_dirs();
    let bases: Vec<PathBuf> = xdg::config_dirs()
        .into_iter()
        .chain(application_dirs.iter().cloned())
        .collect();

    let desktop = xdg::current_desktop().unwrap_or_default();
    let mut slots: Vec<Vec<String>> = xdg::desktop_names(&desktop)
        .map(|name| vec![format!("{name}-mimeapps.list"), "mimeapps.list".into()])
        .collect();
    if slots.is_empty() {
        slots.push(vec!["mimeapps.list".into()]);
    }

    let mut out = Vec::new();
    for names in &slots {
        for base in &bases {
            out.extend(names.iter().map(|name| base.join(name)));
        }
    }
    out.extend(application_dirs.iter().map(|dir| dir.join("defaults.list")));
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
            .filter_map(|path| fs::read_to_string(path).ok())
            .map(|content| parse_mimeapps(&content))
            .collect();
        let entries = desktop::discover_all();
        Resolver { db, files, entries }
    }

    pub fn detect(&self, path: &Path) -> String {
        self.db.detect(path)
    }

    /// Resolve a MIME type to the desktop entry that handles it: for the type
    /// itself and then each parent type, the configured association first,
    /// then an entry claiming the type in `MimeType=`.
    pub fn resolve(&self, mime: &str) -> Option<&DesktopEntry> {
        let canonical = self.db.unalias(mime);
        let mut order = vec![canonical.clone()];
        for candidate in std::iter::once(mime.to_string()).chain(self.db.ancestors(&canonical)) {
            if !order.contains(&candidate) {
                order.push(candidate);
            }
        }

        order
            .iter()
            .find_map(|mime| self.lookup_exact(mime).or_else(|| self.claim(mime)))
    }

    /// The configured association: the first installed default, else the
    /// first installed added association. A `[Removed Associations]` line
    /// only filters files of lower priority than its own.
    fn lookup_exact(&self, mime: &str) -> Option<&DesktopEntry> {
        let installed = |id: &str| {
            let id = id.strip_suffix(".desktop").unwrap_or(id);
            self.entries.iter().find(|entry| entry.id == id)
        };
        let mut removed: BTreeSet<&str> = BTreeSet::new();
        let mut added = None;
        let mut default = None;

        for file in &self.files {
            let first_allowed = |section| {
                section_ids(section, mime)
                    .filter(|id| !removed.contains(id))
                    .find_map(installed)
            };
            if default.is_none() {
                default = first_allowed(&file.defaults);
            }
            if added.is_none() {
                added = first_allowed(&file.added);
            }
            removed.extend(section_ids(&file.removed, mime));
        }

        default.or(added)
    }

    fn claim(&self, mime: &str) -> Option<&DesktopEntry> {
        self.entries.iter().find(|entry| {
            entry
                .mime_types()
                .any(|claimed| claimed == mime || self.db.unalias(claimed) == mime)
        })
    }
}

pub fn scheme_mime(scheme: &str) -> String {
    format!("x-scheme-handler/{}", scheme.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// mimeapps.list writer
// ---------------------------------------------------------------------------

/// Set the default application for each MIME type in
/// `$XDG_CONFIG_HOME/mimeapps.list`.
pub fn set_default(app: &str, mimes: &[String]) -> Result<(), Failure> {
    let config_home = xdg::config_home()
        .ok_or_else(|| Failure::new(Failure::USAGE, "XDG_CONFIG_HOME and HOME are unset"))?;
    set_default_in(&config_home, app, mimes)
}

/// Set defaults in `config_home/mimeapps.list`. Split out so tests do not
/// mutate process environment.
fn set_default_in(config_home: &Path, app: &str, mimes: &[String]) -> Result<(), Failure> {
    if !app.ends_with(".desktop") {
        return Err(Failure::new(
            Failure::USAGE,
            format!("malformed argument '{}', expected *.desktop", app),
        ));
    }
    if mimes.is_empty() {
        return Err(Failure::new(Failure::USAGE, "no MIME type given"));
    }

    let write_error = |path: &Path, err: &io::Error| {
        Failure::new(
            Failure::WRITE,
            format!("cannot write {} ({})", path.display(), err),
        )
    };

    let target = config_home.join("mimeapps.list");
    let resolved = fs::canonicalize(&target).unwrap_or_else(|_| target.clone());

    let existing = match fs::read(&resolved) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
            Failure::new(
                Failure::READ,
                format!("{} is not valid UTF-8", resolved.display()),
            )
        })?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(Failure::new(
                Failure::READ,
                format!("cannot read {} ({})", resolved.display(), err),
            ))
        }
    };

    if resolved.exists() {
        if let Err(err) = fs::OpenOptions::new().write(true).open(&resolved) {
            return Err(write_error(&resolved, &err));
        }
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

/// Rewrite `content` so each MIME type's only default is `app`. New keys go
/// at the end of the first `[Default Applications]` group; every other line
/// is kept as it is.
fn edit_defaults(content: &str, app: &str, mimes: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_defaults = false;
    let mut insert_at = None;

    for line in content.lines() {
        if let Some(name) = desktop::group_name(line) {
            if in_defaults {
                insert_at.get_or_insert(lines.len());
            }
            in_defaults = name == DEFAULT_APPLICATIONS;
        } else if in_defaults {
            let key = line.split_once('=').map(|(key, _)| key.trim());
            if mimes.iter().any(|mime| Some(mime.as_str()) == key) {
                continue;
            }
        }
        lines.push(line.to_string());
    }

    if in_defaults {
        insert_at.get_or_insert(lines.len());
    }
    let insert_at = insert_at.unwrap_or_else(|| {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{DEFAULT_APPLICATIONS}]"));
        lines.len()
    });
    lines.splice(
        insert_at..insert_at,
        mimes.iter().map(|mime| format!("{mime}={app};")),
    );

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_entry, TempDir};

    fn entry(id: &str, mime: Option<&str>) -> DesktopEntry {
        let mut entry = test_entry("prog");
        entry.id = id.into();
        if let Some(mime) = mime {
            entry.raw.insert("MimeType".into(), mime.into());
        }
        entry
    }

    fn resolver(
        db: MimeDatabase,
        files: Vec<MimeAppsFile>,
        entries: Vec<DesktopEntry>,
    ) -> Resolver {
        Resolver { db, files, entries }
    }

    fn resolved<'a>(resolver: &'a Resolver, mime: &str) -> Option<&'a str> {
        resolver.resolve(mime).map(|entry| entry.id.as_str())
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
    fn globs_keep_biggest_weight_then_longest_pattern_and_honor_case_flag() {
        let dir = TempDir::new("globs");
        dir.write(
            "mime/globs2",
            "# comment\n50:text/x-low:*.txt\n80:text/x-short:*.txt\n\
             80:text/x-long:*.long.txt\n50:text/x-cs:*.CS:cs\n",
        );
        let mut db = MimeDatabase::default();
        db.load_mime_dir(&dir.0.join("mime"));
        let detect = |name: &str| db.detect(&dir.write(name, "text\n"));

        assert_eq!(detect("a.long.txt"), "text/x-long");
        assert_eq!(detect("a.txt"), "text/x-short");
        assert_eq!(detect("a.TXT"), "text/x-short");
        assert_eq!(detect("a.CS"), "text/x-cs");
        assert_eq!(detect("a.cs"), "text/plain");
    }

    // -- magic --------------------------------------------------------------

    #[test]
    fn matches_a_rule_as_update_mime_database_writes_it() {
        let entries = parse_magic(b"MIME-Magic\0\n[50:image/png]\n>0=\x00\x04\x89PNG\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mime, "image/png");
        assert!(magic_entry_matches(&entries[0], b"\x89PNG\r\n\x1a\n"));
        assert!(!magic_entry_matches(&entries[0], b"GIF89a"));
    }

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
    fn aliases_and_subclasses_are_read_and_followed() {
        let dir = TempDir::new("aliases");
        dir.write("mime/aliases", "image/jpg image/jpeg\n");
        dir.write(
            "mime/subclasses",
            "image/svg+xml application/xml\napplication/xml text/plain\n",
        );
        let mut db = MimeDatabase::default();
        db.load_mime_dir(&dir.0.join("mime"));

        assert_eq!(db.unalias("image/jpg"), "image/jpeg");
        assert_eq!(
            db.ancestors("image/svg+xml"),
            vec!["application/xml", "text/plain"]
        );
        assert!(db.mime_type_subclass("image/svg+xml", "text/plain"));
        assert!(!db.mime_type_subclass("text/plain", "image/svg+xml"));
    }

    #[test]
    fn scheme_mime_types_are_lowercase() {
        assert_eq!(scheme_mime("HTTPS"), "x-scheme-handler/https");
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
            entry("first", None),
            entry("second", None),
            entry("third", None),
        ];
        let resolver = resolver(MimeDatabase::default(), vec![first, second], entries);

        assert_eq!(resolved(&resolver, "text/html"), Some("first"));
        // A removal in a lower-priority file must not remove a higher one.
        assert_eq!(resolved(&resolver, "application/x-a"), Some("first"));
        // A higher-priority removal filters a lower-priority addition.
        assert_eq!(resolved(&resolver, "application/x-b"), Some("third"));
    }

    #[test]
    fn mimeapps_falls_back_to_claims_and_parents() {
        let mut db = MimeDatabase::default();
        db.parents
            .insert("text/x-python".into(), vec!["text/plain".into()]);
        db.parents
            .insert("text/x-markdown".into(), vec!["text/plain".into()]);

        let entries = vec![
            entry("python", Some("text/x-python;")),
            entry("plain", Some("text/plain;")),
        ];
        let resolver = resolver(db, Vec::new(), entries);

        assert_eq!(resolved(&resolver, "text/x-python"), Some("python"));
        assert_eq!(resolved(&resolver, "text/x-markdown"), Some("plain"));
    }

    #[test]
    fn mimeapps_skips_ids_that_are_not_installed() {
        let file =
            parse_mimeapps("[Default Applications]\ntext/html=missing.desktop;present.desktop;\n");
        let resolver = resolver(
            MimeDatabase::default(),
            vec![file],
            vec![entry("present", None)],
        );
        assert_eq!(resolved(&resolver, "text/html"), Some("present"));
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
        )
        .unwrap();

        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert_eq!(
            content,
            "# keep me\n[Default Applications]\n# tail\nimage/png=new.desktop;\n\
             image/jpeg=new.desktop;\n[Added Associations]\ntext/html=x.desktop;\n"
        );
    }

    #[test]
    fn writer_creates_missing_file() {
        let dir = TempDir::new("writer-new");
        set_default_in(&dir.0, "app.desktop", &["text/plain".into()]).unwrap();
        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert_eq!(content, "[Default Applications]\ntext/plain=app.desktop;\n");
    }

    #[test]
    fn writer_preserves_symlinks() {
        let dir = TempDir::new("writer-link");
        dir.write("real.list", "[Default Applications]\n");
        std::os::unix::fs::symlink("real.list", dir.0.join("mimeapps.list")).unwrap();

        set_default_in(&dir.0, "app.desktop", &["text/plain".into()]).unwrap();

        let link = dir.0.join("mimeapps.list");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let content = fs::read_to_string(dir.0.join("real.list")).unwrap();
        assert!(content.contains("text/plain=app.desktop;"));
    }

    #[test]
    fn writer_requires_desktop_suffix() {
        let dir = TempDir::new("writer-suffix");
        let error = set_default_in(&dir.0, "firefox", &["text/plain".into()]).unwrap_err();
        assert_eq!(error.code, Failure::USAGE);
    }

    #[test]
    fn writer_updates_existing_key_in_place_of_duplicates() {
        let dir = TempDir::new("writer-dupes");
        dir.write(
            "mimeapps.list",
            "[Default Applications]\ntext/plain=a.desktop;\ntext/plain=b.desktop;\n",
        );
        set_default_in(&dir.0, "c.desktop", &["text/plain".into()]).unwrap();
        let content = fs::read_to_string(dir.0.join("mimeapps.list")).unwrap();
        assert_eq!(content.matches("text/plain=").count(), 1);
        assert!(content.contains("text/plain=c.desktop;"));
    }
}
