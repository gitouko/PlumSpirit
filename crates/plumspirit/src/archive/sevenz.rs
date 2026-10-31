//! L-D01 (L-S10): 7z archives.
//!
//! The entries are decoded by `sevenz-rust2` (Apache-2.0; LZMA, LZMA2,
//! BCJ / BCJ2 / delta filters and stored data — no AES, no C codecs), but
//! only after [`survey`] has read the archive's own structure and bounded
//! everything the crate would allocate from it: the start header is checked,
//! the (possibly LZMA-compressed) header is at most [`HEADER_CAP`] bytes, the
//! numbers of files, folders and packed streams are within the entry limit,
//! and every LZMA / LZMA2 dictionary is within the xz limit before a single
//! byte of a folder is decoded — a crafted archive cannot make a 2 GB PC
//! allocate gigabytes. Encrypted folders, methods PlumSpirit does not ship
//! (PPMd, bzip2, Deflate, Zstandard, …) and oversized dictionaries are
//! reported per file; the other files still open. Solid folders decode in
//! order, every entry drained so the next one starts at its own first byte.

use super::*;
use sevenz_rust2::{Archive, BlockDecoder, Password};
use std::io::{Seek, SeekFrom};

/// Bytes a 7z header may take, stored or decoded (a file list of 200 000
/// entries with long names fits in a few MB).
pub const HEADER_CAP: u64 = 64 << 20;
/// LZMA / LZMA2 dictionary a 7z folder may ask for: the xz limit.
pub const DICT_CAP: u64 = XZ_MEM_LIMIT_KB as u64 * 1024;
/// Coders and coder streams in one folder (7-Zip's own limit).
const MAX_CODERS: u64 = 64;

const SIGNATURE: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS: u8 = 0x03;
const K_MAIN_STREAMS: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_ENCODED_HEADER: u8 = 0x17;

const ID_COPY: &[u8] = &[0x00];
const ID_LZMA: &[u8] = &[0x03, 0x01, 0x01];
const ID_LZMA2: &[u8] = &[0x21];
const ID_AES: &[u8] = &[0x06, 0xF1, 0x07, 0x01];
/// Filters the decoder ships (delta, BCJ x86 / BCJ2 / PPC / IA64 / ARM /
/// ARM-Thumb / SPARC / ARM64 / RISC-V).
const FILTERS: [&[u8]; 10] = [
    &[0x03],
    &[0x03, 0x03, 0x01, 0x03],
    &[0x03, 0x03, 0x01, 0x1B],
    &[0x03, 0x03, 0x02, 0x05],
    &[0x03, 0x03, 0x04, 0x01],
    &[0x03, 0x03, 0x05, 0x01],
    &[0x03, 0x03, 0x07, 0x01],
    &[0x03, 0x03, 0x08, 0x05],
    &[0x0A],
    &[0x0B],
];

/// A method by its 7z id, for messages.
fn method_name(id: &[u8]) -> String {
    match id {
        [0x03, 0x04, 0x01] => "PPMd".into(),
        [0x04, 0x02, 0x02] => "bzip2".into(),
        [0x04, 0x01, 0x08] => "Deflate".into(),
        [0x04, 0x01, 0x09] => "Deflate64".into(),
        [0x04, 0xF7, 0x11, 0x01] => "Zstandard".into(),
        [0x04, 0xF7, 0x11, 0x02] => "Brotli".into(),
        [0x04, 0xF7, 0x11, 0x04] => "LZ4".into(),
        [0x04, 0xF7, 0x11, 0x05] => "LZ5".into(),
        [0x04, 0xF7, 0x11, 0x06] => "Lizard".into(),
        _ => {
            let hex: Vec<String> = id.iter().map(|b| format!("{b:02X}")).collect();
            format!("method {}", hex.join(""))
        }
    }
}

