//! v1.0-Launch L-D01 / L-D03: MIDI files inside compressed downloads.
//!
//! [`open_archive`] walks an archive chain — `.zip`, `.7z`, `.gz`, `.xz`,
//! `.zst`, `.tar` and their combinations (`.tar.gz`, `.tar.xz`, `.tar.zst`,
//! an archive inside an archive) — recognising every layer by its decoded
//! magic, never by its file name, and returns the MIDI files it found as
//! [`Candidate`]s: each one decoded into an ephemeral [`Spool`] (a private
//! temporary folder deleted when the spool is dropped) so the normal MIDI
//! import reads it exactly like a file opened directly. A plain MIDI never
//! comes here (the direct path stays the fast path).
//!
//! Every byte goes through one bounded, cancellable reader ([`Meter`]):
//! total and per-entry output, the expansion ratio, the number of entries,
//! the nesting depth and a deadline are limited ([`Limits`]); decoders get
//! fixed memory limits (LZMA dictionary, zstd window). Nothing is read with
//! an unbounded `read_to_end`. Entry names are only displayed — files are
//! spooled under PlumSpirit's own names, so a name with `..`, an absolute
//! path, a drive or a UNC prefix cannot write anywhere; links and devices are
//! skipped; zip CRCs, gzip / xz / zstd checks are verified; encrypted entries
//! and unsupported methods are reported, not guessed. 7z headers are bounded
//! before decoding (`sevenz.rs`). RAR is recognised and reported: no freely
//! licensed RAR decoder exists (L-S10).

pub mod sevenz;
pub mod tar;
pub mod zip;

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// What the first bytes of a file are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Midi,
    Zip,
    Gzip,
    Xz,
    Zstd,
    Tar,
    SevenZip,
    Rar,
    Bzip2,
    Unknown,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Midi => "MIDI",
            Format::Zip => "ZIP",
            Format::Gzip => "gzip",
            Format::Xz => "xz",
            Format::Zstd => "Zstandard",
            Format::Tar => "tar",
            Format::SevenZip => "7z",
            Format::Rar => "RAR",
            Format::Bzip2 => "bzip2",
            Format::Unknown => "unknown",
        }
    }

    /// Something [`open_archive`] can walk or report.
    pub fn is_archive(self) -> bool {
        !matches!(self, Format::Midi | Format::Unknown)
    }
}

/// Bytes [`detect`] looks at.
pub const HEAD: usize = 512;

/// The format of a file from its first bytes (up to [`HEAD`]).
pub fn detect(head: &[u8]) -> Format {
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(b"MThd") || (head.len() >= 12 && starts(b"RIFF") && &head[8..12] == b"RMID") {
        Format::Midi
    } else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        Format::Zip
    } else if starts(&[0x1F, 0x8B]) {
        Format::Gzip
    } else if starts(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        Format::Xz
    } else if starts(&[0x28, 0xB5, 0x2F, 0xFD])
        || (head.len() >= 4 && head[0] & 0xF0 == 0x50 && head[1..4] == [0x2A, 0x4D, 0x18])
    {
        Format::Zstd
    } else if starts(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        Format::SevenZip
    } else if starts(b"Rar!\x1A\x07") {
        Format::Rar
    } else if starts(b"BZh") {
        Format::Bzip2
    } else if tar::looks_like_header(head) {
        Format::Tar
    } else {
        Format::Unknown
    }
}

