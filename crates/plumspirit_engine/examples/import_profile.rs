//! Stage-timed MIDI import probe (v1.0-beta1 B1-0161, B1-S66).
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example import_profile -- [--reps N] a.mid b.mid …
//! cargo run --release -p plumspirit_engine --example import_profile -- --write out.mid TRACKS NOTES
//! ```
//!
//! `--write` (L-S29) writes a deterministic Black-MIDI-shaped file instead:
//! `TRACKS` tracks of `NOTES` notes each, chords of 8 notes every 4 ticks,
//! note on / off pairs in running status (about 6 bytes a note, like the
//! 302M-note community file: 1.89 GB).
//!
//! For every file and repetition it measures, each on its own:
//!
//! * `read`      — the whole file into memory (`std::fs::read`), the disk / cache cost;
//! * `hash`      — the strict-export content hash over those bytes (`WordHash`);
//! * `tokenize`  — `read_smf` over the in-memory bytes with a handler that only
//!   counts tokens (chunk scan, VLQs, running status — no stores);
//! * `import`    — `import_file`, the path the app takes (file → document), with
//!   the engine's own stage split (`ImportSummary::stages`);
//!
//! plus wall vs CPU time (Linux `/proc/self/stat`) and peak RSS (`VmHWM`). It
//! prints one line per run and a median summary; nothing is written.

use plumspirit_engine::midi::error::ImportWarning;
use plumspirit_engine::midi::provenance::WordHash;
use plumspirit_engine::midi::reader::{ByteSource, SmfHandler, Token, read_smf};
use plumspirit_engine::midi::{ImportControl, MidiError, import_file};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Count {
    tokens: u64,
    channel: u64,
    tracks: usize,
}

impl SmfHandler for Count {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        self.tracks += 1;
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        self.tokens += 1;
        if matches!(t, Token::Channel { .. }) {
            self.channel += 1;
        }
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, _: ImportWarning) {}
}

/// User + system CPU seconds of this process (Linux), `None` elsewhere.
fn cpu_seconds() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ut: f64 = f.get(11)?.parse().ok()?;
    let st: f64 = f.get(12)?.parse().ok()?;
    Some((ut + st) / 100.0)
}

fn peak_rss_mb() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut b = [0u8; 5];
    let mut n = 0;
    loop {
        b[n] = (v & 0x7f) as u8;
        v >>= 7;
        n += 1;
        if v == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(b[i] | if i > 0 { 0x80 } else { 0 });
    }
}

/// L-S29: `tracks` × `notes` notes, 960 PPQN, chords of 8 every 4 ticks,
/// each note 30 ticks long (on and off interleave), running status.
fn write_file(path: &Path, tracks: usize, notes: u64) -> std::io::Result<u64> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"MThd")?;
    f.write_all(&6u32.to_be_bytes())?;
    f.write_all(&1u16.to_be_bytes())?;
    f.write_all(&(tracks as u16 + 1).to_be_bytes())?;
    f.write_all(&960u16.to_be_bytes())?;
    // The tempo track.
    let tempo = [
        0x00, 0xff, 0x51, 0x03, 0x07, 0xa1, 0x20, 0x00, 0xff, 0x2f, 0x00,
    ];
    f.write_all(b"MTrk")?;
    f.write_all(&(tempo.len() as u32).to_be_bytes())?;
    f.write_all(&tempo)?;
    let mut total = 22 + tempo.len() as u64;
    const DUR: u64 = 30;
    for t in 0..tracks {
        let mut out = Vec::with_capacity((notes * 7) as usize);
        let ch = (t % 16) as u8;
        out.push(0); // delta
        out.push(0x90 | ch);
        let mut first = true;
        let mut now = 0u64;
        let (mut on, mut off) = (0u64, 0u64);
        let start = |k: u64| (k / 8) * 4;
        let pitch = |k: u64| ((k * 7 + t as u64 * 5) % 120 + 4) as u8;
        while off < notes {
            // Offs first at equal ticks (a note ending where the next starts).
            let take_off = on >= notes || start(off) + DUR <= start(on);
            let (at, key, vel) = if take_off {
                let k = off;
                off += 1;
                (start(k) + DUR, pitch(k), 0u8)
            } else {
                let k = on;
                on += 1;
                (start(k), pitch(k), (k % 126 + 1) as u8)
            };
            if !first {
                vlq(&mut out, (at - now) as u32);
            }
            first = false;
            now = at;
            out.push(key);
            out.push(vel);
        }
        out.extend_from_slice(&[0x00, 0xff, 0x2f, 0x00]);
        f.write_all(b"MTrk")?;
        f.write_all(&(out.len() as u32).to_be_bytes())?;
        f.write_all(&out)?;
        total += 8 + out.len() as u64;
    }
    f.flush()?;
    Ok(total)
}

