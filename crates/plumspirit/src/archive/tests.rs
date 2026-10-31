//! L-D01 / L-D03: archives built in memory (own zip / tar writers, flate2,
//! lzma-rust2 and ruzstd encoders), opened through [`open_archive`].

use super::*;
use std::sync::atomic::AtomicBool;

pub(crate) mod sevenz;

/// A valid MIDI file with `n` notes (distinct per seed).
pub(crate) fn midi(seed: u8, n: usize) -> Vec<u8> {
    let mut trk = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20];
    for i in 0..n {
        trk.extend_from_slice(&[0x00, 0x90, (40 + (i + usize::from(seed)) % 60) as u8, 100]);
        trk.extend_from_slice(&[0x60, 0x80, (40 + (i + usize::from(seed)) % 60) as u8, 0]);
    }
    trk.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    let mut b = b"MThd\0\0\0\x06\0\x00\0\x01\x01\xE0MTrk".to_vec();
    b.extend_from_slice(&(trk.len() as u32).to_be_bytes());
    b.extend_from_slice(&trk);
    b
}

#[derive(Clone)]
pub(crate) struct ZEntry {
    name: String,
    data: Vec<u8>,
    method: u16,
    flags: u16,
    symlink: bool,
    bad_crc: bool,
}

pub(crate) fn ze(name: &str, data: &[u8], method: u16) -> ZEntry {
    ZEntry {
        name: name.into(),
        data: data.to_vec(),
        method,
        flags: 0,
        symlink: false,
        bad_crc: false,
    }
}

fn deflate(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

pub(crate) fn zip(entries: &[ZEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for e in entries {
        let body = match e.method {
            8 => deflate(&e.data),
            _ => e.data.clone(),
        };
        let mut crc = crc32fast::hash(&e.data);
        if e.bad_crc {
            crc ^= 1;
        }
        let off = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0]);
        out.extend_from_slice(&e.flags.to_le_bytes());
        out.extend_from_slice(&e.method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&body);
        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&(if e.symlink { 0x031Eu16 } else { 20 }).to_le_bytes());
        cd.extend_from_slice(&[20, 0]);
        cd.extend_from_slice(&e.flags.to_le_bytes());
        cd.extend_from_slice(&e.method.to_le_bytes());
        cd.extend_from_slice(&[0; 4]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(body.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0; 8]);
        let attr: u32 = if e.symlink { 0o120777 << 16 } else { 0 };
        cd.extend_from_slice(&attr.to_le_bytes());
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(e.name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// A ustar archive: (name, type flag, data).
pub(crate) fn tar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, kind, data) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        let size = if matches!(kind, b'1' | b'2' | b'5') {
            0
        } else {
            data.len()
        };
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = *kind;
        if *kind == b'2' {
            h[157..160].copy_from_slice(b"x.m");
        }
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&c| u32::from(c)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        if size > 0 {
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
    }
    out.resize(out.len() + 1024, 0);
    out
}

