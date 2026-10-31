//! V09-S13 (contract §088 – §091, §257; `docs/PASTE_REPEAT.md`): Paste
//! Repeat's two adapters on the shared timing core. Notes: the
//! `paste_repeat_notes` tool copies every selected note to each planned
//! offset with all its attributes, unselected notes untouched, one undo step
//! restoring the document exactly; End to end with Snap, Even spacing in
//! bars across a signature change (also with a clip session's shift) and in
//! seconds under a tempo step; refusals are sentences. Clips: linked and
//! independent repeats on the clips' rows, one undo step, `.lup` round trip,
//! journal replay after a restart, physical spacing under tempo changes,
//! and the clip cap.

use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::editor::clip_repeat::{MAX_NEW_CLIPS, plan_repeat_clips, repeat_clips};
use plumspirit_engine::editor::clips::add_clip;
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::tempo::TempoValue;
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel,
    SubchannelId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use plumspirit_engine::transform::param::Curve;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::repeat::{Decimal, RepeatSpec, RepeatUnit, Spacing};
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, TEMPO_RATE, TrackScope, TransformError, run_kernel, tool,
};
use plumspirit_engine::view::grid::MeterMap;
use std::sync::Arc;

fn note(t: u64, d: u64, p: u8, v: u8, ch: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        d,
        NoteAttrs::new(p, v, ch, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

fn apply(d: &mut PlumDocument, h: &mut History, op: Operation) {
    h.apply(d, op).unwrap();
}

/// A 480-PPQN song: track 0 holds four selected notes in the first bar
/// (different lengths, velocities, channels) and one unselected note at
/// tick 20,000; track 1 two notes. `tempo`: (tick, BPM, interpolation)
/// points; `meter`: time signatures.
fn song(tempo: &[(u64, &str, Interp)], meter: &[(u64, u8, u8)]) -> PlumDocument {
    let mut d = PlumDocument::new_project("repeat".into(), Bpm::parse("120").unwrap(), 480);
    d.add_track().unwrap();
    let recs = [
        note(0, 400, 60, 90, 0),
        note(480, 200, 64, 70, 1),
        note(960, 480, 67, 50, 2),
        note(1440, 400, 72, 127, 3),
        note(20_000, 100, 40, 10, 0),
    ];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(
            &[note(0, 960, 48, 80, 0), note(1920, 960, 50, 80, 0)],
            false,
        )
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    if !tempo.is_empty() {
        let pts: Vec<(u64, TempoValue, Interp)> = tempo
            .iter()
            .map(|&(t, b, i)| (t, TempoValue::Bpm(Bpm::parse(b).unwrap()), i))
            .collect();
        let edit = d.conductor().prepare_tempo_points(&pts, false).unwrap();
        apply(
            &mut d,
            &mut h,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        );
    }
    for &(t, n, p) in meter {
        let edit = d.conductor().prepare_set_signature(None, t, n, p).unwrap();
        apply(
            &mut d,
            &mut h,
            Operation::Conductor {
                edit,
                label: "Meter",
            },
        );
    }
    d.track_mut(0).unwrap().notes.select_range(0, 2000, false);
    d
}

/// The context the app gives a tool: meter, tempo map at 1 µs, snap, shift.
fn ctx(d: &PlumDocument, snap: u64, shift: i128) -> Ctx {
    let mut c = Ctx::simple(480);
    c.meter = MeterMap::new(d.division(), &d.meter_events(0));
    c.tempo = Some(Arc::new(plumspirit_engine::model::TempoMap::for_document(
        d, TEMPO_RATE,
    )));
    c.snap = snap.max(1);
    c.shift = shift;
    c
}

fn params(count: i64, even: bool, every: &str, unit: RepeatUnit) -> ParamSet {
    let def = tool("paste_repeat_notes").unwrap();
    let mut v = ParamSet::defaults(&(def.spec)().params);
    v.set("count", ParamValue::Int(count));
    v.set("spacing", ParamValue::Choice(u32::from(even)));
    v.set("every", ParamValue::Text(every.into()));
    let u = RepeatUnit::ALL.iter().position(|&x| x == unit).unwrap();
    v.set("unit", ParamValue::Choice(u as u32));
    v
}

/// Runs Paste Repeat on track 0's selection; the operation or the refusal.
fn run(
    d: &mut PlumDocument,
    values: &ParamSet,
    snap: u64,
    shift: i128,
) -> Result<Operation, TransformError> {
    let c = &ctx(d, snap, shift);
    let def = tool("paste_repeat_notes").unwrap();
    let scope = [TrackScope::of(NoteTarget::Track(0), false)];
    let src = collect_for(def, d, &scope, 10_000_000).unwrap();
    let b = BuildCtx {
        ctx: c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(values, &b)?;
    let r = run_kernel(
        k.as_ref(),
        &src,
        def.boundary(values),
        &Limits::default(),
        None,
    )
    .unwrap();
    Ok(build_operation(d, &r, "Paste Repeat").unwrap())
}

/// Track `i`'s notes as (start, duration, pitch, velocity, channel).
fn notes(d: &PlumDocument, i: usize) -> Vec<(u64, u64, u8, u8, u8)> {
    let mut v: Vec<_> = d
        .track(i)
        .unwrap()
        .notes
        .iter()
        .map(|n| {
            let r = n.record();
            (
                r.start_tick,
                r.duration_ticks,
                r.attrs.pitch(),
                r.attrs.velocity(),
                r.attrs.channel(),
            )
        })
        .collect();
    v.sort_unstable();
    v
}

/// What the selection plus copies at `offsets` must be: the four selected
/// notes at each offset (and in place), the unselected note once.
fn expected(offsets: &[u64]) -> Vec<(u64, u64, u8, u8, u8)> {
    let base = [
        (0, 400, 60, 90, 0),
        (480, 200, 64, 70, 1),
        (960, 480, 67, 50, 2),
        (1440, 400, 72, 127, 3),
    ];
    let mut v = vec![(20_000, 100, 40, 10, 0)];
    for &off in std::iter::once(&0).chain(offsets) {
        v.extend(
            base.iter()
                .map(|&(t, d, p, vel, ch)| (t + off, d, p, vel, ch)),
        );
    }
    v.sort_unstable();
    v
}

#[test]
fn end_to_end_copies_every_selected_note_with_its_attributes_in_one_undo_step() {
    let mut d = song(&[], &[]);
    let mut h = History::new(HistoryLimits::default());
    let before = d.state_fingerprint();
    let other = notes(&d, 1);
    // Span 0 → 1840, rounded up to the 1/16 snap (120): copies every 1,920.
    let op = run(
        &mut d,
        &params(3, false, "garbage is ignored", RepeatUnit::Seconds),
        120,
        0,
    )
    .unwrap();
    assert_eq!(op.label(), "Paste Repeat");
    apply(&mut d, &mut h, op);
    assert_eq!(notes(&d, 0), expected(&[1920, 3840, 5760]));
    assert_eq!(notes(&d, 1), other, "another track is untouched");
    let after = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    // Snap off: exactly end to end.
    let mut d = song(&[], &[]);
    let op = run(&mut d, &params(2, false, "1", RepeatUnit::Seconds), 1, 0).unwrap();
    apply(&mut d, &mut h, op);
    assert_eq!(notes(&d, 0), expected(&[1840, 3680]));
}

#[test]
fn even_spacing_follows_bars_across_a_signature_change_also_in_a_clip_session() {
    // 4/4 for two bars, then 3/4 from tick 3,840.
    let mut d = song(&[], &[(3840, 3, 2)]);
    let op = run(&mut d, &params(3, true, "1", RepeatUnit::Bars), 120, 0).unwrap();
    History::default().apply(&mut d, op).unwrap();
    assert_eq!(notes(&d, 0), expected(&[1920, 3840, 5280]));
    // The same notes as a clip's content starting at song tick 1,920: the
    // bars counted are the song's (4/4, then 3/4, 3/4).
    let mut d = song(&[], &[(3840, 3, 2)]);
    let op = run(&mut d, &params(3, true, "1", RepeatUnit::Bars), 120, 1920).unwrap();
    History::default().apply(&mut d, op).unwrap();
    assert_eq!(notes(&d, 0), expected(&[1920, 3360, 4800]));
    // Beats and a note value.
    let mut d = song(&[], &[]);
    let op = run(&mut d, &params(2, true, "3", RepeatUnit::Note(8)), 120, 0).unwrap();
    History::default().apply(&mut d, op).unwrap();
    assert_eq!(notes(&d, 0), expected(&[720, 1440]));
}

#[test]
fn even_spacing_in_seconds_is_real_time_under_a_tempo_step() {
    // 120 BPM (960 ticks a second) until tick 1,920, then 60 BPM (480).
    let mut d = song(&[(1920, "60", Interp::Step)], &[]);
    let op = run(&mut d, &params(4, true, "1", RepeatUnit::Seconds), 120, 0).unwrap();
    History::default().apply(&mut d, op).unwrap();
    assert_eq!(notes(&d, 0), expected(&[960, 1920, 2400, 2880]));
    let mut d = song(&[(1920, "60", Interp::Step)], &[]);
    let op = run(
        &mut d,
        &params(2, true, "1500", RepeatUnit::Milliseconds),
        120,
        0,
    )
    .unwrap();
    History::default().apply(&mut d, op).unwrap();
    assert_eq!(notes(&d, 0), expected(&[1440, 2400]));
}

#[test]
fn refusals_are_sentences_and_change_nothing() {
    let mut d = song(&[], &[]);
    for (count, every, unit, want) in [
        (3, "0", RepeatUnit::Seconds, "more than 0"),
        (3, "abc", RepeatUnit::Seconds, "is not a number"),
        (3, "1.5", RepeatUnit::Bars, "Bars take whole numbers"),
        (3, "1", RepeatUnit::Frames, "SMPTE"),
        (3, "200", RepeatUnit::Microseconds, "shorter than one tick"),
    ] {
        match run(&mut d, &params(count, true, every, unit), 120, 0) {
            Err(TransformError::Invalid(s)) => assert!(s.contains(want), "{every} {unit:?}: {s}"),
            Err(e) => panic!("{every} {unit:?}: {e:?}"),
            Ok(_) => panic!("{every} {unit:?} was accepted"),
        }
    }
}

// ------------------------------------------- S14: the per-copy functions

/// `params` with advanced function `n` switched on and `set` applied.
fn with(mut v: ParamSet, n: u8, set: &[(&str, ParamValue)]) -> ParamSet {
    v.set(&format!("a{n}"), ParamValue::Bool(true));
    for (k, val) in set {
        v.set(k, val.clone());
    }
    v
}

/// Track 0's notes as (start, duration, pitch, velocity, channel, subchannel).
fn notes_sub(d: &PlumDocument) -> Vec<(u64, u64, u8, u8, u8, u32)> {
    let mut v: Vec<_> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| {
            let r = n.record();
            (
                r.start_tick,
                r.duration_ticks,
                r.attrs.pitch(),
                r.attrs.velocity(),
                r.attrs.channel(),
                r.sub.0,
            )
        })
        .collect();
    v.sort_unstable();
    v
}

/// The four selected notes of `song` (start, duration, pitch, velocity, channel).
const BASE: [(u64, u64, u8, u8, u8); 4] = [
    (0, 400, 60, 90, 0),
    (480, 200, 64, 70, 1),
    (960, 480, 67, 50, 2),
    (1440, 400, 72, 127, 3),
];

/// The expected track 0: the source, the unselected note, and per copy
/// `(offset, f)` the base notes mapped by `f` (`None` = dropped).
fn expect(
    copies: &[(u64, u32)],
    f: impl Fn(u32, (u64, u64, u8, u8, u8)) -> Option<(u64, u64, u8, u8, u8, u32)>,
) -> Vec<(u64, u64, u8, u8, u8, u32)> {
    let mut v = vec![(20_000, 100, 40, 10, 0, 0)];
    v.extend(BASE.iter().map(|&(t, d, p, vel, ch)| (t, d, p, vel, ch, 0)));
    for &(off, k) in copies {
        for &b in &BASE {
            if let Some((t, d, p, vel, ch, sub)) = f(k, b) {
                v.push((t + off, d, p, vel, ch, sub));
            }
        }
    }
    v.sort_unstable();
    v
}

fn apply_run(d: &mut PlumDocument, v: &ParamSet, snap: u64) -> Result<(), TransformError> {
    let op = run(d, v, snap, 0)?;
    History::default().apply(d, op).unwrap();
    Ok(())
}

#[test]
fn every_function_is_off_by_default_and_the_output_is_s13s() {
    let def = tool("paste_repeat_notes").unwrap();
    let defaults = ParamSet::defaults(&(def.spec)().params);
    for n in 1..=10u8 {
        assert_eq!(
            defaults.get_raw(&format!("a{n}")),
            Some(&ParamValue::Bool(false)),
            "a{n}"
        );
    }
    let mut d = song(&[], &[]);
    apply_run(&mut d, &params(3, false, "1", RepeatUnit::Seconds), 120).unwrap();
    assert_eq!(notes(&d, 0), expected(&[1920, 3840, 5760]));
}

#[test]
fn subchannel_offset_moves_the_logical_subchannel_per_copy_never_the_midi_channel() {
    // Track 0 gets subchannels 1 – 3 (MIDI channels 5 – 7: a note keeps its own).
    let song4 = || {
        let mut d = song(&[], &[]);
        let subs = (1..4u32)
            .map(|i| Subchannel::new(SubchannelId(i), 4 + i as u8))
            .collect();
        History::default()
            .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
            .unwrap();
        d
    };
    let copies = [(1920, 1), (3840, 2), (5760, 3)];
    let base = params(3, false, "1", RepeatUnit::Seconds);
    // Cycle the track's four subchannels: copy k → position k.
    let mut d = song4();
    let v = with(base.clone(), 1, &[("a1.offset", ParamValue::Int(1))]);
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| Some((
            t,
            dur,
            p,
            vel,
            ch,
            k % 4
        )))
    );
    // Within a range of two from subchannel 2 (id 1): 1, 2, 1.
    let mut d = song4();
    let v = with(
        base.clone(),
        1,
        &[
            ("a1.offset", ParamValue::Int(1)),
            ("a1.wrap", ParamValue::Choice(1)),
            ("a1.lo", ParamValue::Int(2)),
            ("a1.n", ParamValue::Int(2)),
        ],
    );
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| Some((
            t,
            dur,
            p,
            vel,
            ch,
            1 + (k - 1) % 2
        )))
    );
    // Unbounded, +2 per copy from the main subchannel: 2, 4, 6 — the
    // missing 4 and 6 are created in the same undo step.
    let mut d = song4();
    let before = d.state_fingerprint();
    let v = with(
        base,
        1,
        &[
            ("a1.offset", ParamValue::Int(2)),
            ("a1.wrap", ParamValue::Choice(4)),
        ],
    );
    let op = run(&mut d, &v, 120, 0).unwrap();
    let mut h = History::default();
    h.apply(&mut d, op).unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| Some((
            t,
            dur,
            p,
            vel,
            ch,
            2 * k
        )))
    );
    let table = d.track(0).unwrap().subchannels();
    for id in [4, 6] {
        assert!(table.contains(SubchannelId(id)), "subchannel {id} created");
    }
    let after = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    assert!(!d.track(0).unwrap().subchannels().contains(SubchannelId(6)));
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
}

