//! Behavioural fixtures for the FL-inspired tools (TEST_MATRIX T-FID-*), built
//! from the documented controls (docs/references/v0_7/FL_TOOL_BEHAVIOR.md).
//! They compare generated timing, pitch, gate and properties — never pixels.

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TNote, TrackScope, run_kernel, tool};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// Runs tool `id` on `notes` (all selected, one track, 960 PPQN) with `set`.
fn run(id: &str, notes: &[(u64, u64, u8)], set: &[(&str, ParamValue)]) -> Vec<TNote> {
    let mut d = PlumDocument::new_project("fid".into(), Bpm::DEFAULT, 960);
    let mut v: Vec<NoteRecord> = notes.iter().map(|&(s, l, p)| rec(s, l, p)).collect();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, val) in set {
        ps.set(k, val.clone());
    }
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(t, &d, &sc, 1 << 24).unwrap();
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
        .unwrap()
        .notes
}

fn spd(v: &[TNote]) -> Vec<(u64, u8, u64)> {
    v.iter().map(|n| (n.start, n.pitch, n.dur)).collect()
}

fn ch(i: u32) -> ParamValue {
    ParamValue::Choice(i)
}

const CEG: &[(u64, u64, u8)] = &[(0, 1920, 60), (0, 1920, 64), (0, 1920, 67)];

#[test]
fn arpeggiator_patterns_follow_the_documented_controls() {
    // Up, 1/16 steps, range 1, sync Block, gate 100 %.
    let up = run("arpeggiate", CEG, &[("gate", ParamValue::Float(100.0))]);
    let pitches: Vec<u8> = up.iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![60, 64, 67, 60, 64, 67, 60, 64]);
    assert!(
        up.iter()
            .enumerate()
            .all(|(i, n)| n.start == i as u64 * 240 && n.dur == 240)
    );
    // Down; Flip vertical of Up equals Down.
    let down = run("arpeggiate", CEG, &[("pattern", ch(1))]);
    assert_eq!(
        down.iter().map(|n| n.pitch).take(3).collect::<Vec<_>>(),
        vec![67, 64, 60]
    );
    let flipped = run("arpeggiate", CEG, &[("mode", ch(1))]);
    assert_eq!(spd(&flipped), spd(&down));
    // Alternate: normal, flipped, normal…
    let alt = run("arpeggiate", CEG, &[("mode", ch(2))]);
    assert_eq!(
        alt.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![60, 64, 67, 67, 64, 60, 60, 64]
    );
    // Range 2 octaves, Normal and Alternate range patterns.
    let r2 = run("arpeggiate", CEG, &[("range", ParamValue::Int(2))]);
    assert_eq!(
        r2.iter().map(|n| n.pitch).take(6).collect::<Vec<_>>(),
        vec![60, 64, 67, 72, 76, 79]
    );
    let r2a = run(
        "arpeggiate",
        CEG,
        &[("range", ParamValue::Int(2)), ("range.mode", ch(2))],
    );
    assert_eq!(
        r2a.iter().map(|n| n.pitch).take(8).collect::<Vec<_>>(),
        vec![60, 64, 67, 72, 76, 79, 76, 72]
    );
    // Gate 50 % halves the steps.
    let g = run("arpeggiate", CEG, &[("gate", ParamValue::Float(50.0))]);
    assert!(g.iter().all(|n| n.dur == 120));
    // Sync: Time plays the pattern once; Chord stops when any chord note ends.
    let time = run("arpeggiate", CEG, &[("sync", ch(0))]);
    assert_eq!(time.len(), 3);
    let short = [(0u64, 960u64, 60u8), (0, 1920, 64), (0, 1920, 67)];
    let chord = run(
        "arpeggiate",
        &short,
        &[("sync", ch(2)), ("gate", ParamValue::Float(100.0))],
    );
    assert_eq!(chord.len(), 4);
    let block = run("arpeggiate", &short, &[("sync", ch(1))]);
    assert_eq!(block.len(), 8);
    // Levels: the pattern's velocity replaces the source's.
    let lv = run(
        "arpeggiate",
        CEG,
        &[
            ("levels", ParamValue::Float(1.0)),
            ("pvel", ParamValue::List(vec![80])),
        ],
    );
    assert!(lv.iter().all(|n| n.vel == 80));
    // Time multiplication ×2 slower: 1/8 steps.
    let slow = run("arpeggiate", CEG, &[("mul", ch(4))]);
    assert_eq!(slow.len(), 4);
    assert_eq!(slow[1].start, 480);
}