pub(crate) fn gz(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

pub(crate) fn xz(d: &[u8]) -> Vec<u8> {
    let mut w =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(d).unwrap();
    w.finish().unwrap()
}

pub(crate) fn zst(d: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(d, ruzstd::encoding::CompressionLevel::Fastest)
}

struct Case {
    dir: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Case {
        let dir = std::env::temp_dir().join(format!(
            "ps-archive-test-{tag}-{}-{}",
            std::process::id(),
            SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Case { dir }
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
    fn open(&self, name: &str, bytes: &[u8], limits: &Limits) -> Result<Opened, ArchiveError> {
        let p = self.file(name, bytes);
        open_archive(&p, limits, &AtomicBool::new(false), &self.dir)
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn paths(o: &Opened) -> Vec<String> {
    o.candidates.iter().map(Candidate::logical_path).collect()
}

#[test]
fn every_format_is_recognised_by_its_content() {
    let m = midi(0, 4);
    assert_eq!(detect(&m), Format::Midi);
    assert_eq!(detect(&zip(&[ze("a.mid", &m, 0)])), Format::Zip);
    assert_eq!(detect(&gz(&m)), Format::Gzip);
    assert_eq!(detect(&xz(&m)), Format::Xz);
    assert_eq!(detect(&zst(&m)), Format::Zstd);
    assert_eq!(detect(&tar(&[("a.mid", b'0', &m)])), Format::Tar);
    assert_eq!(
        detect(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4]),
        Format::SevenZip
    );
    assert_eq!(detect(b"Rar!\x1A\x07\x01\x00"), Format::Rar);
    assert_eq!(detect(b"RIFF\0\0\0\0RMIDdata"), Format::Midi);
    assert_eq!(detect(b"hello world"), Format::Unknown);
}

#[test]
fn zip_gz_xz_zst_and_tar_chains_yield_the_exact_midi_bytes() {
    let c = Case::new("chains");
    let (a, b) = (midi(1, 50), midi(2, 70));
    let readme = b"just text".to_vec();
    let t = tar(&[
        ("songs/", b'5', b""),
        ("songs/a.mid", b'0', &a),
        ("songs/readme.txt", b'0', &readme),
        ("songs/b.midi", b'0', &b),
        ("songs/link.mid", b'2', b""),
    ]);
    let l = Limits::default();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "pack.zip",
            zip(&[
                ze("a.mid", &a, 8),
                ze("readme.txt", &readme, 0),
                ze("x/b.mid", &b, 0),
            ]),
        ),
        ("pack.tar", t.clone()),
        ("pack.tar.gz", gz(&t)),
        ("pack.tar.xz", xz(&t)),
        ("pack.tar.zst", zst(&t)),
        ("pack.tgz", gz(&t)),
    ];
    for (name, bytes) in cases {
        let o = c.open(name, &bytes, &l).unwrap();
        assert_eq!(o.candidates.len(), 2, "{name}: {:?}", paths(&o));
        let got: Vec<Vec<u8>> = o
            .candidates
            .iter()
            .map(|k| std::fs::read(&k.file).unwrap())
            .collect();
        assert_eq!(
            got,
            [a.clone(), b.clone()],
            "{name}: the bytes of each MIDI"
        );
        assert!(
            o.candidates
                .iter()
                .all(|k| k.file.starts_with(o.spool.dir()))
        );
        assert!(o.candidates.iter().all(|k| {
            plumspirit_engine::midi::import_bytes(&std::fs::read(&k.file).unwrap()).is_ok()
        }));
        if name.starts_with("pack.tar") || name == "pack.tgz" {
            assert!(
                o.notes
                    .iter()
                    .any(|n| n.contains("link.mid") && n.contains("symbolic link")),
                "{name}: {:?}",
                o.notes
            );
        }
    }
    let o = c.open("pack.tar.gz", &gz(&t), &l).unwrap();
    assert_eq!(
        paths(&o),
        [
            "pack.tar.gz › pack.tar › songs/a.mid",
            "pack.tar.gz › pack.tar › songs/b.midi"
        ]
    );
    // A single compressed MIDI is named after the file.
    let o = c.open("song.mid.xz", &xz(&a), &l).unwrap();
    assert_eq!(paths(&o), ["song.mid.xz › song.mid"]);
    assert_eq!(o.candidates[0].size, a.len() as u64);
}

#[test]
fn nested_archives_and_concatenated_streams_are_followed() {
    let c = Case::new("nested");
    let a = midi(3, 20);
    let inner = zip(&[ze("deep/a.mid", &a, 8)]);
    let middle = tar(&[("inner.zip", b'0', &inner)]);
    let outer = zip(&[ze("middle.tar.gz", &gz(&middle), 0)]);
    let o = c.open("outer.zip", &outer, &Limits::default()).unwrap();
    assert_eq!(
        paths(&o),
        ["outer.zip › middle.tar.gz › middle.tar › inner.zip › deep/a.mid"]
    );
    assert_eq!(std::fs::read(&o.candidates[0].file).unwrap(), a);
    // Concatenated gzip members / zstd frames with a skippable frame.
    let mut two = gz(&a[..40]);
    two.extend_from_slice(&gz(&a[40..]));
    let o = c.open("a.mid.gz", &two, &Limits::default()).unwrap();
    assert_eq!(std::fs::read(&o.candidates[0].file).unwrap(), a);
    let mut frames = zst(&a[..30]);
    frames.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 9, 9, 9]);
    frames.extend_from_slice(&zst(&a[30..]));
    let o = c.open("a.mid.zst", &frames, &Limits::default()).unwrap();
    assert_eq!(std::fs::read(&o.candidates[0].file).unwrap(), a);
}

#[test]
fn hostile_names_links_encryption_and_methods_are_contained_and_reported() {
    let c = Case::new("hostile");
    let a = midi(4, 10);
    let mut enc = ze("secret.mid", &a, 0);
    enc.flags = 1;
    let mut link = ze("link.mid", b"/etc/passwd", 0);
    link.symlink = true;
    let mut bz = ze("old.mid", &a, 12);
    bz.data = a.clone();
    let archive = zip(&[
        ze("../../evil.mid", &a, 0),
        ze("C:\\Windows\\b.mid", &a, 0),
        ze("/abs/c.mid", &a, 0),
        ze("\\\\server\\share\\d.mid", &a, 0),
        ze("dup.mid", &a, 0),
        ze("dup.mid", &a, 0),
        enc,
        link,
        bz,
        ze("inner.rar", b"Rar!\x1A\x07\x01\x00rest", 0),
    ]);
    let o = c.open("bad.zip", &archive, &Limits::default()).unwrap();
    assert_eq!(
        paths(&o),
        [
            "bad.zip › evil.mid",
            "bad.zip › Windows/b.mid",
            "bad.zip › abs/c.mid",
            "bad.zip › server/share/d.mid",
            "bad.zip › dup.mid",
            "bad.zip › dup.mid (2)",
        ]
    );
    for k in &o.candidates {
        assert!(
            k.file.starts_with(o.spool.dir()),
            "spooled inside the spool only"
        );
    }
    let notes = o.notes.join("\n");
    assert!(notes.contains("secret.mid” is encrypted"), "{notes}");
    assert!(notes.contains("link.mid” is a link"), "{notes}");
    assert!(
        notes.contains("old.mid” uses a compression method PlumSpirit cannot read (bzip2)"),
        "{notes}"
    );
    assert!(
        notes.contains("inner.rar” is a RAR archive. PlumSpirit cannot open RAR archives")
            && notes.contains("extract it with 7-Zip or WinRAR first"),
        "{notes}"
    );
    // Nothing escaped next to the archive.
    let names: Vec<String> = std::fs::read_dir(&c.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n.ends_with(".mid")), "{names:?}");
}

