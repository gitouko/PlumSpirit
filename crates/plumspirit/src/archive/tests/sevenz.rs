//! L-D01 (L-S10): 7z archives — made by 7-Zip 23.01 (embedded below), by the
//! `sevenz-rust2` encoder, and by hand for hostile headers.

use super::*;
use crate::archive::sevenz as sz;

/// 7-Zip 23.01 `7z a -mx=5` (solid LZMA2, LZMA-compressed file list):
/// songs/a.mid = midi(1, 40), songs/b.mid = midi(2, 60), readme.txt,
/// c.mid.gz = gzip(midi(3, 20)), the empty folder empty/.
const REAL_SOLID: &[&str] = &[
    "N3q8ryccAATLLsRPBgIAAAAAAAAjAAAAAAAAAKZ8nsfgA/wBYF0AD6Om+A0tKE4XDXcUAeUXfvu4o5RMrhZHysebwW0CuJEb",
    "7EW1iahMcu72+IV4Vz7Hc9+1fBo9VXOsEfyQIppI++U9zpssZbB1bCDNXp6txuqeWlxj6qw+O+6xQVv1XQwjfGn5JYNs/tXw",
    "07uASKnYRsYlRsO2G3pyRjHK3h7QwvAZHz8wZakLReMAy+lgfL/GGYzOtkAju6ALV+ix+43S7OiDT7X254C93uDiiGrFQMwQ",
    "E4HPxXd9kDVJrrZMzoRDeKr8P1PmLJTslIqXUq5y9aLpOBDPctAXZdwVncwXJA0opUZnvC6upRmG8tYqBLvMpsLqkm07o4qG",
    "oiTmqksrPIuYLsbOcvkscHuoKflYeDmbuA7h7mPwelV3G00JeZBXrmRcI/9WPq2MsfxdkuHGCvK4vp/7mTWaf/81sq0aLSUz",
    "oT2Kh1I58AUkPy5pI27V0oNg8A6T/Zc0w9KwrHwAAAAAAIEzB64P1Tofyxck0/6zcBiBQB5YhHjHy07Dktl2tY6q0imIN41E",
    "4PCvmd9jVuK1mGVa1vDho8eF5xbk0uGtHEMQJ+FllfijOOdsg1bihxzfJ7p0b5NZ/HXOsmxaZ9vEndMhfWT+D3bwV756hCuO",
    "F0fcQVHaedT1ESKann7Un64cA8OB4cwiqLwfGks1shAkaPBPpL5qfIt8SueDABcGgWgBCYCeAAcLAQABIwMBAQVdABAAAAyB",
    "BgoB7mYTqwAA",
];
/// `7z a -ms=off -mhc=off -m0=BCJ -m1=LZMA:d=1m`: a.mid = midi(1, 40),
/// d.mid = midi(4, 30); one folder each, a stored file list.
const REAL_BCJ_PLAIN: &[&str] = &[
    "N3q8ryccAATFpAst+AAAAAAAAACOAAAAAAAAAFjHFhwAJpUJbzR28wHM6RB7KY3h9nVqjHBht1iXnZ+9jkXNQjndoQ/n7M4B",
    "Ds+zKiPwjizNBLYRHZ8fDsIgVYPvNjynKZU48/XWIY9Pbhg/UIRV10LAnbudReRDcK0Aahpkc/pPtkeTPIK0fKyDHSsTW7fO",
    "fuelOS3YGGaNdYzfZjntAdNjAAAAACaVCW80dvMBzOkQeymN4fZ4b127p8XWmmjXfjMG5my+CYDy6YsOJfky4zysvCaaXKqZ",
    "sEjR07SpWKXCeL8BJuHCijZn8c2Cayv5cVL3k8CiCBNPDf2OvpXAYcS5tb6we3FyogMEgucLV7EfjnKR+oMAAAEEBgACCYCF",
    "cwAHCwIAAiMDAQEFXQAQAAAEAwMBAwEAAiMDAQEFXQAQAAAEAwMBAwEADIFhgWGBEYERAAgKAVtOhyWf82f8AAAFAhkAERkA",
    "YQAuAG0AaQBkAAAAZAAuAG0AaQBkAAAAGQIAABQSAQAAAG5Gb2bdAQAAbkZvZt0BFQoBACCApIEggKSBAAA=",
];
/// `7z a -m0=PPMd`: a.mid.
const REAL_PPMD: &[&str] = &[
    "N3q8ryccAAS2CWQboQAAAAAAAABSAAAAAAAAACNBOIYATQmxPPlbYjDDqvi3uZisaUJ7arKPdC2I3LQlhhMG+QAWKvdAXHes",
    "OtgkPzQV7SP1j3pJfWyxt2P48jdfDnSl7RBCqYSUCpt0to1uAbXcIWiR2PqpfzFFnHLTq2ZtxEviKCSz399632DBQHnwDMuY",
    "URStDAyCkgrmZM8V+d/20guk+5ys4Em/XUbi0pm5se8caDVqvZFGMCMYhfvjlYamMgEEBgABCYChAAcLAQABIwMEAQUGAAAB",
    "AAyBYQAICgFbToclAAAFARkEAAAAABENAGEALgBtAGkAZAAAABQKAQAAAG5Gb2bdARUGAQAggKSBAAA=",
];
/// `7z a -pSecret`: a.mid, AES-encrypted content, file names readable.
const REAL_AES: &[&str] = &[
    "N3q8ryccAASwBiQ5kAAAAAAAAABiAAAAAAAAAPmy0wJmJI4p73Coq+F0slriLE2SQ5FFdjRT2LY8oKja93jBHzhXdwckgYEZ",
    "PBpTeyAizLZiD7mv0Qdg06qMfLuOMI2sVUdw0nd1O4K641f2yu3K4tPHgP87vNTPRFzlcvxcUTDGrK58G7dJCVV2I/9l1fOV",
    "Luz9LmgNlhxBeUUMLLDjmbhGF/hfZl8nE1TC4pGDzCgBBAYAAQmAkAAHCwEAAiQG8QcBElMPjDYiQY+cfELp8uHX7zhkmSEh",
    "AQABAAyAjIFhAAgKAVtOhyUAAAUBEQ0AYQAuAG0AaQBkAAAAFAoBAAAAbkZvZt0BFQYBACCApIEAAA==",
];
/// `7z a -pSecret -mhe=on`: the file list is encrypted too.
const REAL_AES_HEADER: &[&str] = &[
    "N3q8ryccAATdPh86AAEAAAAAAAAvAAAAAAAAAJmYdyrlYZs0I3fzjvudk2IBwegq8PcfnL6t43UzSEWWPoqfNTOLyzNT2YGp",
    "KbaUhQS6GbPWwW0iKK27eGNM5ZW6JFVhRUa6XpXbDIJ7fVZqWBwW+pChgLpchygPPuoNNt9OhTKY9C4BoqKUks8+idoHJSjV",
    "CLX8u3oNbp8Bu+1SDI8ERifD/962+nF62Uw7SzTog5fosh4/x/ZKh42CgHxfk3LeBbF8YkZPz8oyCEksYwcDtxQ+PdKnWAFe",
    "nAenyzE+wfG67AN+RCGytmyK9jfzY6oYiU8V0vxDz2hpZv8vDqWeta7JY71f4prjzKFf09TYVW7tRi+wlvbqb5NgQrV7gJLu",
    "FwaAkAEJcAAHCwEAASQG8QcBElMPhNl08e5tHYdW4Svu8KjY/wxiCgEjYzo+AAA=",
];