/// One coder of a folder as the header declares it.
#[derive(Clone, Debug)]
pub struct Coder {
    pub id: Vec<u8>,
    pub props: Vec<u8>,
    /// Index of the coder's (first) output stream in the folder.
    pub out_index: usize,
}

/// One folder (a block of one or more files decoded as one stream).
#[derive(Clone, Debug, Default)]
pub struct Folder {
    pub coders: Vec<Coder>,
    /// Decoded size of every output stream.
    pub unpack: Vec<u64>,
    /// Packed streams it reads.
    pub packed_streams: u64,
    /// Their size in the file (the expansion ratio's denominator).
    pub packed: u64,
}

/// Why a folder is not decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Encrypted,
    Method(String),
    /// An LZMA / LZMA2 dictionary (bytes) larger than [`DICT_CAP`].
    Dictionary(u64),
}

impl Refusal {
    /// The note for one file of the folder.
    pub fn note(&self, what: &str) -> String {
        match self {
            Refusal::Encrypted => {
                format!("{what} is encrypted (password protected) and was skipped.")
            }
            Refusal::Method(m) => format!(
                "{what} uses a compression method PlumSpirit cannot read ({m}) and was skipped."
            ),
            Refusal::Dictionary(d) => format!(
                "{what} needs a {} LZMA dictionary, more than the {} PlumSpirit allows, and was skipped.",
                dict_text(*d),
                dict_text(DICT_CAP)
            ),
        }
    }
}

/// A dictionary size for messages (4 GB − 1 reads "4 GB", not "3 GB").
fn dict_text(d: u64) -> String {
    if d >= 1 << 30 {
        format!("{} GB", d.div_ceil(1 << 30))
    } else {
        format!("{} MB", d.div_ceil(1 << 20))
    }
}

/// The dictionary an LZMA2 property byte declares (bytes).
fn lzma2_dict(prop: u8) -> Option<u64> {
    match prop {
        0..=39 => Some(u64::from(2 | (u32::from(prop) & 1)) << (u32::from(prop) / 2 + 11)),
        40 => Some(0xFFFF_FFFF),
        _ => None,
    }
}

impl Folder {
    /// The first reason this folder cannot be decoded within the limits.
    pub fn refusal(&self) -> Option<Refusal> {
        if self.coders.iter().any(|c| c.id == ID_AES) {
            return Some(Refusal::Encrypted);
        }
        for c in &self.coders {
            let id = c.id.as_slice();
            let dict = if id == ID_LZMA {
                // LZMA allocates no more than the stream it decodes.
                let d = c
                    .props
                    .get(1..5)
                    .map(|p| u64::from(u32::from_le_bytes([p[0], p[1], p[2], p[3]])));
                let out = self.unpack.get(c.out_index).copied().unwrap_or(u64::MAX);
                d.map(|d| d.min(out))
            } else if id == ID_LZMA2 {
                c.props.first().copied().and_then(lzma2_dict)
            } else if id == ID_COPY || FILTERS.contains(&id) {
                continue;
            } else {
                return Some(Refusal::Method(method_name(id)));
            };
            match dict {
                // Damaged properties: the decoder reports them.
                None => continue,
                Some(d) if d > DICT_CAP => return Some(Refusal::Dictionary(d)),
                Some(_) => {}
            }
        }
        None
    }
}

/// What [`survey`] found.
pub enum Survey {
    /// No header: an archive with no files.
    Empty,
    /// The file list itself is encrypted.
    Encrypted,
    /// The file list is compressed with something other than LZMA / LZMA2.
    HeaderMethod(String),
    Listed {
        folders: Vec<Folder>,
        files: u64,
    },
}