#[test]
fn articulate_sets_lengths_relative_to_the_next_onset() {
    let notes = [(0u64, 100u64, 60u8), (480, 100, 62), (960, 100, 64)];
    let legato = run("articulate", &notes, &[]);
    assert_eq!(
        legato.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![480, 480, 100]
    );
    let stacc = run("articulate", &notes, &[("style", ch(2))]);
    assert_eq!(stacc[0].dur, 216);
    let portato = run("articulate", &notes, &[("style", ch(1))]);
    assert_eq!(portato[0].dur, 384);
    // Use lengths: Custom 50 % of the written length.
    let own = run(
        "articulate",
        &notes,
        &[
            ("style", ch(3)),
            ("multiply", ParamValue::Float(50.0)),
            ("use_lengths", ParamValue::Bool(true)),
        ],
    );
    assert_eq!(own[0].dur, 50);
    // Small gap before the next onset.
    let gap = run("articulate", &notes, &[("gap", ParamValue::Bool(true))]);
    assert_eq!(gap[0].dur, 465);
    // Just chop chords: a held chord is cut where the next onset starts.
    let held = [(0u64, 960u64, 60u8), (0, 960, 64), (480, 480, 67)];
    let cut = run(
        "articulate",
        &held,
        &[("just_chop", ParamValue::Bool(true))],
    );
    assert_eq!(
        cut.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![480, 480, 480]
    );
}

#[test]
fn chopper_slices_relative_or_grid_aligned() {
    let even = run("chop", &[(0, 960, 60)], &[]);
    assert_eq!(
        spd(&even),
        vec![(0, 60, 240), (240, 60, 240), (480, 60, 240), (720, 60, 240)]
    );
    // Relative to the note start vs aligned to the grid (Absolute pattern).
    let rel = run("chop", &[(100, 960, 60)], &[]);
    assert_eq!(
        rel.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![100, 340, 580, 820]
    );
    let abs = run(
        "chop",
        &[(100, 960, 60)],
        &[("absolute", ParamValue::Bool(true))],
    );
    assert_eq!(
        spd(&abs),
        vec![
            (100, 60, 140),
            (240, 60, 240),
            (480, 60, 240),
            (720, 60, 240),
            (960, 60, 100)
        ]
    );
    // Gallop 1+1+2 over two beats.
    let gallop = run("chop", &[(0, 1920, 60)], &[("pattern", ch(2))]);
    assert_eq!(
        gallop.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        vec![
            (0, 240),
            (240, 240),
            (480, 480),
            (960, 240),
            (1200, 240),
            (1440, 480)
        ]
    );
    // Slice transposition and pattern velocities through Levels.
    let tv = run(
        "chop",
        &[(0, 480, 60)],
        &[
            ("ptrans", ParamValue::List(vec![0, 12])),
            ("pvel", ParamValue::List(vec![100, 50])),
        ],
    );
    assert_eq!(
        tv.iter().map(|n| (n.pitch, n.vel)).collect::<Vec<_>>(),
        vec![(60, 100), (72, 50)]
    );
    // A hold slice joins the next one.
    let hold = run(
        "chop",
        &[(0, 960, 60)],
        &[("holds", ParamValue::List(vec![1, 0]))],
    );
    assert_eq!(
        hold.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![480, 480]
    );
}