/// Standard base64 (fixtures only).
fn b64(lines: &[&str]) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64"),
    };
    let s: Vec<u8> = lines.concat().bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::new();
    for ch in s.chunks(4) {
        let v: Vec<u32> = ch.iter().map(|c| u32::from(val(*c))).collect();
        let n = v.iter().fold(0u32, |a, b| (a << 6) | b) << (6 * (4 - v.len()));
        out.extend_from_slice(&n.to_be_bytes()[1..v.len()]);
    }
    out
}

/// A 7z archive from the `sevenz-rust2` encoder (LZMA2): one solid folder,
/// or one folder per file.
pub(crate) fn sz_enc(files: &[(&str, &[u8])], dirs: &[&str], solid: bool) -> Vec<u8> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
    let mut w = ArchiveWriter::new(io::Cursor::new(Vec::new())).unwrap();
    for d in dirs {
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(d), None)
            .unwrap();
    }
    if solid {
        let entries = files
            .iter()
            .map(|(n, _)| ArchiveEntry::new_file(n))
            .collect();
        let readers = files.iter().map(|(_, d)| SourceReader::from(*d)).collect();
        w.push_archive_entries(entries, readers).unwrap();
    } else {
        for (n, d) in files {
            w.push_archive_entry(ArchiveEntry::new_file(n), Some(*d))
                .unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

/// A 7z NUMBER.
fn num(out: &mut Vec<u8>, v: u64) {
    for n in 0..8u32 {
        if v < 1u64 << (7 * n + 7) {
            out.push(!(0xFFu8 >> n) | (v >> (8 * n)) as u8);
            out.extend_from_slice(&v.to_le_bytes()[..n as usize]);
            return;
        }
    }
    out.push(0xFF);
    out.extend_from_slice(&v.to_le_bytes());
}

/// The signature header for a next header at `off` of `size` bytes.
fn start(off: u64, size: u64, crc: u32) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(&off.to_le_bytes());
    s.extend_from_slice(&size.to_le_bytes());
    s.extend_from_slice(&crc.to_le_bytes());
    let mut out = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4];
    out.extend_from_slice(&crc32fast::hash(&s).to_le_bytes());
    out.extend_from_slice(&s);
    out
}