/// A structural problem: damaged data, or a count over the limits.
enum Bad {
    Damaged(&'static str),
    TooMany(&'static str),
}

type P<T> = Result<T, Bad>;

const SHORT: Bad = Bad::Damaged("the header ends early");

/// A cursor over header bytes with the 7z number encoding.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
    /// Upper bound for every count (files, folders, streams).
    max: u64,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> P<u8> {
        let v = *self.b.get(self.p).ok_or(SHORT)?;
        self.p += 1;
        Ok(v)
    }

    fn take(&mut self, n: u64) -> P<&'a [u8]> {
        let n = usize::try_from(n).map_err(|_| SHORT)?;
        if self.b.len() - self.p < n {
            return Err(SHORT);
        }
        self.p += n;
        Ok(&self.b[self.p - n..self.p])
    }

    /// A 7z NUMBER: the leading one bits of the first byte count the bytes
    /// that follow (little endian), the rest of it is the high part.
    fn num(&mut self) -> P<u64> {
        let first = self.u8()?;
        let mut mask = 0x80u8;
        let mut v = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                return Ok(v | (u64::from(first & mask.wrapping_sub(1)) << (8 * i)));
            }
            v |= u64::from(self.u8()?) << (8 * i);
            mask >>= 1;
        }
        Ok(v)
    }

    fn count(&mut self, what: &'static str) -> P<u64> {
        let n = self.num()?;
        if n > self.max {
            return Err(Bad::TooMany(what));
        }
        Ok(n)
    }

    /// `n` flags, most significant bit first; `n` is a bounded count.
    fn bits(&mut self, n: u64) -> P<Vec<bool>> {
        let mut v = Vec::with_capacity(n as usize);
        let mut byte = 0u8;
        for i in 0..n {
            if i % 8 == 0 {
                byte = self.u8()?;
            }
            v.push(byte & (0x80 >> (i % 8)) != 0);
        }
        Ok(v)
    }

    /// A digest list for `n` streams; returns which ones are defined.
    fn digests(&mut self, n: u64) -> P<Vec<bool>> {
        let all = self.u8()?;
        let defined = if all == 0 {
            self.bits(n)?
        } else {
            vec![true; n as usize]
        };
        let k = defined.iter().filter(|d| **d).count() as u64;
        self.take(k * 4)?;
        Ok(defined)
    }
}

#[derive(Default)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
}

fn streams_info(c: &mut Cur) -> P<Streams> {
    let mut s = Streams::default();
    let mut id = c.u8()?;
    if id == K_PACK_INFO {
        s.pack_pos = c.num()?;
        let n = c.count("packed streams")?;
        loop {
            match c.u8()? {
                K_END => break,
                K_SIZE => {
                    s.pack_sizes = (0..n).map(|_| c.num()).collect::<P<_>>()?;
                }
                K_CRC => {
                    c.digests(n)?;
                }
                _ => return Err(Bad::Damaged("unexpected data in the pack information")),
            }
        }
        if s.pack_sizes.len() as u64 != n {
            return Err(Bad::Damaged("packed stream sizes are missing"));
        }
        id = c.u8()?;
    }
    let mut folder_crc = Vec::new();
    if id == K_UNPACK_INFO {
        if c.u8()? != K_FOLDER {
            return Err(Bad::Damaged("the folder list is missing"));
        }
        let n = c.count("folders")?;
        if c.u8()? != 0 {
            return Err(Bad::Damaged("external folder data is not supported"));
        }
        for _ in 0..n {
            s.folders.push(folder(c)?);
        }
        if c.u8()? != K_CODERS_UNPACK_SIZE {
            return Err(Bad::Damaged("folder sizes are missing"));
        }
        for f in &mut s.folders {
            for u in f.unpack.iter_mut() {
                *u = c.num()?;
            }
        }
        folder_crc = vec![false; n as usize];
        loop {
            match c.u8()? {
                K_END => break,
                K_CRC => folder_crc = c.digests(n)?,
                _ => return Err(Bad::Damaged("unexpected data in the folder information")),
            }
        }
        // Packed sizes per folder (consecutive packed streams).
        let mut next = 0usize;
        for f in &mut s.folders {
            let end = next.saturating_add(f.packed_streams as usize);
            let sizes = s
                .pack_sizes
                .get(next..end)
                .ok_or(Bad::Damaged("folders read more packed streams than exist"))?;
            f.packed = sizes.iter().fold(0u64, |a, b| a.saturating_add(*b));
            next = end;
        }
        id = c.u8()?;
    }
    if id == K_SUB_STREAMS {
        let nf = s.folders.len();
        let mut per = vec![1u64; nf];
        let mut t = c.u8()?;
        if t == K_NUM_UNPACK_STREAM {
            let mut total = 0u64;
            for p in per.iter_mut() {
                *p = c.num()?;
                total = total.saturating_add(*p);
                if total > c.max {
                    return Err(Bad::TooMany("files in folders"));
                }
            }
            t = c.u8()?;
        }
        if t == K_SIZE {
            for p in &per {
                for _ in 1..(*p).max(1) {
                    c.num()?;
                }
            }
            t = c.u8()?;
        }
        let unknown_crc: u64 = per
            .iter()
            .zip(folder_crc.iter().chain(std::iter::repeat(&false)))
            .map(|(p, crc)| if *p == 1 && *crc { 0 } else { *p })
            .sum();
        loop {
            match t {
                K_END => break,
                K_CRC => {
                    c.digests(unknown_crc)?;
                }
                _ => {
                    return Err(Bad::Damaged(
                        "unexpected data in the sub-stream information",
                    ));
                }
            }
            t = c.u8()?;
        }
        id = c.u8()?;
    }
    if id != K_END {
        return Err(Bad::Damaged("unexpected data in the stream information"));
    }
    Ok(s)
}