fn main() {
    let mut reps = 3usize;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--reps" {
            reps = args.next().and_then(|n| n.parse().ok()).unwrap_or(3);
        } else if a == "--write" {
            let out = args.next().expect("--write FILE TRACKS NOTES");
            let tracks: usize = args.next().and_then(|n| n.parse().ok()).expect("TRACKS");
            let notes: u64 = args.next().and_then(|n| n.parse().ok()).expect("NOTES");
            let bytes = write_file(Path::new(&out), tracks, notes).expect("write");
            println!(
                "wrote {out}: {tracks} tracks x {notes} notes ({} M), {:.1} MB, {:.2} bytes a note",
                tracks as u64 * notes / 1_000_000,
                bytes as f64 / 1e6,
                bytes as f64 / (tracks as u64 * notes) as f64
            );
            return;
        } else {
            files.push(a);
        }
    }
    println!(
        "threads available: {}",
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    for f in &files {
        let path = Path::new(f);
        let name = path
            .file_name()
            .map_or(f.clone(), |n| n.to_string_lossy().into_owned());
        let (mut rd, mut hs, mut tk, mut im, mut cpu) = (vec![], vec![], vec![], vec![], vec![]);
        let mut st = Vec::new();
        for rep in 0..reps {
            let t = Instant::now();
            let bytes = std::fs::read(path).expect("read");
            let read = t.elapsed();
            let t = Instant::now();
            let mut h = WordHash::new();
            h.bytes(&bytes);
            let digest = h.finish();
            let hash = t.elapsed();
            let t = Instant::now();
            let mut c = Count::default();
            let mut src = ByteSource::new(&bytes[..], Some(bytes.len() as u64));
            read_smf(&mut src, &mut c).expect("tokenize");
            let tokenize = t.elapsed();
            drop(bytes);
            let c0 = cpu_seconds();
            let t = Instant::now();
            let (doc, summary) = import_file(path, ImportControl::default()).expect("import");
            let import = t.elapsed();
            let c1 = cpu_seconds();
            let cpu_s = c0.zip(c1).map(|(a, b)| b - a);
            println!(
                "{name} rep {rep}: read {:.3} s, hash {:.3} s ({digest:016x}), tokenize {:.3} s \
                 ({} tokens, {} channel, {} tracks), import {:.3} s wall / {} CPU [{}], \
                 {} notes, fingerprint {:016x}, {} warning(s), peak RSS {} MB",
                secs(read),
                secs(hash),
                secs(tokenize),
                c.tokens,
                c.channel,
                c.tracks,
                secs(import),
                cpu_s.map_or("n/a".into(), |s| format!("{s:.2} s")),
                summary.stages.describe(),
                doc.note_count(),
                doc.export_fingerprint(),
                summary.warnings.len(),
                peak_rss_mb().map_or("n/a".into(), |m| format!("{m:.0}")),
            );
            rd.push(secs(read));
            hs.push(secs(hash));
            tk.push(secs(tokenize));
            im.push(secs(import));
            if let Some(s) = cpu_s {
                cpu.push(s);
            }
            st.push(summary.stages);
            drop(doc);
        }
        let mut parse: Vec<f64> = st.iter().map(|s| secs(s.parse)).collect();
        let mut rest: Vec<f64> = st
            .iter()
            .map(|s| secs(s.assemble + s.structure + s.provenance + s.counts))
            .collect();
        println!(
            "MEDIAN {name}: read {:.3} s, hash {:.3} s, tokenize {:.3} s, import {:.3} s \
             (parse {:.3} s, after-parse {:.3} s), CPU {}",
            median(&mut rd),
            median(&mut hs),
            median(&mut tk),
            median(&mut im),
            median(&mut parse),
            median(&mut rest),
            if cpu.is_empty() {
                "n/a".into()
            } else {
                format!("{:.2} s", median(&mut cpu))
            },
        );
    }
}
