//! Plugin packages (B1-0037, `docs/LUA_API.md` "Plugins"): the
//! `plum-plugin.toml` manifest, safe file names, and the plugin store
//! (install from a local folder or a single `.lua` file, update, remove,
//! scan). Nothing is ever fetched from the network; every install is a user
//! action on a local file.
//!
//! A package is a folder:
//!
//! ```text
//! my-plugin/
//!   plum-plugin.toml     manifest (required)
//!   main.lua             entry script (named by `entry`)
//!   lib/…, presets/…     more scripts (`plum.plugin.include`) and assets
//! ```
//!
//! Installing copies the package into the store
//! (`<settings folder>\plugins\<id>\` unless the user chose another folder)
//! through a staging folder that is renamed into place only when every file
//! was copied and checked, so a failed install or update leaves the store as
//! it was. Path traversal (`..`, absolute paths, drive letters), reserved
//! Windows names, control characters and symbolic links are refused.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The manifest's file name inside a package.
pub const MANIFEST: &str = "plum-plugin.toml";

/// Largest manifest accepted.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Largest single file of a package.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest package (all files).
pub const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
/// Most files in a package.
pub const MAX_FILES: usize = 1024;
/// Deepest folder nesting inside a package.
pub const MAX_DEPTH: usize = 8;

/// Where an installed plugin came from (shown in the Plugins window).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Installed from a folder on this computer.
    LocalFolder,
    /// Installed from a single `.lua` file on this computer.
    LocalFile,
    /// One of the examples that ship with PlumSpirit.
    Example,
}

impl Origin {
    pub fn key(self) -> &'static str {
        match self {
            Origin::LocalFolder => "local_folder",
            Origin::LocalFile => "local_file",
            Origin::Example => "example",
        }
    }

    pub fn parse(s: &str) -> Option<Origin> {
        [Origin::LocalFolder, Origin::LocalFile, Origin::Example]
            .into_iter()
            .find(|o| o.key() == s)
    }

    /// The label the Plugins window shows.
    pub fn label(self) -> &'static str {
        match self {
            Origin::LocalFolder => "Local folder (not reviewed by PlumSpirit)",
            Origin::LocalFile => "Local file (not reviewed by PlumSpirit)",
            Origin::Example => "Example shipped with PlumSpirit",
        }
    }
}

/// A parsed, checked `plum-plugin.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Stable id: 3 – 64 characters `a-z 0-9 . _ -`, starting with a letter
    /// or digit (`com.example.graph-shift`).
    pub id: String,
    pub name: String,
    /// The plugin's own version (`1.2.0`): numbers separated by dots.
    pub version: String,
    /// The lowest scripting API it needs, `(major, minor)`.
    pub api: (u32, u32),
    /// The entry script, relative to the package folder.
    pub entry: String,
    pub description: String,
    pub author: String,
    pub license: String,
    /// What it asks to be allowed (`project.read`, `notes.edit`, …). The
    /// user grants each one; nothing is granted by installing.
    pub capabilities: Vec<String>,
    /// B1-0035: the layout of its settings (1 – 1,000,000; default 1).
    /// Settings saved with a lower number pass through the plugin's
    /// `migrate_config`.
    pub config_version: u64,
    pub service_dependencies: Vec<super::service_calls::Dependency>,
}