/// Packed data then a header.
fn wrap(packed: &[u8], header: &[u8]) -> Vec<u8> {
    let mut out = start(
        packed.len() as u64,
        header.len() as u64,
        crc32fast::hash(header),
    );
    out.extend_from_slice(packed);
    out.extend_from_slice(header);
    out
}

/// A file of a hand-made archive: its own folder with one coder (stored
/// unless `id` says otherwise) and the data as given.
struct HFile {
    name: &'static str,
    id: Vec<u8>,
    props: Vec<u8>,
    data: Vec<u8>,
    attrs: Option<u32>,
}

fn hf(name: &'static str, data: &[u8]) -> HFile {
    HFile {
        name,
        id: vec![0x00],
        props: Vec::new(),
        data: data.to_vec(),
        attrs: None,
    }
}

/// A hand-made archive with a stored (plain) header; `files_n` /
/// `folders_n` override the declared counts.
fn handmade(files: &[HFile], files_n: Option<u64>, folders_n: Option<u64>) -> Vec<u8> {
    let packed: Vec<u8> = files.iter().flat_map(|f| f.data.clone()).collect();
    let mut h = vec![0x01, 0x04, 0x06];
    num(&mut h, 0);
    num(&mut h, files.len() as u64);
    h.push(0x09);
    for f in files {
        num(&mut h, f.data.len() as u64);
    }
    h.extend_from_slice(&[0x00, 0x07, 0x0B]);
    num(&mut h, folders_n.unwrap_or(files.len() as u64));
    h.push(0);
    for f in files {
        h.push(1);
        h.push(f.id.len() as u8 | if f.props.is_empty() { 0 } else { 0x20 });
        h.extend_from_slice(&f.id);
        if !f.props.is_empty() {
            num(&mut h, f.props.len() as u64);
            h.extend_from_slice(&f.props);
        }
    }
    h.push(0x0C);
    for f in files {
        num(&mut h, f.data.len() as u64);
    }
    h.extend_from_slice(&[0x0A, 0x01]);
    for f in files {
        h.extend_from_slice(&crc32fast::hash(&f.data).to_le_bytes());
    }
    h.extend_from_slice(&[0x00, 0x08, 0x00, 0x00, 0x05]);
    num(&mut h, files_n.unwrap_or(files.len() as u64));
    let mut names = vec![0u8];
    for f in files {
        for u in f.name.encode_utf16() {
            names.extend_from_slice(&u.to_le_bytes());
        }
        names.extend_from_slice(&[0, 0]);
    }
    h.push(0x11);
    num(&mut h, names.len() as u64);
    h.extend_from_slice(&names);
    if files.iter().any(|f| f.attrs.is_some()) {
        let mut a = vec![1u8, 0u8];
        for f in files {
            a.extend_from_slice(&f.attrs.unwrap_or(0x20).to_le_bytes());
        }
        h.push(0x15);
        num(&mut h, a.len() as u64);
        h.extend_from_slice(&a);
    }
    h.extend_from_slice(&[0x00, 0x00]);
    wrap(&packed, &h)
}

