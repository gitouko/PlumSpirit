//! V09-S42 (V09-0075, contract §110 – §111): the Experimental review on real
//! fixtures. Not a pass / fail test of the tools (the v0.7 harness and
//! `v07_experimental.rs` do that) — it measures what each of the seven
//! retained original Experimental tools produces, so the review can score "noticeable
//! output, customisable, distinct, earns its space, WOW potential" from
//! numbers and pictures instead of opinions:
//!
//! ```text
//! PLUMSPIRIT_REVIEW_OUT=/path cargo test --release -p plumspirit_engine \
//!     --test v09_experimental_review -- --ignored --nocapture
//! ```
//!
//! Writes `runs.csv` (one row per tool × fixture × setting), `distinct.csv`
//! (per fixture, every pair of tools at defaults: Jaccard of the cells each
//! one changes) and `<fixture>/<tool>-<setting>.ppm` piano-roll rasters
//! (input grey, output coloured) — convert them with any image tool.
//!
//! Fixtures: the owner's `testing_purposes_only.mid` (its 3,640-note track),
//! a melody over a chord progression (generated, 8 bars) and a Dense
//! Black-MIDI wall (100,000 notes, defaults only).

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::midi::{self, ImportControl};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::{ADV_KEYS, ParamKind, ParamSpec, ParamValue};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_with;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TNote, TrackScope, run_kernel, tool};

/// The seven original tools retained after V09-S71 that V09-0075 audits (Tumours and the 3D Tool are
/// contract-defined and not part of the review).
const AUDITED: [&str; 7] = [
    "gravity_well",
    "kaleidoscope",
    "orbit",
    "vortex_trails",
    "pulse_field",
    "particle_burst",
    "flow_field",
];

/// Non-Experimental tools the S32 design notes name as overlaps
/// (`TOOL_DESIGN.md` "S42 input"): compared at defaults like the others.
const OVERLAP: [&str; 8] = [
    "duplicate",
    "paste_repeat_notes",
    "flip",
    "limit",
    "chord_stamp",
    "wave",
    "paint_color",
    "join",
];

/// Time cell of the occupancy rasters (a sixteenth at 960 PPQN).
const CELL: u64 = 240;

struct Fixture {
    name: &'static str,
    doc: PlumDocument,
    track: usize,
    /// Run every setting (otherwise defaults only).
    full: bool,
}

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn largest_track(doc: &PlumDocument) -> usize {
    (0..doc.track_count())
        .max_by_key(|&i| doc.track(i).map_or(0, |t| t.notes.len()))
        .expect("a track")
}

fn owner_fixture() -> Fixture {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v09/testing_purposes_only.mid");
    let (mut doc, _) =
        midi::import_file(&path, ImportControl::default()).expect("the owner's fixture imports");
    let track = largest_track(&doc);
    doc.track_mut(track).unwrap().notes.select_all();
    Fixture {
        name: "owner",
        doc,
        track,
        full: true,
    }
}

/// Eight bars: a C – Am – F – G progression (whole-note triads, low) under
/// an eighth-note melody that walks the chord tones with passing notes.
fn melody_fixture() -> Fixture {
    let bar = 3840u64;
    let chords: [[u8; 3]; 4] = [[48, 52, 55], [45, 48, 52], [41, 45, 48], [43, 47, 50]];
    let tune: [i8; 8] = [0, 2, 4, 7, 9, 7, 4, 2];
    let scale = [0u8, 2, 4, 5, 7, 9, 11];
    let mut v = Vec::new();
    for b in 0..8u64 {
        let c = chords[(b % 4) as usize];
        for &p in &c {
            v.push(rec(b * bar, bar - 60, p, 80, 0));
        }
        for (i, &d) in tune.iter().enumerate() {
            let deg = (i64::from(d) + b as i64) as usize;
            let pitch = 72 + 12 * (deg / 7) as u8 + scale[deg % 7] - 12;
            v.push(rec(b * bar + i as u64 * 480, 420, pitch, 100, 1));
        }
    }
    v.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    let mut doc = PlumDocument::new_project("melody".into(), Bpm::DEFAULT, 960);
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    Fixture {
        name: "melody",
        doc,
        track: 0,
        full: true,
    }
}

fn dense_fixture() -> Fixture {
    let mut doc = generate(FixtureShape::Dense, 100_000, 960, 42);
    let track = largest_track(&doc);
    doc.track_mut(track).unwrap().notes.select_all();
    Fixture {
        name: "dense100k",
        doc,
        track,
        full: false,
    }
}