#[test]
fn the_pitch_vector_adds_a_step_or_a_cycled_interval_sequence_per_copy() {
    let copies = [(1920, 1), (3840, 2), (5760, 3), (7680, 4)];
    let base = params(4, false, "1", RepeatUnit::Seconds);
    // A step: +7 per copy.
    let mut d = song(&[], &[]);
    apply_run(
        &mut d,
        &with(base.clone(), 2, &[("a2.step", ParamValue::Int(7))]),
        120,
    )
    .unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| Some((
            t,
            dur,
            p + 7 * k as u8,
            vel,
            ch,
            0
        )))
    );
    // A sequence 2, 2, −1 (cycled): +2, +4, +3, +5.
    let mut d = song(&[], &[]);
    let v = with(
        base.clone(),
        2,
        &[("a2.seq", ParamValue::List(vec![2, 2, -1]))],
    );
    apply_run(&mut d, &v, 120).unwrap();
    let shift = [0, 2, 4, 3, 5];
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            Some((t, dur, p + shift[k as usize], vel, ch, 0))
        })
    );
    // Out of the keys: the boundary policy (Discard) drops those notes only.
    let mut d = song(&[], &[]);
    apply_run(
        &mut d,
        &with(base, 2, &[("a2.step", ParamValue::Int(20))]),
        120,
    )
    .unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            let q = u32::from(p) + 20 * k;
            (q <= 127).then_some((t, dur, q as u8, vel, ch, 0))
        })
    );
}