fn read(k: &Candidate) -> Vec<u8> {
    std::fs::read(&k.file).unwrap()
}

#[test]
fn seven_zip_archives_from_7zip_and_the_encoder_yield_the_exact_midi_bytes() {
    let c = Case::new("7z-ok");
    let (a, b, cc, d) = (midi(1, 40), midi(2, 60), midi(3, 20), midi(4, 30));
    // 7-Zip itself: solid LZMA2, compressed file list, a nested .gz.
    let real = b64(REAL_SOLID);
    assert_eq!(detect(&real), Format::SevenZip);
    let o = c.open("pack.7z", &real, &Limits::default()).unwrap();
    let mut got: Vec<(String, Vec<u8>)> = o
        .candidates
        .iter()
        .map(|k| (k.logical_path(), read(k)))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("pack.7z › c.mid.gz › c.mid".to_string(), cc.clone()),
            ("pack.7z › songs/a.mid".to_string(), a.clone()),
            ("pack.7z › songs/b.mid".to_string(), b.clone()),
        ]
    );
    assert!(o.notes.is_empty(), "{:?}", o.notes);
    // 7-Zip, BCJ + LZMA, one folder per file, stored file list.
    let o = c
        .open("bcj.7z", &b64(REAL_BCJ_PLAIN), &Limits::default())
        .unwrap();
    assert_eq!(paths(&o), ["bcj.7z › a.mid", "bcj.7z › d.mid"]);
    assert_eq!(read(&o.candidates[0]), a);
    assert_eq!(read(&o.candidates[1]), d);
    // The encoder, solid and not: text before, after and between MIDIs, a
    // .tar.xz inside, directories, duplicate names.
    let inner = xz(&tar(&[("deep/e.mid", b'0', &d)]));
    let files: [(&str, &[u8]); 6] = [
        ("notes.txt", b"not a song"),
        ("x/a.mid", &a),
        ("big.bin", &[7u8; 300_000]),
        ("x/a.mid", &b),
        ("more.tar.xz", &inner),
        ("z.txt", b"end"),
    ];
    for solid in [true, false] {
        let name = if solid { "solid.7z" } else { "loose.7z" };
        let o = c
            .open(
                name,
                &sz_enc(&files, &["x", "empty"], solid),
                &Limits::default(),
            )
            .unwrap();
        assert_eq!(
            paths(&o),
            [
                format!("{name} › x/a.mid"),
                format!("{name} › x/a.mid (2)"),
                format!("{name} › more.tar.xz › more.tar › deep/e.mid"),
            ],
            "solid {solid}"
        );
        let bytes: Vec<Vec<u8>> = o.candidates.iter().map(read).collect();
        assert_eq!(bytes, [a.clone(), b.clone(), d.clone()], "solid {solid}");
        assert!(o.notes.is_empty(), "{:?}", o.notes);
    }
    // A 7z inside a zip inside a 7z.
    let z = zip(&[ze("in.7z", &sz_enc(&[("q.mid", &cc)], &[], true), 0)]);
    let o = c
        .open(
            "outer.7z",
            &sz_enc(&[("in.zip", &z)], &[], false),
            &Limits::default(),
        )
        .unwrap();
    assert_eq!(paths(&o), ["outer.7z › in.zip › in.7z › q.mid"]);
    assert_eq!(read(&o.candidates[0]), cc);
    // An archive with no files (7-Zip writes no file list at all).
    let o = c
        .open("empty.7z", &start(0, 0, 0), &Limits::default())
        .unwrap();
    assert!(o.candidates.is_empty() && o.notes.is_empty());
    let o = c
        .open("none.7z", &sz_enc(&[], &["d"], true), &Limits::default())
        .unwrap();
    assert!(o.candidates.is_empty() && o.notes.is_empty());
}

