//! A bounded tar reader (PlumSpirit's own): ustar, GNU (`ustar  ` magic,
//! long names `L`, base-256 sizes) and pax (`x` path / size records) headers,
//! read in sequence from any stream. Regular files are handed out; links,
//! directories and devices are skipped and named; header checksums are
//! verified.

use super::{ArchiveError, unwrap_io};
use std::io::{self, Read};

/// Longest GNU long name / pax header read.
const MAX_META: u64 = 1 << 20;

/// Whether a 512-byte block is a tar header (ustar magic or a valid
/// checksum with a plausible type).
pub fn looks_like_header(b: &[u8]) -> bool {
    if b.len() < 512 {
        return false;
    }
    if &b[257..262] == b"ustar" {
        return checksum_ok(b);
    }
    // Old (v7) headers: a NUL-terminated name, octal fields and a checksum.
    b[0] != 0
        && matches!(b[156], 0 | b'0'..=b'7')
        && checksum_ok(b)
        && octal(&b[124..136]).is_some()
}

fn octal(f: &[u8]) -> Option<u64> {
    if f.first().is_some_and(|b| b & 0x80 != 0) {
        // GNU base-256: big-endian after the marker bit.
        let mut v: u64 = u64::from(f[0] & 0x7F);
        for &b in &f[1..] {
            v = v.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(v);
    }
    let s: Vec<u8> = f
        .iter()
        .copied()
        .skip_while(|c| *c == b' ')
        .take_while(|c| *c != 0 && *c != b' ')
        .collect();
    if s.is_empty() {
        return Some(0);
    }
    let mut v = 0u64;
    for c in s {
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        v = v.checked_mul(8)?.checked_add(u64::from(c - b'0'))?;
    }
    Some(v)
}

fn checksum_ok(b: &[u8]) -> bool {
    let Some(want) = octal(&b[148..156]) else {
        return false;
    };
    let sum: u64 = b[..512]
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(c)
            }
        })
        .sum();
    sum == want
}

fn cstr(f: &[u8]) -> String {
    let end = f.iter().position(|&c| c == 0).unwrap_or(f.len());
    String::from_utf8_lossy(&f[..end]).into_owned()
}

/// One entry header.
pub struct TarEntry {
    pub name: String,
    pub regular: bool,
    /// What a skipped entry is, for the note (`None`: a directory or
    /// metadata, said nothing about).
    pub skipped_kind: Option<&'static str>,
}

pub struct TarReader<'r> {
    r: &'r mut dyn Read,
    /// Data bytes of the current entry not read yet, then its padding.
    left: u64,
    pad: u64,
}

impl<'r> TarReader<'r> {
    pub fn new(r: &'r mut dyn Read) -> Self {
        TarReader { r, left: 0, pad: 0 }
    }

    fn block(&mut self, what: &str) -> Result<Option<[u8; 512]>, ArchiveError> {
        let mut b = [0u8; 512];
        let mut n = 0;
        while n < 512 {
            match self.r.read(&mut b[n..]) {
                Ok(0) if n == 0 => return Ok(None),
                Ok(0) => {
                    return Err(ArchiveError::Malformed(format!(
                        "{what} ends inside a tar header (damaged or incomplete)."
                    )));
                }
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(unwrap_io(e, what)),
            }
        }
        Ok(Some(b))
    }

    fn read_meta(&mut self, size: u64, what: &str) -> Result<Vec<u8>, ArchiveError> {
        if size > MAX_META {
            return Err(ArchiveError::Malformed(format!(
                "{what} has a tar name / pax record of {size} bytes (damaged)."
            )));
        }
        let mut v = vec![0u8; size as usize];
        self.r.read_exact(&mut v).map_err(|e| unwrap_io(e, what))?;
        let pad = (512 - size % 512) % 512;
        io::copy(&mut (&mut self.r).take(pad), &mut io::sink()).map_err(|e| unwrap_io(e, what))?;
        Ok(v)
    }