#[test]
fn the_acceleration_graph_rescales_the_gaps_of_a_real_time_plan() {
    // Every 1 s under a 120 → 60 BPM step: gaps 960, 960, 480, 480.
    // The graph falls from the top (200 %) to the middle (100 %): factors
    // 2, 5/3, 4/3, 1 → gaps 1,920, 1,600, 640, 480.
    let mut d = song(&[(1920, "60", Interp::Step)], &[]);
    let graph = Curve::new(vec![(0.0, 1.0), (1.0, 0.5)]);
    let v = with(
        params(4, true, "1", RepeatUnit::Seconds),
        3,
        &[("a3.curve", ParamValue::Curve(graph))],
    );
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(notes(&d, 0), expected(&[1920, 3520, 4160, 4640]));
    // A graph down to the bottom puts the last copy on the one before it.
    let mut d = song(&[], &[]);
    let v = with(
        params(3, false, "1", RepeatUnit::Seconds),
        3,
        &[(
            "a3.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 0.5), (1.0, 0.0)])),
        )],
    );
    match run(&mut d, &v, 120, 0) {
        Err(TransformError::Invalid(s)) => assert!(s.contains("copy 3"), "{s}"),
        other => panic!("{:?}", other.map(|o| o.label())),
    }
}

#[test]
fn gate_and_velocity_morph_over_the_series_independently() {
    let copies = [(1920, 1), (3840, 2), (5760, 3)];
    let base = params(3, false, "1", RepeatUnit::Seconds);
    // Gate 100 % → 50 %: lengths × 1, 0.75, 0.5; velocities untouched.
    let mut d = song(&[], &[]);
    apply_run(&mut d, &with(base.clone(), 4, &[]), 120).unwrap();
    let gate = [1.0, 1.0, 0.75, 0.5];
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            Some((
                t,
                (dur as f64 * gate[k as usize]).round() as u64,
                p,
                vel,
                ch,
                0,
            ))
        })
    );
    // Velocity 100 → 40: every note of copy k gets 100, 70, 40; lengths kept.
    let mut d = song(&[], &[]);
    apply_run(&mut d, &with(base.clone(), 5, &[]), 120).unwrap();
    let target = [0, 100, 70, 40];
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, _, ch)| Some((
            t,
            dur,
            p,
            target[k as usize],
            ch,
            0
        )))
    );
    // Keeping each note's variation: every note moves by target − mean
    // (mean of 90, 70, 50, 127 = 84.25), clamped to 1 – 127.
    let mut d = song(&[], &[]);
    apply_run(
        &mut d,
        &with(base.clone(), 5, &[("a5.keep", ParamValue::Bool(true))]),
        120,
    )
    .unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            let v = (f64::from(vel) + f64::from(target[k as usize]) - 84.25).round();
            Some((t, dur, p, v.clamp(1.0, 127.0) as u8, ch, 0))
        })
    );
    // Graph mode: a flat graph at the bottom holds the start for every copy.
    let mut d = song(&[], &[]);
    let v = with(
        base,
        5,
        &[
            ("a5.mode", ParamValue::Choice(1)),
            (
                "a5.curve",
                ParamValue::Curve(Curve::new(vec![(0.0, 0.0), (1.0, 0.0)])),
            ),
        ],
    );
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |_, (t, dur, p, _, ch)| Some((
            t, dur, p, 100, ch, 0
        )))
    );
}