#[test]
fn seven_zip_encryption_methods_links_and_dictionaries_are_reported_per_file() {
    let c = Case::new("7z-skip");
    let a = midi(5, 12);
    let o = c
        .open("ppmd.7z", &b64(REAL_PPMD), &Limits::default())
        .unwrap();
    assert!(o.candidates.is_empty());
    assert_eq!(
        o.notes,
        [
            "“ppmd.7z › a.mid” uses a compression method PlumSpirit cannot read (PPMd) and was skipped."
        ]
    );
    let o = c
        .open("aes.7z", &b64(REAL_AES), &Limits::default())
        .unwrap();
    assert!(o.candidates.is_empty());
    assert_eq!(
        o.notes,
        ["“aes.7z › a.mid” is encrypted (password protected) and was skipped."]
    );
    let o = c
        .open("aesh.7z", &b64(REAL_AES_HEADER), &Limits::default())
        .unwrap();
    assert!(o.candidates.is_empty());
    assert_eq!(
        o.notes,
        [
            "“aesh.7z” is encrypted (password protected), its file list included; PlumSpirit cannot open it."
        ]
    );
    // Per folder: a 4 GB LZMA2 dictionary and a 512 MB LZMA one are refused
    // before decoding, a link is skipped, the stored MIDI still opens.
    let mut huge = hf("huge.mid", &[0u8; 16]);
    huge.id = vec![0x21];
    huge.props = vec![40];
    let mut link = hf("link.mid", b"/etc/passwd");
    link.attrs = Some(0x8000 | (0o120_777 << 16));
    let mut zstd = hf("z.mid", &[1, 2, 3]);
    zstd.id = vec![0x04, 0xF7, 0x11, 0x01];
    let arch = handmade(&[huge, link, hf("ok.mid", &a), zstd], None, None);
    let o = c.open("mixed.7z", &arch, &Limits::default()).unwrap();
    assert_eq!(paths(&o), ["mixed.7z › ok.mid"]);
    assert_eq!(read(&o.candidates[0]), a);
    assert_eq!(
        o.notes,
        [
            "“mixed.7z › huge.mid” needs a 4 GB LZMA dictionary, more than the 256 MB PlumSpirit allows, and was skipped.",
            "“mixed.7z › link.mid” is a link and was skipped.",
            "“mixed.7z › z.mid” uses a compression method PlumSpirit cannot read (Zstandard) and was skipped.",
        ]
    );
}