impl Manifest {
    /// Parses and checks a manifest; every refusal is a sentence naming the
    /// line or the key.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let kv = parse_toml_subset(text)?;
        let get =
            |k: &str| -> Option<&TomlValue> { kv.iter().find(|(n, _)| n == k).map(|(_, v)| v) };
        let string = |k: &str, required: bool| -> Result<String, String> {
            match get(k) {
                Some(TomlValue::Str(s)) => Ok(s.clone()),
                Some(_) => Err(format!(
                    "{MANIFEST}: \u{201C}{k}\u{201D} must be a text in quotes."
                )),
                None if required => Err(format!("{MANIFEST}: \u{201C}{k}\u{201D} is missing.")),
                None => Ok(String::new()),
            }
        };
        let known = [
            "id",
            "name",
            "version",
            "api",
            "entry",
            "description",
            "author",
            "license",
            "capabilities",
            "config_version",
            "service_dependencies",
        ];
        if let Some((k, _)) = kv.iter().find(|(k, _)| !known.contains(&k.as_str())) {
            return Err(format!(
                "{MANIFEST}: unknown key \u{201C}{k}\u{201D} (known: {}).",
                known.join(", ")
            ));
        }
        let id = string("id", true)?;
        check_id(&id)?;
        let name = string("name", true)?;
        if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control)
        {
            return Err(format!(
                "{MANIFEST}: \u{201C}name\u{201D} must be 1 – 80 characters without control characters."
            ));
        }
        let version = string("version", true)?;
        check_version(&version)?;
        let api_text = string("api", true)?;
        let api = parse_api(&api_text)?;
        let entry = string("entry", false)?;
        let entry = if entry.is_empty() {
            "main.lua".to_owned()
        } else {
            entry
        };
        safe_relative(&entry)?;
        if !entry.to_ascii_lowercase().ends_with(".lua") {
            return Err(format!(
                "{MANIFEST}: the entry script must be a .lua file (it is {entry})."
            ));
        }
        let description = string("description", false)?;
        let author = string("author", false)?;
        let license = string("license", false)?;
        for (k, v, max) in [
            ("description", &description, 2000),
            ("author", &author, 200),
            ("license", &license, 200),
        ] {
            if v.chars().count() > max {
                return Err(format!(
                    "{MANIFEST}: \u{201C}{k}\u{201D} is longer than {max} characters."
                ));
            }
        }
        let capabilities = match get("capabilities") {
            None => Vec::new(),
            Some(TomlValue::List(l)) => {
                let mut seen = BTreeSet::new();
                for c in l {
                    if super::Capability::parse(c).is_none() {
                        let names: Vec<&str> =
                            super::Capability::ALL.iter().map(|c| c.name()).collect();
                        return Err(format!(
                            "{MANIFEST}: unknown capability \u{201C}{c}\u{201D} (known: {}).",
                            names.join(", ")
                        ));
                    }
                    seen.insert(c.clone());
                }
                seen.into_iter().collect()
            }
            Some(_) => {
                return Err(format!(
                    "{MANIFEST}: \u{201C}capabilities\u{201D} must be a list like [\"project.read\", \"notes.edit\"]."
                ));
            }
        };
        let config_version = match get("config_version") {
            None => 1,
            Some(TomlValue::Int(n)) if (1..=1_000_000).contains(n) => *n as u64,
            Some(_) => {
                return Err(format!(
                    "{MANIFEST}: \u{201C}config_version\u{201D} must be a whole number 1 – 1,000,000."
                ));
            }
        };
        let service_dependencies = match get("service_dependencies") {
            None => Vec::new(),
            Some(TomlValue::List(values)) if values.len() <= 32 => {
                let mut seen = BTreeSet::new();
                let mut deps = Vec::new();
                for value in values {
                    let dep = super::service_calls::Dependency::parse(value)
                        .map_err(|e| format!("{MANIFEST}: service_dependencies: {e}"))?;
                    if !seen.insert(dep.namespace.clone()) {
                        return Err(format!(
                            "{MANIFEST}: duplicate service dependency {}",
                            dep.namespace
                        ));
                    }
                    deps.push(dep);
                }
                deps
            }
            _ => {
                return Err(format!(
                    "{MANIFEST}: service_dependencies must be a list of at most32 provider.id/service@major.minor texts."
                ));
            }
        };
        if !service_dependencies.is_empty() && api < (0, 2) {
            return Err(format!(
                "{MANIFEST}: service dependencies require API0.2 or newer."
            ));
        }
        Ok(Manifest {
            id,
            name: name.trim().to_owned(),
            config_version,
            version,
            api,
            entry,
            description,
            author,
            license,
            capabilities,
            service_dependencies,
        })
    }

    /// The manifest as text (written for packages made from a single file).
    pub fn to_text(&self) -> String {
        let q = |s: &str| {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\t' => o.push_str("\\t"),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        };
        let caps: Vec<String> = self.capabilities.iter().map(|c| q(c)).collect();
        let mut text = format!(
            "id = {}\nname = {}\nversion = {}\napi = {}\nentry = {}\ndescription = {}\nauthor = {}\nlicense = {}\ncapabilities = [{}]\nconfig_version = {}\n",
            q(&self.id),
            q(&self.name),
            q(&self.version),
            q(&format!("{}.{}", self.api.0, self.api.1)),
            q(&self.entry),
            q(&self.description),
            q(&self.author),
            q(&self.license),
            caps.join(", "),
            self.config_version
        );
        if !self.service_dependencies.is_empty() {
            let deps: Vec<_> = self
                .service_dependencies
                .iter()
                .map(|d| q(&d.text()))
                .collect();
            text.push_str(&format!("service_dependencies = [{}]\n", deps.join(", ")));
        }
        text
    }

    /// Whether this PlumSpirit's API can run it: same major, equal or later
    /// minor. `Err` is the sentence shown.
    pub fn compatible(&self) -> Result<(), String> {
        let (a, b, _) = super::API_VERSION;
        if self.api.0 == a && self.api.1 <= b {
            Ok(())
        } else {
            Err(format!(
                "This plugin needs scripting API {}.{}; this PlumSpirit has {}. Update PlumSpirit or ask the plugin's author for a version for API {}.{}.",
                self.api.0,
                self.api.1,
                super::api_version(),
                a,
                b
            ))
        }
    }
}

