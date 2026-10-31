//! v0.7 stress (contract §159–161, adversarial review Q3 / Q5 / Q6): million-note
//! transforms through the Note Transformation Platform with exact undo / redo,
//! bounded multiplicative generation, and selection analytics on millions of
//! selected notes.
//!
//! The quick fixture runs with `cargo test`. The heavyweight ones are `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v07 -- --ignored --nocapture --test-threads=1
//! ```

use plumspirit_engine::analysis::Timing;
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{collect_with, commit};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, TrackScope, TransformError, run_kernel, tool,
};
use std::time::{Duration, Instant};

/// The Dense fixture keeps its notes in track 1.
const ART: usize = 1;

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1e3)
}

fn fixture(notes: usize) -> PlumDocument {
    let mut d = generate(FixtureShape::Dense, notes, 960, 7);
    // A mask band for the Clipping Mask (its base track: the one below).
    let end = d.track(ART).unwrap().notes.max_end().unwrap_or(1);
    let t = d.add_track().unwrap();
    let band: Vec<_> = (40u8..80)
        .map(|p| {
            plumspirit_engine::model::NoteRecord::new(
                0,
                end,
                plumspirit_engine::model::NoteAttrs::new(
                    p,
                    100,
                    0,
                    0,
                    plumspirit_engine::model::NoteOffKind::NoteOff,
                )
                .unwrap(),
            )
        })
        .collect();
    d.track_mut(t)
        .unwrap()
        .notes
        .insert_sorted_batch(&band, false)
        .unwrap();
    d.track_mut(ART).unwrap().notes.select_all();
    d
}

/// Runs tool `id` with `set` on the selection, commits it (one undo step),
/// then undoes and redoes; returns (output notes, run, apply, undo, redo).
fn round_trip(
    d: &mut PlumDocument,
    h: &mut History,
    id: &str,
    set: &[(&str, ParamValue)],
) -> Result<(usize, Duration, Duration, Duration, Duration), TransformError> {
    let def = tool(id)
        .or_else(|| plumspirit_engine::transform::command_tool(id))
        .unwrap();
    let mut ps = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let sc = [TrackScope {
        index: ART,
        all: false,
        content: None,
    }];
    let before = d.state_fingerprint();
    let t0 = Instant::now();
    let src = collect_with(def, d, &sc, &ps, 1 << 27)?;
    let mut ctx = Ctx::simple(960);
    ctx.groups = std::sync::Arc::new(d.project().groups.clone());
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&ps, &b)?;
    let est = k.estimate_source(&src).notes;
    let limits = Limits::for_system_memory(None);
    let run = run_kernel(k.as_ref(), &src, def.boundary(&ps), &limits, None)?;
    let t_run = t0.elapsed();
    drop(src);
    let out = run.notes.len();
    assert!(
        out as u64 <= est,
        "{id}: {out} notes out, estimated at most {est}"
    );
    // Previewing (the run) changed nothing.
    assert_eq!(
        d.state_fingerprint(),
        before,
        "{id}: the preview changed the document"
    );
    let t1 = Instant::now();
    commit(h, d, &run, "stress")?;
    let t_apply = t1.elapsed();
    drop(run);
    let after = d.state_fingerprint();
    assert_ne!(after, before, "{id}: nothing changed");
    let t2 = Instant::now();
    h.undo(d).unwrap();
    let t_undo = t2.elapsed();
    assert_eq!(d.state_fingerprint(), before, "{id}: undo is not exact");
    let t3 = Instant::now();
    h.redo(d).unwrap();
    let t_redo = t3.elapsed();
    assert_eq!(d.state_fingerprint(), after, "{id}: redo is not exact");
    h.undo(d).unwrap();
    d.track_mut(ART).unwrap().notes.select_all();
    Ok((out, t_run, t_apply, t_undo, t_redo))
}

