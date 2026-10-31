//! A bounded ZIP reader (PlumSpirit's own): the central directory
//! (Zip64 included) and each entry's data, stored, deflated, Zstandard
//! (method 93) or xz (method 95). Encrypted entries, links and other methods
//! are reported, never guessed. Sizes and offsets are checked against the
//! file before anything is read; the CRC-32 and size of every decoded entry
//! are verified ([`Crc`]).

use super::{ArchiveError, unwrap_io};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

/// One central-directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub flags: u16,
    pub crc32: u32,
    pub compressed: u64,
    pub size: u64,
    pub local_offset: u64,
    /// Directory, link or other non-file entry.
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// Why an entry is not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    Directory,
    Symlink,
    Encrypted,
    Method(u16),
}

impl Skip {
    /// A note for the user (none for a plain directory).
    pub fn note(&self, what: &str) -> Option<String> {
        match self {
            Skip::Directory => None,
            Skip::Symlink => Some(format!("{what} is a link and was skipped.")),
            Skip::Encrypted => Some(format!(
                "{what} is encrypted (password protected) and was skipped."
            )),
            Skip::Method(m) => Some(format!(
                "{what} uses a compression method PlumSpirit cannot read ({}) and was skipped.",
                method_name(*m)
            )),
        }
    }
}

fn method_name(m: u16) -> String {
    match m {
        9 => "Deflate64".into(),
        12 => "bzip2".into(),
        14 => "LZMA".into(),
        98 => "PPMd".into(),
        99 => "AES encryption".into(),
        _ => format!("method {m}"),
    }
}

pub struct ZipFile {
    file: File,
    len: u64,
    entries: Vec<Entry>,
}

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64le(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"))
}

/// Most central-directory bytes read (a huge declared directory is refused).
const MAX_DIRECTORY: u64 = 256 << 20;