/// Resource limits of one [`open_archive`] (L-D03).
#[derive(Clone, Debug)]
pub struct Limits {
    /// Archive layers inside the file (a `.tar.gz` is 2).
    pub max_depth: usize,
    /// Bytes decoded in total (everything spooled or skipped).
    pub max_total: u64,
    /// Bytes one entry or stream may decode to.
    pub max_entry: u64,
    /// Decoded : compressed ratio a stream may reach once past
    /// `ratio_floor` decoded bytes (a decompression bomb, not a song).
    pub max_ratio: u64,
    pub ratio_floor: u64,
    /// Entries listed by all archives together.
    pub max_entries: u64,
    /// MIDI files kept.
    pub max_candidates: usize,
    /// Wall-clock budget.
    pub time: Duration,
    /// Bytes to leave free on the spool's volume.
    pub disk_reserve: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 4,
            max_total: 24 << 30,
            max_entry: 16 << 30,
            max_ratio: 5_000,
            ratio_floor: 512 << 20,
            max_entries: 200_000,
            max_candidates: 10_000,
            time: Duration::from_secs(30 * 60),
            disk_reserve: 512 << 20,
        }
    }
}

/// Why an archive could not be opened (user sentences).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    Cancelled,
    /// A limit was reached: which one, in words.
    Limit(String),
    /// Damaged or not what it claims to be.
    Malformed(String),
    /// Reading the file or writing the spool failed.
    Io(String),
}

impl ArchiveError {
    pub fn user_message(&self) -> String {
        match self {
            ArchiveError::Cancelled => "Opening the archive was canceled.".into(),
            ArchiveError::Limit(m) | ArchiveError::Malformed(m) | ArchiveError::Io(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// An io error that carries an [`ArchiveError`] through decoders.
fn wrap(e: ArchiveError) -> io::Error {
    io::Error::other(Wrapped(e))
}

#[derive(Debug)]
struct Wrapped(ArchiveError);

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Wrapped {}

/// An io error from a reader / decoder as an [`ArchiveError`]: ours as it
/// was, damaged data as `Malformed` with `what`, the rest as `Io`.
fn unwrap_io(e: io::Error, what: &str) -> ArchiveError {
    if let Some(w) = e.get_ref().and_then(|x| x.downcast_ref::<Wrapped>()) {
        return w.0.clone();
    }
    // The operating system failed (disk, permissions): an io error. A
    // decoder that stops on its data (any other kind): damaged content.
    if e.raw_os_error().is_some()
        || matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::OutOfMemory
        )
    {
        return ArchiveError::Io(format!(
            "{what} could not be read: {}",
            crate::audio::engine::sentence(&e)
        ));
    }
    ArchiveError::Malformed(format!(
        "{what} is damaged or incomplete ({}).",
        crate::audio::engine::sentence(&e).trim_end_matches('.')
    ))
}

/// Shared budget of one [`open_archive`]: totals, deadline, cancellation.
pub struct Budget<'a> {
    limits: Limits,
    cancel: &'a AtomicBool,
    deadline: Instant,
    total: AtomicU64,
    entries: AtomicU64,
}

impl<'a> Budget<'a> {
    pub fn new(limits: Limits, cancel: &'a AtomicBool) -> Budget<'a> {
        Budget {
            deadline: Instant::now() + limits.time,
            limits,
            cancel,
            total: AtomicU64::new(0),
            entries: AtomicU64::new(0),
        }
    }

    fn check(&self) -> Result<(), ArchiveError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(ArchiveError::Cancelled);
        }
        if Instant::now() > self.deadline {
            return Err(ArchiveError::Limit(format!(
                "Opening the archive took longer than the {} minute limit.",
                self.limits.time.as_secs() / 60
            )));
        }
        Ok(())
    }

    /// One more entry listed by an archive.
    fn entry(&self) -> Result<(), ArchiveError> {
        let n = self.entries.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.limits.max_entries {
            return Err(ArchiveError::Limit(format!(
                "The archive lists more than {} entries; it is not opened.",
                self.limits.max_entries
            )));
        }
        Ok(())
    }
}

/// The bounded reader every decoded byte passes through: per-entry and total
/// output, the expansion ratio against `compressed()`, cancellation and the
/// deadline (checked every 1 MiB).
pub struct Meter<'b, 'a, R> {
    inner: R,
    budget: &'b Budget<'a>,
    what: String,
    pub out: u64,
    since_check: u64,
    /// Compressed bytes read so far (`None`: not a compressed stream).
    compressed: Option<Box<dyn Fn() -> u64 + 'b>>,
}

impl<'b, 'a, R: Read> Meter<'b, 'a, R> {
    fn new(inner: R, budget: &'b Budget<'a>, what: &str) -> Self {
        Meter {
            inner,
            budget,
            what: what.to_owned(),
            out: 0,
            since_check: 0,
            compressed: None,
        }
    }