#[test]
fn damaged_archives_are_refused_with_a_reason() {
    let c = Case::new("damaged");
    let a = midi(5, 30);
    let mut bad = ze("a.mid", &a, 8);
    bad.bad_crc = true;
    let e = c
        .open("crc.zip", &zip(&[bad]), &Limits::default())
        .err()
        .unwrap();
    assert!(
        matches!(e, ArchiveError::Malformed(ref m) if m.contains("a.mid") && m.contains("CRC")),
        "{e:?}"
    );
    let g = gz(&a);
    let e = c
        .open("cut.mid.gz", &g[..g.len() - 9], &Limits::default())
        .err()
        .unwrap();
    assert!(matches!(e, ArchiveError::Malformed(_)), "{e:?}");
    let x = xz(&a);
    let mut flipped = x.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x55;
    let e = c
        .open("flip.mid.xz", &flipped, &Limits::default())
        .err()
        .unwrap();
    assert!(matches!(e, ArchiveError::Malformed(_)), "{e:?}");
    let z = zip(&[ze("a.mid", &a, 0)]);
    let e = c
        .open("cut.zip", &z[..z.len() - 30], &Limits::default())
        .err()
        .unwrap();
    assert!(
        matches!(e, ArchiveError::Malformed(ref m) if m.contains("ZIP")),
        "{e:?}"
    );
    let t = tar(&[("a.mid", b'0', &a)]);
    let mut tb = t.clone();
    tb[10] ^= 1; // name byte: checksum no longer matches
    let e = c.open("bad.tar.gz", &gz(&tb), &Limits::default()).err();
    assert!(
        e.is_none() || matches!(e, Some(ArchiveError::Malformed(_))),
        "{e:?}"
    );
    let e = c
        .open("cut.tar.gz", &gz(&t[..700]), &Limits::default())
        .err()
        .unwrap();
    assert!(matches!(e, ArchiveError::Malformed(_)), "{e:?}");
}

