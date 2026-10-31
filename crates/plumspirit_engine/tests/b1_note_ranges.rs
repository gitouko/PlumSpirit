//! B1-S36 (B1-0078, contract §010): Note Properties → Ranges (`note_props`
//! n13) — rows over parts of the selection, every target, axis and blend,
//! deterministic overlaps in row order, graphs with Overshoot, invalid rows
//! refused, the preview counts and a bounded large batch.

use plumspirit_engine::editor::ranges::{self, Across, Blend, RangeRow, Target};
use plumspirit_engine::model::TrackId;
use plumspirit_engine::transform::output::Lane;
use plumspirit_engine::transform::param::{Curve, ParamValue};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::source::SourceTrack;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, Source, TNote, TransformError, TransformRun, run_kernel, tool,
};

fn tn(start: u64, dur: u64, pitch: u8, vel: u8) -> TNote {
    TNote {
        start,
        dur,
        pitch,
        vel,
        rel: 64,
        ..Default::default()
    }
}

/// Note Properties with n13 on and `rows` (plus `extra`) on one track with
/// subchannels 0 – 3.
fn run_rows(
    notes: Vec<TNote>,
    rows: &[RangeRow],
    extra: &[(&str, ParamValue)],
) -> Result<TransformRun, TransformError> {
    run_text(notes, &ranges::to_text(rows), extra)
}

fn run_text(
    notes: Vec<TNote>,
    text: &str,
    extra: &[(&str, ParamValue)],
) -> Result<TransformRun, TransformError> {
    let track = SourceTrack {
        id: TrackId(1),
        index: 0,
        content: None,
        revision: 0,
        subs: vec![(0, 0), (1, 1), (2, 2), (3, 3)],
        all: false,
        insert_only: false,
        frame: 0,
    };
    let src = Source::from_notes(notes, vec![track]);
    let t = tool("note_props").unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    ps.set("n13", ParamValue::Bool(true));
    ps.set("n13.rows", ParamValue::Text(text.to_owned()));
    for (k, v) in extra {
        ps.set(k, v.clone());
    }
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b)?;
    let est = k.estimate_source(&src);
    let r = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)?;
    assert!(r.notes.len() as u64 <= est.notes);
    Ok(r)
}

fn ok(notes: Vec<TNote>, rows: &[RangeRow]) -> TransformRun {
    run_rows(notes, rows, &[]).unwrap_or_else(|e| panic!("{e}"))
}

/// Eleven notes, one per eighth (480 ticks), keys 60 … 70, velocity 64.
fn row11() -> Vec<TNote> {
    (0..11)
        .map(|i| tn(i * 480, 240, 60 + i as u8, 64))
        .collect()
}

fn sorted(r: &TransformRun) -> Vec<TNote> {
    let mut v = r.notes.clone();
    v.sort_by_key(|n| (n.start, n.pitch, n.sub));
    v
}

fn row(t: Target, span: (f64, f64), from: i64, to: i64, blend: Blend) -> RangeRow {
    RangeRow {
        span,
        from,
        to,
        blend,
        ..RangeRow::new(t)
    }
}

#[test]
fn a_row_runs_through_its_values_over_its_span() {
    // Velocity 20 → 120 over the whole selection (time).
    let r = ok(
        row11(),
        &[row(Target::Velocities, (0.0, 1.0), 20, 120, Blend::Set)],
    );
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
    assert_eq!(
        r.message.as_deref(),
        Some("Ranges: 11 note(s) changed by 1 range(s).")
    );
    // Only the second half: the first notes keep 64.
    let r = ok(
        row11(),
        &[row(Target::Velocities, (0.5, 1.0), 20, 120, Blend::Set)],
    );
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![64, 64, 64, 64, 64, 20, 40, 60, 80, 100, 120]);
    // An off row and no rows change nothing.
    let mut off = row(Target::Velocities, (0.0, 1.0), 1, 1, Blend::Set);
    off.on = false;
    assert_eq!(sorted(&ok(row11(), &[off])), row11());
    assert_eq!(sorted(&ok(row11(), &[])), row11());
}