fn tools_round_trip(notes: usize) {
    let mut d = fixture(notes);
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    let cases: &[(&str, &[(&str, ParamValue)])] = &[
        ("transpose", &[("semis", ParamValue::Int(5))]),
        // Humanize's defaults are all zero (an identity, not applied).
        (
            "humanize",
            &[
                ("tick.min", ParamValue::Int(-30)),
                ("tick.max", ParamValue::Int(30)),
                ("vel.min", ParamValue::Int(-10)),
                ("vel.max", ParamValue::Int(10)),
            ],
        ),
        // The Dense fixture is on the 1/16 grid already: quantize to 1/4.
        ("quantize", &[("grid", ParamValue::Choice(2))]),
        ("legato", &[]),
        ("arpeggiate", &[]),
        ("repeat", &[]),
        ("twirl", &[]),
        ("wave", &[]),
        ("clip_mask", &[]),
        ("kaleidoscope", &[]),
        ("orbit", &[("copies", ParamValue::Int(2))]),
        ("group", &[]),
        ("mute_notes", &[]),
        // v0.9 §082: Duplicate Up One Octave (a copy of every note).
        ("duplicate_octave", &[("semis", ParamValue::Int(12))]),
        // v0.9 §118: the one-click Slice Notes with its most cuts.
        (
            "slice_notes",
            &[(
                "at",
                ParamValue::List((1..=16).map(|k| k * 100_003).collect()),
            )],
        ),
        (
            "note_props",
            &[
                ("vel.on", ParamValue::Bool(true)),
                ("vel", ParamValue::Int(77)),
            ],
        ),
    ];
    for (id, set) in cases {
        match round_trip(&mut d, &mut h, id, set) {
            Ok((out, r, a, u, re)) => println!(
                "{id:>13} on {notes} notes: {out} out; run {}, apply {}, undo {}, redo {}",
                ms(r),
                ms(a),
                ms(u),
                ms(re)
            ),
            Err(TransformError::Unchanged) => {
                panic!("{id}: the parameters must change the fixture")
            }
            Err(e) => panic!("{id}: {}", e.user_message()),
        }
    }
}

#[test]
fn ten_thousand_notes_round_trip_exactly_through_every_family() {
    tools_round_trip(10_000);
}

#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn a_million_notes_round_trip_exactly_through_every_family() {
    tools_round_trip(1_000_000);
}

/// Q5 / Q6: impossible expansions are refused before anything is allocated,
/// on a million selected notes.
#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn explosive_settings_on_a_million_notes_are_refused_before_allocating() {
    let mut d = fixture(1_000_000);
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    let cases: &[(&str, &[(&str, ParamValue)])] = &[
        (
            "repeat",
            &[
                ("len", ParamValue::Choice(11)),
                ("len.ticks", ParamValue::Int(1)),
                ("a10", ParamValue::Bool(true)),
                ("a10.depth", ParamValue::Int(4)),
                ("a10.div", ParamValue::Int(8)),
            ],
        ),
        // Retained Experimental tools still exercise explosive preflight.
        (
            "orbit",
            &[
                ("copies", ParamValue::Int(64)),
                ("a3", ParamValue::Bool(true)),
                ("a3.rings", ParamValue::Int(8)),
            ],
        ),
        (
            "kaleidoscope",
            &[
                ("a1", ParamValue::Bool(true)),
                ("a1.n", ParamValue::Int(64)),
                ("g.fill", ParamValue::Bool(true)),
            ],
        ),
    ];
    for (id, set) in cases {
        let before = d.state_fingerprint();
        let t = Instant::now();
        match round_trip(&mut d, &mut h, id, set) {
            Err(TransformError::TooLarge(m)) => {
                println!("{id}: refused in {} — {m}", ms(t.elapsed()));
                assert!(
                    t.elapsed() < Duration::from_secs(5),
                    "{id}: refusal is slow"
                );
            }
            Ok((out, ..)) => println!("{id}: within the limits ({out} notes)"),
            Err(TransformError::Unchanged) => {
                panic!("{id}: the parameters must change the fixture")
            }
            Err(e) => panic!("{id}: {}", e.user_message()),
        }
        assert_eq!(d.state_fingerprint(), before);
    }
}

#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn selection_analytics_of_four_million_selected_notes() {
    let mut d = generate(FixtureShape::Dense, 4_000_000, 960, 3);
    d.track_mut(ART).unwrap().notes.select_all();
    let timing = Timing::for_document(&d);
    let t = d.track(ART).unwrap();
    let inp = [SelectionInput {
        id: t.id(),
        notes: t.notes.clone(),
        correction: None,
        shift: 0,
    }];
    let t0 = Instant::now();
    let a = SelectionAnalysis::build(&inp, &timing, None).unwrap();
    let el = t0.elapsed();
    assert_eq!(a.counted, 4_000_000);
    let area: u128 = t.notes.iter().map(|n| u128::from(n.duration_ticks)).sum();
    assert_eq!(a.area_ticks, area);
    println!(
        "selection analytics, 4M selected: {} (peak poly {}, unique {}, {:.2} B/note)",
        ms(el),
        a.peak_poly,
        a.peak_unique,
        a.heap_bytes() as f64 / 4e6
    );
}