#[test]
fn damaged_and_hostile_7z_headers_are_refused_before_they_allocate() {
    let c = Case::new("7z-bad");
    let a = midi(6, 30);
    let lim = Limits::default();
    let malformed = |e: ArchiveError, part: &str| {
        assert!(
            matches!(e, ArchiveError::Malformed(ref m) if m.contains(part)),
            "{e:?} should say {part}"
        );
    };
    let limit = |e: ArchiveError, part: &str| {
        assert!(
            matches!(e, ArchiveError::Limit(ref m) if m.contains(part)),
            "{e:?} should say {part}"
        );
    };
    // Counts far beyond the entry limit, declared by a few bytes.
    let e = c
        .open(
            "files.7z",
            &handmade(&[hf("a.mid", &a)], Some(1 << 40), None),
            &lim,
        )
        .err()
        .unwrap();
    limit(e, "lists more than 200000 files");
    let e = c
        .open(
            "folders.7z",
            &handmade(&[hf("a.mid", &a)], None, Some(1 << 33)),
            &lim,
        )
        .err()
        .unwrap();
    limit(e, "lists more than 200000 folders");
    // A file list declared larger than the cap.
    let mut big = start(0, 1 << 31, 0);
    big.extend_from_slice(&[0u8; 64]);
    let e = c.open("huge-list.7z", &big, &lim).err().unwrap();
    limit(e, "file list, more than the 64 MB");
    // A compressed file list that asks for a 4 GB LZMA2 dictionary.
    let mut h = vec![0x17, 0x06];
    num(&mut h, 0);
    num(&mut h, 1);
    h.push(0x09);
    num(&mut h, 10);
    h.extend_from_slice(&[
        0x00, 0x07, 0x0B, 0x01, 0x00, 0x01, 0x21, 0x21, 0x01, 40, 0x0C,
    ]);
    num(&mut h, 1000);
    h.extend_from_slice(&[0x00, 0x00]);
    let e = c
        .open("dict.7z", &wrap(&[0u8; 10], &h), &lim)
        .err()
        .unwrap();
    limit(e, "needs a 4 GB LZMA dictionary for its file list");
    // Damaged: an unfinished archive, a checksum, a cut file, a flipped
    // byte in the packed data, a newer version.
    let real = b64(REAL_SOLID);
    let mut unfinished = real.clone();
    unfinished[8..32].fill(0);
    let e = c.open("unfinished.7z", &unfinished, &lim).err().unwrap();
    malformed(e, "was not completely written");
    let mut crc = real.clone();
    crc[14] ^= 1;
    malformed(
        c.open("crc.7z", &crc, &lim).err().unwrap(),
        "start header checksum",
    );
    let e = c
        .open("cut.7z", &real[..real.len() - 40], &lim)
        .err()
        .unwrap();
    malformed(e, "lies past the end of the file");
    malformed(
        c.open("short.7z", &real[..20], &lim).err().unwrap(),
        "shorter than",
    );
    let mut flip = real.clone();
    flip[60] ^= 0x40;
    let e = c.open("flip.7z", &flip, &lim).err().unwrap();
    assert!(matches!(e, ArchiveError::Malformed(_)), "{e:?}");
    let mut newer = real.clone();
    newer[6] = 1;
    malformed(
        c.open("v1.7z", &newer, &lim).err().unwrap(),
        "newer format version",
    );
    let stored = handmade(&[hf("a.mid", &a)], None, None);
    let mut bad_data = stored.clone();
    bad_data[40] ^= 1;
    let e = c.open("data.7z", &bad_data, &lim).err().unwrap();
    malformed(e, "a.mid");
    // The per-entry size limit reaches 7z entries: a MIDI that decodes past
    // it, and a skipped entry a solid folder must decode to reach the next.
    let lim = Limits {
        max_entry: 1 << 20,
        ..Limits::default()
    };
    let mut big = a.clone();
    big.resize(3 << 20, 0);
    let e = c
        .open("big.7z", &sz_enc(&[("big.mid", &big)], &[], true), &lim)
        .err()
        .unwrap();
    limit(e, "big.mid” decodes to more than 1 MB");
    let zeros = vec![0u8; 3 << 20];
    let e = c
        .open(
            "zeros.7z",
            &sz_enc(&[("z.bin", &zeros), ("a.mid", &a)], &[], true),
            &lim,
        )
        .err()
        .unwrap();
    limit(e, "z.bin” decodes to more than 1 MB");
    // The same lone non-MIDI entry in its own folder is never decoded.
    let o = c
        .open(
            "lone.7z",
            &sz_enc(&[("z.bin", &zeros), ("a.mid", &a)], &[], false),
            &lim,
        )
        .unwrap();
    assert_eq!(paths(&o), ["lone.7z › a.mid"]);
    let p = c.file("cancel.7z", &sz_enc(&[("a.mid", &a)], &[], true));
    let e = open_archive(&p, &Limits::default(), &AtomicBool::new(true), &c.dir)
        .err()
        .unwrap();
    assert_eq!(e, ArchiveError::Cancelled);
}