fn folder(c: &mut Cur) -> P<Folder> {
    let n = c.num()?;
    if n == 0 || n > MAX_CODERS {
        return Err(Bad::Damaged("a folder has no coders or too many"));
    }
    let mut f = Folder::default();
    let (mut ins, mut outs) = (0u64, 0u64);
    for _ in 0..n {
        let flag = c.u8()?;
        if flag & 0x80 != 0 {
            return Err(Bad::Damaged("alternative coder methods are not supported"));
        }
        let id = c.take(u64::from(flag & 0x0F))?.to_vec();
        let (i, o) = if flag & 0x10 != 0 {
            (c.num()?, c.num()?)
        } else {
            (1, 1)
        };
        let props = if flag & 0x20 != 0 {
            let k = c.num()?;
            c.take(k)?.to_vec()
        } else {
            Vec::new()
        };
        f.coders.push(Coder {
            id,
            props,
            out_index: outs as usize,
        });
        ins = ins.saturating_add(i);
        outs = outs.saturating_add(o);
        if ins > MAX_CODERS || outs > MAX_CODERS {
            return Err(Bad::Damaged("a folder has too many coder streams"));
        }
    }
    if outs == 0 || ins < outs - 1 {
        return Err(Bad::Damaged("a folder's coders are not connected"));
    }
    for _ in 0..outs - 1 {
        c.num()?;
        c.num()?;
    }
    f.packed_streams = ins - (outs - 1);
    if f.packed_streams > 1 {
        for _ in 0..f.packed_streams {
            c.num()?;
        }
    }
    f.unpack = vec![0; outs as usize];
    Ok(f)
}

/// The number of files and the folders of a decoded (plain) header.
fn plain_header(c: &mut Cur) -> P<(Vec<Folder>, u64)> {
    if c.u8()? != K_HEADER {
        return Err(Bad::Damaged("the file list is missing"));
    }
    let mut id = c.u8()?;
    if id == K_ARCHIVE_PROPERTIES {
        while c.u8()? != K_END {
            let k = c.num()?;
            c.take(k)?;
        }
        id = c.u8()?;
    }
    if id == K_ADDITIONAL_STREAMS {
        return Err(Bad::Damaged("additional streams are not supported"));
    }
    let mut folders = Vec::new();
    if id == K_MAIN_STREAMS {
        folders = streams_info(c)?.folders;
        id = c.u8()?;
    }
    let files = match id {
        K_FILES_INFO => c.count("files")?,
        K_END => 0,
        _ => return Err(Bad::Damaged("unexpected data in the header")),
    };
    Ok((folders, files))
}