#[test]
fn legato_fills_gaps_and_exact_touch_also_trims_overlaps() {
    let notes = [(0u64, 100u64, 60u8), (480, 900, 62), (960, 50, 64)];
    let fill = run("legato", &notes, &[]);
    assert_eq!(
        fill.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![480, 900, 50]
    );
    let exact = run("legato", &notes, &[("a1", ParamValue::Bool(true))]);
    assert_eq!(
        exact.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![480, 480, 50]
    );
    let gap = run(
        "legato",
        &notes,
        &[
            ("a3", ParamValue::Bool(true)),
            ("a3.ticks", ParamValue::Int(30)),
        ],
    );
    assert_eq!(gap[0].dur, 450);
    let pct = run(
        "legato",
        &notes,
        &[
            ("a4", ParamValue::Bool(true)),
            ("a4.pct", ParamValue::Float(50.0)),
        ],
    );
    assert_eq!(pct[0].dur, 720);
    // Same pitch only: no later note of the same pitch → unchanged.
    let same = run("legato", &notes, &[("a6", ParamValue::Bool(true))]);
    assert_eq!(
        same.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![100, 900, 50]
    );
    // Never shorter than written keeps the overlapping note.
    let keep = run(
        "legato",
        &notes,
        &[
            ("a1", ParamValue::Bool(true)),
            ("a10", ParamValue::Bool(true)),
            ("a10.mode", ch(0)),
        ],
    );
    assert_eq!(keep[1].dur, 900);
}

#[test]
fn quantizer_at_full_strength_equals_the_v05_quantize() {
    use plumspirit_engine::editor::quantize::{QuantizeMode, QuantizeSpec, quantize_record};
    let notes: Vec<(u64, u64, u8)> = (0..200u64)
        .map(|i| {
            (
                i * 97 + (i * i) % 53,
                30 + (i * 31) % 400,
                40 + (i % 50) as u8,
            )
        })
        .collect();
    // Quantize duration (FL) == v0.5 "Both"; Leave duration == v0.5 "Ticks".
    for (mode, v05) in [(0u32, QuantizeMode::Both), (2, QuantizeMode::Ticks)] {
        let out = run("quantize", &notes, &[("mode", ch(mode))]);
        let spec = QuantizeSpec {
            mode: v05,
            grid: 240,
            origin: 0,
        };
        let mut want: Vec<(u64, u8, u64)> = notes
            .iter()
            .map(|&(s, l, p)| {
                let q = quantize_record(&rec(s, l, p), &spec).unwrap();
                (q.start_tick, p, q.duration_ticks)
            })
            .collect();
        want.sort();
        let mut got = spd(&out);
        got.sort();
        assert_eq!(got, want, "mode {mode}");
    }
    // 50 % start strength halves the distance to the grid.
    let half = run(
        "quantize",
        &[(100, 50, 60)],
        &[("start", ParamValue::Float(50.0))],
    );
    assert_eq!(half[0].start, 50, "nearest grid point 0, half way");
    // Sensitivity 10 %: a note 100 ticks off a 240 grid is not touched.
    let sens = run(
        "quantize",
        &[(100, 50, 60)],
        &[("sens", ParamValue::Float(10.0))],
    );
    assert_eq!(sens[0].start, 100);
    // Quantize end time: the end snaps, the start too.
    let end = run("quantize", &[(100, 300, 60)], &[("mode", ch(1))]);
    assert_eq!((end[0].start, end[0].end()), (0, 480));
    // Groove template: onsets at 0 and 14/48 beat (swung 16ths).
    let groove = run(
        "quantize",
        &[(250, 50, 60)],
        &[
            ("groove", ParamValue::Bool(true)),
            ("groove.pos", ParamValue::List(vec![0, 14, 24, 38])),
        ],
    );
    assert_eq!(groove[0].start, 280);
}

