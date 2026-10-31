//! Export timing probe (v1.0-beta1 B1-0162, B1-S67): serialization and
//! validation timed separately for both export paths.
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example export_profile -- [--reps N] [--out DIR] a.mid …
//! ```
//!
//! For every file: import it, then per repetition
//!
//! * **strict** (the unchanged-tape fast path the app takes for a project not
//!   edited since its import): `export_strict` with running status, then with
//!   explicit status bytes — `write` (stream the source through the tokenizer
//!   into a temporary file) and `validate` (re-read the output) separately;
//! * **model** (the path of an edited project): `plan_export` + `export_to_path`
//!   — plan, `write` and `validate` separately.
//!
//! Output files go to `--out` (default: the system temp folder) and are removed
//! (`--keep` keeps the last model export).

use plumspirit_engine::midi::strict::{StatusBytes, export_strict};
use plumspirit_engine::midi::{
    ExportControl, ImportControl, export_to_path, import_file, plan_export,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn peak_rss_mb() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn main() {
    let mut reps = 3usize;
    let mut out = std::env::temp_dir();
    let mut files = Vec::new();
    let mut keep = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--reps" => reps = args.next().and_then(|n| n.parse().ok()).unwrap_or(3),
            "--out" => out = PathBuf::from(args.next().expect("--out DIR")),
            "--keep" => keep = true,
            _ => files.push(a),
        }
    }
    for f in &files {
        let path = Path::new(f);
        let name = path
            .file_name()
            .map_or(f.clone(), |n| n.to_string_lossy().into_owned());
        let t = Instant::now();
        let (doc, _) = import_file(path, ImportControl::default()).expect("import");
        println!(
            "{name}: imported in {:.3} s, {} notes",
            t.elapsed().as_secs_f64(),
            doc.note_count()
        );
        let dest = out.join(format!(
            "plumspirit-export-probe-{}.mid",
            std::process::id()
        ));
        let mut cols: [Vec<f64>; 7] = Default::default();
        for rep in 0..reps {
            let r = export_strict(&doc, &dest, StatusBytes::Running, ExportControl::default())
                .expect("strict export");
            let (sw, sv) = (
                r.write_duration.as_secs_f64(),
                r.validate_duration.as_secs_f64(),
            );
            let identical = r.identical_to_source;
            let r = export_strict(&doc, &dest, StatusBytes::Explicit, ExportControl::default())
                .expect("strict export (explicit status)");
            let (ew, ev) = (
                r.write_duration.as_secs_f64(),
                r.validate_duration.as_secs_f64(),
            );
            let t = Instant::now();
            let plan = plan_export(&doc);
            let plan_s = t.elapsed().as_secs_f64();
            let r = export_to_path(&doc, &plan, &dest, ExportControl::default()).expect("export");
            let (mw, mv) = (
                r.write_duration.as_secs_f64(),
                r.validate_duration.as_secs_f64(),
            );
            println!(
                "{name} rep {rep}: strict RSB write {sw:.3} s + validate {sv:.3} s (identical to \
                 source: {identical}); strict explicit write {ew:.3} s + validate {ev:.3} s; model \
                 plan {plan_s:.3} s, write {mw:.3} s, validate {mv:.3} s ({} events, {} bytes); \
                 peak RSS {} MB",
                r.validation.events_checked,
                r.write.bytes,
                peak_rss_mb().map_or("n/a".into(), |m| format!("{m:.0}")),
            );
            for (c, v) in cols.iter_mut().zip([sw, sv, ew, ev, plan_s, mw, mv]) {
                c.push(v);
            }
        }
        if keep {
            println!("kept the last model export: {}", dest.display());
        } else {
            let _ = std::fs::remove_file(&dest);
        }
        let m: Vec<f64> = cols.iter_mut().map(|c| median(c)).collect();
        println!(
            "MEDIAN {name}: strict RSB write {:.3} + validate {:.3} s; strict explicit write {:.3} \
             + validate {:.3} s; model plan {:.3} + write {:.3} + validate {:.3} s",
            m[0], m[1], m[2], m[3], m[4], m[5], m[6]
        );
    }
}