/// One run of `id` on the fixture's selection with `set`.
fn run(id: &str, fx: &Fixture, set: &ParamSet) -> Result<(Vec<TNote>, Vec<TNote>, f64), String> {
    let t = tool(id).ok_or_else(|| format!("no tool {id}"))?;
    let sc = [TrackScope {
        index: fx.track,
        all: false,
        content: None,
    }];
    let src = collect_with(t, &fx.doc, &sc, set, 1 << 24).map_err(|e| e.user_message())?;
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let t0 = std::time::Instant::now();
    let k = t.kernel(set, &b).map_err(|e| e.user_message())?;
    let out = run_kernel(k.as_ref(), &src, t.boundary(set), &Limits::default(), None)
        .map(|r| r.notes)
        .map_err(|e| e.user_message())?;
    Ok((src.notes.clone(), out, t0.elapsed().as_secs_f64() * 1000.0))
}

type Cell = (u64, u8);

/// Distinct (time cell, pitch) cells the notes cover.
fn cells(v: &[TNote]) -> HashSet<Cell> {
    let mut s = HashSet::new();
    for n in v {
        let a = n.start / CELL;
        let b = n.end().div_ceil(CELL).max(a + 1).min(a + 4096);
        for c in a..b {
            s.insert((c, n.pitch));
        }
    }
    s
}

/// What a run changed, on the cell grid: notes added (`true`) and removed
/// (`false`) as (start cell, pitch, length in cells), with multiplicity — so
/// copies stacked on existing notes and velocity-only edits still count.
type Change = BTreeMap<(bool, u64, u8, u64), usize>;

fn change(input: &[TNote], out: &[TNote]) -> Change {
    let q = |n: &TNote| (n.start / CELL, n.pitch, n.dur.div_ceil(CELL).max(1));
    let mut bal: BTreeMap<(u64, u8, u64), i64> = BTreeMap::new();
    for n in out {
        *bal.entry(q(n)).or_default() += 1;
    }
    for n in input {
        *bal.entry(q(n)).or_default() -= 1;
    }
    bal.into_iter()
        .filter(|(_, c)| *c != 0)
        .map(|((s, p, d), c)| ((c > 0, s, p, d), c.unsigned_abs() as usize))
        .collect()
}

/// Weighted Jaccard of two changes; `None` when either run changed nothing
/// on the grid (no evidence of overlap either way).
fn jaccard(a: &Change, b: &Change, added_only: bool) -> Option<f64> {
    let keep = |k: &&(bool, u64, u8, u64)| !added_only || k.0;
    if !a.keys().any(|k| keep(&k)) || !b.keys().any(|k| keep(&k)) {
        return None;
    }
    let (mut inter, mut union) = (0usize, 0usize);
    let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).filter(keep).collect();
    for k in keys {
        let (x, y) = (
            a.get(k).copied().unwrap_or(0),
            b.get(k).copied().unwrap_or(0),
        );
        inter += x.min(y);
        union += x.max(y);
    }
    Some(inter as f64 / union as f64)
}

/// The note identity used to tell changed notes from kept ones (start,
/// length, pitch, velocity, channel).
type NoteKey = (u64, u64, u8, u8, u8);

fn key(n: &TNote) -> NoteKey {
    (n.start, n.dur, n.pitch, n.vel, n.ch)
}

struct Metrics {
    out: usize,
    kept: usize,
    new: usize,
    span_in: u64,
    span_out: u64,
    pitch_in: (u8, u8),
    pitch_out: (u8, u8),
    cells_in: usize,
    cells_out: usize,
    delta: Change,
}

fn metrics(input: &[TNote], out: &[TNote]) -> Metrics {
    let mut multi: BTreeMap<_, usize> = BTreeMap::new();
    for n in input {
        *multi.entry(key(n)).or_default() += 1;
    }
    let mut kept = 0;
    for n in out {
        if let Some(c) = multi.get_mut(&key(n))
            && *c > 0
        {
            *c -= 1;
            kept += 1;
        }
    }
    let span = |v: &[TNote]| {
        let lo = v.iter().map(|n| n.start).min().unwrap_or(0);
        let hi = v.iter().map(|n| n.end()).max().unwrap_or(0);
        hi.saturating_sub(lo)
    };
    let pr = |v: &[TNote]| {
        (
            v.iter().map(|n| n.pitch).min().unwrap_or(0),
            v.iter().map(|n| n.pitch).max().unwrap_or(0),
        )
    };
    let ci = cells(input);
    let co = cells(out);
    let delta = change(input, out);
    Metrics {
        out: out.len(),
        kept,
        new: out.len() - kept,
        span_in: span(input),
        span_out: span(out),
        pitch_in: pr(input),
        pitch_out: pr(out),
        cells_in: ci.len(),
        cells_out: co.len(),
        delta,
    }
}