#[test]
fn limits_stop_bombs_depth_size_entries_and_cancellation() {
    let c = Case::new("limits");
    let a = midi(6, 10);
    // Depth: a MIDI gzipped six times.
    let mut deep = a.clone();
    for _ in 0..6 {
        deep = gz(&deep);
    }
    let e = c.open("deep.gz", &deep, &Limits::default()).err().unwrap();
    assert!(
        matches!(e, ArchiveError::Limit(ref m) if m.contains("nested")),
        "{e:?}"
    );
    // Ratio: 8 MiB of zeros in a few KiB.
    let zeros = vec![0u8; 8 << 20];
    // Content that looks like a MIDI is decoded to the end (anything else
    // is left unread after its first bytes).
    let mut bomb = midi(0, 1);
    bomb.extend_from_slice(&zeros);
    let tight = Limits {
        ratio_floor: 1 << 20,
        max_ratio: 100,
        ..Limits::default()
    };
    let e = c.open("bomb.gz", &gz(&bomb), &tight).err().unwrap();
    assert!(
        matches!(e, ArchiveError::Limit(ref m) if m.contains("bomb")),
        "{e:?}"
    );
    // Size per entry and total.
    let small = Limits {
        max_entry: 1 << 20,
        ..Limits::default()
    };
    let e = c.open("big.gz", &gz(&bomb), &small).err().unwrap();
    assert!(
        matches!(e, ArchiveError::Limit(ref m) if m.contains("decodes to more than")),
        "{e:?}"
    );
    let total = Limits {
        max_total: 1 << 20,
        ..Limits::default()
    };
    let e = c.open(
        "big.zip",
        &zip(&[
            ze("z1.bin", &zeros[..900_000], 8),
            ze("z2.bin", &zeros[..900_000], 8),
        ]),
        &total,
    );
    // Unknown entries are not decoded beyond their head: no total reached.
    assert!(e.is_ok());
    let e = c
        .open(
            "big.tar.gz",
            &gz(&tar(&[
                ("z1.bin", b'0', &zeros[..900_000]),
                ("z2.bin", b'0', &zeros[..900_000]),
            ])),
            &total,
        )
        .err()
        .unwrap();
    assert!(
        matches!(e, ArchiveError::Limit(ref m) if m.contains("in total")),
        "{e:?}"
    );
    // Entry count.
    let many: Vec<ZEntry> = (0..50).map(|i| ze(&format!("f{i}.txt"), b"x", 0)).collect();
    let few = Limits {
        max_entries: 20,
        ..Limits::default()
    };
    let e = c.open("many.zip", &zip(&many), &few).err().unwrap();
    assert!(
        matches!(e, ArchiveError::Limit(ref m) if m.contains("entries")),
        "{e:?}"
    );
    // Cancellation before anything is decoded.
    let p = c.file("a.tar.xz", &xz(&tar(&[("a.mid", b'0', &a)])));
    let cancel = AtomicBool::new(true);
    let e = open_archive(&p, &Limits::default(), &cancel, &c.dir)
        .err()
        .unwrap();
    assert_eq!(e, ArchiveError::Cancelled);
}

#[test]
fn the_spool_is_removed_with_everything_in_it() {
    let c = Case::new("spool");
    let a = midi(7, 10);
    let o = c
        .open(
            "a.zip",
            &zip(&[ze("a.mid", &a, 8), ze("b.mid", &a, 0)]),
            &Limits::default(),
        )
        .unwrap();
    let dir = o.spool.dir().to_path_buf();
    let files: Vec<PathBuf> = o.candidates.iter().map(|k| k.file.clone()).collect();
    assert!(files.iter().all(|f| f.exists()));
    drop(o);
    assert!(!dir.exists(), "spool removed");
    assert!(files.iter().all(|f| !f.exists()));
    // A failed open removes its spool too.
    let before: usize = std::fs::read_dir(&c.dir).unwrap().count();
    let mut bad = ze("a.mid", &a, 8);
    bad.bad_crc = true;
    assert!(c.open("crc.zip", &zip(&[bad]), &Limits::default()).is_err());
    let after: usize = std::fs::read_dir(&c.dir).unwrap().count();
    assert_eq!(after, before + 1, "only the archive itself was added");
}

#[test]
fn display_names_drop_roots_drives_and_dot_segments() {
    assert_eq!(display_name("../../a/./b.mid"), "a/b.mid");
    assert_eq!(display_name("C:\\x\\y.mid"), "x/y.mid");
    assert_eq!(display_name("\\\\srv\\share\\z.mid"), "srv/share/z.mid");
    assert_eq!(display_name("/abs/q.mid"), "abs/q.mid");
    assert_eq!(display_name("../.."), "(unnamed)");
}