fn read_at(f: &mut File, pos: u64, buf: &mut [u8]) -> io::Result<()> {
    f.seek(SeekFrom::Start(pos))?;
    f.read_exact(buf)
}

/// Reads and bounds the structure of the 7z archive `f` (`len` bytes)
/// before anything is decoded with the crate.
pub fn survey(f: &mut File, len: u64, max: u64, what: &str) -> Result<Survey, ArchiveError> {
    let damaged =
        |why: &str| ArchiveError::Malformed(format!("{what} is damaged or incomplete ({why})."));
    let bad = |b: Bad| match b {
        Bad::Damaged(why) => damaged(why),
        Bad::TooMany(t) => ArchiveError::Limit(format!(
            "{what} lists more than {max} {t}; it is not opened."
        )),
    };
    let io = |e: io::Error| unwrap_io(e, what);
    if len < 32 {
        return Err(damaged("it is shorter than a 7z header"));
    }
    let mut start = [0u8; 32];
    read_at(f, 0, &mut start).map_err(io)?;
    if start[..6] != SIGNATURE {
        return Err(damaged("the 7z signature is missing"));
    }
    if start[6] != 0 {
        return Err(ArchiveError::Malformed(format!(
            "{what} is a 7z archive of a newer format version ({}.{}) that PlumSpirit cannot read.",
            start[6], start[7]
        )));
    }
    let crc = u32::from_le_bytes([start[8], start[9], start[10], start[11]]);
    if crc32fast::hash(&start[12..32]) != crc {
        return Err(damaged(
            if crc == 0 && start[12..32].iter().all(|b| *b == 0) {
                "it was not completely written: the file list is missing"
            } else {
                "the start header checksum does not match"
            },
        ));
    }
    let le64 = |b: &[u8]| u64::from_le_bytes(b.try_into().expect("8 bytes"));
    let (off, size) = (le64(&start[12..20]), le64(&start[20..28]));
    let hcrc = u32::from_le_bytes([start[28], start[29], start[30], start[31]]);
    if size == 0 {
        return Ok(Survey::Empty);
    }
    if size > HEADER_CAP {
        return Err(ArchiveError::Limit(format!(
            "{what} has a {} file list, more than the {} PlumSpirit reads; it is not opened.",
            bytes(size),
            bytes(HEADER_CAP)
        )));
    }
    let pos = 32u64
        .checked_add(off)
        .filter(|p| p.checked_add(size).is_some_and(|e| e <= len))
        .ok_or_else(|| damaged("the file list lies past the end of the file"))?;
    let mut header = vec![0u8; size as usize];
    read_at(f, pos, &mut header).map_err(io)?;
    if crc32fast::hash(&header) != hcrc {
        return Err(damaged("the file list checksum does not match"));
    }
    if header[0] == K_ENCODED_HEADER {
        let mut c = Cur {
            b: &header,
            p: 1,
            max,
        };
        let s = streams_info(&mut c).map_err(bad)?;
        let [fo] = s.folders.as_slice() else {
            return Err(damaged("the compressed file list is not one folder"));
        };
        if fo.coders.iter().any(|c| c.id == ID_AES) {
            return Ok(Survey::Encrypted);
        }
        let [coder] = fo.coders.as_slice() else {
            return Ok(Survey::HeaderMethod(
                fo.coders
                    .iter()
                    .map(|c| method_name(&c.id))
                    .collect::<Vec<_>>()
                    .join(" + "),
            ));
        };
        if let Some(r) = fo.refusal() {
            return Ok(match r {
                Refusal::Encrypted => Survey::Encrypted,
                Refusal::Method(m) => Survey::HeaderMethod(m),
                Refusal::Dictionary(d) => {
                    return Err(ArchiveError::Limit(format!(
                        "{what} needs a {} LZMA dictionary for its file list, more than the {} PlumSpirit allows; it is not opened.",
                        dict_text(d),
                        dict_text(DICT_CAP)
                    )));
                }
            });
        }
        let unpack = fo.unpack[0];
        if unpack > HEADER_CAP {
            return Err(ArchiveError::Limit(format!(
                "{what} has a {} file list, more than the {} PlumSpirit reads; it is not opened.",
                bytes(unpack),
                bytes(HEADER_CAP)
            )));
        }
        let ppos = 32u64
            .checked_add(s.pack_pos)
            .filter(|p| p.checked_add(fo.packed).is_some_and(|e| e <= len))
            .ok_or_else(|| damaged("the compressed file list lies past the end of the file"))?;
        f.seek(SeekFrom::Start(ppos)).map_err(io)?;
        let input = BufReader::new((&mut *f).take(fo.packed));
        let mut dec: Box<dyn Read + '_> = if coder.id == ID_LZMA {
            let props = &coder.props;
            if props.len() < 5 {
                return Err(damaged("the file list's LZMA properties are too short"));
            }
            let dict = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
            Box::new(
                lzma_rust2::LzmaReader::new_with_props(input, unpack, props[0], dict, None)
                    .map_err(io)?,
            )
        } else if coder.id == ID_LZMA2 {
            let dict = coder
                .props
                .first()
                .copied()
                .and_then(lzma2_dict)
                .unwrap_or(0);
            Box::new(lzma_rust2::Lzma2Reader::new(
                input,
                dict.min(u64::from(u32::MAX)) as u32,
                None,
            ))
        } else {
            Box::new(input)
        };
        let mut plain = Vec::with_capacity(unpack as usize);
        (&mut dec)
            .take(unpack)
            .read_to_end(&mut plain)
            .map_err(io)?;
        if plain.len() as u64 != unpack {
            return Err(damaged("the compressed file list is cut off"));
        }
        drop(dec);
        header = plain;
    }
    let mut c = Cur {
        b: &header,
        p: 0,
        max,
    };
    let (folders, files) = plain_header(&mut c).map_err(bad)?;
    Ok(Survey::Listed { folders, files })
}