/// A plugin id: `a-z 0-9 . _ -`, 3 – 64 characters, starting with a letter
/// or digit, no `..`.
pub fn check_id(id: &str) -> Result<(), String> {
    let ok = (3..=64).contains(&id.len())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
        })
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && !id.contains("..")
        && !id.ends_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{MANIFEST}: the id \u{201C}{id}\u{201D} must be 3 – 64 characters of a – z, 0 – 9, \u{201C}.\u{201D}, \u{201C}_\u{201D} or \u{201C}-\u{201D}, starting with a letter or a digit."
        ))
    }
}

fn check_version(v: &str) -> Result<(), String> {
    let parts: Vec<&str> = v.split('.').collect();
    if (1..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err(format!(
            "{MANIFEST}: the version \u{201C}{v}\u{201D} must be numbers separated by dots, like 1.0.2."
        ))
    }
}

/// `1.2.10` as numbers for comparisons (missing parts are 0).
pub fn version_key(v: &str) -> [u64; 4] {
    let mut k = [0u64; 4];
    for (i, p) in v.split('.').take(4).enumerate() {
        k[i] = p.parse().unwrap_or(0);
    }
    k
}

fn parse_api(t: &str) -> Result<(u32, u32), String> {
    let mut it = t.split('.');
    let major = it.next().and_then(|p| p.parse::<u32>().ok());
    let minor = it.next().map_or(Some(0), |p| p.parse::<u32>().ok());
    let rest = it.next();
    match (major, minor, rest) {
        (Some(a), Some(b), None) => Ok((a, b)),
        _ => Err(format!(
            "{MANIFEST}: \u{201C}api\u{201D} must be the lowest scripting API version the plugin needs, like \"{}\" (it is \u{201C}{t}\u{201D}).",
            {
                let (a, b, _) = super::API_VERSION;
                format!("{a}.{b}")
            }
        )),
    }
}

/// A file or folder name inside a package, checked for every system
/// PlumSpirit runs on.
pub(super) fn safe_component(c: &str) -> Result<(), String> {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = c
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_lowercase();
    let bad = c.is_empty()
        || c.len() > 120
        || c == "."
        || c == ".."
        || c.ends_with('.')
        || c.ends_with(' ')
        || c.chars()
            .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
        || RESERVED.contains(&stem.as_str());
    if bad {
        Err(format!(
            "The name \u{201C}{c}\u{201D} cannot be used inside a plugin (reserved or unsafe characters)."
        ))
    } else {
        Ok(())
    }
}