#[test]
fn lzma_dictionaries_are_bounded_by_what_the_folder_decodes() {
    let coder = |id: &[u8], props: &[u8]| sz::Coder {
        id: id.to_vec(),
        props: props.to_vec(),
        out_index: 0,
    };
    let folder = |c: sz::Coder, unpack: u64| sz::Folder {
        coders: vec![c],
        unpack: vec![unpack],
        packed_streams: 1,
        packed: 1,
    };
    let lzma_512m = [0x5D, 0, 0, 0, 0x20];
    // LZMA keeps no more history than it outputs.
    assert_eq!(
        folder(coder(&[3, 1, 1], &lzma_512m), 1 << 20).refusal(),
        None
    );
    assert_eq!(
        folder(coder(&[3, 1, 1], &lzma_512m), 1 << 30).refusal(),
        Some(sz::Refusal::Dictionary(512 << 20))
    );
    // LZMA2 allocates the declared dictionary: 24 = 16 MB, 34 = 512 MB.
    assert_eq!(folder(coder(&[0x21], &[24]), 1 << 30).refusal(), None);
    assert_eq!(
        folder(coder(&[0x21], &[34]), 16).refusal(),
        Some(sz::Refusal::Dictionary(512 << 20))
    );
    assert_eq!(
        folder(coder(&[0x21], &[33]), 16).refusal(),
        Some(sz::Refusal::Dictionary(384 << 20))
    );
    assert_eq!(folder(coder(&[0x21], &[32]), 16).refusal(), None);
    assert_eq!(
        folder(coder(&[6, 0xF1, 7, 1], &[]), 16).refusal(),
        Some(sz::Refusal::Encrypted)
    );
}

#[test]
fn mutated_7z_archives_never_panic_and_never_escape_the_spool() {
    let c = Case::new("7z-fuzz");
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let a = midi(7, 25);
    let sources = [
        b64(REAL_SOLID),
        b64(REAL_BCJ_PLAIN),
        b64(REAL_AES),
        sz_enc(&[("a.mid", &a), ("b.txt", b"text")], &["d"], true),
        handmade(&[hf("a.mid", &a)], None, None),
    ];
    // More draws: PLUMSPIRIT_7Z_MUTATIONS=5000 (per source).
    let per = std::env::var("PLUMSPIRIT_7Z_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150usize);
    let (mut opened, mut refused) = (0, 0);
    for (si, src) in sources.iter().enumerate() {
        for i in 0..per {
            let mut m = src.clone();
            match rnd() % 4 {
                0 => {
                    let n = 1 + (rnd() % m.len() as u64) as usize;
                    m.truncate(n);
                }
                _ => {
                    for _ in 0..1 + rnd() % 3 {
                        let at = (rnd() % m.len() as u64) as usize;
                        m[at] ^= 1 << (rnd() % 8);
                    }
                }
            }
            match c.open(&format!("m{si}-{i}.7z"), &m, &Limits::default()) {
                Ok(o) => {
                    opened += 1;
                    assert!(
                        o.candidates
                            .iter()
                            .all(|k| k.file.starts_with(o.spool.dir()))
                    );
                }
                Err(ArchiveError::Malformed(_) | ArchiveError::Limit(_)) => refused += 1,
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
    }
    assert_eq!(opened + refused, 5 * per);
    assert!(refused > 2 * per, "most mutations are refused ({refused})");
}