/// A crate error as an [`ArchiveError`].
pub fn error(e: sevenz_rust2::Error, what: &str) -> ArchiveError {
    match e {
        sevenz_rust2::Error::Io(e, _) | sevenz_rust2::Error::FileOpen(e, _) => unwrap_io(e, what),
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            ArchiveError::Malformed(format!(
                "{what} is encrypted (password protected); PlumSpirit cannot open it."
            ))
        }
        e => ArchiveError::Malformed(format!(
            "{what} is damaged or incomplete ({}).",
            e.to_string().trim_end_matches('.')
        )),
    }
}

/// A Unix symbolic link or a Windows reparse point (junction / link).
fn is_link(e: &sevenz_rust2::ArchiveEntry) -> bool {
    if !e.has_windows_attributes {
        return false;
    }
    let a = e.windows_attributes;
    a & 0x400 != 0 || (a & 0x8000 != 0 && (a >> 16) & 0o170_000 == 0o120_000)
}

impl Walker<'_, '_> {
    /// The files of a 7z archive.
    pub(super) fn walk_7z(
        &mut self,
        path: &Path,
        chain: Vec<String>,
        depth: usize,
    ) -> Result<(), ArchiveError> {
        let what = format!("“{}”", chain.join(" › "));
        let mut f = self.open(path, &chain)?;
        let len = f.metadata().map_err(|e| unwrap_io(e, &what))?.len();
        let folders = match survey(&mut f, len, self.budget.limits.max_entries, &what)? {
            Survey::Empty => return Ok(()),
            Survey::Encrypted => {
                self.notes.push(format!(
                    "{what} is encrypted (password protected), its file list included; PlumSpirit cannot open it."
                ));
                return Ok(());
            }
            Survey::HeaderMethod(m) => {
                self.notes.push(format!(
                    "{what} stores its file list with a compression method PlumSpirit cannot read ({m}); it was skipped."
                ));
                return Ok(());
            }
            Survey::Listed { folders, files } => {
                for _ in 0..files {
                    self.budget.entry()?;
                }
                folders
            }
        };
        let mut src = BufReader::with_capacity(1 << 16, f);
        src.seek(SeekFrom::Start(0))
            .map_err(|e| unwrap_io(e, &what))?;
        let password = Password::empty();
        let archive = Archive::read(&mut src, &password).map_err(|e| error(e, &what))?;
        if archive.blocks.len() != folders.len() {
            return Err(ArchiveError::Malformed(format!(
                "{what} is damaged or incomplete (its folder list is inconsistent)."
            )));
        }
        // Display names, duplicates numbered in file order.
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        let names: Vec<String> = archive
            .files
            .iter()
            .map(|e| {
                let name = display_name(e.name());
                let k = seen.entry(name.clone()).or_default();
                *k += 1;
                if *k > 1 {
                    format!("{name} ({k})")
                } else {
                    name
                }
            })
            .collect();
        let chain_of = |k: usize| {
            let mut c = chain.clone();
            c.push(names[k].clone());
            c
        };
        let budget = self.budget;
        for (bi, folder) in folders.iter().enumerate() {
            budget.check()?;
            let first = archive.stream_map.block_first_file_index[bi];
            let in_block = |k: &usize| archive.stream_map.file_block_index[*k] == Some(bi);
            if let Some(r) = folder.refusal() {
                for k in (first..archive.files.len()).filter(in_block) {
                    let what2 = format!("“{}”", chain_of(k).join(" › "));
                    self.notes.push(r.note(&what2));
                }
                continue;
            }
            let dec = BlockDecoder::new(1, bi, &archive, &password, &mut src);
            let solid = dec.entry_count() > 1;
            let packed = folder.packed;
            let mut k = first;
            let mut failure: Option<ArchiveError> = None;
            let walked = dec.for_each_entries(&mut |entry, reader| {
                let idx = k;
                k += 1;
                if entry.is_directory()
                    || entry.is_anti_item()
                    || !entry.has_stream()
                    || entry.size() == 0
                {
                    return Ok(true);
                }
                let chain2 = chain_of(idx);
                let what2 = format!("“{}”", chain2.join(" › "));
                let mut m = Meter::new(reader, budget, &what2);
                m.compressed = Some(Box::new(move || packed));
                let r = (|| {
                    if is_link(entry) {
                        self.notes
                            .push(format!("{what2} is a link and was skipped."));
                    } else {
                        let head = read_head(&mut m).map_err(|e| unwrap_io(e, &what2))?;
                        let used = detect(&head) != Format::Unknown;
                        if !used && !solid {
                            // Its own folder: nothing after it to reach.
                            return Ok(());
                        }
                        let c = io::Cursor::new(head).chain(&mut m);
                        self.take(c, chain2, depth, &what2)?;
                    }
                    // The next file of a solid folder starts where this one
                    // ends; draining also verifies this file's CRC.
                    io::copy(&mut m, &mut io::sink()).map_err(|e| unwrap_io(e, &what2))?;
                    Ok(())
                })();
                match r {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        failure = Some(e);
                        Ok(false)
                    }
                }
            });
            if let Some(e) = failure {
                return Err(e);
            }
            walked.map_err(|e| error(e, &what))?;
        }
        Ok(())
    }
}