/// A path inside a package: relative, no `..`, no drive or root, every
/// component safe, at most [`MAX_DEPTH`] deep. Both `/` and `\` separate.
pub fn safe_relative(p: &str) -> Result<PathBuf, String> {
    if p.is_empty() {
        return Err("An empty path cannot be used inside a plugin.".into());
    }
    if p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return Err(format!(
            "\u{201C}{p}\u{201D} is not inside the plugin's folder (absolute paths are refused)."
        ));
    }
    let parts: Vec<&str> = p.split(['/', '\\']).collect();
    if parts.len() > MAX_DEPTH {
        return Err(format!(
            "\u{201C}{p}\u{201D} is nested deeper than {MAX_DEPTH} folders."
        ));
    }
    let mut out = PathBuf::new();
    for c in parts {
        if c == ".." {
            return Err(format!(
                "\u{201C}{p}\u{201D} leaves the plugin's folder (\u{201C}..\u{201D} is refused)."
            ));
        }
        safe_component(c)?;
        out.push(c);
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TomlValue {
    Str(String),
    List(Vec<String>),
    Int(i64),
    Other,
}

/// The subset of TOML a manifest uses: `key = "text"`, `key = ["a", "b"]`
/// (one line), numbers / booleans (accepted, unused), `#` comments. Tables
/// are refused (the manifest is flat).
fn parse_toml_subset(text: &str) -> Result<Vec<(String, TomlValue)>, String> {
    let mut out: Vec<(String, TomlValue)> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line_no = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            return Err(format!(
                "{MANIFEST}, line {line_no}: tables are not used in a plugin manifest; write key = value lines."
            ));
        }
        let Some(eq) = line.find('=') else {
            return Err(format!("{MANIFEST}, line {line_no}: expected key = value."));
        };
        let key = line[..eq].trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(format!(
                "{MANIFEST}, line {line_no}: \u{201C}{key}\u{201D} is not a valid key."
            ));
        }
        if out.iter().any(|(k, _)| k == key) {
            return Err(format!(
                "{MANIFEST}, line {line_no}: \u{201C}{key}\u{201D} is given twice."
            ));
        }
        let rest = line[eq + 1..].trim();
        let (value, after) = if rest.starts_with('"') {
            let (s, after) = parse_string(rest, line_no)?;
            (TomlValue::Str(s), after)
        } else if let Some(mut r) = rest.strip_prefix('[') {
            let mut items = Vec::new();
            loop {
                r = r.trim_start();
                if let Some(a) = r.strip_prefix(']') {
                    break (TomlValue::List(items), a);
                }
                if !r.starts_with('"') {
                    return Err(format!(
                        "{MANIFEST}, line {line_no}: a list holds texts in quotes, separated by commas, on one line."
                    ));
                }
                let (s, a) = parse_string(r, line_no)?;
                items.push(s);
                r = a.trim_start();
                if let Some(a) = r.strip_prefix(',') {
                    r = a;
                }
            }
        } else {
            let v = rest.split('#').next().unwrap_or("").trim();
            if let Ok(n) = v.replace('_', "").parse::<i64>() {
                (TomlValue::Int(n), "")
            } else if v == "true" || v == "false" || v.parse::<f64>().is_ok() {
                (TomlValue::Other, "")
            } else {
                return Err(format!(
                    "{MANIFEST}, line {line_no}: the value of \u{201C}{key}\u{201D} must be in quotes."
                ));
            }
        };
        let after = after.trim();
        if !after.is_empty() && !after.starts_with('#') {
            return Err(format!(
                "{MANIFEST}, line {line_no}: unexpected text after the value."
            ));
        }
        out.push((key.to_owned(), value));
    }
    Ok(out)
}

/// A basic TOML string at the start of `s` and what follows it.
fn parse_string(s: &str, line_no: usize) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &s[i + 1..])),
            '\\' => match chars.next().map(|(_, c)| c) {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                _ => {
                    return Err(format!(
                        "{MANIFEST}, line {line_no}: unknown escape in a text (use \\\\, \\\", \\n or \\t)."
                    ));
                }
            },
            c => out.push(c),
        }
    }
    Err(format!(
        "{MANIFEST}, line {line_no}: a text is not closed with a quote."
    ))
}