/// A piano-roll raster: input grey, output in colour (by MIDI channel hue),
/// overlap white. 2 px per pitch row; time scaled to at most 640 px.
fn write_ppm(path: &Path, input: &[TNote], out: &[TNote]) {
    let all = input.iter().chain(out);
    let lo = all.clone().map(|n| n.start).min().unwrap_or(0);
    let hi = all.clone().map(|n| n.end()).max().unwrap_or(1).max(lo + 1);
    let (plo, phi) = (
        all.clone()
            .map(|n| n.pitch)
            .min()
            .unwrap_or(0)
            .saturating_sub(2),
        all.map(|n| n.pitch).max().unwrap_or(127).saturating_add(2),
    );
    let rows = usize::from(phi - plo) + 1;
    let (w, h) = (640usize, rows * 3);
    let scale = (hi - lo) as f64 / w as f64;
    let mut px = vec![[18u8, 18, 24]; w * h];
    let mut paint = |v: &[TNote], f: &dyn Fn(&TNote, [u8; 3]) -> [u8; 3]| {
        for n in v {
            let x0 = ((n.start - lo) as f64 / scale) as usize;
            let x1 = (((n.end() - lo) as f64 / scale).ceil() as usize).clamp(x0 + 1, w);
            let y = (usize::from(phi - n.pitch.min(phi).max(plo))) * 3;
            for yy in y..(y + 2).min(h) {
                for xx in x0.min(w - 1)..x1 {
                    let p = &mut px[yy * w + xx];
                    *p = f(n, *p);
                }
            }
        }
    };
    paint(input, &|_, _| [92, 92, 100]);
    const HUE: [[u8; 3]; 8] = [
        [255, 150, 40],
        [80, 200, 255],
        [255, 90, 160],
        [140, 230, 90],
        [200, 140, 255],
        [255, 220, 70],
        [70, 230, 200],
        [255, 110, 90],
    ];
    paint(out, &|n, p| {
        if p == [92, 92, 100] || p == [255, 255, 255] {
            [255, 255, 255]
        } else {
            HUE[usize::from(n.ch) % 8]
        }
    });
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in px {
        bytes.extend_from_slice(&p);
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The ends of a control's range worth trying (none for free-form kinds).
fn ends(p: &ParamSpec, base: &ParamSet) -> Vec<(String, ParamValue)> {
    let v: Vec<(&str, ParamValue)> = match &p.kind {
        ParamKind::Int { min, max } => vec![
            ("min", ParamValue::Int(*min)),
            ("max", ParamValue::Int((*max).min(*min + 400))),
        ],
        ParamKind::Float { min, max, .. } => vec![
            ("min", ParamValue::Float(*min)),
            ("max", ParamValue::Float(*max)),
        ],
        ParamKind::Ticks { min, max } => vec![
            ("min", ParamValue::Int(*min as i64)),
            ("max", ParamValue::Int((*max).min(15360) as i64)),
        ],
        ParamKind::Choice(o) => (0..o.len() as u32)
            .map(|c| ("choice", ParamValue::Choice(c)))
            .collect(),
        ParamKind::Bool => vec![(
            "flip",
            ParamValue::Bool(base.get_raw(p.key) != Some(&ParamValue::Bool(true))),
        )],
        _ => vec![],
    };
    v.into_iter()
        .enumerate()
        .filter(|(_, (_, val))| Some(val) != base.get_raw(p.key))
        .map(|(j, (tag, val))| (format!("{}.{tag}{j}", p.key), val))
        .collect()
}

/// The settings measured per tool, each with the setting it is compared
/// with (customisability: how many of them change the result): defaults;
/// each v0.8 advanced function alone (against defaults); each of its own
/// controls at the ends of its range with the function on (against the
/// function alone); each base control at its ends (against defaults). The
/// boundary policies (`b.*`) are left out: they act only at the edges.
fn settings(spec: &[ParamSpec], advanced: usize) -> Vec<(String, String, ParamSet)> {
    let base = ParamSet::defaults(spec);
    let mut v = vec![("defaults".to_string(), String::new(), base.clone())];
    for (i, k) in ADV_KEYS.iter().take(advanced).enumerate() {
        let mut on = base.clone();
        on.set(k, ParamValue::Bool(true));
        let fname = format!("a{}", i + 1);
        v.push((fname.clone(), "defaults".into(), on.clone()));
        for p in spec
            .iter()
            .filter(|p| usize::from(p.adv) == i + 1 && p.key != *k)
        {
            for (name, val) in ends(p, &on) {
                let mut s = on.clone();
                s.set(p.key, val);
                v.push((format!("{fname}|{name}"), fname.clone(), s));
            }
        }
    }
    for p in spec
        .iter()
        .filter(|p| p.adv == 0 && !p.key.starts_with("b."))
    {
        for (name, val) in ends(p, &base) {
            let mut s = base.clone();
            s.set(p.key, val);
            v.push((name, "defaults".into(), s));
        }
    }
    v
}

#[test]
#[ignore = "measurement for the V09-S42 review: run with --release --ignored --nocapture"]
fn experimental_review_on_real_fixtures() {
    let out_dir = PathBuf::from(std::env::var("PLUMSPIRIT_REVIEW_OUT").unwrap_or_else(|_| {
        format!(
            "{}/../../target/experimental-review",
            env!("CARGO_MANIFEST_DIR")
        )
    }));
    std::fs::create_dir_all(&out_dir).unwrap();
    let fixtures = [owner_fixture(), melody_fixture(), dense_fixture()];
    let mut csv = String::from(
        "fixture,tool,setting,result,ms,in,out,kept,new,changed_frac,span_in,span_out,pitch_in_lo,pitch_in_hi,pitch_out_lo,pitch_out_hi,cells_in,cells_out,changed_notes,material\n",
    );
    let mut dist = String::from("fixture,a,b,change_jaccard,added_jaccard\n");
    for fx in &fixtures {
        let mut deltas: BTreeMap<&str, Change> = BTreeMap::new();
        let ids: Vec<&str> = AUDITED.iter().chain(OVERLAP.iter()).copied().collect();
        for id in ids {
            let audited = AUDITED.contains(&id);
            let spec = (tool(id).expect("tool").spec)();
            let all = if fx.full && audited {
                settings(&spec.params, spec.advanced.len())
            } else {
                vec![(
                    "defaults".to_string(),
                    String::new(),
                    ParamSet::defaults(&spec.params),
                )]
            };
            let mut outs: BTreeMap<String, Vec<NoteKey>> = BTreeMap::new();
            for (name, reference, set) in &all {
                match run(id, fx, set) {
                    Ok((input, out, ms)) => {
                        let m = metrics(&input, &out);
                        let mut sig: Vec<_> = out.iter().map(key).collect();
                        sig.sort_unstable();
                        let material = match outs.get(reference) {
                            None => "-",
                            Some(d) if *d == sig => "no",
                            Some(_) => "yes",
                        };
                        outs.insert(name.clone(), sig);
                        let changed = 1.0 - m.kept as f64 / input.len().max(1) as f64;
                        writeln!(
                            csv,
                            "{},{id},{name},ok,{ms:.1},{},{},{},{},{changed:.4},{},{},{},{},{},{},{},{},{},{material}",
                            fx.name,
                            input.len(),
                            m.out,
                            m.kept,
                            m.new,
                            m.span_in,
                            m.span_out,
                            m.pitch_in.0,
                            m.pitch_in.1,
                            m.pitch_out.0,
                            m.pitch_out.1,
                            m.cells_in,
                            m.cells_out,
                            m.delta.values().sum::<usize>()
                        )
                        .unwrap();
                        if name == "defaults" {
                            if audited && fx.name != "dense100k" {
                                write_ppm(
                                    &out_dir.join(fx.name).join(format!("{id}-defaults.ppm")),
                                    &input,
                                    &out,
                                );
                            }
                            deltas.insert(id, m.delta);
                        } else if audited
                            && fx.name == "melody"
                            && name.starts_with('a')
                            && name[1..].bytes().all(|b| b.is_ascii_digit())
                        {
                            write_ppm(
                                &out_dir.join(fx.name).join(format!("{id}-{name}.ppm")),
                                &input,
                                &out,
                            );
                        }
                    }
                    Err(e) => {
                        writeln!(
                            csv,
                            "{},{id},{name},refused: {},,,,,,,,,,,,,,,,",
                            fx.name,
                            e.replace(',', ";")
                        )
                        .unwrap();
                    }
                }
            }
            eprintln!("{} {id}: {} settings", fx.name, all.len());
        }
        let names: Vec<&str> = deltas.keys().copied().collect();
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                if !AUDITED.contains(a) && !AUDITED.contains(b) {
                    continue;
                }
                let f = |x: Option<f64>| x.map_or("n/a".to_string(), |j| format!("{j:.4}"));
                let (j, ja) = (
                    f(jaccard(&deltas[a], &deltas[b], false)),
                    f(jaccard(&deltas[a], &deltas[b], true)),
                );
                writeln!(dist, "{},{a},{b},{j},{ja}", fx.name).unwrap();
            }
        }
    }
    std::fs::write(out_dir.join("runs.csv"), csv).unwrap();
    std::fs::write(out_dir.join("distinct.csv"), dist).unwrap();
    eprintln!("wrote {}", out_dir.display());
}