#[test]
fn overlapping_rows_apply_top_to_bottom() {
    let all = row(Target::Velocities, (0.0, 1.0), 100, 100, Blend::Set);
    let mid = row(Target::Velocities, (0.4, 0.6), 10, 10, Blend::Add);
    let v = |rows: &[RangeRow]| -> Vec<u8> {
        sorted(&ok(row11(), rows)).iter().map(|n| n.vel).collect()
    };
    assert_eq!(
        v(&[all.clone(), mid.clone()]),
        vec![100, 100, 100, 100, 110, 110, 110, 100, 100, 100, 100]
    );
    // Reordered: the Set below replaces what Add did.
    assert_eq!(v(&[mid.clone(), all.clone()]), vec![100; 11]);
    // Multiply %, At most, At least.
    let half = row(Target::Velocities, (0.0, 1.0), 50, 50, Blend::Multiply);
    assert_eq!(v(&[all.clone(), half])[0], 50);
    let cap = row(Target::Velocities, (0.0, 1.0), 40, 40, Blend::Min);
    assert_eq!(v(&[cap]), vec![40; 11]);
    let floor = row(Target::Velocities, (0.0, 1.0), 90, 90, Blend::Max);
    assert_eq!(v(&[floor]), vec![90; 11]);
    // The overlap is listed and explained.
    let o = ranges::overlaps(&[all, mid]);
    assert_eq!(o.len(), 1);
}

#[test]
fn every_target_writes_its_property_within_limits() {
    let base = sorted(&ok(row11(), &[]));
    let one = |t: Target, from: i64, to: i64, blend: Blend| {
        sorted(&ok(row11(), &[row(t, (0.0, 1.0), from, to, blend)]))
    };
    // Ticks: Set = offset from the note's own start; Add accumulates.
    let r = one(Target::Ticks, 0, 100, Blend::Set);
    assert_eq!(
        r.iter()
            .map(|n| n.start - base[0].start)
            .collect::<Vec<_>>()[10],
        4800 + 100
    );
    assert_eq!(r[5].start, 2400 + 50);
    // Gates (absolute, at least 1 tick).
    let r = one(Target::Gates, -10, 960, Blend::Set);
    assert_eq!(r[0].dur, 1);
    assert_eq!(r[10].dur, 960);
    // Transpose (offset) and Keys (absolute), held in 0 – 127.
    let r = one(Target::Transpose, 0, 12, Blend::Set);
    assert_eq!((r[0].pitch, r[10].pitch), (60, 82));
    let r = one(Target::Keys, 120, 200, Blend::Set);
    assert_eq!((r[0].pitch, r[10].pitch), (120, 127));
    // Channels 1 – 16 (note channels 0 – 15), subchannels by place.
    let r = one(Target::Channels, 1, 16, Blend::Set);
    assert_eq!((r[0].ch, r[10].ch), (0, 15));
    let r = one(Target::Subchannels, 0, 10, Blend::Set);
    assert_eq!((r[0].sub, r[3].sub, r[10].sub), (0, 3, 3));
    // A controller value at each note's start on its channel.
    let r = ok(
        row11(),
        &[row(Target::Controller(74), (0.0, 1.0), 0, 127, Blend::Set)],
    );
    let cc: Vec<(u64, u16)> = r
        .events
        .iter()
        .filter(|e| e.lane == Lane::Cc(74))
        .map(|e| (e.tick, e.value))
        .collect();
    assert_eq!(cc.len(), 11);
    assert_eq!(cc[0], (0, 0));
    assert_eq!(cc[10], (4800, 127));
    assert!(r.message.as_deref().unwrap().contains("Controller events"));
}

#[test]
fn rows_run_along_time_key_order_or_velocity() {
    // Keys descending in time: along Key the lowest key gets From.
    let notes: Vec<TNote> = (0..5)
        .map(|i| tn(i * 480, 240, 70 - i as u8 * 2, 30 + i as u8 * 20))
        .collect();
    let mk = |a: Across| RangeRow {
        across: a,
        ..row(Target::Gates, (0.0, 1.0), 100, 500, Blend::Set)
    };
    let gates = |a: Across| -> Vec<u64> {
        sorted(&ok(notes.clone(), &[mk(a)]))
            .iter()
            .map(|n| n.dur)
            .collect()
    };
    assert_eq!(gates(Across::Time), vec![100, 200, 300, 400, 500]);
    assert_eq!(gates(Across::Order), vec![100, 200, 300, 400, 500]);
    assert_eq!(gates(Across::Key), vec![500, 400, 300, 200, 100]);
    assert_eq!(gates(Across::Velocity), vec![100, 200, 300, 400, 500]);
    // Order counts notes, not time: three notes at one start and one later.
    let chord = vec![
        tn(0, 100, 60, 90),
        tn(0, 100, 64, 90),
        tn(0, 100, 67, 90),
        tn(9600, 100, 72, 90),
    ];
    let r = sorted(&ok(
        chord.clone(),
        &[RangeRow {
            across: Across::Order,
            ..row(Target::Velocities, (0.0, 1.0), 10, 40, Blend::Set)
        }],
    ));
    assert_eq!(
        r.iter().map(|n| n.vel).collect::<Vec<_>>(),
        vec![10, 20, 30, 40]
    );
    let r = sorted(&ok(
        chord,
        &[row(Target::Velocities, (0.0, 1.0), 10, 40, Blend::Set)],
    ));
    assert_eq!(
        r.iter().map(|n| n.vel).collect::<Vec<_>>(),
        vec![10, 10, 10, 40]
    );
}