/// An installed package found in the store.
#[derive(Clone, Debug)]
pub struct Installed {
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
    /// Changes when any file of the package changes (paths, sizes, times):
    /// Refresh reloads a plugin whose stamp moved.
    pub stamp: u64,
}

/// A fingerprint of a package folder's files (names, sizes, modification
/// times), bounded like an install.
pub fn stamp(dir: &Path) -> u64 {
    // FNV-1a over the sorted entries.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut es: Vec<fs::DirEntry> = rd.filter_map(Result::ok).collect();
        es.sort_by_key(fs::DirEntry::file_name);
        for e in es {
            seen += 1;
            if seen > MAX_FILES * 2 {
                return h;
            }
            eat(e.file_name().as_encoded_bytes());
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((e.path(), depth + 1));
                }
                continue;
            }
            eat(&m.len().to_le_bytes());
            if let Ok(t) = m.modified()
                && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
            {
                eat(&d.as_nanos().to_le_bytes());
            }
        }
    }
    h
}

/// The plugin store: one folder per plugin id.
#[derive(Clone, Debug)]
pub struct Store {
    pub root: PathBuf,
}

/// Every file of a package, checked: (relative path, absolute source path).
fn package_files(src: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut stack: Vec<(PathBuf, usize)> = vec![(PathBuf::new(), 0)];
    while let Some((rel, depth)) = stack.pop() {
        let dir = src.join(&rel);
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(&dir)
            .map_err(|e| format!("The folder {} could not be read ({e}).", dir.display()))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("The folder {} could not be read ({e}).", dir.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for e in entries {
            let name = e.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| "A file name in the plugin is not valid text.".to_owned())?
                .to_owned();
            safe_component(&name)?;
            let ft = e
                .file_type()
                .map_err(|err| format!("{name} could not be read ({err})."))?;
            let r = rel.join(&name);
            if ft.is_symlink() {
                return Err(format!(
                    "{} is a link; plugins may contain only plain files and folders.",
                    r.display()
                ));
            } else if ft.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    return Err(format!(
                        "{} is nested deeper than {MAX_DEPTH} folders.",
                        r.display()
                    ));
                }
                stack.push((r, depth + 1));
            } else if ft.is_file() {
                let len = e
                    .metadata()
                    .map_err(|err| format!("{name} could not be read ({err})."))?
                    .len();
                if len > MAX_FILE_BYTES {
                    return Err(format!(
                        "{} is larger than {} MB.",
                        r.display(),
                        MAX_FILE_BYTES / (1024 * 1024)
                    ));
                }
                total += len;
                if total > MAX_PACKAGE_BYTES {
                    return Err(format!(
                        "The plugin is larger than {} MB.",
                        MAX_PACKAGE_BYTES / (1024 * 1024)
                    ));
                }
                out.push((r, e.path()));
                if out.len() > MAX_FILES {
                    return Err(format!("The plugin has more than {MAX_FILES} files."));
                }
            }
        }
    }
    Ok(out)
}

/// Reads and checks the manifest of the package folder `dir`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let meta = fs::metadata(&path).map_err(|_| {
        format!(
            "{} has no {MANIFEST}; a plugin folder needs one (see the Lua API reference).",
            dir.display()
        )
    })?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST} is larger than 64 KB."));
    }
    let text =
        fs::read_to_string(&path).map_err(|e| format!("{MANIFEST} could not be read ({e})."))?;
    let m = Manifest::parse(&text)?;
    let entry = dir.join(safe_relative(&m.entry)?);
    if !entry.is_file() {
        return Err(format!(
            "The entry script {} named in {MANIFEST} is not in the plugin's folder.",
            m.entry
        ));
    }
    Ok(m)
}

