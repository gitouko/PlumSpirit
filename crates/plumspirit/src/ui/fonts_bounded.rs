//! Controlled installed-font metadata using the existing traversal/name parser.
use super::*;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::time::SystemTime;
#[path = "fonts_paged.rs"]
pub mod paged;

pub const MAX_FACES: usize = 512;
pub const MAX_FILES: usize = 1024;
pub const MAX_NAME: usize = 512;
pub const MAX_IO: usize = 16 * 1024 * 1024;
const MAX_TABLE: usize = 256 * 1024;
const MAX_PATH: usize = 4096;
pub const WORKSPACE: usize = 24 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Face {
    pub names: FaceNames,
    pub path: PathBuf,
    pub weight: u16,
    pub italic: bool,
    pub length: u64,
    pub modified: Option<SystemTime>,
}
#[derive(Default)]
pub struct Inventory {
    pub roots: Vec<PathBuf>,
    pub faces: Vec<Face>,
    pub files: usize,
    pub entries: usize,
    pub read_bytes: usize,
    pub truncated: bool,
    pub paged: bool,
    pub page: usize,
    pub has_more: bool,
    pub scan_truncated: bool,
}
impl Inventory {
    pub fn retained_bytes(&self) -> usize {
        self.roots
            .iter()
            .map(|p| p.as_os_str().len() * 2 + 128)
            .sum::<usize>()
            + self
                .faces
                .iter()
                .map(|f| {
                    f.path.as_os_str().len() * 2
                        + f.names.family.capacity()
                        + f.names.subfamily.capacity()
                        + f.names
                            .typographic_family
                            .as_ref()
                            .map_or(0, String::capacity)
                        + f.names
                            .typographic_subfamily
                            .as_ref()
                            .map_or(0, String::capacity)
                        + 512
                })
                .sum::<usize>()
            + self.faces.capacity() * std::mem::size_of::<Face>()
            + self.roots.capacity() * std::mem::size_of::<PathBuf>()
            + 4096
    }
}
struct Control<'a> {
    check: &'a mut dyn FnMut() -> bool,
    stopped: bool,
    limited: bool,
    truncated: bool,
    bytes: usize,
    decoded: usize,
}
impl Control<'_> {
    fn proceed(&mut self) -> bool {
        if self.stopped || self.limited {
            return false;
        }
        if !(self.check)() {
            self.stopped = true;
            return false;
        }
        true
    }
}
struct Reader<'a, 'b> {
    file: &'a mut File,
    control: &'a mut Control<'b>,
}
impl NameReader for Reader<'_, '_> {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        if !self.control.proceed() {
            return None;
        }
        if len > MAX_TABLE {
            self.control.truncated = true;
            return None;
        }
        if self.control.bytes + len > MAX_IO {
            self.control.limited = true;
            return None;
        }
        self.control.bytes += len;
        self.file.seek(SeekFrom::Start(off)).ok()?;
        let mut out = vec![0; len];
        for chunk in out.chunks_mut(64 * 1024) {
            if !self.control.proceed() {
                return None;
            }
            self.file.read_exact(chunk).ok()?;
        }
        Some(out)
    }
    fn max_name(&self) -> usize {
        MAX_NAME
    }
    fn charge(&mut self, bytes: usize) -> bool {
        if bytes > MAX_NAME {
            self.control.truncated = true;
        }
        self.control.decoded = self.control.decoded.saturating_add(bytes);
        if self.control.decoded > MAX_IO {
            self.control.limited = true;
        }
        self.control.proceed()
    }
    fn face_limit(&mut self, n: usize) -> usize {
        let take = n.min(256);
        if take < n {
            self.control.truncated = true;
        }
        take
    }
}
fn directory(reader: &mut (impl NameReader + ?Sized), index: u32) -> Option<u64> {
    let h = reader.read_at(0, 12)?;
    if &h[..4] == b"ttcf" {
        if index >= be32(&h, 8)? {
            return None;
        }
        Some(u64::from(be32(
            &reader.read_at(12 + u64::from(index) * 4, 4)?,
            0,
        )?))
    } else if index == 0 {
        Some(0)
    } else {
        None
    }
}
fn properties(reader: &mut impl NameReader, index: u32) -> (u16, bool) {
    let read = |reader: &mut dyn NameReader| -> Option<(u16, bool)> {
        let dir = directory(reader, index)?;
        let head = reader.read_at(dir, 12)?;
        let n = usize::from(be16(&head, 4)?).min(512);
        let recs = reader.read_at(dir + 12, n * 16)?;
        let (mut weight, mut italic) = (400, false);
        for r in recs.chunks_exact(16) {
            let offset = u64::from(be32(r, 8)?);
            let length = be32(r, 12)?;
            if &r[..4] == b"OS/2" && length >= 64 {
                if let Some(os) = reader.read_at(offset, 64) {
                    weight = be16(&os, 4)
                        .filter(|n| (1..=1000).contains(n))
                        .unwrap_or(400);
                    italic |= be16(&os, 62).unwrap_or(0) & 1 != 0;
                }
            } else if &r[..4] == b"head"
                && length >= 46
                && let Some(head) = reader.read_at(offset, 46)
            {
                italic |= be16(&head, 44).unwrap_or(0) & 2 != 0;
            }
        }
        Some((weight, italic))
    };
    read(reader).unwrap_or((400, false))
}
pub fn same_file(face: &Face, metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.len() == face.length && metadata.modified().ok() == face.modified
}
/// Recheck scope and the actual opened handle, rather than trusting a retained path.
pub fn open_checked(face: &Face, roots: &[PathBuf]) -> Result<File, ()> {
    let file = open_scoped(&face.path, roots)?;
    if !same_file(face, &file.metadata().map_err(|_| ())?) {
        return Err(());
    }
    Ok(file)
}
fn open_scoped(expected: &Path, roots: &[PathBuf]) -> Result<File, ()> {
    let path = expected.canonicalize().map_err(|_| ())?;
    if path != expected || !roots.iter().any(|r| path.starts_with(r)) {
        return Err(());
    }
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
            .open(&path)
            .map_err(|_| ())?
    };
    #[cfg(not(windows))]
    let file = File::open(&path).map_err(|_| ())?;
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        let mut name = vec![0u16; MAX_PATH];
        // The API fills only this bounded buffer; flags0 requests the normalized
        // DOS-volume path, as std::fs::canonicalize uses on Windows.
        let n = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                name.as_mut_ptr(),
                name.len() as u32,
                0,
            )
        } as usize;
        if n == 0
            || n >= name.len()
            || std::ffi::OsString::from_wide(&name[..n]) != path.as_os_str()
        {
            return Err(());
        }
    }
    #[cfg(not(windows))]
    {
        if path.canonicalize().map_err(|_| ())? != path {
            return Err(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|_| ())?;
            let current = std::fs::metadata(&path).map_err(|_| ())?;
            if opened.dev() != current.dev() || opened.ino() != current.ino() {
                return Err(());
            }
        }
    }
    Ok(file)
}
pub fn host_roots() -> Vec<PathBuf> {
    override_dirs()
        .into_iter()
        .chain(system_font_dirs())
        .take(16)
        .collect()
}
pub fn collect(
    roots: Vec<PathBuf>,
    query: &str,
    cap: usize,
    check: &mut dyn FnMut() -> bool,
) -> Result<Inventory, ()> {
    let mut control = Control {
        check,
        stopped: false,
        limited: false,
        truncated: false,
        bytes: 0,
        decoded: 0,
    };
    let mut result = Inventory {
        page: 1,
        ..Inventory::default()
    };
    for root in roots.into_iter().take(16) {
        if !control.proceed() {
            return Err(());
        }
        if let Ok(root) = root.canonicalize()
            && root.as_os_str().len() <= MAX_PATH
        {
            result.roots.push(root);
        }
    }
    result.roots.sort();
    result.roots.dedup();
    let mut files = Vec::new();
    let mut entries = 20_000;
    for root in &result.roots {
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
            &mut || control.proceed(),
        ) {
            control.truncated = true;
            break;
        }
    }
    result.entries = 20_000 - entries;
    files.sort();
    files.dedup();
    let query = query.to_lowercase();
    for path in files {
        if !control.proceed() {
            break;
        }
        if result.faces.len() == cap {
            control.truncated = true;
            break;
        }
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if path.as_os_str().len() > MAX_PATH || !result.roots.iter().any(|r| path.starts_with(r)) {
            continue;
        }
        let Ok(mut file) = open_scoped(&path, &result.roots) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
            control.truncated = true;
            continue;
        }
        result.files += 1;
        let mut reader = Reader {
            file: &mut file,
            control: &mut control,
        };
        for names in read_faces_with(&mut reader) {
            if !format!("{} {}", names.effective_family(), names.effective_style())
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            if result.faces.len() == cap {
                reader.control.truncated = true;
                break;
            }
            let (weight, italic) = properties(&mut reader, names.index);
            if !reader.control.proceed() {
                break;
            }
            result.faces.push(Face {
                names,
                path: path.clone(),
                weight,
                italic,
                length: metadata.len(),
                modified: metadata.modified().ok(),
            });
        }
    }
    if control.stopped {
        return Err(());
    }
    result.faces.sort_by(|a, b| {
        a.names
            .effective_family()
            .cmp(b.names.effective_family())
            .then(a.names.effective_style().cmp(b.names.effective_style()))
            .then(a.path.cmp(&b.path))
            .then(a.names.index.cmp(&b.names.index))
    });
    result.read_bytes = control.bytes;
    result.truncated = control.truncated || control.limited;
    result.scan_truncated = result.truncated;
    Ok(result)
}
/// Verify one loaded snapshot against a catalog row using the same bounded name parser.
pub fn snapshot_matches(bytes: &[u8], face: &Face, check: &mut impl FnMut() -> bool) -> bool {
    let mut work = 0usize;
    let dir = if bytes.starts_with(b"ttcf") {
        be32(bytes, 12 + face.names.index as usize * 4).map(|n| n as usize)
    } else if face.names.index == 0 {
        Some(0)
    } else {
        None
    };
    let Some((off, len)) = dir.and_then(|d| name_table_at(bytes, d)) else {
        return false;
    };
    if len > MAX_TABLE {
        return false;
    }
    let Some(table) = bytes.get(off..off.saturating_add(len)) else {
        return false;
    };
    let names = parse_names_checked(table, face.names.index, MAX_NAME, &mut |n| {
        work += n;
        work <= MAX_IO && check()
    });
    names.is_some_and(|n| n == face.names)
}