    /// The next entry header (`None` at the end). The previous entry's data
    /// must have been finished ([`Self::finish_data`] / [`Self::skip_data`]).
    pub fn next_entry(&mut self, what: &str) -> Result<Option<TarEntry>, ArchiveError> {
        let mut long_name: Option<String> = None;
        let mut pax_path: Option<String> = None;
        let mut pax_size: Option<u64> = None;
        loop {
            let Some(b) = self.block(what)? else {
                return Ok(None);
            };
            if b.iter().all(|&c| c == 0) {
                return Ok(None); // end-of-archive marker
            }
            if !checksum_ok(&b) {
                return Err(ArchiveError::Malformed(format!(
                    "{what} has a damaged tar header (checksum)."
                )));
            }
            let size = octal(&b[124..136]).ok_or_else(|| {
                ArchiveError::Malformed(format!("{what} has a damaged tar header (size)."))
            })?;
            let kind = b[156];
            match kind {
                b'L' => {
                    let v = self.read_meta(size, what)?;
                    long_name = Some(cstr(&v));
                    continue;
                }
                b'x' => {
                    let v = self.read_meta(size, what)?;
                    for rec in String::from_utf8_lossy(&v).split('\n') {
                        // "len key=value"
                        if let Some((_, kv)) = rec.split_once(' ')
                            && let Some((k, val)) = kv.split_once('=')
                        {
                            match k {
                                "path" => pax_path = Some(val.to_owned()),
                                "size" => pax_size = val.parse().ok(),
                                _ => {}
                            }
                        }
                    }
                    continue;
                }
                b'g' | b'K' => {
                    self.read_meta(size, what)?;
                    continue;
                }
                _ => {}
            }
            let mut name = cstr(&b[0..100]);
            if &b[257..263] == b"ustar\0" {
                let prefix = cstr(&b[345..500]);
                if !prefix.is_empty() {
                    name = format!("{prefix}/{name}");
                }
            }
            let name = pax_path.take().or(long_name.take()).unwrap_or(name);
            let size = pax_size.take().unwrap_or(size);
            let (regular, skipped_kind) = match kind {
                0 | b'0' | b'7' => (true, None),
                b'5' => (false, None),
                b'1' => (false, Some("hard link")),
                b'2' => (false, Some("symbolic link")),
                b'3' | b'4' => (false, Some("device")),
                b'6' => (false, Some("pipe")),
                _ => (false, Some("special tar entry")),
            };
            // Links and directories carry no data (their size is not read).
            let data = if matches!(kind, b'1' | b'2' | b'5') {
                0
            } else {
                size
            };
            self.left = data;
            self.pad = (512 - data % 512) % 512;
            return Ok(Some(TarEntry {
                name,
                regular,
                skipped_kind,
            }));
        }
    }

    /// The current entry's data (exactly its size).
    pub fn data(&mut self) -> impl Read + '_ {
        Data { t: self }
    }

    /// Reads past what is left of the data and its padding.
    pub fn finish_data(&mut self, what: &str) -> Result<(), ArchiveError> {
        let n = self.left + self.pad;
        let got = io::copy(&mut (&mut self.r).take(n), &mut io::sink())
            .map_err(|e| unwrap_io(e, what))?;
        if got < n {
            return Err(ArchiveError::Malformed(format!(
                "{what} ends inside a tar entry (damaged or incomplete)."
            )));
        }
        self.left = 0;
        self.pad = 0;
        Ok(())
    }

    pub fn skip_data(&mut self, what: &str) -> Result<(), ArchiveError> {
        self.finish_data(what)
    }
}

struct Data<'t, 'r> {
    t: &'t mut TarReader<'r>,
}

impl Read for Data<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.t.left == 0 {
            return Ok(0);
        }
        let want = buf
            .len()
            .min(usize::try_from(self.t.left).unwrap_or(usize::MAX));
        let n = self.t.r.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the tar entry ends early",
            ));
        }
        self.t.left -= n as u64;
        Ok(n)
    }
}
