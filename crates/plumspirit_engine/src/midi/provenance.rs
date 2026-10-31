//! Where an imported project came from (v1.0-beta1 B1-0121 / B1-0124, strict
//! no-edit export).
//!
//! Importing a `.mid` file records [`SourceProvenance`]: the file's path,
//! size and a 64-bit content hash read while it was imported, and the
//! document's [`PlumDocument::export_fingerprint`] right after the import.
//! While the fingerprint is unchanged (no MIDI edit, or every edit undone)
//! and the file on disk still has that size and hash, the project still *is*
//! that file, and strict export can rewrite the file's own tracks instead of
//! re-deriving them from the editable model (B1-S10). Nothing of the file is
//! kept in memory: the check re-reads it.
//!
//! The hash is for detecting change, not for security: a word-wise
//! multiply–rotate chain with a SplitMix64 finish (64 bits; an accidental
//! edit goes unnoticed with probability about 2^-64).
//!
//! [`PlumDocument::export_fingerprint`]: crate::model::PlumDocument::export_fingerprint

use crate::model::PlumDocument;
use crate::model::project::SourceProvenance;
use std::io::{self, Read};
use std::path::Path;

pub use crate::codec::WordHash;

/// A reader that hashes everything read through it.
pub struct HashingReader<R> {
    inner: R,
    hash: WordHash,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        HashingReader {
            inner,
            hash: WordHash::new(),
        }
    }

    /// Reads (and hashes) whatever is left, then returns `(bytes, hash)` of
    /// everything read through this reader.
    pub fn finish(mut self) -> io::Result<(u64, u64)> {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match self.inner.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.hash.bytes(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok((self.hash.byte_count(), self.hash.finish()))
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.bytes(&buf[..n]);
        Ok(n)
    }
}

/// `(size, hash)` of a file, read once in 1 MiB blocks.
pub fn file_digest(path: &Path) -> io::Result<(u64, u64)> {
    let file = std::fs::File::open(path)?;
    HashingReader::new(io::BufReader::with_capacity(1 << 20, file)).finish()
}

/// Whether the project still is the file it was imported from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceStatus {
    /// Unchanged project and file: strict export can rewrite the file.
    Unchanged,
    /// The project was not imported from a MIDI file (or the source is not
    /// known).
    NotImported,
    /// The project has MIDI edits since the import (undo them to get strict
    /// export back).
    Edited,
    /// The source file can no longer be read.
    SourceMissing(String),
    /// The source file changed since the import.
    SourceChanged,
}

impl SourceStatus {
    /// One sentence for the export dialog.
    pub fn sentence(&self) -> String {
        match self {
            SourceStatus::Unchanged => {
                "The project has no MIDI edits since it was imported, so its tracks are written exactly as in the original file.".into()
            }
            SourceStatus::NotImported => {
                "The project was not imported from a MIDI file, so it is written from its contents.".into()
            }
            SourceStatus::Edited => {
                "The project has MIDI edits since it was imported, so it is written from its contents (undo every edit to write the original tracks exactly).".into()
            }
            SourceStatus::SourceMissing(e) => format!(
                "The original MIDI file cannot be read ({e}), so the project is written from its contents."
            ),
            SourceStatus::SourceChanged => {
                "The original MIDI file has changed since the import, so the project is written from its contents.".into()
            }
        }
    }
}

/// Compares the document with its recorded source: the fingerprint first
/// (no I/O when there are edits), then the file's size and hash.
pub fn source_status(doc: &PlumDocument) -> SourceStatus {
    let Some(p) = doc.project().source.as_ref() else {
        return SourceStatus::NotImported;
    };
    if p.path.is_empty() {
        return SourceStatus::NotImported;
    }
    if doc.export_fingerprint() != p.fingerprint {
        return SourceStatus::Edited;
    }
    match file_digest(Path::new(&p.path)) {
        Err(e) => SourceStatus::SourceMissing(e.to_string()),
        Ok((size, hash)) if size == p.size && hash == p.hash => SourceStatus::Unchanged,
        Ok(_) => SourceStatus::SourceChanged,
    }
}

