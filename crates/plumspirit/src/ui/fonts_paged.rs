//! Opt-in bounded continuation over the same private roots and metadata parser.
use super::*;
pub const MAX_PAGES: usize = 64;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Stopped,
    Budget,
    Changed,
    Memory,
}
pub struct Pager {
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    file: usize,
    face: u32,
    source: Option<(u64, Option<SystemTime>)>,
    query: String,
    cap: usize,
    page: usize,
    entries: usize,
    truncated: bool,
}
impl Pager {
    pub fn new(
        roots: Vec<PathBuf>,
        query: &str,
        cap: usize,
        check: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let mut normalized = Vec::new();
        normalized.try_reserve(16).map_err(|_| Error::Memory)?;
        for root in roots.into_iter().take(16) {
            if !check() {
                return Err(Error::Stopped);
            }
            if let Ok(root) = root.canonicalize()
                && root.as_os_str().len() <= MAX_PATH
            {
                normalized.push(root);
            }
        }
        normalized.sort();
        normalized.dedup();
        let mut files = Vec::new();
        files.try_reserve(MAX_FILES).map_err(|_| Error::Memory)?;
        let mut entries = 20_000;
        let mut truncated = false;
        let mut stopped = false;
        for root in &normalized {
            if !scan_dir_checked(
                root,
                4,
                "",
                &mut files,
                &mut entries,
                ScanLimits {
                    files: MAX_FILES,
                    path_bytes: MAX_PATH,
                    no_links: true,
                },
                &mut || {
                    if !check() {
                        stopped = true;
                        false
                    } else {
                        true
                    }
                },
            ) {
                truncated = true;
                break;
            }
        }
        if stopped {
            return Err(Error::Stopped);
        }
        files.sort();
        files.dedup();
        Ok(Self {
            roots: normalized,
            files,
            file: 0,
            face: 0,
            source: None,
            query: query.to_lowercase(),
            cap: cap.clamp(1, MAX_FACES),
            page: 0,
            entries: 20_000 - entries,
            truncated,
        })
    }
    pub fn retained_bytes(&self) -> usize {
        self.roots
            .iter()
            .chain(self.files.iter())
            .map(|p| p.as_os_str().len() * 2 + 128)
            .sum::<usize>()
            + self.files.capacity() * std::mem::size_of::<PathBuf>()
            + self.query.capacity()
            + 4096
    }
    pub fn has_more(&self) -> bool {
        self.file < self.files.len()
    }
    pub fn pages(&self) -> usize {
        self.page
    }
    fn advance_file(&mut self) {
        self.file += 1;
        self.face = 0;
        self.source = None;
    }
    pub fn next(&mut self, check: &mut dyn FnMut() -> bool) -> Result<Inventory, Error> {
        if !check() {
            return Err(Error::Stopped);
        }
        if self.page >= MAX_PAGES {
            return Err(Error::Budget);
        }
        let started = (self.file, self.face);
        let mut out = Inventory {
            roots: self.roots.clone(),
            entries: self.entries,
            paged: true,
            page: self.page + 1,
            ..Inventory::default()
        };
        out.faces
            .try_reserve_exact(self.cap)
            .map_err(|_| Error::Memory)?;
        let mut control = Control {
            check,
            stopped: false,
            limited: false,
            truncated: false,
            bytes: 0,
            decoded: 0,
        };
        while self.has_more() && out.faces.len() < self.cap {
            if !control.proceed() {
                break;
            }
            let path = &self.files[self.file];
            let Ok(path) = path.canonicalize() else {
                if self.source.is_some() {
                    return Err(Error::Changed);
                }
                self.truncated = true;
                self.advance_file();
                continue;
            };
            if path != self.files[self.file]
                || path.as_os_str().len() > MAX_PATH
                || !self.roots.iter().any(|r| path.starts_with(r))
            {
                if self.source.is_some() {
                    return Err(Error::Changed);
                }
                self.truncated = true;
                self.advance_file();
                continue;
            }
            let Ok(mut file) = open_scoped(&path, &self.roots) else {
                if self.source.is_some() {
                    return Err(Error::Changed);
                }
                self.truncated = true;
                self.advance_file();
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                if self.source.is_some() {
                    return Err(Error::Changed);
                }
                self.truncated = true;
                self.advance_file();
                continue;
            };
            let identity = (metadata.len(), metadata.modified().ok());
            if self.source.is_some_and(|s| s != identity) {
                return Err(Error::Changed);
            }
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 * 1024 {
                self.truncated = true;
                self.advance_file();
                continue;
            }
            self.source = Some(identity);
            out.files += 1;
            let mut reader = Reader {
                file: &mut file,
                control: &mut control,
            };
            let Some(head) = reader.read_at(0, 12) else {
                if reader.control.limited || reader.control.stopped {
                    break;
                }
                self.advance_file();
                continue;
            };
            let count = if &head[..4] == b"ttcf" {
                reader.face_limit(be32(&head, 8).unwrap_or(0) as usize) as u32
            } else if matches!(&head[..4], [0, 1, 0, 0] | b"OTTO" | b"true") {
                1
            } else {
                0
            };
            while self.face < count && out.faces.len() < self.cap {
                if !reader.control.proceed() {
                    break;
                }
                let names = directory(&mut reader, self.face)
                    .and_then(|dir| read_face_with(&mut reader, dir, self.face));
                if reader.control.limited || reader.control.stopped {
                    break;
                }
                let Some(names) = names else {
                    self.face += 1;
                    continue;
                };
                if !format!("{} {}", names.effective_family(), names.effective_style())
                    .to_lowercase()
                    .contains(&self.query)
                {
                    self.face += 1;
                    continue;
                }
                let (weight, italic) = properties(&mut reader, self.face);
                if reader.control.limited || reader.control.stopped {
                    break;
                }
                out.faces.push(Face {
                    names,
                    path: path.clone(),
                    weight,
                    italic,
                    length: identity.0,
                    modified: identity.1,
                });
                self.face += 1;
            }
            if self.face >= count {
                self.advance_file();
            }
            if reader.control.limited || reader.control.stopped {
                break;
            }
        }
        if control.stopped {
            return Err(Error::Stopped);
        }
        // A single pathological face must not cause endlessly empty retries.
        if control.limited && out.faces.is_empty() && (self.file, self.face) == started {
            return Err(Error::Budget);
        }
        self.truncated |= control.truncated;
        out.faces.sort_by(|a, b| {
            a.names
                .effective_family()
                .cmp(b.names.effective_family())
                .then(a.names.effective_style().cmp(b.names.effective_style()))
                .then(a.path.cmp(&b.path))
                .then(a.names.index.cmp(&b.names.index))
        });
        out.read_bytes = control.bytes;
        out.has_more = self.has_more();
        out.scan_truncated = self.truncated;
        out.truncated = out.has_more || out.scan_truncated || control.limited;
        self.page += 1;
        Ok(out)
    }
}