#[test]
fn all_five_functions_together_undo_and_redo_exactly_in_a_clip_session() {
    // Bars across 4/4 → 3/4, as a clip's content at song tick 1,920.
    let mut d = song(&[], &[(3840, 3, 2)]);
    let mut v = params(3, true, "1", RepeatUnit::Bars);
    for (n, set) in [
        (
            1u8,
            vec![
                ("a1.offset", ParamValue::Int(1)),
                ("a1.wrap", ParamValue::Choice(4)),
            ],
        ),
        (2, vec![("a2.step", ParamValue::Int(-5))]),
        (
            3,
            vec![(
                "a3.curve",
                ParamValue::Curve(Curve::new(vec![(0.0, 0.75), (1.0, 0.25)])),
            )],
        ),
        (4, vec![]),
        (5, vec![("a5.keep", ParamValue::Bool(true))]),
    ] {
        v = with(v, n, &set);
    }
    let before = d.state_fingerprint();
    let op = run(&mut d, &v, 120, 1920).unwrap();
    let mut h = History::default();
    h.apply(&mut d, op).unwrap();
    // Plan 1,920 / 3,360 / 4,800 (gaps 1,920, 1,440, 1,440) × 1.5, 1, 0.5 →
    // 2,880 / 4,320 / 5,040. The note at 0 (60, 400 ticks, velocity 90):
    // pitch −5 per copy, length × 1, 0.75, 0.5, velocity + target − 84.25
    // (targets 100, 70, 40), subchannel k (Unbounded).
    let got = notes_sub(&d);
    for (k, off, dur, vel) in [
        (1u32, 2880, 400, 106),
        (2, 4320, 300, 76),
        (3, 5040, 200, 46),
    ] {
        let want = (off, dur, 60 - 5 * k as u8, vel, 0, k);
        assert!(got.contains(&want), "copy {k}: {want:?} in {got:?}");
    }
    assert_eq!(got.len(), 1 + 4 * 4);
    let after = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
}