/// [`source_status`] without reading the file — the quick check before an
/// export (B1-S10): [`SourceStatus::Unchanged`] here means "strict export
/// can be tried"; the strict writer then checks the file's size and hash
/// while it streams it.
pub fn strict_candidate(doc: &PlumDocument) -> SourceStatus {
    let Some(p) = doc.project().source.as_ref() else {
        return SourceStatus::NotImported;
    };
    if p.path.is_empty() {
        return SourceStatus::NotImported;
    }
    if doc.export_fingerprint() != p.fingerprint {
        return SourceStatus::Edited;
    }
    match std::fs::metadata(&p.path) {
        Err(e) => SourceStatus::SourceMissing(e.to_string()),
        Ok(m) if !m.is_file() || m.len() != p.size => SourceStatus::SourceChanged,
        Ok(_) => SourceStatus::Unchanged,
    }
}

/// The provenance of a document just imported from `bytes` bytes hashing to
/// `hash` (path filled in by file imports).
pub fn record(doc: &PlumDocument, path: &str, size: u64, hash: u64) -> SourceProvenance {
    SourceProvenance {
        path: path.to_owned(),
        size,
        hash,
        fingerprint: doc.export_fingerprint(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_hashing_ignores_how_the_bytes_are_split() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut whole = WordHash::new();
        whole.bytes(&data);
        for split in [1usize, 3, 7, 8, 9, 64, 999] {
            let mut h = WordHash::new();
            for part in data.chunks(split) {
                h.bytes(part);
            }
            assert_eq!(h.finish(), whole.finish(), "split {split}");
        }
        assert_eq!(whole.byte_count(), 1000);
    }

    #[test]
    fn any_changed_byte_or_length_changes_the_hash() {
        let data: Vec<u8> = (0..257u32).map(|i| i as u8).collect();
        let base = {
            let mut h = WordHash::new();
            h.bytes(&data);
            h.finish()
        };
        for i in 0..data.len() {
            let mut d = data.clone();
            d[i] ^= 1;
            let mut h = WordHash::new();
            h.bytes(&d);
            assert_ne!(h.finish(), base, "byte {i}");
        }
        let mut h = WordHash::new();
        h.bytes(&data[..256]);
        assert_ne!(h.finish(), base, "one byte shorter");
        let mut longer = data.clone();
        longer.push(0);
        let mut h = WordHash::new();
        h.bytes(&longer);
        assert_ne!(h.finish(), base, "a trailing zero byte");
    }

    #[test]
    fn words_and_pairs_hash_like_single_words_at_any_offset() {
        let ws: Vec<u64> = (0..41u64)
            .map(|i| i.wrapping_mul(0x9E37_79B9) ^ 7)
            .collect();
        for skip in 0..3 {
            let mut one = WordHash::new();
            let mut run = WordHash::new();
            let mut pairs = WordHash::new();
            for &w in &ws[..skip] {
                one.word(w);
                run.word(w);
                pairs.word(w);
            }
            for &w in &ws[skip..] {
                one.word(w);
            }
            run.words(&ws[skip..]);
            let rest = &ws[skip..];
            for p in rest.chunks(2) {
                match p {
                    [a, b] => pairs.pair(*a, *b),
                    [a] => pairs.word(*a),
                    _ => unreachable!(),
                }
            }
            assert_eq!(run.finish(), one.finish(), "words after {skip}");
            assert_eq!(pairs.finish(), one.finish(), "pairs after {skip}");
        }
        // Swapping two words (even across lanes) changes the hash.
        let mut a = WordHash::new();
        a.words(&[1, 2, 3]);
        let mut b = WordHash::new();
        b.words(&[2, 1, 3]);
        assert_ne!(a.finish(), b.finish());
        let mut c = WordHash::new();
        c.words(&[3, 2, 1]);
        assert_ne!(a.finish(), c.finish());
    }

    #[test]
    fn fields_are_length_delimited() {
        let hash = |parts: &[&[u8]]| {
            let mut h = WordHash::new();
            for p in parts {
                h.field(p);
            }
            h.finish()
        };
        assert_ne!(hash(&[b"ab", b"c"]), hash(&[b"a", b"bc"]));
        assert_ne!(hash(&[b"", b"x"]), hash(&[b"x", b""]));
    }

    #[test]
    fn the_hashing_reader_hashes_the_whole_stream() {
        let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let mut r = HashingReader::new(&data[..]);
        let mut first = [0u8; 100];
        r.read_exact(&mut first).unwrap();
        let (n, h) = r.finish().unwrap();
        let mut whole = WordHash::new();
        whole.bytes(&data);
        assert_eq!((n, h), (70_000, whole.finish()));
    }
}