impl ZipFile {
    pub fn open(mut file: File, what: &str) -> Result<ZipFile, ArchiveError> {
        let bad = |m: &str| {
            ArchiveError::Malformed(format!("{what} is not a readable ZIP archive ({m})."))
        };
        let io_err = |e: io::Error| unwrap_io(e, what);
        let len = file.seek(SeekFrom::End(0)).map_err(io_err)?;
        if len < 22 {
            return Err(bad("too short"));
        }
        // End of central directory: the last 22 bytes plus a comment.
        let tail_len = len.min(22 + 65_535);
        let mut tail = vec![0u8; tail_len as usize];
        file.seek(SeekFrom::Start(len - tail_len)).map_err(io_err)?;
        file.read_exact(&mut tail).map_err(io_err)?;
        let eocd = (0..=tail.len() - 22)
            .rev()
            .find(|&i| u32le(&tail, i) == 0x0605_4b50)
            .ok_or_else(|| bad("no end of central directory"))?;
        let e = &tail[eocd..];
        let mut count = u64::from(u16le(e, 10));
        let mut cd_size = u64::from(u32le(e, 12));
        let mut cd_off = u64::from(u32le(e, 16));
        let eocd_pos = len - tail_len + eocd as u64;
        if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
            // Zip64: locator right before the EOCD.
            if eocd_pos < 20 {
                return Err(bad("Zip64 locator missing"));
            }
            let mut loc = [0u8; 20];
            file.seek(SeekFrom::Start(eocd_pos - 20)).map_err(io_err)?;
            file.read_exact(&mut loc).map_err(io_err)?;
            if u32le(&loc, 0) != 0x0706_4b50 {
                return Err(bad("Zip64 locator missing"));
            }
            let z64 = u64le(&loc, 8);
            if z64.checked_add(56).is_none_or(|end| end > len) {
                return Err(bad("Zip64 record outside the file"));
            }
            let mut rec = [0u8; 56];
            file.seek(SeekFrom::Start(z64)).map_err(io_err)?;
            file.read_exact(&mut rec).map_err(io_err)?;
            if u32le(&rec, 0) != 0x0606_4b50 {
                return Err(bad("Zip64 record damaged"));
            }
            count = u64le(&rec, 32);
            cd_size = u64le(&rec, 40);
            cd_off = u64le(&rec, 48);
        }
        if cd_off.checked_add(cd_size).is_none_or(|end| end > len) {
            return Err(bad("central directory outside the file"));
        }
        if cd_size > MAX_DIRECTORY || count > cd_size / 46 + 1 {
            return Err(bad("central directory too large for its entries"));
        }
        let mut cd = vec![0u8; cd_size as usize];
        file.seek(SeekFrom::Start(cd_off)).map_err(io_err)?;
        file.read_exact(&mut cd).map_err(io_err)?;
        let mut entries = Vec::with_capacity(count.min(1 << 16) as usize);
        let mut i = 0usize;
        while entries.len() < count as usize {
            if i + 46 > cd.len() || u32le(&cd, i) != 0x0201_4b50 {
                return Err(bad("central directory entry damaged"));
            }
            let made_by = u16le(&cd, i + 4);
            let flags = u16le(&cd, i + 8);
            let method = u16le(&cd, i + 10);
            let crc32 = u32le(&cd, i + 16);
            let mut compressed = u64::from(u32le(&cd, i + 20));
            let mut size = u64::from(u32le(&cd, i + 24));
            let n = usize::from(u16le(&cd, i + 28));
            let x = usize::from(u16le(&cd, i + 30));
            let c = usize::from(u16le(&cd, i + 32));
            let ext_attr = u32le(&cd, i + 38);
            let mut local_offset = u64::from(u32le(&cd, i + 42));
            let end = i + 46 + n + x + c;
            if end > cd.len() {
                return Err(bad("central directory entry damaged"));
            }
            let raw_name = &cd[i + 46..i + 46 + n];
            let name = if flags & 0x0800 != 0 {
                String::from_utf8_lossy(raw_name).into_owned()
            } else {
                raw_name.iter().map(|&b| cp437(b)).collect()
            };
            // Zip64 extra field: the fields that were 0xFFFFFFFF, in order.
            let mut extra = &cd[i + 46 + n..i + 46 + n + x];
            while extra.len() >= 4 {
                let id = u16le(extra, 0);
                let sz = usize::from(u16le(extra, 2));
                let body = &extra[4..(4 + sz).min(extra.len())];
                if id == 0x0001 {
                    let mut k = 0;
                    let mut take = |v: &mut u64| {
                        if *v == 0xFFFF_FFFF && k + 8 <= body.len() {
                            *v = u64le(body, k);
                            k += 8;
                        }
                    };
                    take(&mut size);
                    take(&mut compressed);
                    take(&mut local_offset);
                }
                extra = &extra[(4 + sz).min(extra.len())..];
            }
            let unix_mode = if made_by >> 8 == 3 { ext_attr >> 16 } else { 0 };
            let kind = if name.ends_with('/') || name.ends_with('\\') {
                EntryKind::Directory
            } else if unix_mode & 0o170000 == 0o120000 {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            entries.push(Entry {
                name,
                method,
                flags,
                crc32,
                compressed,
                size,
                local_offset,
                kind,
            });
            i = end;
        }
        Ok(ZipFile { file, len, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Whether `e` is read, or why not.
    pub fn check(&self, e: &Entry) -> Result<(), Skip> {
        match e.kind {
            EntryKind::Directory => return Err(Skip::Directory),
            EntryKind::Symlink => return Err(Skip::Symlink),
            EntryKind::File => {}
        }
        if e.flags & 1 != 0 || e.method == 99 {
            return Err(Skip::Encrypted);
        }
        if !matches!(e.method, 0 | 8 | 93 | 95) {
            return Err(Skip::Method(e.method));
        }
        Ok(())
    }

    /// The decoded data of `e` and its compressed size.
    pub fn reader(
        &mut self,
        e: &Entry,
        what: &str,
    ) -> Result<(Box<dyn Read + '_>, u64), ArchiveError> {
        let bad = |m: &str| {
            ArchiveError::Malformed(format!("{what} is damaged in its ZIP archive ({m})."))
        };
        let io_err = |err: io::Error| unwrap_io(err, what);
        if e.local_offset.checked_add(30).is_none_or(|x| x > self.len) {
            return Err(bad("entry outside the file"));
        }
        let mut lh = [0u8; 30];
        self.file
            .seek(SeekFrom::Start(e.local_offset))
            .map_err(io_err)?;
        self.file.read_exact(&mut lh).map_err(io_err)?;
        if u32le(&lh, 0) != 0x0403_4b50 {
            return Err(bad("local header missing"));
        }
        let data = e.local_offset + 30 + u64::from(u16le(&lh, 26)) + u64::from(u16le(&lh, 28));
        if data.checked_add(e.compressed).is_none_or(|x| x > self.len) {
            return Err(bad("data outside the file"));
        }
        if e.method == 0 && e.compressed != e.size {
            return Err(bad("stored sizes differ"));
        }
        self.file.seek(SeekFrom::Start(data)).map_err(io_err)?;
        let raw = BufReader::with_capacity(1 << 16, (&mut self.file).take(e.compressed));
        let r: Box<dyn Read + '_> = match e.method {
            0 => Box::new(raw),
            8 => Box::new(flate2::read::DeflateDecoder::new(raw)),
            93 => Box::new(super::ZstdReader::new(raw, what)?),
            95 => Box::new(lzma_rust2::XzReader::new_mem_limit(
                raw,
                true,
                super::XZ_MEM_LIMIT_KB,
            )),
            m => return Err(bad(&format!("method {m}"))),
        };
        Ok((r, e.compressed))
    }
}

/// CP437 (the original ZIP name encoding) for the bytes ASCII lacks; a
/// replacement character for box drawing and the like.
fn cp437(b: u8) -> char {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»";
    if b < 0x80 {
        char::from(b)
    } else {
        HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('_')
    }
}

/// Verifies the CRC-32 and size of a decoded entry at its end.
pub struct Crc<R> {
    inner: R,
    hasher: crc32fast::Hasher,
    want_crc: u32,
    want_size: u64,
    size: u64,
}

impl<R> Crc<R> {
    pub fn new(inner: R, want_crc: u32, want_size: u64) -> Self {
        Crc {
            inner,
            hasher: crc32fast::Hasher::new(),
            want_crc,
            want_size,
            size: 0,
        }
    }
}

impl<R: Read> Read for Crc<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 && !buf.is_empty() {
            let crc = self.hasher.clone().finalize();
            if self.size != self.want_size || crc != self.want_crc {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CRC {crc:08x} / {} bytes instead of {:08x} / {}",
                        self.size, self.want_crc, self.want_size
                    ),
                ));
            }
            return Ok(0);
        }
        self.hasher.update(&buf[..n]);
        self.size += n as u64;
        if self.size > self.want_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("more than the {} bytes listed", self.want_size),
            ));
        }
        Ok(n)
    }
}