#[test]
fn mirror_and_reverse_change_every_other_copy_inside_the_source_box() {
    let copies = [(1920, 1), (3840, 2), (5760, 3)];
    let base = params(3, false, "1", RepeatUnit::Seconds);
    // Pitches 60 – 72: mirrored p → 132 − p on copies 1 and 3.
    let mut d = song(&[], &[]);
    apply_run(&mut d, &with(base.clone(), 6, &[]), 120).unwrap();
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            Some((t, dur, if k % 2 == 1 { 132 - p } else { p }, vel, ch, 0))
        })
    );
    // Reverse time (span 0 → 1,840): start → 1,840 − end; and both.
    for (mode, pitch, time) in [(1u32, false, true), (2, true, true)] {
        let mut d = song(&[], &[]);
        apply_run(
            &mut d,
            &with(base.clone(), 6, &[("a6.mode", ParamValue::Choice(mode))]),
            120,
        )
        .unwrap();
        assert_eq!(
            notes_sub(&d),
            expect(&copies, |k, (t, dur, p, vel, ch)| {
                let on = k % 2 == 1;
                let t = if on && time { 1840 - t - dur } else { t };
                let p = if on && pitch { 132 - p } else { p };
                Some((t, dur, p, vel, ch, 0))
            }),
            "mode {mode}"
        );
    }
    // Every third copy: copies 1 and 4.
    let mut d = song(&[], &[]);
    let v = with(
        params(4, false, "1", RepeatUnit::Seconds),
        6,
        &[("a6.every", ParamValue::Int(3))],
    );
    apply_run(&mut d, &v, 120).unwrap();
    let four = [(1920, 1), (3840, 2), (5760, 3), (7680, 4)];
    assert_eq!(
        notes_sub(&d),
        expect(&four, |k, (t, dur, p, vel, ch)| {
            Some((
                t,
                dur,
                if k == 1 || k == 4 { 132 - p } else { p },
                vel,
                ch,
                0,
            ))
        })
    );
}

#[test]
fn fracture_omits_whole_copies_and_rearranges_generations_deterministically() {
    // Chance 1 up to the middle of the series, 0 after: copies 1 – 3 of 5
    // go, 4 and 5 stay — whatever the seed. The pitch step (+k) tells the
    // generations apart.
    let v = |seed: i64, mode: u32, chance: Curve| {
        with(
            with(
                params(5, false, "1", RepeatUnit::Seconds),
                2,
                &[("a2.step", ParamValue::Int(1))],
            ),
            7,
            &[
                ("a7.mode", ParamValue::Choice(mode)),
                ("a7.curve", ParamValue::Curve(chance)),
                ("a7.seed", ParamValue::Int(seed)),
            ],
        )
    };
    let first_half = Curve::new(vec![(0.0, 1.0), (0.5, 1.0), (0.51, 0.0), (1.0, 0.0)]);
    for seed in [1, 2, 99] {
        let mut d = song(&[], &[]);
        apply_run(&mut d, &v(seed, 0, first_half.clone()), 120).unwrap();
        assert_eq!(
            notes_sub(&d),
            expect(&[(7680, 4), (9600, 5)], |k, (t, dur, p, vel, ch)| {
                Some((t, dur, p + k as u8, vel, ch, 0))
            }),
            "seed {seed}"
        );
    }
    // Rearranged: every slot keeps its time; the generations (pitch + k)
    // are a permutation of 1 – 5, the same for the same seed.
    let always = Curve::new(vec![(0.0, 1.0), (1.0, 1.0)]);
    let shifts = |seed: i64| {
        let mut d = song(&[], &[]);
        apply_run(&mut d, &v(seed, 1, always.clone()), 120).unwrap();
        let got = notes_sub(&d);
        // The copy of the note at 0 (400 ticks, pitch 60) in each slot.
        (1..=5u64)
            .map(|slot| {
                got.iter()
                    .find(|n| n.0 == slot * 1920 && n.1 == 400)
                    .map(|n| n.2 - 60)
                    .unwrap()
            })
            .collect::<Vec<u8>>()
    };
    let a = shifts(5);
    assert_eq!(a, shifts(5));
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![1, 2, 3, 4, 5]);
    assert!((1..=20).any(|s| shifts(s) != a), "seeds give other orders");
}