    fn limit(&self, n: u64) -> Result<(), ArchiveError> {
        let l = &self.budget.limits;
        if self.out + n > l.max_entry {
            return Err(ArchiveError::Limit(format!(
                "{} decodes to more than {} — larger than PlumSpirit opens from an archive.",
                self.what,
                bytes(l.max_entry)
            )));
        }
        let total = self.budget.total.fetch_add(n, Ordering::Relaxed) + n;
        if total > l.max_total {
            return Err(ArchiveError::Limit(format!(
                "The archive decodes to more than {} in total; it is not opened.",
                bytes(l.max_total)
            )));
        }
        if let Some(c) = &self.compressed {
            let out = self.out + n;
            if out > l.ratio_floor && out / c().max(1) > l.max_ratio {
                return Err(ArchiveError::Limit(format!(
                    "{} expands more than {}:1 (a decompression bomb); it is not opened.",
                    self.what, l.max_ratio
                )));
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for Meter<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.since_check >= 1 << 20 {
            self.since_check = 0;
            self.budget.check().map_err(wrap)?;
        }
        let n = self.inner.read(buf)?;
        self.limit(n as u64).map_err(wrap)?;
        self.out += n as u64;
        self.since_check += n as u64;
        Ok(n)
    }
}

/// Human byte size for messages.
fn bytes(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{} GB", n >> 30)
    } else {
        format!("{} MB", n >> 20)
    }
}

/// A private temporary folder for decoded files; removed (with everything
/// in it) when dropped.
pub struct Spool {
    dir: PathBuf,
    next: u32,
    reserve: u64,
}

static SPOOL_SEQ: AtomicU64 = AtomicU64::new(0);

impl Spool {
    /// A new spool under `root` (the system temporary folder in the app).
    pub fn new(root: &Path, reserve: u64) -> Result<Spool, ArchiveError> {
        let n = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!(
            "plumspirit-archive-{}-{n}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos() ^ u128::from(n)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| {
            ArchiveError::Io(format!(
                "A temporary folder for the archive could not be made: {}",
                crate::audio::engine::sentence(&e)
            ))
        })?;
        Ok(Spool {
            dir,
            next: 0,
            reserve,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Copies `head` then the rest of `r` into a new spool file (64 KiB at a
    /// time; free space checked every 64 MiB).
    fn write(
        &mut self,
        head: &[u8],
        r: &mut dyn Read,
        what: &str,
    ) -> Result<(PathBuf, u64), ArchiveError> {
        self.next += 1;
        let path = self.dir.join(format!("{:05}.bin", self.next));
        let fail = |e: io::Error| {
            ArchiveError::Io(format!(
                "{what} could not be written to the temporary folder: {}",
                crate::audio::engine::sentence(&e)
            ))
        };
        self.room(0)?;
        let mut f = io::BufWriter::with_capacity(1 << 16, File::create(&path).map_err(fail)?);
        f.write_all(head).map_err(fail)?;
        let mut size = head.len() as u64;
        let mut buf = vec![0u8; 1 << 16];
        let mut since = 0u64;
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(unwrap_io(e, what)),
            };
            f.write_all(&buf[..n]).map_err(fail)?;
            size += n as u64;
            since += n as u64;
            if since >= 64 << 20 {
                since = 0;
                self.room(0)?;
            }
        }
        f.flush().map_err(fail)?;
        Ok((path, size))
    }

    fn room(&self, need: u64) -> Result<(), ArchiveError> {
        if let Some(free) = crate::platform::free_disk_bytes(&self.dir)
            && free < need + self.reserve
        {
            return Err(ArchiveError::Limit(format!(
                "There is not enough free disk space to unpack the archive ({} free).",
                bytes(free)
            )));
        }
        Ok(())
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            log::warn!(
                "temporary archive folder {} not removed: {e}",
                self.dir.display()
            );
        }
    }
}

/// A MIDI file found in an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Where it was: the archive's name, then each entry (`›` joined for
    /// display by [`Candidate::logical_path`]).
    pub chain: Vec<String>,
    /// The decoded file in the spool.
    pub file: PathBuf,
    pub size: u64,
}

impl Candidate {
    pub fn logical_path(&self) -> String {
        self.chain.join(" › ")
    }
}

/// What an archive held.
pub struct Opened {
    pub candidates: Vec<Candidate>,
    /// Entries that were skipped and why (encrypted, unsupported method,
    /// link, 7z / RAR inside, …), as sentences.
    pub notes: Vec<String>,
    /// Owns the decoded files; drop it when the MIDI has been read.
    pub spool: Spool,
}

/// The display form of an entry name: `/` separators, no drive, UNC or
/// root prefix and no `.` / `..` segments (names are never used as paths).
pub fn display_name(raw: &str) -> String {
    let s = raw.replace('\\', "/");
    let parts: Vec<&str> = s
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .filter(|p| !(p.len() == 2 && p.ends_with(':')))
        .collect();
    let joined = parts.join("/");
    if joined.is_empty() {
        "(unnamed)".into()
    } else {
        joined
    }
}

/// The name a compressed stream's content gets: `a.mid.gz` → `a.mid`,
/// `a.tgz` → `a.tar`.
fn inner_name(name: &str, f: Format) -> String {
    let lower = name.to_ascii_lowercase();
    for (ext, rep) in [
        (".tgz", ".tar"),
        (".txz", ".tar"),
        (".tzst", ".tar"),
        (".gz", ""),
        (".gzip", ""),
        (".xz", ""),
        (".zst", ""),
        (".zstd", ""),
    ] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return format!("{}{rep}", &name[..name.len() - ext.len()]);
        }
    }
    format!("{name} ({} content)", f.name())
}

