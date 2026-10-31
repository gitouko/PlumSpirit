//! `.lup` save / load timing probe (v1.0-beta1 B1-0163, B1-S68).
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example lup_profile -- [--reps N] [--out DIR] a.mid …
//! ```
//!
//! Imports each MIDI file, then per repetition saves it as a project the way
//! the app does (`save_project`: write to a temporary file + sync, re-read and
//! verify every note, event and setting, rename — write and verify timed
//! separately) and loads it back (`load_project`), checking the loaded
//! document's export fingerprint against the saved one.

use plumspirit_engine::midi::{ImportControl, import_file};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn peak_rss_mb() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or("n/a".into(), |kb| format!("{:.0}", kb / 1024.0))
}

fn main() {
    let mut reps = 3usize;
    let mut out = std::env::temp_dir();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--reps" => reps = args.next().and_then(|n| n.parse().ok()).unwrap_or(3),
            "--out" => out = PathBuf::from(args.next().expect("--out DIR")),
            _ => files.push(a),
        }
    }
    for f in &files {
        let path = Path::new(f);
        let name = path
            .file_name()
            .map_or(f.clone(), |n| n.to_string_lossy().into_owned());
        let (doc, _) = import_file(path, ImportControl::default()).expect("import");
        let fp = doc.export_fingerprint();
        println!("{name}: {} notes, fingerprint {fp:016x}", doc.note_count());
        let dest = out.join(format!("plumspirit-lup-probe-{}.lup", std::process::id()));
        let (mut w, mut v, mut l) = (Vec::new(), Vec::new(), Vec::new());
        for rep in 0..reps {
            let r = save_project(&doc, &dest, LupControl::default()).expect("save");
            let t = Instant::now();
            let (back, report) = load_project(&dest, LupControl::default()).expect("load");
            let load = t.elapsed().as_secs_f64();
            assert_eq!(back.export_fingerprint(), fp, "the loaded project differs");
            drop(back);
            println!(
                "{name} rep {rep}: save write {:.3} s + verify {:.3} s ({} bytes); load {:.3} s \
                 ({} notes); peak RSS {} MB",
                r.write_duration.as_secs_f64(),
                r.verify_duration.as_secs_f64(),
                r.write.bytes,
                load,
                report.notes,
                peak_rss_mb()
            );
            w.push(r.write_duration.as_secs_f64());
            v.push(r.verify_duration.as_secs_f64());
            l.push(load);
        }
        let _ = std::fs::remove_file(&dest);
        println!(
            "MEDIAN {name}: save write {:.3} + verify {:.3} s; load {:.3} s",
            median(&mut w),
            median(&mut v),
            median(&mut l)
        );
    }
}