#[test]
fn randomizer_generates_in_key_and_repeats_with_the_seed() {
    let sel = [(0u64, 3840u64, 60u8)];
    let a = run(
        "randomize",
        &sel,
        &[("population", ParamValue::Float(80.0))],
    );
    let b = run(
        "randomize",
        &sel,
        &[("population", ParamValue::Float(80.0))],
    );
    assert_eq!(a, b);
    let c = run(
        "randomize",
        &sel,
        &[
            ("population", ParamValue::Float(80.0)),
            ("seed", ParamValue::Int(99)),
        ],
    );
    assert_ne!(a, c);
    // Kept selection + generated notes, all in C major, inside the range.
    let generated: Vec<&TNote> = a
        .iter()
        .filter(|n| !(n.start == 0 && n.dur == 3840))
        .collect();
    assert!(!generated.is_empty());
    let major = [0, 2, 4, 5, 7, 9, 11];
    assert!(
        generated
            .iter()
            .all(|n| major.contains(&(n.pitch % 12)) && (48..=60).contains(&n.pitch))
    );
    assert!(generated.iter().all(|n| n.start < 3840 && n.dur == 240));
    let none = run("randomize", &sel, &[("population", ParamValue::Float(0.0))]);
    assert_eq!(none.len(), 1);
    // Levels mode keeps timing and pitch.
    let lv = run("randomize", &sel, &[("mode", ch(0))]);
    assert_eq!((lv[0].start, lv[0].dur, lv[0].pitch), (0, 3840, 60));
}

#[test]
fn strum_spreads_chords_low_to_high_or_high_to_low() {
    let c = [(0u64, 960u64, 60u8), (0, 960, 64), (0, 960, 67)];
    let up = run("strum", &c, &[("time", ParamValue::Int(60))]);
    assert_eq!(spd(&up), vec![(0, 60, 960), (30, 64, 960), (60, 67, 960)]);
    let down = run("strum", &c, &[("time", ParamValue::Int(-60))]);
    let mut d: Vec<(u8, u64)> = down.iter().map(|n| (n.pitch, n.start)).collect();
    d.sort_unstable();
    assert_eq!(d, vec![(60, 60), (64, 30), (67, 0)]);
    // Preserve end keeps the chord's end.
    let keep = run(
        "strum",
        &c,
        &[
            ("time", ParamValue::Int(60)),
            ("preserve_end", ParamValue::Bool(true)),
        ],
    );
    assert!(keep.iter().all(|n| n.end() == 960));
    // Velocity +50 % decreases across the strum.
    let v = run(
        "strum",
        &c,
        &[
            ("time", ParamValue::Int(60)),
            ("vel", ParamValue::Float(50.0)),
        ],
    );
    assert_eq!(
        v.iter().map(|n| n.vel).collect::<Vec<_>>(),
        vec![100, 75, 50]
    );
    // Trigger ahead straddles the original start.
    let ahead = run(
        "strum",
        &[(1000, 960, 60), (1000, 960, 64), (1000, 960, 67)],
        &[
            ("time", ParamValue::Int(60)),
            ("ahead", ParamValue::Bool(true)),
        ],
    );
    assert_eq!(
        ahead.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![970, 1000, 1030]
    );
    // Alternate direction on the second chord.
    let two = [
        (0u64, 400u64, 60u8),
        (0, 400, 64),
        (480, 400, 60),
        (480, 400, 64),
    ];
    let alt = run(
        "strum",
        &two,
        &[
            ("time", ParamValue::Int(40)),
            ("alternate", ParamValue::Bool(true)),
        ],
    );
    let second: Vec<(u8, u64)> = alt
        .iter()
        .filter(|n| n.start >= 480)
        .map(|n| (n.pitch, n.start))
        .collect();
    assert_eq!(second, vec![(64, 480), (60, 520)]);
}

#[test]
fn flam_adds_grace_strokes_before_the_note() {
    let f = run(
        "flam",
        &[(960, 480, 60)],
        &[
            ("absolute", ParamValue::Bool(false)),
            ("time", ParamValue::Float(30.0)),
            ("vel", ParamValue::Float(50.0)),
        ],
    );
    assert_eq!(f.len(), 2);
    assert_eq!((f[0].start, f[0].vel), (930, 50));
    assert_eq!((f[1].start, f[1].dur, f[1].vel), (960, 480, 100));
    // After instead of before.
    let after = run(
        "flam",
        &[(960, 480, 60)],
        &[
            ("absolute", ParamValue::Bool(false)),
            ("before", ParamValue::Bool(false)),
        ],
    );
    assert!(after.iter().any(|n| n.start == 990));
    // 30 ms at 120 BPM / 960 PPQN = 57.6 ticks.
    let ms = run(
        "flam",
        &[(960, 480, 60)],
        &[("time", ParamValue::Float(30.0))],
    );
    assert_eq!(ms[0].start, 902);
}