/// Opens the archive at `path`: every MIDI in it (by content), decoded into
/// a spool under `spool_root`. `Ok` with no candidates when it holds none.
pub fn open_archive(
    path: &Path,
    limits: &Limits,
    cancel: &AtomicBool,
    spool_root: &Path,
) -> Result<Opened, ArchiveError> {
    let budget = Budget::new(limits.clone(), cancel);
    let mut w = Walker {
        budget: &budget,
        spool: Spool::new(spool_root, limits.disk_reserve)?,
        candidates: Vec::new(),
        notes: Vec::new(),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    w.walk_file(path, vec![name], 0)?;
    Ok(Opened {
        candidates: w.candidates,
        notes: w.notes,
        spool: w.spool,
    })
}

struct Walker<'b, 'a> {
    budget: &'b Budget<'a>,
    spool: Spool,
    candidates: Vec<Candidate>,
    notes: Vec<String>,
}

/// Reads up to [`HEAD`] bytes.
fn read_head(r: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut head = vec![0u8; HEAD];
    let mut n = 0;
    while n < HEAD {
        match r.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    head.truncate(n);
    Ok(head)
}

impl Walker<'_, '_> {
    fn open(&self, path: &Path, chain: &[String]) -> Result<File, ArchiveError> {
        File::open(path).map_err(|e| {
            ArchiveError::Io(format!(
                "“{}” could not be opened: {}",
                chain.join(" › "),
                crate::audio::engine::sentence(&e)
            ))
        })
    }

    /// A file on disk (the archive itself or a spooled entry).
    fn walk_file(
        &mut self,
        path: &Path,
        chain: Vec<String>,
        depth: usize,
    ) -> Result<(), ArchiveError> {
        self.budget.check()?;
        let mut f = self.open(path, &chain)?;
        let what = format!("“{}”", chain.join(" › "));
        let head = read_head(&mut f).map_err(|e| unwrap_io(e, &what))?;
        let format = detect(&head);
        if format == Format::Midi {
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            return self.found(chain, path.to_path_buf(), size);
        }
        if !format.is_archive() {
            return Ok(());
        }
        self.deeper(depth, &what)?;
        drop(f);
        match format {
            Format::Zip => {
                let f = self.open(path, &chain)?;
                let mut z = zip::ZipFile::open(f, &what)?;
                let entries = z.entries().to_vec();
                let mut seen: std::collections::HashMap<String, usize> = Default::default();
                for e in entries {
                    self.budget.entry()?;
                    let mut name = display_name(&e.name);
                    let k = seen.entry(name.clone()).or_default();
                    *k += 1;
                    if *k > 1 {
                        name = format!("{name} ({k})");
                    }
                    let mut chain2 = chain.clone();
                    chain2.push(name);
                    let what2 = format!("“{}”", chain2.join(" › "));
                    if let Err(skip) = z.check(&e) {
                        if let Some(note) = skip.note(&what2) {
                            self.notes.push(note);
                        }
                        continue;
                    }
                    let (mut reader, compressed) = z.reader(&e, &what2)?;
                    let mut m = Meter::new(&mut reader, self.budget, &what2);
                    if e.method != 0 {
                        m.compressed = Some(Box::new(move || compressed));
                    }
                    let crc = zip::Crc::new(&mut m, e.crc32, e.size);
                    self.take(crc, chain2, depth, &what2)?;
                }
                Ok(())
            }
            Format::Tar => {
                let f = BufReader::with_capacity(1 << 16, self.open(path, &chain)?);
                let mut m = Meter::new(f, self.budget, &what);
                self.walk_tar(&mut m, chain, depth)
            }
            Format::Gzip | Format::Xz | Format::Zstd => {
                let f = BufReader::with_capacity(1 << 16, self.open(path, &chain)?);
                let inner = inner_name(chain.last().map_or("", String::as_str), format);
                let mut chain2 = chain.clone();
                chain2.push(inner);
                let what2 = format!("“{}”", chain2.join(" › "));
                let counter = std::rc::Rc::new(std::cell::Cell::new(0u64));
                let shared = SharedCount {
                    inner: f,
                    count: std::rc::Rc::clone(&counter),
                };
                let mut dec = stream_decoder(format, shared, &what)?;
                let mut m = Meter::new(&mut *dec, self.budget, &what2);
                let c = std::rc::Rc::clone(&counter);
                m.compressed = Some(Box::new(move || c.get()));
                self.take(&mut m, chain2, depth, &what2)
            }
            Format::SevenZip => self.walk_7z(path, chain, depth),
            Format::Rar => {
                self.notes.push(format!(
                    "{what} is a RAR archive. PlumSpirit cannot open RAR archives (no freely \
                     licensed RAR decoder exists); extract it with 7-Zip or WinRAR first."
                ));
                Ok(())
            }
            Format::Bzip2 => {
                self.notes.push(format!(
                    "{what} is a {} archive, which PlumSpirit cannot open yet.",
                    format.name()
                ));
                Ok(())
            }
            Format::Midi | Format::Unknown => Ok(()),
        }
    }

    /// Entries of a tar stream.
    fn walk_tar(
        &mut self,
        r: &mut dyn Read,
        chain: Vec<String>,
        depth: usize,
    ) -> Result<(), ArchiveError> {
        let what = format!("“{}”", chain.join(" › "));
        let mut t = tar::TarReader::new(r);
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        while let Some(e) = t.next_entry(&what)? {
            self.budget.entry()?;
            let mut name = display_name(&e.name);
            let k = seen.entry(name.clone()).or_default();
            *k += 1;
            if *k > 1 {
                name = format!("{name} ({k})");
            }
            let mut chain2 = chain.clone();
            chain2.push(name);
            let what2 = format!("“{}”", chain2.join(" › "));
            if !e.regular {
                if let Some(kind) = e.skipped_kind {
                    self.notes
                        .push(format!("{what2} is a {kind} and was skipped."));
                }
                t.skip_data(&what)?;
                continue;
            }
            {
                let mut data = t.data();
                self.take(&mut data, chain2, depth, &what2)?;
            }
            t.finish_data(&what)?;
        }
        Ok(())
    }

    /// Decoded content of an entry or a stream: a MIDI is spooled and kept,
    /// a tar is walked as it streams, another archive is spooled, walked and
    /// removed again; anything else is left unread.
    fn take(
        &mut self,
        mut r: impl Read,
        chain: Vec<String>,
        depth: usize,
        what: &str,
    ) -> Result<(), ArchiveError> {
        let head = read_head(&mut r).map_err(|e| unwrap_io(e, what))?;
        match detect(&head) {
            Format::Midi => {
                let (path, size) = self.spool.write(&head, &mut r, what)?;
                self.found(chain, path, size)
            }
            Format::Tar => {
                self.deeper(depth + 1, what)?;
                let mut c = io::Cursor::new(head).chain(r);
                self.walk_tar(&mut c, chain, depth + 1)
            }
            f if f.is_archive() => {
                let (path, _) = self.spool.write(&head, &mut r, what)?;
                let walked = self.walk_file(&path, chain, depth + 1);
                let _ = std::fs::remove_file(&path);
                walked
            }
            _ => Ok(()),
        }
    }

    /// An archive layer at `depth` is allowed.
    fn deeper(&self, depth: usize, what: &str) -> Result<(), ArchiveError> {
        if depth >= self.budget.limits.max_depth {
            return Err(ArchiveError::Limit(format!(
                "{what} is nested more than {} archives deep; it is not opened.",
                self.budget.limits.max_depth
            )));
        }
        Ok(())
    }

    fn found(&mut self, chain: Vec<String>, file: PathBuf, size: u64) -> Result<(), ArchiveError> {
        if self.candidates.len() >= self.budget.limits.max_candidates {
            return Err(ArchiveError::Limit(format!(
                "The archive holds more than {} MIDI files; it is not opened.",
                self.budget.limits.max_candidates
            )));
        }
        self.candidates.push(Candidate { chain, file, size });
        Ok(())
    }
}

/// What the archive chooser shows about a MIDI candidate without importing
/// it (L-D02): the header's format and track count, and the note-ons a quick
/// scan counts (velocity > 0; `None` past [`COUNT_LIMIT`] or when the scan
/// meets something it does not understand — the import is the exact count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmfInfo {
    pub format: Option<u16>,
    pub tracks: Option<u16>,
    pub notes: Option<u64>,
}

/// Files up to this size get their notes counted for the chooser.
pub const COUNT_LIMIT: u64 = 512 << 20;

/// A buffered byte source for the quick scan.
struct Bytes<R> {
    r: R,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    read: u64,
}

impl<R: Read> Bytes<R> {
    fn fill(&mut self) -> io::Result<bool> {
        if self.pos == self.len {
            self.len = loop {
                match self.r.read(&mut self.buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    r => break r?,
                }
            };
            self.pos = 0;
        }
        Ok(self.len > 0)
    }

    fn byte(&mut self) -> io::Result<Option<u8>> {
        if !self.fill()? {
            return Ok(None);
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        self.read += 1;
        Ok(Some(b))
    }

    /// Skips `n` bytes; false at the end of the input.
    fn skip(&mut self, mut n: u64) -> io::Result<bool> {
        while n > 0 {
            if !self.fill()? {
                return Ok(false);
            }
            let k = n.min((self.len - self.pos) as u64) as usize;
            self.pos += k;
            self.read += k as u64;
            n -= k as u64;
        }
        Ok(true)
    }

    fn be(&mut self, n: usize) -> io::Result<Option<u32>> {
        let mut v = 0u32;
        for _ in 0..n {
            match self.byte()? {
                Some(b) => v = (v << 8) | u32::from(b),
                None => return Ok(None),
            }
        }
        Ok(Some(v))
    }

    fn vlq(&mut self) -> io::Result<Option<u32>> {
        let mut v = 0u32;
        for _ in 0..4 {
            let Some(b) = self.byte()? else {
                return Ok(None);
            };
            v = (v << 7) | u32::from(b & 0x7F);
            if b & 0x80 == 0 {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }
}

/// [`SmfInfo`] of the MIDI file at `path` (a plain or RIFF RMID file).
pub fn quick_info(path: &Path, cancel: &AtomicBool) -> SmfInfo {
    let mut info = SmfInfo::default();
    let Ok(f) = File::open(path) else {
        return info;
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    let mut b = Bytes {
        r: f,
        buf: vec![0u8; 1 << 20],
        pos: 0,
        len: 0,
        read: 0,
    };
    let _ = (|| -> io::Result<Option<()>> {
        // The header: at the start, or inside a RIFF RMID `data` chunk.
        let mut tag = [0u8; 4];
        for i in 0..64 {
            let Some(x) = b.byte()? else { return Ok(None) };
            tag = [tag[1], tag[2], tag[3], x];
            if &tag == b"MThd" {
                break;
            }
            if i == 63 {
                return Ok(None);
            }
        }
        let Some(hlen) = b.be(4)? else {
            return Ok(None);
        };
        let (Some(format), Some(tracks), Some(_division)) = (b.be(2)?, b.be(2)?, b.be(2)?) else {
            return Ok(None);
        };
        info.format = Some(format as u16);
        info.tracks = Some(tracks as u16);
        if hlen < 6 || size > COUNT_LIMIT || !b.skip(u64::from(hlen) - 6)? {
            return Ok(None);
        }
        let mut notes = 0u64;
        let mut next_check = 16u64 << 20;
        loop {
            let mut id = [0u8; 4];
            for (k, slot) in id.iter_mut().enumerate() {
                match b.byte()? {
                    Some(x) => *slot = x,
                    // The end of the file between chunks: done.
                    None if k == 0 => {
                        info.notes = Some(notes);
                        return Ok(Some(()));
                    }
                    None => return Ok(None),
                }
            }
            let Some(len) = b.be(4)? else { return Ok(None) };
            let end = b.read + u64::from(len);
            if &id != b"MTrk" {
                if !b.skip(u64::from(len))? {
                    return Ok(None);
                }
                continue;
            }
            let mut running = 0u8;
            while b.read < end {
                if b.read >= next_check {
                    next_check += 16 << 20;
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                }
                if b.vlq()?.is_none() {
                    return Ok(None);
                }
                let Some(mut st) = b.byte()? else {
                    return Ok(None);
                };
                let mut first = None;
                if st < 0x80 {
                    if running == 0 {
                        return Ok(None);
                    }
                    first = Some(st);
                    st = running;
                }
                match st {
                    0x80..=0xEF => {
                        running = st;
                        let d1 = match first {
                            Some(d) => d,
                            None => match b.byte()? {
                                Some(d) => d,
                                None => return Ok(None),
                            },
                        };
                        if !matches!(st & 0xF0, 0xC0 | 0xD0) {
                            let Some(d2) = b.byte()? else { return Ok(None) };
                            if st & 0xF0 == 0x90 && d2 > 0 && d1 < 0x80 {
                                notes += 1;
                            }
                        }
                    }
                    0xFF => {
                        let (Some(_), Some(n)) = (b.byte()?, b.vlq()?) else {
                            return Ok(None);
                        };
                        if !b.skip(u64::from(n))? {
                            return Ok(None);
                        }
                    }
                    0xF0 | 0xF7 => {
                        let Some(n) = b.vlq()? else { return Ok(None) };
                        if !b.skip(u64::from(n))? {
                            return Ok(None);
                        }
                    }
                    _ => return Ok(None),
                }
            }
            if b.read != end {
                return Ok(None);
            }
        }
    })();
    info
}

/// A reader that publishes how much it has read through a shared counter
/// (the decoder owns it; the meter reads the count for the ratio).
struct SharedCount<R> {
    inner: R,
    count: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<R: Read> Read for SharedCount<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

/// LZMA dictionary memory an xz stream may ask for (KiB).
const XZ_MEM_LIMIT_KB: u32 = 256 * 1024;

/// A streaming decoder for a single-stream format (concatenated members /
/// streams / frames included).
fn stream_decoder<'r, R: Read + 'r>(
    f: Format,
    r: R,
    what: &str,
) -> Result<Box<dyn Read + 'r>, ArchiveError> {
    Ok(match f {
        Format::Gzip => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Format::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(
            r,
            true,
            XZ_MEM_LIMIT_KB,
        )),
        Format::Zstd => Box::new(ZstdReader::new(r, what)?),
        _ => unreachable!("not a stream format"),
    })
}

/// Zstandard frames one after another (skippable frames skipped), each
/// within ruzstd's default 128 MiB window limit; checksums verified.
struct ZstdReader<R: Read> {
    dec: Option<
        ruzstd::decoding::StreamingDecoder<io::BufReader<R>, ruzstd::decoding::FrameDecoder>,
    >,
    src: Option<io::BufReader<R>>,
    what: String,
}

impl<R: Read> ZstdReader<R> {
    fn new(r: R, what: &str) -> Result<Self, ArchiveError> {
        let mut z = ZstdReader {
            dec: None,
            src: Some(io::BufReader::with_capacity(1 << 16, r)),
            what: what.to_owned(),
        };
        if !z.next_frame().map_err(|e| unwrap_io(e, what))? {
            return Err(ArchiveError::Malformed(format!(
                "{what} holds no Zstandard frame."
            )));
        }
        Ok(z)
    }

    /// Starts the next frame; false at the end of the input.
    fn next_frame(&mut self) -> io::Result<bool> {
        use std::io::BufRead;
        let mut src = match self.dec.take() {
            Some(d) => d.into_inner(),
            None => self.src.take().expect("source"),
        };
        loop {
            if src.fill_buf()?.is_empty() {
                self.src = Some(src);
                return Ok(false);
            }
            let b = src.fill_buf()?;
            if b.len() >= 4 && b[0] & 0xF0 == 0x50 && b[1..4] == [0x2A, 0x4D, 0x18] {
                // Skippable frame: magic, 4-byte length, payload.
                let mut hdr = [0u8; 8];
                src.read_exact(&mut hdr)?;
                let len = u64::from(u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]));
                io::copy(&mut (&mut src).take(len), &mut io::sink())?;
                continue;
            }
            match ruzstd::decoding::StreamingDecoder::new(src) {
                Ok(d) => {
                    self.dec = Some(d);
                    return Ok(true);
                }
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} is not a valid Zstandard frame: {e}", self.what),
                    ));
                }
            }
        }
    }
}

impl<R: Read> Read for ZstdReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(d) = self.dec.as_mut() else {
                return Ok(0);
            };
            let n = d.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            if let (Some(want), got) = (
                d.decoder.get_checksum_from_data(),
                d.decoder.get_calculated_checksum(),
            ) && Some(want) != got
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: Zstandard checksum mismatch", self.what),
                ));
            }
            if !self.next_frame()? {
                return Ok(0);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