/// What an install or update did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallReport {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub files: usize,
    /// The version it replaced (updates).
    pub replaced: Option<String>,
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    /// The folder of plugin `id` (the id is checked first).
    pub fn dir_of(&self, id: &str) -> Result<PathBuf, String> {
        check_id(id)?;
        Ok(self.root.join(id))
    }

    /// Every package in the store, in id order; folders whose name starts
    /// with a dot (staging) are skipped. A broken manifest is listed with its
    /// error (the Plugins window shows it).
    pub fn scan(&self) -> Vec<Installed> {
        let Ok(rd) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<Installed> = rd
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                let dir = e.path();
                let manifest = read_manifest(&dir).and_then(|m| {
                    if dir.file_name().is_some_and(|n| n.to_string_lossy() == m.id) {
                        Ok(m)
                    } else {
                        Err(format!(
                            "The folder name does not match the plugin id \u{201C}{}\u{201D}.",
                            m.id
                        ))
                    }
                });
                let stamp = stamp(&dir);
                Installed {
                    dir,
                    manifest,
                    stamp,
                }
            })
            .collect();
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        out
    }

    /// A fresh staging folder in the store.
    fn staging(&self, what: &str) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.root).map_err(|e| {
            format!(
                "The plugins folder {} could not be created ({e}).",
                self.root.display()
            )
        })?;
        for n in 0..10_000u32 {
            let d = self
                .root
                .join(format!(".staging-{what}-{}-{n}", std::process::id()));
            if fs::create_dir(&d).is_ok() {
                return Ok(d);
            }
        }
        Err("No staging folder could be created in the plugins folder.".into())
    }

    /// Installs the package folder `src` (`update`: replaces the installed
    /// one with the same id; else an installed id is refused).
    pub fn install_folder(&self, src: &Path, update: bool) -> Result<InstallReport, String> {
        let manifest = read_manifest(src)?;
        let files = package_files(src)?;
        let stage = self.staging(&manifest.id)?;
        let copied = (|| -> Result<(), String> {
            for (rel, from) in &files {
                let to = stage.join(rel);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("Copying failed ({e})."))?;
                }
                fs::copy(from, &to)
                    .map_err(|e| format!("{} could not be copied ({e}).", rel.display()))?;
            }
            // The copy must read back as the same package.
            let again = read_manifest(&stage)?;
            if again != manifest {
                return Err("The plugin changed while it was being installed; try again.".into());
            }
            Ok(())
        })();
        if let Err(e) = copied {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        self.commit(stage, manifest, files.len(), update)
    }

    /// Installs a single `.lua` file as a package: its id is
    /// `local.<file name>`, it asks for the capabilities listed on a first-
    /// lines comment `-- plum-capabilities: project.read, notes.edit`, and a
    /// manifest is written for it.
    pub fn install_file(&self, src: &Path, update: bool) -> Result<InstallReport, String> {
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "The file name is not valid text.".to_owned())?;
        safe_component(name)?;
        if !name.to_ascii_lowercase().ends_with(".lua") {
            return Err("Only .lua files can be installed as a single-file plugin.".into());
        }
        let len = fs::metadata(src)
            .map_err(|e| format!("{name} could not be read ({e})."))?
            .len();
        if len > MAX_FILE_BYTES {
            return Err(format!(
                "{name} is larger than {} MB.",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let text = fs::read_to_string(src)
            .map_err(|e| format!("{name} could not be read as text ({e})."))?;
        let stem = &name[..name.len() - 4];
        let mut slug: String = stem
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        slug.truncate(40);
        let slug = slug.trim_matches('-').to_owned();
        let id = format!("local.{}", if slug.is_empty() { "script" } else { &slug });
        check_id(&id)?;
        let mut caps = Vec::new();
        for line in text.lines().take(20) {
            if let Some(rest) = line.trim().strip_prefix("--")
                && let Some(list) = rest.trim().strip_prefix("plum-capabilities:")
            {
                for c in list.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                    if super::Capability::parse(c).is_none() {
                        return Err(format!(
                            "{name}: unknown capability \u{201C}{c}\u{201D} in its plum-capabilities line."
                        ));
                    }
                    caps.push(c.to_owned());
                }
            }
        }
        caps.sort();
        caps.dedup();
        let (a, b, _) = super::API_VERSION;
        let manifest = Manifest {
            id,
            name: stem.chars().take(80).collect(),
            version: "0.0.0".into(),
            api: (a, b),
            entry: name.to_owned(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            capabilities: caps,
            config_version: 1,
            service_dependencies: Vec::new(),
        };
        let stage = self.staging(&manifest.id)?;
        let written = fs::write(stage.join(name), text.as_bytes())
            .and_then(|()| fs::write(stage.join(MANIFEST), manifest.to_text()))
            .map_err(|e| format!("The plugin could not be written ({e})."))
            .and_then(|()| read_manifest(&stage));
        match written {
            Ok(m) if m == manifest => self.commit(stage, manifest, 2, update),
            Ok(_) => {
                let _ = fs::remove_dir_all(&stage);
                Err("The generated manifest did not read back; nothing was installed.".into())
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&stage);
                Err(e)
            }
        }
    }

    /// Moves a checked staging folder into place (an update keeps the old
    /// folder until the new one is in place and restores it on failure).
    fn commit(
        &self,
        stage: PathBuf,
        manifest: Manifest,
        files: usize,
        update: bool,
    ) -> Result<InstallReport, String> {
        let dest = match self.dir_of(&manifest.id) {
            Ok(d) => d,
            Err(e) => {
                let _ = fs::remove_dir_all(&stage);
                return Err(e);
            }
        };
        let mut replaced = None;
        let mut old = None;
        if dest.exists() {
            if !update {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!(
                    "A plugin with the id \u{201C}{}\u{201D} is already installed; use Update to replace it.",
                    manifest.id
                ));
            }
            replaced = read_manifest(&dest).ok().map(|m| m.version);
            let o = self
                .root
                .join(format!(".old-{}-{}", manifest.id, std::process::id()));
            let _ = fs::remove_dir_all(&o);
            if let Err(e) = fs::rename(&dest, &o) {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!(
                    "The installed version could not be moved aside ({e}); nothing was changed."
                ));
            }
            old = Some(o);
        } else if update {
            let _ = fs::remove_dir_all(&stage);
            return Err(format!(
                "\u{201C}{}\u{201D} is not installed, so it cannot be updated; install it instead.",
                manifest.id
            ));
        }
        if let Err(e) = fs::rename(&stage, &dest) {
            if let Some(o) = &old {
                let _ = fs::rename(o, &dest);
            }
            let _ = fs::remove_dir_all(&stage);
            return Err(format!(
                "The plugin could not be put in place ({e}); nothing was changed."
            ));
        }
        if let Some(o) = old {
            let _ = fs::remove_dir_all(o);
        }
        Ok(InstallReport {
            manifest,
            dir: dest,
            files,
            replaced,
        })
    }

    /// Removes plugin `id` from the store.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let dir = self.dir_of(id)?;
        if !dir.is_dir() {
            return Err(format!("\u{201C}{id}\u{201D} is not installed."));
        }
        // Move it aside first, so a half-deleted plugin is never scanned.
        let gone = self
            .root
            .join(format!(".removed-{id}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&gone);
        fs::rename(&dir, &gone).map_err(|e| format!("The plugin could not be removed ({e})."))?;
        fs::remove_dir_all(&gone).map_err(|e| {
            format!(
                "The plugin was disabled but some of its files could not be deleted ({e}); delete {} by hand.",
                gone.display()
            )
        })
    }

    /// Reads a script of plugin `id` (`rel` checked: inside the package).
    pub fn read_script(&self, id: &str, rel: &str) -> Result<String, String> {
        let p = self.dir_of(id)?.join(safe_relative(rel)?);
        let len = fs::metadata(&p)
            .map_err(|_| format!("{rel} is not in the plugin's folder."))?
            .len();
        if len > MAX_FILE_BYTES {
            return Err(format!(
                "{rel} is larger than {} MB.",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        fs::read_to_string(&p).map_err(|e| format!("{rel} could not be read ({e})."))
    }
}