/// A chord of three (60, 64, 67 at tick 0, 480 ticks) on track 0, selected,
/// in subchannels 0, 1, 2 (the track has four).
fn chord_song() -> PlumDocument {
    let mut d = PlumDocument::new_project("chord".into(), Bpm::DEFAULT, 480);
    let subs = (1..4u32)
        .map(|i| Subchannel::new(SubchannelId(i), i as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    let recs: Vec<NoteRecord> = [60u8, 64, 67]
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let mut r = note(0, 480, p, 80, 0);
            r.sub = SubchannelId(i as u32);
            r
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d.track_mut(0).unwrap().notes.select_all();
    d
}

#[test]
fn voice_phases_stagger_chord_voices_or_subchannels_by_part_of_the_spacing() {
    // End to end: one copy every 480 ticks; phases 0 / 25 / 50 % → +0 /
    // +120 / +240 for the lowest, middle and highest voice.
    let base = params(3, false, "1", RepeatUnit::Seconds);
    let starts = |d: &PlumDocument, p: u8| -> Vec<u64> {
        notes_sub(d)
            .iter()
            .filter(|n| n.2 == p)
            .map(|n| n.0)
            .collect()
    };
    let mut d = chord_song();
    let v = with(
        base.clone(),
        8,
        &[("a8.phases", ParamValue::List(vec![0, 25, 50]))],
    );
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(starts(&d, 60), vec![0, 480, 960, 1440]);
    assert_eq!(starts(&d, 64), vec![0, 600, 1080, 1560]);
    assert_eq!(starts(&d, 67), vec![0, 720, 1200, 1680]);
    // By subchannel, phases 0 / 50 cycled: subchannels 0 and 2 on the
    // copy, subchannel 1 half a spacing later.
    let mut d = chord_song();
    let v = with(
        base,
        8,
        &[
            ("a8.by", ParamValue::Choice(1)),
            ("a8.phases", ParamValue::List(vec![0, 50])),
        ],
    );
    apply_run(&mut d, &v, 120).unwrap();
    assert_eq!(starts(&d, 60), vec![0, 480, 960, 1440]);
    assert_eq!(starts(&d, 64), vec![0, 720, 1200, 1680]);
    assert_eq!(starts(&d, 67), vec![0, 480, 960, 1440]);
}

#[test]
fn geometric_drift_maps_each_copy_through_the_kth_power_in_the_source_box() {
    // Pitch scale 50 % per copy: rows squeeze toward the box's middle
    // (rows 60 – 72: 13 rows); times stay.
    let copies = [(1920, 1), (3840, 2), (5760, 3)];
    let base = params(3, false, "1", RepeatUnit::Seconds);
    let mut d = song(&[], &[]);
    let v = with(
        base.clone(),
        9,
        &[
            ("a9.rotate", ParamValue::Float(0.0)),
            ("a9.scale_y", ParamValue::Float(50.0)),
        ],
    );
    apply_run(&mut d, &v, 120).unwrap();
    let row = |p: u8, k: u32| -> u8 {
        let v = (f64::from(p) + 0.5 - 60.0) / 13.0;
        let v = 0.5 + 0.5f64.powi(k as i32) * (v - 0.5);
        (60.0 + 13.0 * v).floor() as u8
    };
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| Some((
            t,
            dur,
            row(p, k),
            vel,
            ch,
            0
        )))
    );
    // Time scale 50 % per copy around the box's middle (0 → 1,840: 920).
    let mut d = song(&[], &[]);
    let v = with(
        base,
        9,
        &[
            ("a9.rotate", ParamValue::Float(0.0)),
            ("a9.scale_x", ParamValue::Float(50.0)),
        ],
    );
    apply_run(&mut d, &v, 120).unwrap();
    let x = |t: u64, k: u32| -> f64 { 920.0 + 0.5f64.powi(k as i32) * (t as f64 - 920.0) };
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |k, (t, dur, p, vel, ch)| {
            let (a, e) = (x(t, k).round() as u64, x(t + dur, k).round() as u64);
            Some((a, (e - a).max(1), p, vel, ch, 0))
        })
    );
}

#[test]
fn the_recursive_tree_adds_bounded_child_blocks_and_refuses_too_many() {
    // End to end every 1,920; depth 1, two children half a spacing apart.
    let mut d = song(&[], &[]);
    let v = with(
        params(2, false, "1", RepeatUnit::Seconds),
        10,
        &[("a10.depth", ParamValue::Int(1))],
    );
    apply_run(&mut d, &v, 120).unwrap();
    let copies = [
        (1920, 1),
        (3840, 2),
        (2880, 1),
        (3840, 1),
        (4800, 2),
        (5760, 2),
    ];
    assert_eq!(
        notes_sub(&d),
        expect(&copies, |_, (t, dur, p, vel, ch)| Some((
            t, dur, p, vel, ch, 0
        )))
    );
    // 100 copies × (1 + 16 + … + 16⁴) is far past the limit: a sentence.
    let mut d = song(&[], &[]);
    let v = with(
        params(100, false, "1", RepeatUnit::Seconds),
        10,
        &[
            ("a10.depth", ParamValue::Int(4)),
            ("a10.children", ParamValue::Int(16)),
        ],
    );
    match run(&mut d, &v, 120, 0) {
        Err(TransformError::Invalid(s)) => assert!(s.contains("6,990,500 copies"), "{s}"),
        other => panic!("{:?}", other.map(|o| o.label())),
    }
}