#[test]
fn a_rows_graph_shapes_it_and_may_overshoot() {
    let mut r = row(Target::Velocities, (0.0, 1.0), 20, 100, Blend::Set);
    r.graph = Curve::new(vec![(0.0, 0.0), (0.5, 1.5), (1.0, 0.0)]).with_overshoot(0.5);
    let v: Vec<u8> = sorted(&ok(row11(), &[r])).iter().map(|n| n.vel).collect();
    // 20 + 80·1.5 = 140 → 127 in the middle; back to 20 at the ends.
    assert_eq!(v[0], 20);
    assert_eq!(v[5], 127);
    assert_eq!(v[10], 20);
    assert_eq!(v[2], 68, "20 + 80 · 0.6");
    assert_eq!(v[4], 116, "20 + 80 · 1.2: past the nominal To (100)");
}

#[test]
fn invalid_rows_and_huge_ticks_are_refused() {
    let e = run_text(row11(), "1~vel~time~0~1~40~120~set~0;0,0;1,1|nonsense", &[]).unwrap_err();
    assert!(
        e.user_message()
            .starts_with("Range 2: a range needs 9 fields"),
        "{e}"
    );
    let far = vec![tn(1 << 54, 10, 60, 90)];
    let e = run_rows(
        far.clone(),
        &[row(Target::Ticks, (0.0, 1.0), 0, 1, Blend::Set)],
        &[],
    )
    .unwrap_err();
    assert!(e.user_message().contains("2^53"), "{e}");
    // Other targets work there.
    let r = run_rows(
        far,
        &[row(Target::Velocities, (0.0, 1.0), 5, 5, Blend::Set)],
        &[],
    )
    .unwrap();
    assert_eq!(r.notes[0].vel, 5);
}

#[test]
fn ranges_run_before_the_math_expressions_and_under_the_lock() {
    // n13 (205) then n12 (210): the expression reads the range's value.
    let rows = [row(Target::Velocities, (0.0, 1.0), 50, 50, Blend::Set)];
    let r = run_rows(
        row11(),
        &rows,
        &[
            ("n12", ParamValue::Bool(true)),
            ("n12.v", ParamValue::Text("v + 1".into())),
        ],
    )
    .unwrap();
    assert!(r.notes.iter().all(|n| n.vel == 51));
    let m = r.message.unwrap();
    assert!(
        m.contains("Ranges: 11 note(s) changed") && m.contains("Math expressions"),
        "{m}"
    );
    // The lock mask (a7, velocity locked by default) still wins.
    let r = run_rows(row11(), &rows, &[("a7", ParamValue::Bool(true))]).unwrap();
    assert!(r.notes.iter().all(|n| n.vel == 64));
    // Deterministic: the same rows give the same result (Preview = Apply).
    let a = ok(row11(), &rows);
    let b = ok(row11(), &rows);
    assert_eq!(a.notes, b.notes);
}

/// B1-0078 bounded batch: 1,000,000 notes through eight rows (release:
/// `cargo test --release -p plumspirit_engine --test b1_note_ranges --
/// --ignored --nocapture`).
#[test]
#[ignore = "release measurement"]
fn a_million_notes_through_eight_rows() {
    let n = 1_000_000u64;
    let notes: Vec<TNote> = (0..n)
        .map(|i| tn(i * 7, 30, 20 + (i % 88) as u8, 1 + (i % 127) as u8))
        .collect();
    let rows = vec![
        row(Target::Velocities, (0.0, 1.0), 20, 120, Blend::Set),
        row(Target::Velocities, (0.3, 0.6), 10, -10, Blend::Add),
        RangeRow {
            across: Across::Key,
            ..row(Target::Gates, (0.0, 1.0), 10, 60, Blend::Set)
        },
        row(Target::Transpose, (0.5, 1.0), 0, 12, Blend::Set),
        RangeRow {
            across: Across::Order,
            ..row(Target::Channels, (0.0, 1.0), 1, 16, Blend::Set)
        },
        row(Target::Subchannels, (0.0, 1.0), 0, 3, Blend::Set),
        RangeRow {
            across: Across::Velocity,
            ..row(Target::Velocities, (0.0, 1.0), 90, 90, Blend::Min)
        },
        row(Target::Controller(11), (0.0, 1.0), 0, 127, Blend::Set),
    ];
    let t0 = std::time::Instant::now();
    let r = ok(notes, &rows);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(r.notes.len() as u64, n);
    println!(
        "B1-0078 1,000,000 notes, 8 rows: {ms:.0} ms ({:.1} ns/note); {}",
        ms * 1e6 / n as f64,
        r.message.unwrap_or_default()
    );
}