#[test]
fn quick_info_reads_the_header_and_counts_note_ons() {
    let c = Case::new("info");
    let never = AtomicBool::new(false);
    let a = midi(3, 1234);
    let info = quick_info(&c.file("a.mid", &a), &never);
    assert_eq!(
        info,
        SmfInfo {
            format: Some(0),
            tracks: Some(1),
            notes: Some(1234)
        }
    );
    // Format 1, two tracks, running status, velocity-0 note-offs, a
    // program change, sysex, an unknown chunk, inside RIFF RMID.
    let t1 = [0, 0xFF, 0x03, 2, b'h', b'i', 0, 0xFF, 0x2F, 0].to_vec();
    let t2 = [
        0x00, 0xC0, 5, // program change: one data byte
        0x00, 0x90, 60, 100, // note on
        0x10, 62, 90, // running status: note on
        0x10, 60, 0, // running status: velocity 0 = note off
        0x00, 0xF0, 2, 0x7E, 0xF7, // sysex
        0x10, 0x91, 64, 1, // note on, channel 2
        0x00, 0xFF, 0x2F, 0x00,
    ]
    .to_vec();
    let mut smf = b"MThd\0\0\0\x06\0\x01\0\x02\x01\xE0".to_vec();
    for (tag, body) in [(b"MTrk", &t1), (b"XYZW", &vec![1u8, 2, 3]), (b"MTrk", &t2)] {
        smf.extend_from_slice(tag);
        smf.extend_from_slice(&(body.len() as u32).to_be_bytes());
        smf.extend_from_slice(body);
    }
    let mut riff = b"RIFF".to_vec();
    riff.extend_from_slice(&((smf.len() + 12) as u32).to_le_bytes());
    riff.extend_from_slice(b"RMIDdata");
    riff.extend_from_slice(&(smf.len() as u32).to_le_bytes());
    riff.extend_from_slice(&smf);
    for (name, bytes) in [("b.mid", &smf), ("b.rmi", &riff)] {
        let info = quick_info(&c.file(name, bytes), &never);
        assert_eq!(
            info,
            SmfInfo {
                format: Some(1),
                tracks: Some(2),
                notes: Some(3)
            },
            "{name}"
        );
    }
    // A cut file or a chunk length that lies: the header still shows, the
    // notes are left to the import.
    let cut = quick_info(&c.file("cut.mid", &a[..a.len() - 7]), &never);
    assert_eq!(
        (cut.format, cut.tracks, cut.notes),
        (Some(0), Some(1), None)
    );
    let mut lying = a.clone();
    lying[21] = lying[21].wrapping_add(1); // MTrk length + 1
    assert_eq!(quick_info(&c.file("lie.mid", &lying), &never).notes, None);
    assert_eq!(
        quick_info(&c.file("text.mid", b"not a midi"), &never),
        SmfInfo::default()
    );
}

/// L-D04: the MIDI opened from every archive type is the document the
/// direct file opens to.
#[test]
fn every_archive_type_opens_to_the_document_of_the_direct_file() {
    payload_digests_match(&midi(9, 3000));
}

/// L-D04 with a 1M-note file (release: `cargo test --release -p plumspirit
/// --offline archive_payload_with_a_million_notes -- --ignored`).
#[test]
#[ignore = "heavy: 1M notes through every archive type"]
fn archive_payload_with_a_million_notes() {
    payload_digests_match(&midi(1, 1_000_000));
}

fn payload_digests_match(m: &[u8]) {
    use plumspirit_engine::midi::{ImportControl, import_file};
    let c = Case::new("digest");
    let direct = c.file("direct.mid", m);
    let (want, _) = import_file(&direct, ImportControl::default()).unwrap();
    let want = want.fingerprint();
    let t = tar(&[("dir/song.mid", b'0', m)]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("stored.zip", zip(&[ze("song.mid", m, 0)])),
        ("deflate.zip", zip(&[ze("song.mid", m, 8)])),
        ("song.mid.gz", gz(m)),
        ("song.mid.xz", xz(m)),
        ("song.mid.zst", zst(m)),
        ("song.tar", t.clone()),
        ("song.tar.gz", gz(&t)),
        ("song.tar.xz", xz(&t)),
        ("song.tar.zst", zst(&t)),
        ("solid.7z", sevenz::sz_enc(&[("song.mid", m)], &[], true)),
        (
            "nested.7z",
            sevenz::sz_enc(&[("in.zip", &zip(&[ze("song.mid", m, 8)]))], &[], false),
        ),
    ];
    for (name, bytes) in cases {
        let o = c.open(name, &bytes, &Limits::default()).unwrap();
        assert_eq!(o.candidates.len(), 1, "{name}");
        let (doc, _) = import_file(&o.candidates[0].file, ImportControl::default()).unwrap();
        assert_eq!(doc.fingerprint(), want, "{name}");
    }
}