// ------------------------------------------------------------------ clips

/// `song` plus an added one-bar clip on track 1 at bar 2 (tick 1,920).
fn clip_song(h: &mut History, tempo: &[(u64, &str, Interp)]) -> (PlumDocument, Vec<ClipId>) {
    let mut d = song(tempo, &[]);
    let first = d.clips().next_ids().0;
    let op = add_clip(&d, 1, 1920, 1920).unwrap();
    apply(&mut d, h, op);
    let ids = vec![
        ClipId::Primary(d.track(0).unwrap().id()),
        ClipId::Placed(first),
    ];
    (d, ids)
}

fn spec(count: u32, spacing: Spacing, every: &str, unit: RepeatUnit) -> RepeatSpec {
    RepeatSpec {
        count,
        spacing,
        every: Decimal::parse(every).unwrap(),
        unit,
    }
}

/// The explicit placements as (id, track index, start, content).
fn placements(d: &PlumDocument) -> Vec<(ClipId, usize, u64, ContentRef)> {
    d.clips()
        .explicit_placements()
        .iter()
        .map(|p| (p.id, d.index_of(p.track).unwrap(), p.start, p.content))
        .collect()
}

#[test]
fn clips_repeat_linked_and_independent_in_one_undo_step_and_round_trip() {
    for linked in [true, false] {
        let mut h = History::new(HistoryLimits::default());
        let (mut d, ids) = clip_song(&mut h, &[]);
        let before = d.state_fingerprint();
        let n0 = placements(&d).len();
        let next = d.clips().next_ids().0;
        // Span: track 0's notes reach 20,100; the added clip 1,920 → 3,840.
        let plan = plan_repeat_clips(
            &d,
            &ids,
            &spec(2, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
            Some(480),
        )
        .unwrap();
        assert_eq!(plan.offsets, vec![20_160, 40_320]);
        let op = repeat_clips(&d, &ids, &plan, linked).unwrap();
        apply(&mut d, &mut h, op);
        let p = placements(&d);
        assert_eq!(p.len(), n0 + 4);
        let new: Vec<_> = p
            .iter()
            .filter(|x| matches!(x.0, ClipId::Placed(k) if k >= next))
            .collect();
        let starts: Vec<(usize, u64)> = new.iter().map(|x| (x.1, x.2)).collect();
        assert_eq!(
            starts,
            vec![(0, 20_160), (1, 22_080), (0, 40_320), (1, 42_240)]
        );
        let t0 = d.track(0).unwrap().id();
        for x in &new {
            match (linked, x.3) {
                (true, c) => {
                    assert!(c == ContentRef::Track(t0) || matches!(c, ContentRef::Owned(_)))
                }
                (false, ContentRef::Owned(_)) => {}
                (false, c) => panic!("an independent copy shares {c:?}"),
            }
        }
        if linked {
            assert_eq!(
                new[0].3,
                ContentRef::Track(t0),
                "a linked copy of the primary plays the track's notes"
            );
            assert_eq!(d.clips().contents().len(), 1, "no new content");
        } else {
            assert_eq!(d.clips().contents().len(), 5, "one new content per copy");
        }
        let after = d.state_fingerprint();
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), before, "linked {linked}");
        h.redo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), after);
        // Save and reopen.
        let dir =
            std::env::temp_dir().join(format!("plumspirit-repeat-{}-{linked}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("song.lup");
        save_project(&d, &file, LupControl::default()).unwrap();
        let (back, _) = load_project(&file, LupControl::default()).unwrap();
        assert_eq!(back.state_fingerprint(), after);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn clip_repeats_in_seconds_follow_the_tempo_and_too_many_are_refused() {
    let mut h = History::new(HistoryLimits::default());
    let (d, ids) = clip_song(&mut h, &[(1920, "60", Interp::Step)]);
    // Anchor tick 0: 1 s = 960 ticks until 1,920, then 480 ticks a second.
    let plan = plan_repeat_clips(
        &d,
        &ids,
        &spec(4, Spacing::Even, "1", RepeatUnit::Seconds),
        None,
    )
    .unwrap();
    assert_eq!(plan.offsets, vec![960, 1920, 2400, 2880]);
    let e = plan_repeat_clips(
        &d,
        &ids,
        &spec(
            MAX_NEW_CLIPS as u32 / 2 + 1,
            Spacing::EndToEnd,
            "1",
            RepeatUnit::Ticks,
        ),
        None,
    )
    .unwrap_err();
    assert!(e.0.contains("at most 1,000,000"), "{}", e.0);
    assert!(
        plan_repeat_clips(
            &d,
            &[],
            &spec(1, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
            None
        )
        .is_err()
    );
}

#[test]
fn a_clip_repeat_survives_a_restart_in_the_journal() {
    let dir = std::env::temp_dir().join(format!("plumspirit-repeat-j-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("song.lup");
    let jpath = dir.join("song.plh");
    let mut h = History::new(HistoryLimits::default());
    let (mut d, ids) = clip_song(&mut h, &[]);
    save_project(&d, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&d)),
    )
    .unwrap();
    let before = d.state_fingerprint();
    let plan = plan_repeat_clips(
        &d,
        &ids,
        &spec(3, Spacing::Even, "2", RepeatUnit::Bars),
        None,
    )
    .unwrap();
    let op = repeat_clips(&d, &ids, &plan, false).unwrap();
    apply(&mut d, &mut h, op);
    let after = d.state_fingerprint();
    save_project(&d, &file, LupControl::default()).unwrap();
    let st = h.current_state();
    h.mark_saved(st);
    h.persist_saved(st, identity(&d));
    drop(h);
    let (mut d, _) = load_project(&file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&d)).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cost of the functions (release): 1,000,000 selected notes (chords of
/// four) pasted 4 times — plain, with the drift, and with all ten on (chord
/// voices, the drift, a depth-1 tree with one child).
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn paste_repeat_functions_cost_at_scale() {
    use std::time::Instant;
    let mut d = PlumDocument::new_project("big".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..1_000_000u64)
        .map(|k| note((k / 4) * 8, 6, 30 + (k % 60) as u8, 90, 0))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d.track_mut(0).unwrap().notes.select_all();
    let c = ctx(&d, 240, 0);
    let def = tool("paste_repeat_notes").unwrap();
    let scope = [TrackScope::of(NoteTarget::Track(0), false)];
    let src = collect_for(def, &d, &scope, 100_000_000).unwrap();
    let b = BuildCtx {
        ctx: &c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let mut all = params(4, false, "1", RepeatUnit::Seconds);
    for n in 1..=10u8 {
        all = with(all, n, &[]);
    }
    all.set("a10.depth", ParamValue::Int(1));
    all.set("a10.children", ParamValue::Int(1));
    let drift = with(params(4, false, "1", RepeatUnit::Seconds), 9, &[]);
    let plain = params(4, false, "1", RepeatUnit::Seconds);
    for (name, v) in [("plain", &plain), ("drift", &drift), ("all ten", &all)] {
        let t = Instant::now();
        let k = def.kernel(v, &b).unwrap();
        let build_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let r = run_kernel(k.as_ref(), &src, def.boundary(v), &Limits::default(), None).unwrap();
        let run_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "paste repeat functions, 1,000,000 notes x 4, {name}: build {build_ms:.1} ms, kernel run {run_ms:.1} ms, {} notes out",
            r.notes.len()
        );
    }
}

/// Cost at scale (release): a 1,000,000-note selection pasted 16 times
/// (plan, kernel run, operation build, apply) and 10,000 clips × 100 linked
/// copies.
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn paste_repeat_cost_at_scale() {
    use std::time::Instant;
    let mut d = PlumDocument::new_project("big".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..1_000_000u64)
        .map(|k| note(k * 2, 2, 30 + (k % 60) as u8, 90, 0))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d.track_mut(0).unwrap().notes.select_all();
    let c = ctx(&d, 240, 0);
    let def = tool("paste_repeat_notes").unwrap();
    let values = params(16, false, "1", RepeatUnit::Seconds);
    let t = Instant::now();
    let scope = [TrackScope::of(NoteTarget::Track(0), false)];
    let src = collect_for(def, &d, &scope, 100_000_000).unwrap();
    let b = BuildCtx {
        ctx: &c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&values, &b).unwrap();
    let plan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let r = run_kernel(
        k.as_ref(),
        &src,
        def.boundary(&values),
        &Limits::default(),
        None,
    )
    .unwrap();
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let op = build_operation(&mut d, &r, "Paste Repeat").unwrap();
    // The app's largest undo budget (RAM / 16, at most 512 MiB): the step
    // holds 16,000,000 new notes.
    let mut h = History::new(HistoryLimits {
        max_bytes: 512 << 20,
        max_entries: 2000,
    });
    if h.apply(&mut d, op).is_err() {
        panic!("the paste does not fit the largest undo budget");
    }
    let commit_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(d.track(0).unwrap().notes.len(), 17_000_000);
    println!(
        "paste repeat 1,000,000 notes x 16: collect + plan: {plan_ms:.1} ms, kernel run: {run_ms:.1} ms, build + apply: {commit_ms:.1} ms"
    );
    // Clips.
    let mut d = PlumDocument::new_project("clips".into(), Bpm::DEFAULT, 960);
    let mut h = History::new(HistoryLimits::default());
    let first = d.clips().next_ids().0;
    let mut ids = Vec::new();
    for k in 0..10_000u64 {
        let op = add_clip(&d, 0, k * 960, 960).unwrap();
        h.apply(&mut d, op).unwrap();
        ids.push(ClipId::Placed(first + k as u32));
    }
    let t = Instant::now();
    let plan = plan_repeat_clips(
        &d,
        &ids,
        &spec(100, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
        Some(240),
    )
    .unwrap();
    let plan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let op = repeat_clips(&d, &ids, &plan, true).unwrap();
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    h.apply(&mut d, op).unwrap();
    let apply_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(d.clips().explicit_placements().len(), 1_010_000);
    println!(
        "paste repeat 10,000 clips x 100 (linked): plan: {plan_ms:.1} ms, build: {build_ms:.1} ms, apply: {apply_ms:.1} ms"
    );
}