#[test]
fn limit_snaps_to_the_scale_and_moves_by_octaves_into_the_range() {
    let n = [(0u64, 10u64, 61u8), (10, 10, 85), (20, 10, 30)];
    let above = run("limit", &n, &[]);
    // C#4 → D4; C#6 → D6 → down two octaves to D4; F#1 → G1 → up to G3.
    assert_eq!(
        above.iter().map(|x| x.pitch).collect::<Vec<_>>(),
        vec![62, 62, 55]
    );
    let under = run("limit", &n, &[("snap", ch(1))]);
    assert_eq!(under[0].pitch, 60);
    // Wrap: above the range → its lowest octave; below → its highest.
    let wrap = run("limit", &n, &[("wrap", ParamValue::Bool(true))]);
    assert_eq!(
        wrap.iter().map(|x| x.pitch).collect::<Vec<_>>(),
        vec![62, 50, 67]
    );
}

#[test]
fn chord_progression_types_romans_and_names_and_is_seeded() {
    let sel = [(0u64, 100u64, 72u8)];
    let typed = run(
        "chord_progression",
        &sel,
        &[
            ("mode", ch(1)),
            ("text", ParamValue::Text("C Am F G".into())),
            ("bass", ParamValue::Bool(false)),
        ],
    );
    let chords_at = |v: &[TNote], t: u64| -> Vec<u8> {
        let mut x: Vec<u8> = v
            .iter()
            .filter(|n| n.start == t && n.dur == 3840)
            .map(|n| n.pitch % 12)
            .collect();
        x.sort_unstable();
        x.dedup();
        x
    };
    assert_eq!(chords_at(&typed, 0), vec![0, 4, 7]);
    assert_eq!(chords_at(&typed, 3840), vec![0, 4, 9]);
    assert_eq!(chords_at(&typed, 7680), vec![0, 5, 9]);
    assert_eq!(chords_at(&typed, 11520), vec![2, 7, 11]);
    let roman = run(
        "chord_progression",
        &sel,
        &[
            ("mode", ch(1)),
            ("text", ParamValue::Text("I vi IV V".into())),
            ("bass", ParamValue::Bool(false)),
        ],
    );
    assert_eq!(spd(&roman).len(), spd(&typed).len());
    assert_eq!(chords_at(&roman, 3840), vec![0, 4, 9]);
    // The selected melody note is kept.
    assert!(typed.iter().any(|n| n.pitch == 72 && n.dur == 100));
    // Generated progressions repeat with the seed.
    let a = run("chord_progression", &sel, &[]);
    let b = run("chord_progression", &sel, &[]);
    assert_eq!(a, b);
    // Analyze finds C major from C-E-G.
    let ceg = [(0u64, 3840u64, 60u8), (0, 3840, 64), (0, 3840, 67)];
    let an = run(
        "chord_progression",
        &ceg,
        &[("mode", ch(2)), ("bass", ParamValue::Bool(false))],
    );
    assert_eq!(chords_at(&an, 0), vec![0, 4, 7]);
}

#[test]
fn chord_stamp_builds_chords_on_notes() {
    let s = run("chord_stamp", &[(0, 480, 60)], &[]);
    assert_eq!(
        s.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![60, 64, 67]
    );
    let minor_top = run(
        "chord_stamp",
        &[(0, 480, 72)],
        &[("chord", ch(1)), ("placement", ch(1))],
    );
    assert_eq!(
        minor_top.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![65, 68, 72]
    );
    let inv = run(
        "chord_stamp",
        &[(0, 480, 60)],
        &[("inversion", ParamValue::Int(1))],
    );
    assert_eq!(
        inv.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![64, 67, 72]
    );
}

