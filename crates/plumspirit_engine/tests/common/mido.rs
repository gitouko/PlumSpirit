//! The reader matrix's second standard parser (V09-S23 step 5, V09-0099):
//! `tools/mido_check.py` reads files with Python's mido (a test tool, never
//! a PlumSpirit dependency) and prints them in a normalised line form;
//! [`digest_lines`] prints the independent Rust reader's digest
//! (`smf_digest`) the same way, so the two parsers can be compared file by
//! file.
#![allow(dead_code)]

use super::smf_digest::Digest;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `d` in the line form of `tools/mido_check.py` (without the `file` line).
/// SysEx payloads lose a leading `F0` and a trailing `F7`, and `F0` / `F7`
/// events are not told apart — as mido reads them.
pub fn digest_lines(d: &Digest) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "header {} {} {}", d.format, d.division, d.tracks.len());
    for (i, t) in d.tracks.iter().enumerate() {
        let _ = writeln!(s, "track {i}");
        for &(tick, ch, key, vel) in &t.ons {
            let _ = writeln!(s, "on {tick} {ch} {key} {vel}");
        }
        for &(tick, ch, key, vel, on0) in &t.offs {
            let _ = writeln!(s, "off {tick} {ch} {key} {vel} {}", u8::from(on0));
        }
        for &(tick, ch, c, v) in &t.controllers {
            let _ = writeln!(s, "cc {tick} {ch} {c} {v}");
        }
        for &(tick, ch, v) in &t.pitch_bends {
            let _ = writeln!(s, "pb {tick} {ch} {v}");
        }
        for &(tick, ch, p) in &t.programs {
            let _ = writeln!(s, "pc {tick} {ch} {p}");
        }
        for &(tick, ch, key, v) in &t.pressures {
            let _ = writeln!(s, "press {tick} {ch} {key} {v}");
        }
        for &(tick, us) in &t.tempos {
            let _ = writeln!(s, "tempo {tick} {us}");
        }
        for &(tick, n, dp, c, b) in &t.time_signatures {
            let _ = writeln!(s, "ts {tick} {n} {dp} {c} {b}");
        }
        for (tick, ty, p) in &t.metas {
            let _ = writeln!(s, "meta {tick} {ty} {}", hex(p));
        }
        for (tick, _, p) in &t.sysex {
            let mut p: &[u8] = p;
            if p.first() == Some(&0xF0) {
                p = &p[1..];
            }
            if p.last() == Some(&0xF7) {
                p = &p[..p.len() - 1];
            }
            let _ = writeln!(s, "sysex {tick} {}", hex(p));
        }
        for (tick, b) in &t.system {
            let _ = writeln!(s, "system {tick} {}", hex(b));
        }
        if let Some(e) = t.end_of_track {
            let _ = writeln!(s, "eot {e}");
        }
    }
    s
}

/// Runs `tools/mido_check.py` on `paths`: per path, its lines (without the
/// `file` line; a refusal is one `error …` line). `None` when Python or
/// mido is not available — the caller skips, unless
/// `PLUMSPIRIT_REQUIRE_MIDO=1` (then this panics).
pub fn mido_lines(paths: &[PathBuf]) -> Option<Vec<String>> {
    mido_lines_with(paths, false)
}

/// [`mido_lines`]; `unpatched`: mido exactly as shipped (without the
/// tool's fix of its unknown-meta delta time, `tools/mido_check.py`).
pub fn mido_lines_with(paths: &[PathBuf], unpatched: bool) -> Option<Vec<String>> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mido_check.py");
    let require = std::env::var_os("PLUMSPIRIT_REQUIRE_MIDO").is_some_and(|v| v == "1");
    let flag: &[&str] = if unpatched { &["--unpatched"] } else { &[] };
    let mut out = None;
    for python in ["python3", "python"] {
        if let Ok(o) = Command::new(python)
            .arg(&script)
            .args(flag)
            .args(paths)
            .output()
        {
            if o.status.success() {
                out = Some(o.stdout);
                break;
            }
            if o.status.code() == Some(3) {
                break; // Python runs, mido is missing.
            }
        }
    }
    let Some(stdout) = out else {
        assert!(!require, "PLUMSPIRIT_REQUIRE_MIDO=1 but mido could not run");
        eprintln!("mido (Python) is not available: the second-parser check is skipped");
        return None;
    };
    let text = String::from_utf8(stdout).expect("mido_check prints UTF-8");
    let mut files: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with("file ") {
            files.push(String::new());
        } else if let Some(f) = files.last_mut() {
            f.push_str(line);
            f.push('\n');
        }
    }
    assert_eq!(files.len(), paths.len(), "one block per file");
    Some(files)
}

/// A fresh scratch folder for files the matrix hands to mido.
pub fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("plumspirit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