#[test]
fn a_riff_with_only_arpeggiation_equals_the_arpeggiator() {
    let only_arp = [
        ("source", ch(0)),
        ("s2.on", ParamValue::Bool(false)),
        ("s3.pattern", ch(0)),
        ("s3.range", ParamValue::Int(1)),
        ("s5.vel", ParamValue::Float(0.0)),
        ("s6.style", ch(0)),
        ("s8.on", ParamValue::Bool(false)),
    ];
    let riff = run("riff", CEG, &only_arp);
    let arp = run("arpeggiate", CEG, &[]);
    assert_eq!(spd(&riff), spd(&arp));
    // Stage order matters: chords before vs after the arpeggio.
    let mut a: Vec<(&str, ParamValue)> = only_arp.to_vec();
    a.retain(|x| x.0 != "s2.on");
    let chords_first = run("riff", &[(0, 1920, 60)], &a);
    let mut b = a.clone();
    b.push(("a1", ParamValue::Bool(true)));
    b.push(("a1.order", ParamValue::List(vec![3, 2])));
    let arp_first = run("riff", &[(0, 1920, 60)], &b);
    assert_ne!(spd(&chords_first), spd(&arp_first));
    // A generated riff keeps the selection and adds material.
    let gen_riff = run("riff", &[(0, 100, 90)], &[]);
    assert!(gen_riff.iter().any(|n| n.pitch == 90 && n.dur == 100));
    assert!(gen_riff.len() > 10);
}

#[test]
fn edit_tools_follow_their_base_behaviour() {
    let notes = [
        (0u64, 480u64, 48u8),
        (480, 480, 60),
        (960, 480, 72),
        (1440, 480, 61),
    ];
    // Delete: all; with a pitch range only those in it.
    assert!(run("delete", &notes, &[]).is_empty());
    let low = run(
        "delete",
        &notes,
        &[
            ("a1", ParamValue::Bool(true)),
            ("a1.lo", ParamValue::Int(0)),
            ("a1.hi", ParamValue::Int(59)),
        ],
    );
    assert_eq!(
        low.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![60, 72, 61]
    );
    // Select every 2nd note.
    let sel = run(
        "select",
        &notes,
        &[("a9", ParamValue::Bool(true)), ("a9.n", ParamValue::Int(2))],
    );
    assert_eq!(
        sel.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![0, 960]
    );
    // Slice at every 1/8 (480 at 960 PPQN): a 960-tick note → two.
    let sl = run("slice", &[(0, 960, 60)], &[]);
    assert_eq!(
        sl.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        vec![(0, 480), (480, 480)]
    );
    // Snap to C major: C#4 → C4 (tie → down).
    let sn = run("snap_scale", &[(0, 10, 61)], &[]);
    assert_eq!(sn[0].pitch, 60);
    // Flip X over the selection.
    let fx = run("flip", &[(0, 100, 60), (300, 100, 62)], &[]);
    assert_eq!(
        fx.iter().map(|n| (n.start, n.pitch)).collect::<Vec<_>>(),
        vec![(0, 62), (300, 60)]
    );
    // Duplicate once, right after the selection (snapped).
    let du = run("duplicate", &[(0, 400, 60)], &[]);
    assert_eq!(du.iter().map(|n| n.start).collect::<Vec<_>>(), vec![0, 480]);
    // Stretch 200 % around the start.
    let st = run("stretch", &[(100, 100, 60), (300, 100, 62)], &[]);
    assert_eq!(
        st.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        vec![(100, 200), (500, 200)]
    );
}

/// v0.9 §115, §116, §238: Interpolate, LFO and Level Scaling are gone from
/// the platform; their ids give the removal notice, never another tool.
#[test]
fn interpolate_lfo_and_level_scaling_are_removed() {
    for id in ["interpolate", "lfo", "level_scale"] {
        assert!(tool(id).is_none(), "{id}");
        let notice = plumspirit_engine::transform::removed_tool(id).unwrap();
        assert!(notice.contains("removed in PlumSpirit 0.9"), "{notice}");
        assert!(notice.contains("no other tool is run"), "{notice}");
    }
}
