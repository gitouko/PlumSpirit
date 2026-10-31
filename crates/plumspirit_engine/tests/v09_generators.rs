//! V09-S51 (V09-0088, contract §151, §162, §163, §239, §240, §256): every
//! Note Generator of the registry through the shared engine
//! (`transform::generator`, `docs/GENERATORS.md`), generated from the
//! registry and each generator's schema — a new generator is covered by
//! adding it to the registry:
//!
//! * the base fixture (an empty track, defaults): valid notes inside the
//!   generation box, on the resolution grid, with the shared velocity and
//!   subchannel;
//! * the shared controls: placement (cursor, time selection, selected
//!   notes' box and their fallbacks), resolution, gate, velocity,
//!   subchannel position, keeping or replacing the selection;
//! * seeded determinism (defaults and random draws);
//! * Preview / Apply identity: the worker's run is what the commit writes,
//!   the preview never mutates the document; Undo / Redo exact;
//! * boundary policies (Standard and 256-Key Mode keys);
//! * the preflight: the counting pass is exact, the cheap bound holds, an
//!   output over the limit is refused with a sentence before allocating;
//! * every declared function alone (the contract's 25 per generator, from
//!   V09-S52) changes the base result and stays valid;
//! * presets round-trip.

mod tool_support;

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::transform::generator::{COUNT_CAP, MAX_LENGTH};
use plumspirit_engine::transform::param::{ParamKind, ParamValue};
use plumspirit_engine::transform::registry::{BuildCtx, Family};
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, TNote, ToolDef, TrackScope, TransformError, TransformRun, commit,
    preset, registry, run_kernel,
};
use tool_support::random_value;

fn generators() -> Vec<&'static ToolDef> {
    registry()
        .iter()
        .filter(|t| (t.spec)().family == Family::Generator)
        .collect()
}

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// A 960-PPQ project of key range `keys`: track 0 with subchannels 0 – 3;
/// `notes` on it (selected).
fn doc(keys: KeyRange, notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("gen".into(), Bpm::DEFAULT, 960);
    d.project_mut().keys = keys;
    let subs: Vec<Subchannel> = (1..4)
        .map(|i| Subchannel::new(SubchannelId(i), i as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    if !notes.is_empty() {
        let mut v = notes.to_vec();
        v.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, true)
            .unwrap();
    }
    d
}

/// The editor's context: cursor `cursor`, track 0 active.
fn ctx(d: &PlumDocument, cursor: u64) -> Ctx {
    let mut c = Ctx::simple(960);
    c.keys = d.project().keys;
    c.cursor = cursor;
    c.active = Some(0);
    c
}

const SCOPE: [TrackScope; 1] = [TrackScope {
    index: 0,
    all: false,
    content: None,
}];

/// Runs `def` with `set` in `c` on `d` (the platform's way: the active
/// track's selected notes, possibly none).
fn run_in(
    def: &ToolDef,
    d: &PlumDocument,
    set: &ParamSet,
    c: &Ctx,
    limits: &Limits,
) -> Result<TransformRun, TransformError> {
    let src = collect_for(def, d, &SCOPE, 10_000_000)?;
    let b = BuildCtx {
        ctx: c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(set, &b)?;
    let est = k.estimate(&src.stats).notes;
    let exact = k.estimate_source(&src).notes;
    let r = run_kernel(k.as_ref(), &src, def.boundary(set), limits, None)?;
    // The cheap bound holds and the counting pass is exact (nothing is
    // dropped by a boundary policy inside the keyboard).
    assert!(exact <= est, "{}: count {exact} > bound {est}", def.id);
    assert!(r.notes.len() as u64 <= exact, "{}", def.id);
    Ok(r)
}

fn run(def: &ToolDef, d: &PlumDocument, set: &ParamSet, c: &Ctx) -> TransformRun {
    run_in(def, d, set, c, &Limits::default()).unwrap_or_else(|e| panic!("{}: {e}", def.id))
}

fn defaults(def: &ToolDef, keys: KeyRange) -> ParamSet {
    ParamSet::defaults_in(&(def.spec)().params, keys)
}

fn with(mut s: ParamSet, kv: &[(&str, ParamValue)]) -> ParamSet {
    for (k, v) in kv {
        s.set(k, v.clone());
    }
    s
}

fn valid(n: &TNote, keys: KeyRange) -> bool {
    n.dur >= 1 && (1..=127).contains(&n.vel) && n.ch <= 15 && n.pitch <= keys.max_pitch()
}

#[test]
fn there_is_a_generator_section_and_every_generator_is_selection_free() {
    let g = generators();
    assert!(g.iter().any(|t| t.id == "gen.lines"));
    for t in &g {
        let s = (t.spec)();
        assert!(!s.needs_selection, "{}", t.id);
        assert!(
            s.advanced.is_empty(),
            "{}: functions are declared by id",
            t.id
        );
        // The contract's 25 per generator once its slice declares them.
        assert!(
            s.functions.is_empty() || s.functions.len() == 25,
            "{}: {} functions",
            t.id,
            s.functions.len()
        );
        for k in [
            "gen.place",
            "gen.length",
            "gen.step",
            "gen.vel",
            "gen.gate",
            "gen.sub",
        ] {
            assert!(s.params.iter().any(|p| p.key == k), "{}: {k}", t.id);
        }
    }
}

#[test]
fn the_base_fixture_fills_the_generation_box_on_the_grid() {
    for def in generators() {
        let d = doc(KeyRange::Standard, &[]);
        let c = ctx(&d, 1920);
        let r = run(def, &d, &defaults(def, KeyRange::Standard), &c);
        assert!(!r.notes.is_empty(), "{}", def.id);
        for n in &r.notes {
            assert!(valid(n, KeyRange::Standard), "{}: {n:?}", def.id);
            assert_eq!(n.track, 0);
            assert_eq!((n.sub, n.ch, n.vel), (0, 0, 100), "{}", def.id);
            // Inside the box: 1,920 … 1,920 + 3,840 (one step of rounding),
            // pitches 48 … 84; on the snap grid (240).
            assert!(
                n.start >= 1920 && n.end() <= 1920 + 3840 + 240,
                "{}: {n:?}",
                def.id
            );
            assert!((48..=84).contains(&n.pitch), "{}: {n:?}", def.id);
            assert_eq!(n.start % 240, 0, "{}: {n:?}", def.id);
        }
    }
}

#[test]
fn shared_controls_place_quantize_gate_colour_and_keep() {
    let sel = [rec(4800, 960, 60), rec(5280, 1440, 72)];
    for def in generators() {
        let keys = KeyRange::Standard;
        let base = defaults(def, keys);
        // Time selection (and its fallback to the cursor).
        let d = doc(keys, &[]);
        let mut c = ctx(&d, 0);
        c.time_range = Some((960, 2880));
        let ts = with(base.clone(), &[("gen.place", ParamValue::Choice(1))]);
        let r = run(def, &d, &ts, &c);
        assert!(
            r.notes
                .iter()
                .all(|n| n.start >= 960 && n.end() <= 2880 + 240),
            "{}",
            def.id
        );
        let r0 = run(def, &d, &ts, &ctx(&d, 0));
        assert_eq!(
            r0.notes,
            run(def, &d, &base, &ctx(&d, 0)).notes,
            "{}",
            def.id
        );
        // The selected notes' box (time and pitch), the selection kept or
        // replaced; without a selection, the cursor.
        let d = doc(keys, &sel);
        let bx = with(base.clone(), &[("gen.place", ParamValue::Choice(2))]);
        let r = run(def, &d, &bx, &ctx(&d, 0));
        let made: Vec<&TNote> = r
            .notes
            .iter()
            .filter(|n| {
                !sel.iter().any(|s| {
                    s.start_tick == n.start
                        && s.attrs.pitch() == n.pitch
                        && s.duration_ticks == n.dur
                })
            })
            .collect();
        assert!(!made.is_empty(), "{}", def.id);
        assert!(
            made.iter()
                .all(|n| n.start >= 4800 && n.end() <= 6720 + 240),
            "{}",
            def.id
        );
        assert!(
            made.iter().all(|n| (60..=72).contains(&n.pitch)),
            "{}",
            def.id
        );
        assert_eq!(
            r.notes.len(),
            made.len() + 2,
            "{}: the selection is kept",
            def.id
        );
        let replace = with(bx.clone(), &[("gen.keep", ParamValue::Bool(false))]);
        assert_eq!(
            run(def, &d, &replace, &ctx(&d, 0)).notes.len(),
            made.len(),
            "{}",
            def.id
        );
        // Resolution 1/8 (480), gate 50 %, velocity 33, subchannel position 2.
        let d = doc(keys, &[]);
        let s = with(
            base.clone(),
            &[
                ("gen.step", ParamValue::Choice(3)),
                ("gen.gate", ParamValue::Float(50.0)),
                ("gen.vel", ParamValue::Int(33)),
                ("gen.sub", ParamValue::Int(2)),
            ],
        );
        let full = run(
            def,
            &d,
            &with(s.clone(), &[("gen.gate", ParamValue::Float(100.0))]),
            &ctx(&d, 0),
        );
        let r = run(def, &d, &s, &ctx(&d, 0));
        assert_eq!(r.notes.len(), full.notes.len(), "{}", def.id);
        for (n, f) in r.notes.iter().zip(&full.notes) {
            assert_eq!(n.start % 480, 0, "{}", def.id);
            assert_eq!(f.dur % 480, 0, "{}", def.id);
            assert_eq!(n.dur, (f.dur / 2).max(1), "{}: gate", def.id);
            assert_eq!((n.vel, n.sub, n.ch), (33, 2, 2), "{}", def.id);
        }
    }
}

/// Random values for every non-boundary parameter (seeded), functions as
/// drawn.
fn draw(def: &ToolDef, seed: u64, keys: KeyRange) -> ParamSet {
    let mut r = Rng::new(0x6E4E_0000 + seed);
    let mut s = defaults(def, keys);
    for p in &(def.spec)().params {
        if p.key.starts_with("b.") || p.key == "gen.length" {
            continue;
        }
        if let Some(v) = random_value(p, &mut r, keys) {
            s.set(p.key, v);
        }
    }
    s
}

#[test]
fn generators_are_deterministic_for_a_seed() {
    for def in generators() {
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            let d = doc(keys, &[rec(0, 480, 60)]);
            for seed in 0..12 {
                let s = draw(def, seed, keys);
                let c = ctx(&d, 960);
                match run_in(def, &d, &s, &c, &Limits::default()) {
                    Ok(a) => {
                        let b = run(def, &d, &s, &c);
                        assert_eq!(a.notes, b.notes, "{} draw {seed}", def.id);
                        assert!(
                            a.notes.iter().all(|n| valid(n, keys)),
                            "{} draw {seed}",
                            def.id
                        );
                    }
                    Err(e) => {
                        // A refusal is a sentence (a boundary policy of
                        // Refuse, a limit), never a panic.
                        assert!(e.user_message().ends_with('.'), "{}: {e}", def.id);
                    }
                }
            }
        }
    }
}

#[test]
fn preview_is_what_apply_writes_and_undo_redo_are_exact() {
    for def in generators() {
        for notes in [vec![], vec![rec(0, 480, 60), rec(960, 240, 64)]] {
            let mut d = doc(KeyRange::Standard, &notes);
            let mut h = History::default();
            let before = d.state_fingerprint();
            let c = ctx(&d, 480);
            let r = run(def, &d, &defaults(def, KeyRange::Standard), &c);
            assert_eq!(d.state_fingerprint(), before, "{}: preview mutated", def.id);
            commit(&mut h, &mut d, &r, "Generator").unwrap();
            assert_eq!(h.undo_len(), 1, "{}: one undo step", def.id);
            let mut got: Vec<(u64, u64, u8, u32, u8)> = Vec::new();
            d.track(0).unwrap().notes.for_each_selected(|v| {
                got.push((
                    v.start_tick,
                    v.duration_ticks,
                    v.attrs.pitch(),
                    v.sub.0,
                    v.attrs.velocity(),
                ))
            });
            let mut want: Vec<(u64, u64, u8, u32, u8)> = r
                .notes
                .iter()
                .map(|n| (n.start, n.dur, n.pitch, n.sub, n.vel))
                .collect();
            got.sort();
            want.sort();
            assert_eq!(
                got, want,
                "{}: the committed notes are the preview's",
                def.id
            );
            let after = d.state_fingerprint();
            h.undo(&mut d).unwrap();
            assert_eq!(d.state_fingerprint(), before, "{}: undo", def.id);
            h.redo(&mut d).unwrap();
            assert_eq!(d.state_fingerprint(), after, "{}: redo", def.id);
        }
    }
}

#[test]
fn boundary_policies_hold_in_both_key_ranges() {
    for def in generators() {
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            let top = i64::from(keys.max_pitch());
            let d = doc(keys, &[]);
            for policy in 0..4u32 {
                let s = with(
                    defaults(def, keys),
                    &[
                        ("gen.low", ParamValue::Int(0)),
                        ("gen.high", ParamValue::Int(top)),
                        ("b.pitch", ParamValue::Choice(policy)),
                    ],
                );
                match run_in(def, &d, &s, &ctx(&d, 0), &Limits::default()) {
                    Ok(r) => {
                        assert!(r.notes.iter().all(|n| valid(n, keys)), "{}", def.id);
                        assert!(
                            r.notes.iter().any(|n| i64::from(n.pitch) == top),
                            "{}",
                            def.id
                        );
                        assert!(r.notes.iter().any(|n| n.pitch == 0), "{}", def.id);
                    }
                    Err(e) => assert!(e.user_message().ends_with('.'), "{}: {e}", def.id),
                }
            }
        }
    }
}

#[test]
fn an_output_over_the_limit_is_refused_by_the_preflight() {
    for def in generators() {
        let d = doc(KeyRange::Standard, &[]);
        let s = with(
            defaults(def, KeyRange::Standard),
            &[
                ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
                ("gen.step", ParamValue::Choice(11)),
                ("gen.step_ticks", ParamValue::Int(1)),
            ],
        );
        let c = ctx(&d, 0);
        let full = run(def, &d, &s, &c).notes.len();
        let tight = Limits {
            max_notes: (full as u64 / 2).max(1),
            ..Limits::default()
        };
        match run_in(def, &d, &s, &c, &tight) {
            Err(TransformError::TooLarge(m)) => {
                assert!(m.contains("notes") && m.ends_with('.'), "{m}")
            }
            other => panic!("{}: {:?}", def.id, other.map(|r| r.notes.len())),
        }
    }
    // The counting pass stops at the platform's cap.
    assert_eq!(COUNT_CAP, Limits::default().max_notes);
}

#[test]
fn every_declared_function_alone_changes_the_base_result() {
    for def in generators() {
        let spec = (def.spec)();
        let d = doc(KeyRange::Standard, &[]);
        let c = ctx(&d, 0);
        let base = defaults(def, KeyRange::Standard);
        let plain = run(def, &d, &base, &c).notes;
        for f in &spec.functions {
            let s = with(base.clone(), &[(f.id, ParamValue::Bool(true))]);
            let r = run(def, &d, &s, &c);
            assert_ne!(r.notes, plain, "{} {}", def.id, f.id);
            assert!(
                r.notes.iter().all(|n| valid(n, KeyRange::Standard)),
                "{} {}",
                def.id,
                f.id
            );
        }
    }
}

#[test]
fn generator_presets_round_trip() {
    for def in generators() {
        let specs = (def.spec)().params;
        for seed in 0..6 {
            let values = draw(def, seed, KeyRange::Standard);
            let p = preset::Preset {
                tool: def.id.to_owned(),
                name: format!("{} {seed}", (def.spec)().name),
                values,
            };
            let text = preset::to_text(&p);
            let (back, skipped) = preset::parse(&text, def.id, &specs).unwrap();
            assert!(skipped.is_empty(), "{skipped:?}");
            assert_eq!(preset::to_text(&back), text, "{}", def.id);
            for s in &specs {
                if matches!(s.kind, ParamKind::Text { .. }) {
                    continue;
                }
                assert!(back.values.get_raw(s.key).is_some(), "{} {}", def.id, s.key);
            }
        }
    }
}

// ------------------------------------------------------------ Lines (V09-S52)
//
// What each of Lines' 25 functions means (V09-0089, `docs/GENERATORS.md`
// "Lines"), on a small box: ticks 0 … 3,840 (16 steps of 240, the snap),
// rows 60 … 63 — expected notes computed by hand or by an independent
// formula in the test.

fn lines_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.lines")
        .unwrap()
}

fn lines_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = lines_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(63)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 0), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.start, n.pitch, n.dur, n.vel, n.sub));
    Ok(r)
}

fn lines(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    lines_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

fn on(id: &'static str) -> (&'static str, ParamValue) {
    (id, ParamValue::Bool(true))
}

fn int(k: &'static str, v: i64) -> (&'static str, ParamValue) {
    (k, ParamValue::Int(v))
}

fn num(k: &'static str, v: f64) -> (&'static str, ParamValue) {
    (k, ParamValue::Float(v))
}

/// (start, duration, pitch).
fn sdp(v: &[TNote]) -> Vec<(u64, u64, u8)> {
    v.iter().map(|n| (n.start, n.dur, n.pitch)).collect()
}

/// A horizontal line on row 60 from tick `from` to `to` (Exact Start/End
/// Anchors).
fn flat(from: i64, to: i64) -> Vec<(&'static str, ParamValue)> {
    vec![
        on("c02_exact_start_end_anchors"),
        int("c02_exact_start_end_anchors.start", from),
        int("c02_exact_start_end_anchors.start_pitch", 60),
        int("c02_exact_start_end_anchors.end", to),
        int("c02_exact_start_end_anchors.end_pitch", 60),
    ]
}

fn plus(
    mut a: Vec<(&'static str, ParamValue)>,
    b: &[(&'static str, ParamValue)],
) -> Vec<(&'static str, ParamValue)> {
    a.extend_from_slice(b);
    a
}

/// Notes' (start, duration) on a whole-step grid, merged like the pen: equal
/// neighbours along a horizontal line with values `v[j]` per step.
fn runs<T: PartialEq + Copy>(v: &[T]) -> Vec<(u64, u64, T)> {
    let mut out: Vec<(u64, u64, T)> = Vec::new();
    for (j, &x) in v.iter().enumerate() {
        match out.last_mut() {
            Some(l) if l.2 == x => l.1 += 240,
            _ => out.push((240 * j as u64, 240, x)),
        }
    }
    out
}

#[test]
fn lines_the_base_is_the_box_diagonal_on_the_grid() {
    // From row coordinate 60.5 at tick 0 to 63.5 at 3,840: the row edges
    // 61, 62, 63 are crossed at ticks 640, 1,920 and 3,200, each rounded to
    // the 240 grid.
    assert_eq!(
        sdp(&lines(&[])),
        vec![
            (0, 720, 60),
            (720, 1200, 61),
            (1920, 1200, 62),
            (3120, 720, 63)
        ]
    );
}

#[test]
fn lines_free_angle_turns_the_line_about_the_box_centre_on_the_grid() {
    // 0°: through the centre (row coordinate 62.0) — one note on row 62.
    let a = num("c01_free_angle.angle", 0.0);
    assert_eq!(sdp(&lines(&[on("c01_free_angle"), a])), vec![(0, 3840, 62)]);
    // 90°: one step at the centre time on every row.
    let v = lines(&[on("c01_free_angle"), num("c01_free_angle.angle", 90.0)]);
    assert_eq!(
        sdp(&v),
        vec![
            (1920, 240, 60),
            (1920, 240, 61),
            (1920, 240, 62),
            (1920, 240, 63)
        ]
    );
    // 45° = one row per step: four notes, one per row, rising — from row
    // coordinate 60.5 to 63.5, symmetric about the centre (8, 62): row 60
    // spans steps 6.5 … 7 (the cell containing it, 6), row 63 steps 9 …
    // 9.5 (cell 9).
    let v = lines(&[on("c01_free_angle"), num("c01_free_angle.angle", 45.0)]);
    assert_eq!(v.len(), 4);
    assert_eq!(
        v.iter()
            .map(|n| (n.pitch, n.start / 240))
            .collect::<Vec<_>>(),
        vec![(60, 6), (61, 7), (62, 8), (63, 9)]
    );
    // −45° falls: the top row starts first, the bottom row last.
    let v = lines(&[on("c01_free_angle"), num("c01_free_angle.angle", -45.0)]);
    let start = |p: u8| v.iter().find(|n| n.pitch == p).unwrap().start;
    assert_eq!(v.len(), 4);
    assert!(start(63) <= start(62) && start(62) <= start(61) && start(61) < start(60));
}

#[test]
fn lines_anchors_put_the_ends_at_exact_times_and_pitches() {
    assert_eq!(sdp(&lines(&flat(0, 960))), vec![(0, 960, 60)]);
    // Anywhere, even outside the box: from (480, 70) to (1,440, 72) — row
    // edges 71 and 72 at ticks 720 and 1,200.
    let v = lines(&[
        on("c02_exact_start_end_anchors"),
        int("c02_exact_start_end_anchors.start", 480),
        int("c02_exact_start_end_anchors.start_pitch", 70),
        int("c02_exact_start_end_anchors.end", 1440),
        int("c02_exact_start_end_anchors.end_pitch", 72),
    ]);
    assert_eq!(
        sdp(&v),
        vec![(480, 240, 70), (720, 480, 71), (1200, 240, 72)]
    );
}

#[test]
fn lines_polyline_segments_place_points_at_even_times_and_listed_heights() {
    let h = |v: &[i64]| {
        lines(&[
            on("c03_polyline_segments"),
            (
                "c03_polyline_segments.heights",
                ParamValue::List(v.to_vec()),
            ),
        ])
    };
    // One height: a horizontal at 50 % (row coordinate 62.0: row 62).
    assert_eq!(sdp(&h(&[50])), vec![(0, 3840, 62)]);
    // 0 / 100: the diagonal itself.
    assert_eq!(sdp(&h(&[0, 100])), sdp(&lines(&[])));
    // Up and down: rows 60 61 62 63 62 61 60 (the peak's two pieces merge).
    let v = h(&[0, 100, 0]);
    let mut count = [0; 4];
    for n in &v {
        count[usize::from(n.pitch - 60)] += 1;
    }
    assert_eq!(count, [2, 2, 2, 1]);
    assert!(
        v.iter()
            .any(|n| n.pitch == 63 && n.start <= 1920 && 1920 < n.end())
    );
}

#[test]
fn lines_thickness_fills_rows_around_the_line() {
    // A horizontal on row 60 (centre 60.5), three rows thick: 59, 60, 61.
    let v = lines(&plus(
        flat(0, 3840),
        &[on("c04_line_thickness"), int("c04_line_thickness.rows", 3)],
    ));
    assert_eq!(sdp(&v), vec![(0, 3840, 59), (0, 3840, 60), (0, 3840, 61)]);
    // Four rows: the centres in (58.5, 62.5] — 59 … 62 (an even thickness
    // leans up, like a one-row band takes the row above a row edge).
    let v = lines(&plus(
        flat(0, 3840),
        &[on("c04_line_thickness"), int("c04_line_thickness.rows", 4)],
    ));
    assert_eq!(
        v.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![59, 60, 61, 62]
    );
}

#[test]
fn lines_parallel_copies_repeat_the_line_with_cumulative_offsets() {
    let v = lines(&plus(
        flat(0, 960),
        &[
            on("c05_parallel_copies"),
            int("c05_parallel_copies.count", 3),
            int("c05_parallel_copies.pitch", 12),
            int("c05_parallel_copies.time", 480),
        ],
    ));
    assert_eq!(sdp(&v), vec![(0, 960, 60), (480, 960, 72), (960, 960, 84)]);
}

#[test]
fn lines_note_spacing_samples_a_note_every_n_grid_units() {
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c06_note_spacing_density"),
            num("c06_note_spacing_density.every", 4.0),
            int("c06_note_spacing_density.length", 2),
        ],
    ));
    // Every 4 steps from the start (arc 0, 4, 8, 12, 16), two steps long.
    assert_eq!(
        sdp(&v),
        [0, 960, 1920, 2880, 3840].map(|s| (s, 480, 60)).to_vec()
    );
}

#[test]
fn lines_a_spacing_graph_spreads_the_notes_along_the_line() {
    // Spacing 1 + curve(s) × 7 grid units with the identity curve, on a
    // 16-step horizontal: positions by the same formula, then the grid.
    let v = lines(&plus(flat(0, 3840), &[on("c07_variable_spacing_graph")]));
    let mut want = Vec::new();
    let mut a = 0.0f64;
    while a <= 16.0 {
        want.push(((a).round() as u64 * 240, 240, 60u8));
        a += 1.0 + (a / 16.0) * 7.0;
    }
    assert_eq!(sdp(&v), want);
    // The gaps widen.
    assert!(v[1].start - v[0].start < v[v.len() - 1].start - v[v.len() - 2].start);
}

#[test]
fn lines_gradients_change_gate_velocity_and_subchannel_along_the_line() {
    let line = flat(0, 3840);
    // Pieces of one grid unit, each with its value at its middle.
    let mid = |j: usize| (j as f64 + 0.5) / 16.0;
    // Gate 100 % → 25 % (whole percent).
    let v = lines(&plus(line.clone(), &[on("c08_gate_gradient")]));
    let want: Vec<(u64, u64, u8)> = (0..16)
        .map(|j| {
            let g = (100.0 - 75.0 * mid(j)).round() / 100.0;
            (240 * j as u64, ((240.0 * g).round() as u64).max(1), 60)
        })
        .collect();
    assert_eq!(sdp(&v), want);
    // Velocity 127 → 40.
    let v = lines(&plus(line.clone(), &[on("c09_velocity_gradient")]));
    let vels: Vec<u8> = (0..16)
        .map(|j| (127.0 - 87.0 * mid(j)).round() as u8)
        .collect();
    let want = runs(&vels);
    assert_eq!(
        v.iter()
            .map(|n| (n.start, n.dur, n.vel))
            .collect::<Vec<_>>(),
        want
    );
    // Subchannel positions 0 → 3: equal neighbours merge into one note.
    let v = lines(&plus(line, &[on("c10_subchannel_gradient")]));
    let subs: Vec<u32> = (0..16).map(|j| (3.0 * mid(j)).round() as u32).collect();
    assert_eq!(
        v.iter()
            .map(|n| (n.start, n.dur, n.sub))
            .collect::<Vec<_>>(),
        runs(&subs)
    );
    assert_eq!(v.len(), 4);
    assert!(v.iter().all(|n| u32::from(n.ch) == n.sub));
}

#[test]
fn lines_dashes_cut_the_line_along_time() {
    let v = lines(&plus(flat(0, 3840), &[on("c11_dash_pattern")]));
    assert_eq!(sdp(&v), [0, 960, 1920, 2880].map(|s| (s, 480, 60)).to_vec());
    // A dash of 240 every 720.
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c11_dash_pattern"),
            int("c11_dash_pattern.dash", 240),
            int("c11_dash_pattern.gap", 480),
        ],
    ));
    assert_eq!(
        v.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![0, 720, 1440, 2160, 2880, 3600]
    );
}

#[test]
fn lines_a_dot_pattern_samples_dots_and_rests() {
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c12_dot_pattern"),
            ("c12_dot_pattern.pattern", ParamValue::List(vec![1, 0, 0])),
        ],
    ));
    // One sample per grid unit (arc 0 … 16), every third a dot.
    assert_eq!(
        sdp(&v),
        [0, 720, 1440, 2160, 2880, 3600]
            .map(|s| (s, 240, 60))
            .to_vec()
    );
}

#[test]
fn lines_taper_narrows_velocity_and_thickness_at_both_ends() {
    let taper = |s: f64| {
        let e = s.min(1.0 - s);
        0.3 + 0.7 * (e / 0.25).min(1.0)
    };
    let v = lines(&plus(flat(0, 3840), &[on("c13_endpoint_taper")]));
    let vels: Vec<u8> = (0..16)
        .map(|j| (100.0 * taper((j as f64 + 0.5) / 16.0)).round() as u8)
        .collect();
    assert_eq!(
        v.iter()
            .map(|n| (n.start, n.dur, n.vel))
            .collect::<Vec<_>>(),
        runs(&vels)
    );
    assert_eq!(v.iter().map(|n| n.vel).max(), Some(100));
    // Five rows thick: the ends keep fewer rows (at least one).
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c13_endpoint_taper"),
            on("c04_line_thickness"),
            int("c04_line_thickness.rows", 5),
        ],
    ));
    let rows_at = |t: u64| v.iter().filter(|n| n.start <= t && t < n.end()).count();
    assert_eq!(rows_at(1920), 5);
    assert!((1..5).contains(&rows_at(0)));
}

#[test]
fn lines_mirrors_reflect_about_the_box_centre() {
    let half = flat(0, 1920);
    // X: about tick 1,920 — kept and added, or instead.
    let v = lines(&plus(half.clone(), &[on("c14_mirror_x")]));
    assert_eq!(sdp(&v), vec![(0, 1920, 60), (1920, 1920, 60)]);
    let v = lines(&plus(
        half.clone(),
        &[on("c14_mirror_x"), int("c14_mirror_x.mode", 1)],
    ));
    assert_eq!(sdp(&v), vec![(1920, 1920, 60)]);
    // Y: about the pitch centre (row 60 ↔ row 63).
    let v = lines(&plus(half.clone(), &[on("c15_mirror_y")]));
    assert_eq!(sdp(&v), vec![(0, 1920, 60), (0, 1920, 63)]);
    let v = lines(&plus(
        half,
        &[on("c15_mirror_y"), int("c15_mirror_y.mode", 1)],
    ));
    assert_eq!(sdp(&v), vec![(0, 1920, 63)]);
}

#[test]
fn lines_variants_compose_in_the_chosen_order() {
    // B2-F01: a horizontal on row 62 in a box of rows 60 – 67 (Mirror Y maps
    // row r to 127 − r); two parallel lines a row apart; Mirror Y alone.
    let def = lines_def();
    let d = doc(KeyRange::Standard, &[]);
    let run = |order: Option<&str>| {
        let mut kv = vec![
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(67)),
            on("c02_exact_start_end_anchors"),
            int("c02_exact_start_end_anchors.start", 0),
            int("c02_exact_start_end_anchors.start_pitch", 62),
            int("c02_exact_start_end_anchors.end", 1920),
            int("c02_exact_start_end_anchors.end_pitch", 62),
            on("c05_parallel_copies"),
            int("c05_parallel_copies.count", 2),
            int("c05_parallel_copies.pitch", 1),
            on("c15_mirror_y"),
            int("c15_mirror_y.mode", 1),
        ];
        if let Some(o) = order {
            kv.push(("order", ParamValue::Text(o.into())));
        }
        let set = with(defaults(def, KeyRange::Standard), &kv);
        let mut v: Vec<u8> = run_in(def, &d, &set, &ctx(&d, 0), &Limits::default())
            .unwrap()
            .notes
            .iter()
            .map(|n| n.pitch)
            .collect();
        v.sort_unstable();
        v
    };
    // Declared: the copy (62, 63), then mirrored (65, 64).
    assert_eq!(run(None), vec![64, 65]);
    // Mirrored first (65), then the copy a row up (66).
    assert_eq!(run(Some("c15_mirror_y,c05_parallel_copies")), vec![65, 66]);
}

/// Generator `id` with `kv` (and `order`, when given) in rows 60 – 71:
/// its notes' (start, duration, pitch), sorted.
fn gen_with_order(
    id: &str,
    kv: &[(&'static str, ParamValue)],
    order: Option<&str>,
) -> Vec<(u64, u64, u8)> {
    let def = generators().into_iter().find(|t| t.id == id).unwrap();
    let d = doc(KeyRange::Standard, &[]);
    let mut all = vec![
        ("gen.low", ParamValue::Int(60)),
        ("gen.high", ParamValue::Int(71)),
    ];
    all.extend_from_slice(kv);
    if let Some(o) = order {
        all.push(("order", ParamValue::Text(o.into())));
    }
    let set = with(defaults(def, KeyRange::Standard), &all);
    let mut v = sdp(&run_in(def, &d, &set, &ctx(&d, 0), &Limits::default())
        .unwrap()
        .notes);
    v.sort_unstable();
    v
}

#[test]
fn rectangle_shear_and_rotation_compose_in_the_chosen_order() {
    // B2-F01: shear then rotate is not rotate then shear.
    let kv = [
        on("c09_rotation"),
        num("c09_rotation.angle", 30.0),
        on("c10_shear"),
        num("c10_shear.angle", 40.0),
    ];
    let declared = gen_with_order("gen.rect", &kv, None);
    assert!(!declared.is_empty());
    assert_eq!(
        gen_with_order("gen.rect", &kv, Some("c10_shear,c09_rotation")),
        declared
    );
    assert_ne!(
        gen_with_order("gen.rect", &kv, Some("c09_rotation,c10_shear")),
        declared
    );
    // One map alone: the order changes nothing.
    let one = [on("c09_rotation"), num("c09_rotation.angle", 30.0)];
    assert_eq!(
        gen_with_order("gen.rect", &one, Some("c09_rotation,c10_shear")),
        gen_with_order("gen.rect", &one, None)
    );
}

#[test]
fn arcs_copy_expansions_compose_in_the_chosen_order() {
    // B2-F01: a mirrored chain is not a chain of the mirrored arc.
    let kv = [
        on("c18_tangent_aligned_duplication"),
        int("c18_tangent_aligned_duplication.count", 2),
        on("c17_mirror"),
    ];
    let declared = gen_with_order("gen.arcs", &kv, None);
    assert!(!declared.is_empty());
    assert_eq!(
        gen_with_order(
            "gen.arcs",
            &kv,
            Some("c18_tangent_aligned_duplication,c19_normal_offset_duplication,c17_mirror")
        ),
        declared
    );
    assert_ne!(
        gen_with_order(
            "gen.arcs",
            &kv,
            Some("c17_mirror,c18_tangent_aligned_duplication")
        ),
        declared
    );
}

#[test]
fn custom_shape_symmetry_and_point_maps_compose_in_the_chosen_order() {
    // B2-F01: mirrors about an off-centre axis vs turned copies; a mesh warp
    // vs the full transform.
    let sym = [
        on("c17_mirror_symmetry"),
        num("c17_mirror_symmetry.offset", 20.0),
        on("c18_radial_symmetry"),
        int("c18_radial_symmetry.count", 3),
    ];
    let declared = gen_with_order("gen.custom", &sym, None);
    assert!(!declared.is_empty());
    assert_eq!(
        gen_with_order(
            "gen.custom",
            &sym,
            Some("c17_mirror_symmetry,c18_radial_symmetry")
        ),
        declared
    );
    assert_ne!(
        gen_with_order(
            "gen.custom",
            &sym,
            Some("c18_radial_symmetry,c17_mirror_symmetry")
        ),
        declared
    );
    let maps = [
        on("c23_mesh_warp"),
        on("c16_full_transform"),
        num("c16_full_transform.angle", 30.0),
    ];
    let declared = gen_with_order("gen.custom", &maps, None);
    assert_ne!(
        gen_with_order(
            "gen.custom",
            &maps,
            Some("c16_full_transform,c23_mesh_warp")
        ),
        declared
    );
}

#[test]
fn lines_a_radial_fan_rotates_the_line_about_its_start_on_the_grid() {
    // A horizontal of 8 steps from (0, 60.5), fanned over 180°: straight
    // down (rows 52 … 60), along (row 60) and straight up (rows 60 … 68),
    // each vertical one step per row at tick 0.
    let v = lines(&plus(
        flat(0, 1920),
        &[
            on("c16_radial_fan"),
            int("c16_radial_fan.count", 3),
            num("c16_radial_fan.spread", 180.0),
        ],
    ));
    assert!(v.iter().any(|n| (n.start, n.dur, n.pitch) == (0, 1920, 60)));
    for p in 52..=68u8 {
        assert!(
            v.iter().any(|n| (n.start, n.dur, n.pitch) == (0, 240, p)),
            "row {p}"
        );
    }
    assert_eq!(v.len(), 1 + 9 + 9);
}

#[test]
fn lines_crosshatch_draws_parallels_every_s_rows_and_their_time_mirror_in_the_box() {
    // Horizontal on row 60, spacing 2: the parallels inside rows 60 … 63
    // are rows 60 and 62, each with its (equal) time mirror.
    let v = lines(&plus(
        flat(0, 3840),
        &[on("c17_crosshatch"), int("c17_crosshatch.spacing", 2)],
    ));
    assert_eq!(
        sdp(&v),
        vec![(0, 3840, 60), (0, 3840, 60), (0, 3840, 62), (0, 3840, 62)]
    );
    // On the diagonal the family and its mirror cross: rising and falling
    // lines, all inside the box.
    let v = lines(&[on("c17_crosshatch"), int("c17_crosshatch.spacing", 1)]);
    assert!(
        v.iter()
            .all(|n| (60..=63).contains(&n.pitch) && n.end() <= 3840 + 240)
    );
    assert!(v.len() > 2 * lines(&[]).len());
    // Thick crosshatch lines stay in the box too: a band of a line outside
    // the box's rows is empty, never the rows between it and the box.
    let v = lines(&[
        on("c17_crosshatch"),
        int("c17_crosshatch.spacing", 1),
        on("c04_line_thickness"),
        int("c04_line_thickness.rows", 9),
        on("c13_endpoint_taper"),
    ]);
    assert!(!v.is_empty());
    assert!(v.iter().all(|n| (60..=63).contains(&n.pitch)));
}

#[test]
fn lines_scanline_fill_covers_the_region_to_the_chosen_boundary() {
    let fill = |to: i64, h: i64| {
        lines(&[
            on("c03_polyline_segments"),
            ("c03_polyline_segments.heights", ParamValue::List(vec![h])),
            on("c18_scanline_fill_between_boundaries"),
            int("c18_scanline_fill_between_boundaries.to", to),
        ])
    };
    let all = vec![(0, 3840, 60), (0, 3840, 61), (0, 3840, 62), (0, 3840, 63)];
    // The top row to the bottom, the bottom row to the top, the bottom row
    // to its mirror (the top row): every row.
    assert_eq!(sdp(&fill(0, 100)), all);
    assert_eq!(sdp(&fill(1, 0)), all);
    assert_eq!(sdp(&fill(2, 0)), all);
    // The middle (62.0) to the bottom: rows 60 … 62 (the line's own row
    // band reaches 62.5).
    assert_eq!(sdp(&fill(0, 50)), all[..3].to_vec());
    // The diagonal to the bottom: a triangle, each row from where the line
    // passes it to the end.
    let v = lines(&[
        on("c18_scanline_fill_between_boundaries"),
        int("c18_scanline_fill_between_boundaries.to", 0),
    ]);
    for p in 60..=63u8 {
        let row: Vec<_> = v.iter().filter(|n| n.pitch == p).collect();
        assert_eq!(row.len(), 1, "row {p}");
        assert!(row[0].end() >= 3840, "row {p}");
    }
    let start = |p: u8| v.iter().find(|n| n.pitch == p).unwrap().start;
    assert!(start(60) < start(61) && start(61) < start(62) && start(62) < start(63));
}

#[test]
fn lines_alternating_direction_zig_zags_through_the_box() {
    let v = lines(&[
        on("c19_alternating_direction"),
        int("c19_alternating_direction.passes", 2),
    ]);
    // Up over the first half (row edges at 320, 960, 1,600), down over the
    // second; the top row's two pieces meet at 1,920 and merge.
    let mut by_time: Vec<(u64, u8)> = v.iter().map(|n| (n.start, n.pitch)).collect();
    by_time.sort();
    let first_half: Vec<u8> = by_time.iter().filter(|x| x.0 < 1920).map(|x| x.1).collect();
    let second: Vec<u8> = by_time
        .iter()
        .filter(|x| x.0 >= 1920)
        .map(|x| x.1)
        .collect();
    assert_eq!(first_half, vec![60, 61, 62, 63]);
    assert_eq!(second, vec![62, 61, 60]);
    assert!(
        v.iter()
            .any(|n| (n.start, n.end(), n.pitch) == (1680, 2160, 63))
    );
    assert!(v.iter().all(|n| n.end() <= 3840 + 240));
}

#[test]
fn lines_scale_snap_moves_rows_onto_the_scale_and_merges_them() {
    // C major over C, C♯, D, D♯: C C D D — merged.
    let v = lines(&[on("c20_scale_pitch_snap")]);
    assert_eq!(sdp(&v), vec![(0, 1920, 60), (1920, 1920, 62)]);
    // D minor pentatonic-like check: every note on the scale.
    let v = lines(&[
        on("c20_scale_pitch_snap"),
        int("c20_scale_pitch_snap.key", 2),
        int("c20_scale_pitch_snap.scale", 10),
    ]);
    let pcs = [2, 5, 7, 9, 0]; // D F G A C
    assert!(v.iter().all(|n| pcs.contains(&(n.pitch % 12))));
}

#[test]
fn lines_time_snap_pulls_starts_to_the_grid_by_a_strength() {
    // The diagonal's starts 0, 720, 1,920, 3,120 to the 1/2 grid (1,920):
    // 0, 0, 1,920, 3,840; lengths kept.
    let grid = int("c21_time_grid_snap.grid", 1);
    let v = lines(&[on("c21_time_grid_snap"), grid.clone()]);
    assert_eq!(
        sdp(&v),
        vec![
            (0, 720, 60),
            (0, 1200, 61),
            (1920, 1200, 62),
            (3840, 720, 63)
        ]
    );
    // Half way: 720 → −360 = −1.5 steps → −2 steps → 240; 3,120 → +360 →
    // 3,600.
    let v = lines(&[
        on("c21_time_grid_snap"),
        grid,
        num("c21_time_grid_snap.strength", 50.0),
    ]);
    let starts: Vec<(u8, u64)> = v.iter().map(|n| (n.pitch, n.start)).collect();
    assert!(starts.contains(&(61, 240)) && starts.contains(&(63, 3600)));
    assert!(starts.contains(&(60, 0)) && starts.contains(&(62, 1920)));
}

#[test]
fn lines_intersection_accents_mark_beats_and_crossings() {
    // Beats only (radius 0): notes every 3 steps; those containing or
    // touching a beat line (every 960) are accented.
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c06_note_spacing_density"),
            num("c06_note_spacing_density.every", 3.0),
            on("c22_intersection_accent"),
            num("c22_intersection_accent.radius", 0.0),
            ("c22_intersection_accent.lines", ParamValue::Bool(false)),
        ],
    ));
    assert_eq!(
        v.iter().map(|n| (n.start, n.vel)).collect::<Vec<_>>(),
        vec![
            (0, 127),
            (720, 127),
            (1440, 100),
            (2160, 100),
            (2880, 127),
            (3600, 127)
        ]
    );
    // Crossings only: the diagonal and its X mirror cross at the centre
    // (tick 1,920, row coordinate 62.0, the edge between rows 61 and 62);
    // radius 0 accents the notes touching that point — on rows 61 and 62,
    // ending or starting at tick 1,920, two per line.
    let v = lines(&[
        on("c14_mirror_x"),
        on("c22_intersection_accent"),
        num("c22_intersection_accent.radius", 0.0),
        ("c22_intersection_accent.beats", ParamValue::Bool(false)),
    ]);
    for n in &v {
        let hit = (61..=62).contains(&n.pitch) && n.start <= 1920 && 1920 <= n.end();
        assert_eq!(n.vel, if hit { 127 } else { 100 }, "{n:?}");
    }
    assert_eq!(v.iter().filter(|n| n.vel == 127).count(), 4);
}

#[test]
fn lines_wrap_and_reflect_fold_the_extended_travel_back_into_the_box() {
    // 100 % more travel: rows 60 … 66 over the box (row edges every 640
    // ticks from 320).
    let w = lines(&[on("c23_wrap_boundary")]);
    let rows: Vec<u8> = w.iter().map(|n| n.pitch).collect();
    assert_eq!(rows, vec![60, 61, 62, 63, 60, 61, 62]);
    let r = lines(&[on("c24_reflect_boundary")]);
    let rows: Vec<u8> = r.iter().map(|n| n.pitch).collect();
    // 60 61 62 63 63 62 61: the two pieces on 63 merge.
    assert_eq!(rows, vec![60, 61, 62, 63, 62, 61]);
    assert!(
        r.iter()
            .any(|n| (n.start, n.end(), n.pitch) == (1680, 2880, 63))
    );
    // Both on: Reflect folds pitch.
    assert_eq!(
        sdp(&lines(&[
            on("c23_wrap_boundary"),
            on("c24_reflect_boundary")
        ])),
        sdp(&r)
    );
    // Time too: the twice-as-long line folded into the box's time — two
    // passes over it, both inside.
    let t = lines(&[
        on("c23_wrap_boundary"),
        ("c23_wrap_boundary.time", ParamValue::Bool(true)),
    ]);
    assert!(t.iter().all(|n| n.end() <= 3840 + 240));
    assert_eq!(t.iter().filter(|n| n.start == 0).count(), 2);
    let t = lines(&[
        on("c24_reflect_boundary"),
        ("c24_reflect_boundary.time", ParamValue::Bool(true)),
    ]);
    assert!(t.iter().all(|n| n.end() <= 3840 + 240));
}

#[test]
fn lines_recursive_subdivision_is_seeded_and_bounded_by_its_displacement() {
    let s = |seed: i64| {
        lines(&[
            on("c25_recursive_line_subdivision"),
            ("gen.seed", ParamValue::Int(seed)),
        ])
    };
    assert_eq!(s(7), s(7));
    assert_ne!(s(7), s(8));
    // Displacement 25 % of 4 rows, halving per level: at most
    // 1 + ½ + ¼ + ⅛ rows from the line.
    for seed in 0..20 {
        assert!(s(seed).iter().all(|n| (58..=65).contains(&n.pitch)));
    }
    // No displacement: a line with more points on the same place.
    let v = lines(&[
        on("c25_recursive_line_subdivision"),
        num("c25_recursive_line_subdivision.displacement", 0.0),
    ]);
    let rows: Vec<u8> = v.iter().map(|n| n.pitch).collect();
    assert_eq!(rows, vec![60, 61, 62, 63]);
}

#[test]
fn lines_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], words: &str| match lines_try(kv) {
        Err(TransformError::TooLarge(m)) => {
            assert!(m.contains(words) && m.ends_with('.'), "{m}");
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    };
    // The path: 4,096 segments × 64 passes.
    refused(
        &[
            on("c25_recursive_line_subdivision"),
            int("c25_recursive_line_subdivision.depth", 12),
            on("c19_alternating_direction"),
            int("c19_alternating_direction.passes", 64),
        ],
        "points",
    );
    // The copies: 2 × 9 crosshatch lines × 32 fan lines × 32 copies.
    refused(
        &[
            on("c17_crosshatch"),
            int("c17_crosshatch.spacing", 1),
            on("c16_radial_fan"),
            int("c16_radial_fan.count", 32),
            on("c05_parallel_copies"),
            int("c05_parallel_copies.count", 32),
        ],
        "copies",
    );
    // The crossing search.
    refused(
        &[
            on("c25_recursive_line_subdivision"),
            int("c25_recursive_line_subdivision.depth", 12),
            on("c05_parallel_copies"),
            int("c05_parallel_copies.count", 32),
            on("c22_intersection_accent"),
        ],
        "Intersection Accent",
    );
}

#[test]
fn lines_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = lines_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        // Without the band (thousands of copies × the box's rows) and with
        // the crossing search kept small.
        s.set(
            "c18_scanline_fill_between_boundaries",
            ParamValue::Bool(false),
        );
        s.set("c04_line_thickness", ParamValue::Bool(false));
        s.set("c17_crosshatch", ParamValue::Bool(false));
        s.set("c22_intersection_accent.lines", ParamValue::Bool(false));
        let c = ctx(&d, 960);
        let a = run(def, &d, &s, &c);
        let b = run(def, &d, &s, &c);
        assert_eq!(a.notes, b.notes);
        assert!(!a.notes.is_empty());
        assert!(a.notes.iter().all(|n| valid(n, keys)));
    }
}

#[test]
fn lines_dashes_snap_to_the_grid_and_restart_the_sampling() {
    // 300 / 400 ticks on the 240 grid: a dash of one step every three.
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c11_dash_pattern"),
            int("c11_dash_pattern.dash", 300),
            int("c11_dash_pattern.gap", 400),
        ],
    ));
    assert_eq!(
        v.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        [0, 720, 1440, 2160, 2880, 3600].map(|s| (s, 240)).to_vec()
    );
    // A gap under half a step is no gap: the plain line.
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c11_dash_pattern"),
            int("c11_dash_pattern.dash", 100),
            int("c11_dash_pattern.gap", 100),
        ],
    ));
    assert_eq!(sdp(&v), vec![(0, 3840, 60)]);
    // Sampling every 3 units with dashes of 2 steps every 4: each dash
    // starts with a note; the next sample (3 units on) falls in the gap.
    let v = lines(&plus(
        flat(0, 3840),
        &[
            on("c06_note_spacing_density"),
            num("c06_note_spacing_density.every", 3.0),
            on("c11_dash_pattern"),
        ],
    ));
    assert_eq!(
        v.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![0, 960, 1920, 2880]
    );
}

#[test]
fn lines_a_huge_request_is_refused_at_once_and_a_long_gradient_costs_its_brushes() {
    let huge = [
        int("gen.length", MAX_LENGTH as i64),
        int("gen.step", 11),
        int("gen.step_ticks", 1),
    ];
    // A note every grid unit over 2^36 one-tick steps: the lower bound
    // passes the counting cap, so nothing is counted.
    let t = std::time::Instant::now();
    match lines_try(&plus(
        huge.to_vec(),
        &[
            on("c06_note_spacing_density"),
            num("c06_note_spacing_density.every", 1.0),
        ],
    )) {
        Err(TransformError::TooLarge(m)) => assert!(m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    // Dashes of one step every two: the same.
    match lines_try(&plus(
        huge.to_vec(),
        &[
            on("c11_dash_pattern"),
            int("c11_dash_pattern.dash", 1),
            int("c11_dash_pattern.gap", 1),
        ],
    )) {
        Err(TransformError::TooLarge(m)) => assert!(m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    // A velocity gradient along a horizontal of 2^36 steps: at most
    // MAX_PIECES pieces, merged by velocity — one note per velocity, in
    // order.
    let t = std::time::Instant::now();
    let v = lines(&plus(
        huge.to_vec(),
        &[
            on("c03_polyline_segments"),
            ("c03_polyline_segments.heights", ParamValue::List(vec![50])),
            on("c09_velocity_gradient"),
        ],
    ));
    assert!(v.len() <= 88 && v.len() > 80, "{}", v.len());
    assert!(
        v.windows(2)
            .all(|w| w[0].vel > w[1].vel && w[0].end() == w[1].start)
    );
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

#[test]
fn lines_over_a_box_spanning_every_tick_stay_quick() {
    // The selected notes' box of a note from tick 0 to the last tick: far
    // positions lose precision in floating point; dashed, sampled and
    // crosshatched lines there are refused at once by the lower bound or
    // stop where a dash no longer has a width — never a long walk.
    let def = lines_def();
    let d = doc(KeyRange::Standard, &[rec(0, u64::MAX - 1, 60)]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[("gen.place", ParamValue::Choice(2))],
    );
    let t = std::time::Instant::now();
    for kv in [
        vec![on("c11_dash_pattern")],
        vec![on("c06_note_spacing_density"), on("c11_dash_pattern")],
        vec![
            on("c06_note_spacing_density"),
            on("c11_dash_pattern"),
            on("c17_crosshatch"),
        ],
        vec![on("c12_dot_pattern"), on("c17_crosshatch")],
        vec![on("c09_velocity_gradient"), on("c04_line_thickness")],
    ] {
        match run_in(
            def,
            &d,
            &with(base.clone(), &kv),
            &ctx(&d, 0),
            &Limits::default(),
        ) {
            Ok(r) => assert!(r.notes.iter().all(|n| valid(n, KeyRange::Standard))),
            Err(e) => assert!(e.user_message().ends_with('.'), "{e}"),
        }
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

// ------------------------------------------------- Rectangle / Square (V09-S53)
//
// What each of Rectangle's 25 functions means (V09-0090, `docs/GENERATORS.md`
// "Rectangle / Square"), on a small box: ticks 960 … 4,800 (cells 4 … 19 of
// the 240 snap), rows 60 … 67 — expected cells computed by hand or by an
// independent formula in the test.

use std::collections::BTreeSet;

fn rect_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.rect")
        .unwrap()
}

fn rect_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = rect_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(67)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn rect(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    rect_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

fn ch(k: &'static str, v: u32) -> (&'static str, ParamValue) {
    (k, ParamValue::Choice(v))
}

/// The cells (column = tick / 240, row) the notes cover (their whole
/// length, gate 100 %); no two notes share a cell, and touching notes on
/// a row differ in brush (the pen merged the others).
fn cells(v: &[TNote]) -> BTreeSet<(i64, u8)> {
    let mut out = BTreeSet::new();
    for n in v {
        assert_eq!(n.start % 240, 0, "{n:?}");
        assert_eq!(n.dur % 240, 0, "{n:?}");
        for c in n.start / 240..n.end() / 240 {
            assert!(
                out.insert((c as i64, n.pitch)),
                "overlap at {c}, {}",
                n.pitch
            );
        }
    }
    for a in v {
        for b in v {
            if a.pitch == b.pitch && a.end() == b.start {
                assert!((a.vel, a.sub) != (b.vel, b.sub), "unmerged {a:?} {b:?}");
            }
        }
    }
    out
}

/// The cells of `[c0, c1) × [r0, r1)`.
fn block(c0: i64, c1: i64, r0: u8, r1: u8) -> BTreeSet<(i64, u8)> {
    let mut s = BTreeSet::new();
    for c in c0..c1 {
        for r in r0..r1 {
            s.insert((c, r));
        }
    }
    s
}

/// The outline `t` cells wide of `[c0, c1) × [r0, r1)` (inside it).
fn frame_of(c0: i64, c1: i64, r0: u8, r1: u8, t: u8) -> BTreeSet<(i64, u8)> {
    let inner = block(c0 + i64::from(t), c1 - i64::from(t), r0 + t, r1 - t);
    block(c0, c1, r0, r1).difference(&inner).copied().collect()
}

/// The box's interior cells (inside the one-cell outline).
fn interior() -> BTreeSet<(i64, u8)> {
    block(5, 19, 61, 67)
}

#[test]
fn rect_the_base_is_the_box_outline_one_cell_wide() {
    let v = rect(&[]);
    // The bottom and top rows one note each; each side one step per row.
    let mut want = vec![(960, 3840, 60), (960, 3840, 67)];
    for r in 61..67 {
        want.push((960, 240, r));
        want.push((4560, 240, r));
    }
    want.sort_by_key(|&(s, _, p)| (p, s));
    assert_eq!(sdp(&v), want);
    assert_eq!(cells(&v), frame_of(4, 20, 60, 68, 1));
}

#[test]
fn rect_width_height_and_square_place_the_rectangle_in_the_box() {
    // Width 1,920 ticks (8 steps) at the box's centre; 4 rows at its top.
    let v = rect(&[
        on("c01_width"),
        int("c01_width.width", 1920),
        ch("c01_width.align", 1),
        on("c02_height"),
        int("c02_height.rows", 4),
        ch("c02_height.align", 2),
    ]);
    assert_eq!(cells(&v), frame_of(8, 16, 64, 68, 1));
    // At the start / bottom by default; the end.
    let v = rect(&[on("c01_width"), ch("c01_width.align", 2)]);
    assert_eq!(cells(&v), frame_of(12, 20, 60, 68, 1));
    // A square on the grid: the smaller side (8 rows → 8 steps); the width
    // (16 steps → 16 rows); the height.
    let v = rect(&[on("c03_square_aspect_lock")]);
    assert_eq!(cells(&v), frame_of(4, 12, 60, 68, 1));
    let v = rect(&[
        on("c03_square_aspect_lock"),
        ch("c03_square_aspect_lock.follow", 1),
    ]);
    assert_eq!(cells(&v), frame_of(4, 20, 60, 76, 1));
    let v = rect(&[
        on("c03_square_aspect_lock"),
        ch("c03_square_aspect_lock.follow", 2),
    ]);
    assert_eq!(cells(&v), frame_of(4, 12, 60, 68, 1));
}

#[test]
fn rect_outline_mode_places_the_outline_outside_centred_or_inside() {
    assert_eq!(
        cells(&rect(&[on("c04_outline_mode")])),
        frame_of(3, 21, 59, 69, 1)
    );
    // Centred, two cells: one out, one in.
    let v = rect(&[
        on("c04_outline_mode"),
        ch("c04_outline_mode.placement", 1),
        on("c06_border_thickness"),
    ]);
    assert_eq!(cells(&v), frame_of(3, 21, 59, 69, 2));
    let inside = rect(&[on("c04_outline_mode"), ch("c04_outline_mode.placement", 2)]);
    assert_eq!(inside, rect(&[]));
}

#[test]
fn rect_filled_mode_and_border_thickness() {
    let v = rect(&[on("c05_filled_mode")]);
    assert_eq!(
        sdp(&v),
        (60..68).map(|r| (960, 3840, r)).collect::<Vec<_>>()
    );
    let v = rect(&[on("c06_border_thickness")]);
    assert_eq!(cells(&v), frame_of(4, 20, 60, 68, 2));
    let v = rect(&[
        on("c06_border_thickness"),
        int("c06_border_thickness.size", 3),
    ]);
    assert_eq!(cells(&v), frame_of(4, 20, 60, 68, 3));
}

#[test]
fn rect_chamfered_and_rounded_corners_cut_the_corner_cells() {
    // A cut of 3 units: at the bottom row's centre (half a row up) the
    // octagon spans 4 + 2.5 … 20 − 2.5 = 6.5 … 17.5, rounded 7 … 18.
    let v = rect(&[on("c08_chamfered_corners")]);
    let c = cells(&v);
    let row = |r: u8| -> Vec<i64> { c.iter().filter(|x| x.1 == r).map(|x| x.0).collect() };
    assert_eq!(row(60), (7..18).collect::<Vec<_>>());
    assert_eq!(row(67), (7..18).collect::<Vec<_>>());
    // Half-way up the sides are untouched.
    assert_eq!(row(63), vec![4, 19]);
    assert_eq!(row(64), vec![4, 19]);
    assert!(!c.contains(&(4, 60)) && !c.contains(&(19, 67)));
    // Rounded corners of radius 3: the corner cells go, the middle stays;
    // a larger radius cuts more.
    let r3 = cells(&rect(&[on("c07_rounded_corners")]));
    let r5 = cells(&rect(&[
        on("c07_rounded_corners"),
        num("c07_rounded_corners.radius", 5.0),
    ]));
    for c in [&r3, &r5] {
        assert!(!c.contains(&(4, 60)) && !c.contains(&(19, 60)));
        assert!(c.contains(&(4, 63)) && c.contains(&(19, 64)));
        assert!(c.contains(&(12, 60)) && c.contains(&(12, 67)));
    }
    let bottom = |c: &BTreeSet<(i64, u8)>| c.iter().filter(|x| x.1 == 60).count();
    assert!(bottom(&r5) < bottom(&r3) && bottom(&r3) < 16);
    // Both: the chamfer's corners rounded (still inside the chamfer).
    let both = cells(&rect(&[
        on("c08_chamfered_corners"),
        on("c07_rounded_corners"),
    ]));
    assert!(both.iter().all(|x| x.1 != 60 || (7..18).contains(&x.0)));
}

#[test]
fn rect_rotation_shear_and_perspective_map_the_shape_on_the_grid() {
    // 90° about the centre (12, 64): 8 steps wide, 16 rows tall.
    let v = rect(&[on("c09_rotation"), num("c09_rotation.angle", 90.0)]);
    assert_eq!(cells(&v), frame_of(8, 16, 56, 72, 1));
    // 30° shear: row r's span moves by (r + ½ − 64) · tan 30° steps.
    let c = cells(&rect(&[on("c10_shear"), num("c10_shear.angle", 30.0)]));
    for r in 60u8..68 {
        let k = (f64::from(r) + 0.5 - 64.0) * 30f64.to_radians().tan();
        let xs: Vec<i64> = c.iter().filter(|x| x.1 == r).map(|x| x.0).collect();
        assert_eq!(xs[0], (4.0 + k).round() as i64, "row {r}");
        assert_eq!(
            *xs.last().unwrap() + 1,
            (20.0 + k).round() as i64,
            "row {r}"
        );
        if r == 60 || r == 67 {
            assert_eq!(xs.len(), 16, "row {r}");
        }
    }
    // Perspective, top at 50 %: the bottom kept; at the top row's centre
    // (7.5 / 8 of the height drawn) the source height y solves
    // 0.9375 = 2y / (1 + y), so the span is 12 ± 8 / (1 + y) = 7.75 … 16.25.
    let c = cells(&rect(&[on("c11_perspective")]));
    let row = |r: u8| -> Vec<i64> { c.iter().filter(|x| x.1 == r).map(|x| x.0).collect() };
    assert_eq!(row(60), (4..20).collect::<Vec<_>>());
    assert_eq!(row(67), (8..16).collect::<Vec<_>>());
    // The sides lean in, straight: each row's first cell moves right.
    let firsts: Vec<i64> = (61..67).map(|r| row(r)[0]).collect();
    assert!(firsts.windows(2).all(|w| w[0] <= w[1]) && firsts[5] > firsts[0]);
}

#[test]
fn rect_nested_rectangles_and_concentric_spacing() {
    // A 16 × 16 box (rows 60 … 75): three nested outlines evenly spaced —
    // the inradius 8 over 4 gives 2 units.
    let tall = [int("gen.high", 75)];
    let v = rect(&plus(tall.to_vec(), &[on("c12_nested_rectangles")]));
    let mut want = frame_of(4, 20, 60, 76, 1);
    for k in 1..=3u8 {
        let d = 2 * i64::from(k);
        want.extend(frame_of(4 + d, 20 - d, 60 + 2 * k, 76 - 2 * k, 1));
    }
    assert_eq!(cells(&v), want);
    // Every 3 units, as many as fit (8 / 3 → 2).
    let v = rect(&plus(tall.to_vec(), &[on("c13_concentric_spacing")]));
    let mut want = frame_of(4, 20, 60, 76, 1);
    want.extend(frame_of(7, 17, 63, 73, 1));
    want.extend(frame_of(10, 14, 66, 70, 1));
    assert_eq!(cells(&v), want);
    // Both: the count and the spacing.
    let v = rect(&plus(
        tall.to_vec(),
        &[
            on("c12_nested_rectangles"),
            int("c12_nested_rectangles.count", 1),
            on("c13_concentric_spacing"),
            int("c13_concentric_spacing.spacing", 4),
        ],
    ));
    let mut want = frame_of(4, 20, 60, 76, 1);
    want.extend(frame_of(8, 16, 64, 72, 1));
    assert_eq!(cells(&v), want);
}

/// The outline plus the interior cells `keep` chooses.
fn with_interior(keep: impl Fn(i64, u8) -> bool) -> BTreeSet<(i64, u8)> {
    let mut s = frame_of(4, 20, 60, 68, 1);
    s.extend(interior().into_iter().filter(|&(c, r)| keep(c, r)));
    s
}

#[test]
fn rect_stripes_crosshatch_and_grid_fill_the_interior() {
    let v = rect(&[on("c14_horizontal_stripe_fill")]);
    assert_eq!(cells(&v), with_interior(|_, r| (r - 60) % 2 == 0));
    // A stripe row merges with the outline into one note.
    assert!(sdp(&v).contains(&(960, 3840, 62)));
    let v = rect(&[
        on("c15_vertical_stripe_fill"),
        int("c15_vertical_stripe_fill.spacing", 3),
    ]);
    assert_eq!(cells(&v), with_interior(|c, _| (c - 4) % 3 == 0));
    let v = rect(&[on("c16_crosshatch_fill")]);
    assert_eq!(
        cells(&v),
        with_interior(|c, r| {
            let (x, y) = (c - 4, i64::from(r) - 60);
            (x - y).rem_euclid(4) == 0 || (x + y).rem_euclid(4) == 0
        })
    );
    let v = rect(&[on("c17_grid_fill"), int("c17_grid_fill.spacing", 3)]);
    assert_eq!(
        cells(&v),
        with_interior(|c, r| (c - 4) % 3 == 0 || (r - 60) % 3 == 0)
    );
}

#[test]
fn rect_filled_mode_draws_between_the_patterns() {
    let v = rect(&[on("c05_filled_mode"), on("c15_vertical_stripe_fill")]);
    assert_eq!(cells(&v), with_interior(|c, _| (c - 4) % 2 != 0));
    let v = rect(&[on("c05_filled_mode"), on("c14_horizontal_stripe_fill")]);
    assert_eq!(cells(&v), with_interior(|_, r| (r - 60) % 2 != 0));
}

#[test]
fn rect_diagonal_corner_lines_join_the_corners_inside() {
    // From (4, 60) to (20, 68): two steps across per row; and from (4, 68)
    // to (20, 60). Each row's slab crossing, clipped to the interior.
    let slash = |c: i64, r: u8| {
        let k = i64::from(r) - 60;
        (4 + 2 * k..4 + 2 * (k + 1)).contains(&c)
    };
    let back = |c: i64, r: u8| {
        let k = 67 - i64::from(r);
        (4 + 2 * k..4 + 2 * (k + 1)).contains(&c)
    };
    assert_eq!(
        cells(&rect(&[on("c18_diagonal_corner_lines")])),
        with_interior(|c, r| slash(c, r) || back(c, r))
    );
    assert_eq!(
        cells(&rect(&[
            on("c18_diagonal_corner_lines"),
            ch("c18_diagonal_corner_lines.which", 1)
        ])),
        with_interior(slash)
    );
    assert_eq!(
        cells(&rect(&[
            on("c18_diagonal_corner_lines"),
            ch("c18_diagonal_corner_lines.which", 2)
        ])),
        with_interior(back)
    );
}

#[test]
fn rect_per_edge_subchannels_colour_each_side() {
    let v = rect(&[on("c19_per_edge_subchannels")]);
    for n in &v {
        // Positions 0 … 3 = subchannels 0 … 3 of the test track.
        let want = match (n.pitch, n.start) {
            (60, _) => 0,
            (67, _) => 2,
            (_, 960) => 3,
            _ => 1,
        };
        assert_eq!((n.sub, u32::from(n.ch)), (want, want), "{n:?}");
    }
    assert_eq!(cells(&v), frame_of(4, 20, 60, 68, 1));
}

#[test]
fn rect_per_edge_density_keeps_every_nth_cell_of_each_side() {
    let v = rect(&[
        on("c20_per_edge_density"),
        int("c20_per_edge_density.right", 3),
        int("c20_per_edge_density.top", 1),
        int("c20_per_edge_density.left", 1),
    ]);
    // Bottom: every 2nd cell from its start; right (going up, 16 units
    // round the outline): rows where (16 + r + ½ − 60) mod 3 < 1.
    let mut want: BTreeSet<(i64, u8)> = (4..20).step_by(2).map(|c| (c, 60)).collect();
    for r in 61u8..67 {
        want.insert((4, r));
        if (16.0 + f64::from(r) + 0.5 - 60.0).rem_euclid(3.0) < 1.0 {
            want.insert((19, r));
        }
    }
    want.extend((4..20).map(|c| (c, 67)));
    assert_eq!(cells(&v), want);
}

#[test]
fn rect_dashes_run_round_the_outline_with_a_phase() {
    // Dashes of 2 cells every 4, phase 1: on where (u + 1) mod 4 < 2, u
    // the position along the outline (bottom: c − 4; right: 16 + r + ½ −
    // 60; top, leftwards: 24 + 20 − c − ½ …).
    let v = rect(&[
        on("c23_dashed_border_phase"),
        num("c23_dashed_border_phase.phase", 1.0),
    ]);
    let on_at = |u: f64| (u + 1.0).rem_euclid(4.0) < 2.0;
    let c = cells(&v);
    let bottom: Vec<i64> = c.iter().filter(|x| x.1 == 60).map(|x| x.0).collect();
    assert_eq!(
        bottom,
        (4..20)
            .filter(|&x| on_at((x - 4) as f64 + 0.5))
            .collect::<Vec<_>>()
    );
    assert_eq!(bottom, vec![4, 7, 8, 11, 12, 15, 16, 19]);
    for r in 61u8..67 {
        let u = 16.0 + f64::from(r) + 0.5 - 60.0;
        assert_eq!(c.contains(&(19, r)), on_at(u), "row {r}");
    }
    // Another phase moves them.
    let v0 = rect(&[on("c23_dashed_border_phase")]);
    assert_ne!(cells(&v0), c);
    let b0: Vec<i64> = cells(&v0)
        .iter()
        .filter(|x| x.1 == 60)
        .map(|x| x.0)
        .collect();
    assert_eq!(b0, vec![4, 5, 8, 9, 12, 13, 16, 17]);
}

#[test]
fn rect_gradients_change_velocity_and_gate_across_the_shape() {
    // Filled, velocity 127 → 40 from the start to the end: cell c's centre
    // is at (c + ½ − 4) / 16; equal neighbours merge.
    let vel = |c: i64| (127.0 + (40.0 - 127.0) * ((c - 4) as f64 + 0.5) / 16.0).round() as u8;
    let v = rect(&[on("c05_filled_mode"), on("c21_velocity_gradient")]);
    for r in 60u8..68 {
        let got: Vec<(u64, u64, u8)> = v
            .iter()
            .filter(|n| n.pitch == r)
            .map(|n| (n.start, n.dur, n.vel))
            .collect();
        let want: Vec<(u64, u64, u8)> = runs(&(4..20).map(vel).collect::<Vec<_>>())
            .into_iter()
            .map(|(s, d, x)| (s + 960, d, x))
            .collect();
        assert_eq!(got, want, "row {r}");
    }
    // Bottom to top: one velocity per row.
    let v = rect(&[
        on("c05_filled_mode"),
        on("c21_velocity_gradient"),
        ch("c21_velocity_gradient.direction", 1),
    ]);
    for n in &v {
        let s = (f64::from(n.pitch) + 0.5 - 60.0) / 8.0;
        assert_eq!(n.vel, (127.0 + (40.0 - 127.0) * s).round() as u8);
        assert_eq!((n.start, n.dur), (960, 3840));
    }
    // Gate 100 → 25 % bottom to top: each row's note keeps its share.
    let v = rect(&[
        on("c05_filled_mode"),
        on("c22_gate_gradient"),
        ch("c22_gate_gradient.direction", 1),
    ]);
    for n in &v {
        let s = (f64::from(n.pitch) + 0.5 - 60.0) / 8.0;
        let g = ((1.0 + (0.25 - 1.0) * s) * 100.0).round() / 100.0;
        assert_eq!(n.dur, (3840.0 * g).round() as u64, "{n:?}");
    }
    // Around the shape from its bottom-left corner, counter-clockwise: the
    // bottom row starts loud, the left side (last) is the quietest.
    let v = rect(&[
        on("c21_velocity_gradient"),
        ch("c21_velocity_gradient.direction", 2),
    ]);
    let mean = |f: &dyn Fn(&&TNote) -> bool| {
        let s: Vec<f64> = v.iter().filter(f).map(|n| f64::from(n.vel)).collect();
        s.iter().sum::<f64>() / s.len() as f64
    };
    let bottom = mean(&|n| n.pitch == 60 && n.start > 960);
    let right = mean(&|n| (61..67).contains(&n.pitch) && n.start == 4560);
    let top = mean(&|n| n.pitch == 67);
    let left = mean(&|n| (61..67).contains(&n.pitch) && n.start == 960);
    assert!(
        bottom > right && right > top && top > left,
        "{bottom} {right} {top} {left}"
    );
}

#[test]
fn rect_recursive_subdivision_is_seeded_and_walls_split_the_interior() {
    let walls = |seed: i64, depth: i64| {
        let c = cells(&rect(&[
            on("c24_recursive_subdivision"),
            int("c24_recursive_subdivision.depth", depth),
            int("gen.seed", seed),
        ]));
        let outline = frame_of(4, 20, 60, 68, 1);
        assert!(outline.is_subset(&c));
        c.difference(&outline).copied().collect::<BTreeSet<_>>()
    };
    // Depth 1: one wall across the longer side (14 × 6 → a column through
    // every interior row), between 30 and 70 % of the way.
    for seed in 1..8 {
        let w = walls(seed, 1);
        let cols: BTreeSet<i64> = w.iter().map(|x| x.0).collect();
        assert_eq!(cols.len(), 1, "seed {seed}");
        let c = *cols.iter().next().unwrap();
        assert!((9..=14).contains(&c), "seed {seed}: {c}");
        assert_eq!(w, block(c, c + 1, 61, 67));
    }
    // Seeded: the same seed the same walls, other seeds others.
    assert_eq!(walls(5, 3), walls(5, 3));
    let all: BTreeSet<Vec<(i64, u8)>> = (1..8).map(|s| walls(s, 3).into_iter().collect()).collect();
    assert!(all.len() > 1);
}

#[test]
fn rect_a_hole_cuts_the_middle_with_its_own_outline() {
    // 50 %: the hole [8, 16) × [62, 66); its outline the ring around it.
    let hole = block(8, 16, 62, 66);
    let ring: BTreeSet<(i64, u8)> = block(7, 17, 61, 67).difference(&hole).copied().collect();
    let v = rect(&[on("c25_hole_negative_interior_mask")]);
    let mut want = frame_of(4, 20, 60, 68, 1);
    want.extend(ring.iter().copied());
    assert_eq!(cells(&v), want);
    // Filled: a frame — every cell but the hole's.
    let v = rect(&[on("c25_hole_negative_interior_mask"), on("c05_filled_mode")]);
    assert_eq!(
        cells(&v),
        block(4, 20, 60, 68).difference(&hole).copied().collect()
    );
    // Patterns stop at it.
    let v = rect(&[
        on("c25_hole_negative_interior_mask"),
        on("c15_vertical_stripe_fill"),
    ]);
    assert!(cells(&v).is_disjoint(&hole));
}

#[test]
fn rect_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = rect_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run(def, &d, &s, &c);
        let b = run(def, &d, &s, &c);
        assert_eq!(a.notes, b.notes);
        assert!(!a.notes.is_empty());
        assert!(a.notes.iter().all(|n| valid(n, keys)));
    }
}

#[test]
fn rect_a_huge_request_is_refused_at_once_and_a_long_gradient_costs_its_brushes() {
    let huge = [
        int("gen.length", MAX_LENGTH as i64),
        int("gen.step", 11),
        int("gen.step_ticks", 1),
    ];
    let t = std::time::Instant::now();
    // Stripes every 2 cells over 2^36 one-tick steps × 6 rows, and dashes
    // along the outline: the lower bound passes the counting cap.
    for kv in [
        vec![on("c15_vertical_stripe_fill")],
        vec![on("c16_crosshatch_fill")],
        vec![on("c23_dashed_border_phase")],
        vec![on("c20_per_edge_density")],
        vec![on("c05_filled_mode"), on("c17_grid_fill")],
    ] {
        match rect_try(&plus(huge.to_vec(), &kv)) {
            Err(TransformError::TooLarge(m)) => assert!(m.ends_with('.'), "{m}"),
            other => panic!("{kv:?}: {:?}", other.map(|v| v.len())),
        }
    }
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    // A velocity gradient across a filled box 2^36 steps wide: at most
    // MAX_PIECES pieces per row, merged by velocity — one note per
    // velocity and row, in order.
    let t = std::time::Instant::now();
    let v = rect(&plus(
        huge.to_vec(),
        &[on("c05_filled_mode"), on("c21_velocity_gradient")],
    ));
    for r in 60u8..68 {
        let row: Vec<&TNote> = v.iter().filter(|n| n.pitch == r).collect();
        assert!(row.len() <= 88 && row.len() > 80, "{}", row.len());
        assert!(
            row.windows(2)
                .all(|w| w[0].vel > w[1].vel && w[0].end() == w[1].start)
        );
    }
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

#[test]
fn rect_over_a_box_spanning_every_tick_stays_quick() {
    let def = rect_def();
    let d = doc(KeyRange::Standard, &[rec(0, u64::MAX - 1, 60)]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[("gen.place", ParamValue::Choice(2))],
    );
    let t = std::time::Instant::now();
    for kv in [
        vec![on("c15_vertical_stripe_fill")],
        vec![on("c16_crosshatch_fill"), on("c05_filled_mode")],
        vec![on("c23_dashed_border_phase"), on("c09_rotation")],
        vec![on("c21_velocity_gradient"), on("c06_border_thickness")],
        vec![on("c18_diagonal_corner_lines"), on("c17_grid_fill")],
        vec![on("c12_nested_rectangles"), on("c24_recursive_subdivision")],
    ] {
        match run_in(
            def,
            &d,
            &with(base.clone(), &kv),
            &ctx(&d, 0),
            &Limits::default(),
        ) {
            Ok(r) => assert!(r.notes.iter().all(|n| valid(n, KeyRange::Standard))),
            Err(e) => assert!(e.user_message().ends_with('.'), "{e}"),
        }
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

/// A refactoring check (V09-S53, not a pass / fail test): one line per seeded
/// random draw of generator `PLUMSPIRIT_GEN_DIGEST_TOOL` (default gen.lines)
/// in both key ranges — the refusal, or the bound, the note count and a
/// digest of the notes — written to `PLUMSPIRIT_GEN_DIGEST_OUT`. Run it
/// before and after a change that must keep a generator byte for byte and
/// compare the files (`PLUMSPIRIT_GEN_DIGEST_N` draws per key range, 300).
#[test]
#[ignore]
fn generator_output_digest_dump() {
    use std::io::Write;
    let tool = std::env::var("PLUMSPIRIT_GEN_DIGEST_TOOL").unwrap_or("gen.lines".into());
    let out = std::env::var("PLUMSPIRIT_GEN_DIGEST_OUT").expect("PLUMSPIRIT_GEN_DIGEST_OUT");
    let n: u64 = std::env::var("PLUMSPIRIT_GEN_DIGEST_N")
        .map(|v| v.parse().expect("a number"))
        .unwrap_or(300);
    let def = generators()
        .into_iter()
        .find(|t| t.id == tool)
        .expect("a generator id");
    let mut f = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        for seed in 0..n {
            let mut r = Rng::new(0x9000 + seed);
            let mut s = defaults(def, keys);
            for p in &(def.spec)().params {
                if p.key.starts_with("b.") {
                    continue;
                }
                if p.key == "gen.length" {
                    s.set(p.key, ParamValue::Int(r.range_i64(1, 20000)));
                    continue;
                }
                if let Some(v) = random_value(p, &mut r, keys) {
                    s.set(p.key, v);
                }
            }
            // Half the draws: each function on with probability ¼ only.
            if seed % 2 == 0 {
                for fun in &(def.spec)().functions {
                    s.set(fun.id, ParamValue::Bool(r.below(4) == 0));
                }
            }
            let c = ctx(&d, r.below(5000));
            let src = collect_for(def, &d, &SCOPE, 1_000_000).unwrap();
            let b = BuildCtx {
                ctx: &c,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let line = match def.kernel(&s, &b) {
                Err(e) => format!("ERR {e}"),
                Ok(k) => {
                    let est = k.estimate(&src.stats).notes;
                    let lim = Limits {
                        max_notes: 2_000_000,
                        ..Limits::default()
                    };
                    match run_kernel(k.as_ref(), &src, def.boundary(&s), &lim, None) {
                        Err(e) => format!("bound {est} RUNERR {e}"),
                        Ok(run) => {
                            let mut h = 0xcbf2_9ce4_8422_2325u64;
                            for n in &run.notes {
                                for v in [
                                    n.start,
                                    n.dur,
                                    u64::from(n.pitch),
                                    u64::from(n.vel),
                                    u64::from(n.sub),
                                    u64::from(n.ch),
                                ] {
                                    h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
                                }
                            }
                            format!("bound {est} n {} h {h:016x}", run.notes.len())
                        }
                    }
                }
            };
            writeln!(f, "{keys:?} {seed} {line}").unwrap();
        }
    }
}

// ------------------------------------------------------------- Circles (V09-S54)
//
// What each of Circles' 25 functions means (V09-0091, `docs/GENERATORS.md`
// "Circles"), on the Rectangle tests' box: ticks 960 … 4,800 (cells 4 … 19),
// rows 60 … 67 — its inscribed ellipse has its centre at (12, 64) and radii
// 8 steps and 4 rows. Expected rows from the exact curve where the polygon's
// chords (≤ 0.05 units off) cannot change the rounding.

fn circ_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.circles")
        .unwrap()
}

fn circ_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = circ_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(67)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn circ(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    circ_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

/// The cells of an ellipse filled by the row-centre rule: centre (cu, cv),
/// radii (rx, ry), rows `r0 .. r1`.
fn ellipse_cells(cu: f64, cv: f64, rx: f64, ry: f64) -> BTreeSet<(i64, u8)> {
    let mut s = BTreeSet::new();
    for r in 0u8..=127 {
        let dy = (f64::from(r) + 0.5 - cv) / ry;
        if dy.abs() >= 1.0 {
            continue;
        }
        let h = rx * (1.0 - dy * dy).sqrt();
        let (a, b) = ((cu - h).round() as i64, (cu + h).round() as i64);
        for c in a..b.max(a + 1) {
            s.insert((c, r));
        }
    }
    s
}

/// Each row's cells.
fn rows_of(c: &BTreeSet<(i64, u8)>) -> std::collections::BTreeMap<u8, Vec<i64>> {
    let mut m: std::collections::BTreeMap<u8, Vec<i64>> = Default::default();
    for &(x, r) in c {
        m.entry(r).or_default().push(x);
    }
    m
}

#[test]
fn circ_the_base_is_the_outline_of_the_ellipse_inscribed_in_the_box() {
    // Filled: rows [8, 16), [6, 18), [5, 19), [4, 20), mirrored.
    let disc = cells(&circ(&[on("c06_filled_disc")]));
    assert_eq!(disc, ellipse_cells(12.0, 64.0, 8.0, 4.0));
    assert_eq!(
        rows_of(&disc)[&61],
        (6..18).collect::<Vec<_>>(),
        "row 61: 8 · √(1 − (2.5 / 4)²) = 6.245 either side"
    );
    // The outline: inside the disc, the top and bottom rows whole, each
    // other row's ends the disc's, a hollow middle.
    let o = cells(&circ(&[]));
    assert!(o.is_subset(&disc));
    let (rd, ro) = (rows_of(&disc), rows_of(&o));
    for r in 60u8..68 {
        assert_eq!(ro[&r].first(), rd[&r].first(), "row {r}");
        assert_eq!(ro[&r].last(), rd[&r].last(), "row {r}");
    }
    assert_eq!(ro[&60], rd[&60]);
    assert!(!o.contains(&(12, 63)) && !o.contains(&(12, 64)));
}

#[test]
fn circ_radius_radii_centre_and_rotation_place_the_curve() {
    // A circle of radius 3 at (12, 64).
    let c = cells(&circ(&[
        on("c06_filled_disc"),
        on("c02_radius"),
        num("c02_radius.radius", 3.0),
    ]));
    assert_eq!(c, ellipse_cells(12.0, 64.0, 3.0, 3.0));
    // Radii 6 across and 3 up.
    let c = cells(&circ(&[
        on("c06_filled_disc"),
        on("c03_ellipse_x_y_radius"),
        num("c03_ellipse_x_y_radius.x", 6.0),
        num("c03_ellipse_x_y_radius.y", 3.0),
    ]));
    assert_eq!(c, ellipse_cells(12.0, 64.0, 6.0, 3.0));
    // Turned 90°: 3 across and 6 up.
    let c = cells(&circ(&[
        on("c06_filled_disc"),
        on("c03_ellipse_x_y_radius"),
        num("c03_ellipse_x_y_radius.x", 6.0),
        num("c03_ellipse_x_y_radius.y", 3.0),
        on("c04_ellipse_rotation"),
        num("c04_ellipse_rotation.angle", 90.0),
    ]));
    assert_eq!(c, ellipse_cells(12.0, 64.0, 3.0, 6.0));
    // The centre at tick 960 + 1,920 (cell 12) and pitch 63 (row centre
    // 63.5): rows 60 … 66.
    let c = cells(&circ(&[
        on("c06_filled_disc"),
        on("c01_center"),
        int("c01_center.time", 1920),
        int("c01_center.pitch", 63),
    ]));
    assert_eq!(c, ellipse_cells(12.0, 63.5, 8.0, 4.0));
    assert_eq!(rows_of(&c)[&60], (7..17).collect::<Vec<_>>());
}

#[test]
fn circ_ring_thickness_and_concentric_rings() {
    let disc = ellipse_cells(12.0, 64.0, 8.0, 4.0);
    let one = cells(&circ(&[]));
    let two = cells(&circ(&[on("c05_ring_thickness")]));
    assert!(one.is_subset(&two) && two.is_subset(&disc) && two.len() > one.len());
    // A circle of radius 6 with one ring inside at half its radius: the
    // ring is the outline of a circle of radius 3 (moved in along the
    // normals — exact for a circle).
    let r6 = [on("c02_radius"), num("c02_radius.radius", 6.0)];
    let outer = cells(&circ(&r6));
    let nested = cells(&circ(&plus(
        r6.to_vec(),
        &[
            on("c07_concentric_rings"),
            int("c07_concentric_rings.count", 1),
        ],
    )));
    let inner: BTreeSet<(i64, u8)> = nested.difference(&outer).copied().collect();
    let r3 = ellipse_cells(12.0, 64.0, 3.0, 3.0);
    assert!(inner.is_subset(&r3), "{inner:?}");
    for r in 61u8..67 {
        let row: Vec<i64> = inner.iter().filter(|x| x.1 == r).map(|x| x.0).collect();
        let want: Vec<i64> = r3.iter().filter(|x| x.1 == r).map(|x| x.0).collect();
        assert_eq!(row.first(), want.first(), "row {r}");
        assert_eq!(row.last(), want.last(), "row {r}");
    }
    // The spacing graph alone: its own count of rings, placed by the curve.
    let g = cells(&circ(&plus(r6.to_vec(), &[on("c08_ring_spacing_graph")])));
    assert!(g.len() > outer.len() && outer.is_subset(&g));
}

#[test]
fn circ_spokes_sectors_and_chords_fill_the_interior_or_its_gaps() {
    let r6 = vec![on("c02_radius"), num("c02_radius.radius", 6.0)];
    let outline = cells(&circ(&r6));
    let disc = ellipse_cells(12.0, 64.0, 6.0, 6.0);
    let inside: BTreeSet<(i64, u8)> = disc.difference(&outline).copied().collect();
    // Four spokes at 0°: the centre row edge (64 — the row 64 above it)
    // and the column at 12.
    let c = cells(&circ(&plus(
        r6.clone(),
        &[on("c09_radial_spokes"), int("c09_radial_spokes.count", 4)],
    )));
    let want: BTreeSet<(i64, u8)> = inside
        .iter()
        .filter(|&&(x, r)| r == 64 || x == 12)
        .copied()
        .collect();
    assert_eq!(
        c.difference(&outline).copied().collect::<BTreeSet<_>>(),
        want
    );
    // A quarter sector (0 … 90°): the interior right of the centre and
    // above it.
    let c = cells(&circ(&plus(r6.clone(), &[on("c17_sector_fill")])));
    let want: BTreeSet<(i64, u8)> = inside
        .iter()
        .filter(|&&(x, r)| x >= 12 && r >= 64)
        .copied()
        .collect();
    assert_eq!(
        c.difference(&outline).copied().collect::<BTreeSet<_>>(),
        want
    );
    // Filled: everything else.
    let c = cells(&circ(&plus(
        r6.clone(),
        &[on("c17_sector_fill"), on("c06_filled_disc")],
    )));
    let gaps: BTreeSet<(i64, u8)> = inside.difference(&want).copied().collect();
    assert_eq!(
        c.difference(&outline).copied().collect::<BTreeSet<_>>(),
        gaps
    );
    // Chords at 0° every 3 rows from the centre's row: rows 61, 64, 67.
    let c = cells(&circ(&plus(r6.clone(), &[on("c18_chord_line_fill")])));
    let want: BTreeSet<(i64, u8)> = inside
        .iter()
        .filter(|&&(_, r)| (i64::from(r) - 64).rem_euclid(3) == 0)
        .copied()
        .collect();
    assert_eq!(
        c.difference(&outline).copied().collect::<BTreeSet<_>>(),
        want
    );
    // A spiral reaches from the centre to the outline.
    let c = cells(&circ(&plus(r6, &[on("c10_spiral_fill")])));
    assert!(c.contains(&(12, 64)) || c.contains(&(11, 64)) || c.contains(&(12, 63)));
    assert!(c.len() > outline.len() + 10);
}

#[test]
fn circ_densities_keep_their_share_of_the_cells() {
    let r6 = vec![
        on("c02_radius"),
        num("c02_radius.radius", 6.0),
        on("c06_filled_disc"),
    ];
    let all = cells(&circ(&r6)).len() as f64;
    // Half everywhere (a flat curve at 0.5).
    let half = (
        "c11_angular_density_graph.curve",
        ParamValue::Curve(plumspirit_engine::transform::Curve::new(vec![
            (0.0, 0.5),
            (1.0, 0.5),
        ])),
    );
    let k = cells(&circ(&plus(
        r6.clone(),
        &[on("c11_angular_density_graph"), half],
    )))
    .len() as f64;
    assert!((k / all - 0.5).abs() < 0.12, "{k} of {all}");
    // From all at the centre to a fifth at the outline: the inner cells
    // keep more than the outer ones.
    let rd = (
        "c12_radial_density_graph.curve",
        ParamValue::Curve(plumspirit_engine::transform::Curve::new(vec![
            (0.0, 1.0),
            (1.0, 0.2),
        ])),
    );
    let c = cells(&circ(&plus(r6, &[on("c12_radial_density_graph"), rd])));
    let near = |x: i64, r: u8, lo: f64, hi: f64| {
        let d = ((x as f64 + 0.5 - 12.0).powi(2) + (f64::from(r) + 0.5 - 64.0).powi(2)).sqrt();
        d >= lo && d < hi
    };
    let share = |lo: f64, hi: f64| {
        let n = ellipse_cells(12.0, 64.0, 6.0, 6.0)
            .iter()
            .filter(|&&(x, r)| near(x, r, lo, hi))
            .count() as f64;
        c.iter().filter(|&&(x, r)| near(x, r, lo, hi)).count() as f64 / n
    };
    assert!(
        share(0.0, 2.5) > share(4.0, 6.5) + 0.2,
        "{} {}",
        share(0.0, 2.5),
        share(4.0, 6.5)
    );
}

#[test]
fn circ_gradients_by_radius_and_angle() {
    let r6 = vec![
        on("c02_radius"),
        num("c02_radius.radius", 6.0),
        on("c06_filled_disc"),
    ];
    // Velocity 127 at the centre to 40 at the outline: the middle row's
    // notes get quieter outwards.
    let v = circ(&plus(r6.clone(), &[on("c13_velocity_by_radius")]));
    let row: Vec<&TNote> = v.iter().filter(|n| n.pitch == 64).collect();
    let mid = row
        .iter()
        .find(|n| n.start <= 12 * 240 && n.end() > 12 * 240)
        .unwrap();
    assert!(mid.vel >= 110, "{}", mid.vel);
    assert!(row[0].vel < 60 && row.last().unwrap().vel < 60);
    // Gate 100 → 25 %: outer notes shorter than their cells.
    let v = circ(&plus(r6.clone(), &[on("c14_gate_by_radius")]));
    assert!(v.iter().any(|n| n.dur % 240 != 0));
    // Subchannel 0 → 3 once round from the angle 0 (later in time): the
    // right side first, then the top, the left, the bottom.
    let v = circ(&[
        on("c02_radius"),
        num("c02_radius.radius", 6.0),
        on("c15_subchannel_by_angle"),
    ]);
    let at = |x: i64, r: u8| {
        v.iter()
            .find(|n| n.pitch == r && n.start <= x as u64 * 240 && n.end() > x as u64 * 240)
            .map(|n| n.sub)
    };
    // Each outline cell: round(3 · its angle from the centre / 360°).
    let want = |x: i64, r: u8| {
        let a = (f64::from(r) + 0.5 - 64.0).atan2(x as f64 + 0.5 - 12.0);
        (3.0 * a.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU).round() as u32
    };
    for n in &v {
        for x in (n.start / 240) as i64..(n.end() / 240) as i64 {
            assert_eq!(n.sub, want(x, n.pitch), "cell ({x}, {})", n.pitch);
        }
    }
    assert_eq!(at(12, 69), Some(1));
    assert_eq!(want(12, 69), 1); // the top: a quarter round
    assert_eq!(want(6, 63), 2); // the left, just below the centre: past half
}

#[test]
fn circ_dashes_halves_and_the_polar_wrap() {
    let r6 = vec![on("c02_radius"), num("c02_radius.radius", 6.0)];
    let full = cells(&circ(&r6));
    let d0 = cells(&circ(&plus(r6.clone(), &[on("c16_dashed_arc_pattern")])));
    let d1 = cells(&circ(&plus(
        r6.clone(),
        &[
            on("c16_dashed_arc_pattern"),
            num("c16_dashed_arc_pattern.phase", 2.0),
        ],
    )));
    assert!(d0.is_subset(&full) && d1.is_subset(&full) && d0 != d1);
    let share = d0.len() as f64 / full.len() as f64;
    assert!((share - 0.5).abs() < 0.15, "{share}");
    // Two-cell band, outer half subchannel 1, inner half 2.
    let v = circ(&plus(
        r6.clone(),
        &[
            on("c05_ring_thickness"),
            on("c23_inner_outer_separate_subchannels"),
        ],
    ));
    assert!(v.iter().any(|n| n.sub == 1) && v.iter().any(|n| n.sub == 2));
    let left = v
        .iter()
        .filter(|n| n.pitch == 64)
        .min_by_key(|n| n.start)
        .unwrap();
    assert_eq!(left.sub, 1, "the outermost cell is the outer half's");
    // Polar wrap 0 … 270°: the quarter below and right of the centre is
    // gone, the rest kept.
    let w = cells(&circ(&plus(r6, &[on("c24_polar_wrap")])));
    assert!(w.is_subset(&full));
    assert!(w.iter().all(|&(x, r)| !(x >= 13 && r <= 62)), "{w:?}");
    let kept: BTreeSet<(i64, u8)> = full
        .iter()
        .filter(|&&(x, r)| !(x >= 12 && r < 64))
        .copied()
        .collect();
    assert!(kept.is_subset(&w));
}

#[test]
fn circ_waves_and_seeded_roughness_shape_the_radius() {
    let r6 = vec![
        on("c02_radius"),
        num("c02_radius.radius", 6.0),
        on("c06_filled_disc"),
    ];
    let plain = cells(&circ(&r6));
    let wave = cells(&circ(&plus(
        r6.clone(),
        &[
            on("c19_wave_modulated_radius"),
            num("c19_wave_modulated_radius.amount", 30.0),
        ],
    )));
    // Six waves of ±30 %: cells beyond the circle (out to 7.8) and gaps
    // inside it (in to 4.2).
    assert!(wave.difference(&plain).count() > 0 && plain.difference(&wave).count() > 0);
    let far = |c: &BTreeSet<(i64, u8)>| {
        c.iter()
            .map(|&(x, r)| (x as f64 + 0.5 - 12.0).hypot(f64::from(r) + 0.5 - 64.0))
            .fold(0.0, f64::max)
    };
    assert!(far(&wave) <= 6.0 * 1.3 + 1.0 && far(&wave) > 6.6);
    let rough = |seed: i64| {
        cells(&circ(&plus(
            r6.clone(),
            &[
                on("c20_seeded_radius_roughness"),
                num("c20_seeded_radius_roughness.amount", 30.0),
                int("gen.seed", seed),
            ],
        )))
    };
    assert_eq!(rough(3), rough(3));
    assert!((1..6).map(rough).collect::<BTreeSet<_>>().len() > 1);
}

#[test]
fn circ_centres_copies_and_packing() {
    // Two centres in a row: each half of the box (8 × 8) holds its circle
    // of radius 4.
    let c = cells(&circ(&[
        on("c06_filled_disc"),
        on("c21_multiple_centers"),
        int("c21_multiple_centers.count", 2),
    ]));
    let mut want = ellipse_cells(8.0, 64.0, 4.0, 4.0);
    want.extend(ellipse_cells(16.0, 64.0, 4.0, 4.0));
    assert_eq!(c, want);
    // Four tangent copies of half size round a circle of radius 4: discs
    // of radius 2 centred 6 away, touching it.
    let r4 = vec![
        on("c02_radius"),
        num("c02_radius.radius", 4.0),
        on("c06_filled_disc"),
    ];
    let c = cells(&circ(&plus(
        r4.clone(),
        &[
            on("c22_tangent_copy_pattern"),
            int("c22_tangent_copy_pattern.count", 4),
        ],
    )));
    let mut want = ellipse_cells(12.0, 64.0, 4.0, 4.0);
    for (dx, dy) in [(6.0, 0.0), (0.0, 6.0), (-6.0, 0.0), (0.0, -6.0)] {
        want.extend(ellipse_cells(12.0 + dx, 64.0 + dy, 2.0, 2.0));
    }
    assert_eq!(c, want);
    // Packing: seeded circles inside the shape, the same for a seed.
    let r6 = vec![on("c02_radius"), num("c02_radius.radius", 6.0)];
    let p = |seed: i64| {
        cells(&circ(&plus(
            r6.clone(),
            &[
                on("c25_recursive_circle_packing"),
                int("c25_recursive_circle_packing.depth", 1),
                int("gen.seed", seed),
            ],
        )))
    };
    assert_eq!(p(2), p(2));
    let outline = cells(&circ(&r6));
    let disc = ellipse_cells(12.0, 64.0, 6.0, 6.0);
    for s in 1..5 {
        let c = p(s);
        assert!(c.is_subset(&disc) && c.len() > outline.len(), "seed {s}");
    }
}

#[test]
fn circ_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], words: &str| match circ_try(kv) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains(words) && m.ends_with('.'), "{m}"),
        other => panic!("{words}: {:?}", other.map(|v| v.len())),
    };
    refused(
        &[
            on("c25_recursive_circle_packing"),
            int("c25_recursive_circle_packing.count", 16),
            int("c25_recursive_circle_packing.depth", 4),
        ],
        "Packing",
    );
    refused(
        &[
            on("c21_multiple_centers"),
            int("c21_multiple_centers.count", 64),
            on("c07_concentric_rings"),
            int("c07_concentric_rings.count", 64),
        ],
        "rings",
    );
    refused(
        &[
            on("c21_multiple_centers"),
            int("c21_multiple_centers.count", 31),
            on("c02_radius"),
            num("c02_radius.radius", 1024.0),
            on("c07_concentric_rings"),
            int("c07_concentric_rings.count", 64),
        ],
        "points",
    );
    refused(
        &[
            on("c21_multiple_centers"),
            int("c21_multiple_centers.count", 64),
            on("c02_radius"),
            num("c02_radius.radius", 1024.0),
            on("c18_chord_line_fill"),
            num("c18_chord_line_fill.angle", 10.0),
            int("c18_chord_line_fill.spacing", 2),
        ],
        "line segments",
    );
}

#[test]
fn circ_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = circ_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run(def, &d, &s, &c);
        let b = run(def, &d, &s, &c);
        assert_eq!(a.notes, b.notes);
        assert!(!a.notes.is_empty());
        assert!(a.notes.iter().all(|n| valid(n, keys)));
    }
}

// ------------------------------------------------------- Arcs / Curves (V09-S55)
//
// What each of Arcs / Curves' 25 functions means (V09-0092, `docs/GENERATORS.md`
// "Arcs / Curves"), on the Rectangle tests' box: ticks 960 … 4,800 (cells 4 … 19,
// u = tick / 240), rows 60 … 67 (row centres 60.5 … 67.5). The exact curves are
// sampled densely; the drawn cells must trace them (every cell within a cell's
// reach of the curve, every point of the curve covered on its row), and exact
// rows are checked where rounding cannot tip.

fn arc_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.arcs")
        .unwrap()
}

fn arc_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = arc_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(67)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn arc(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    arc_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

/// The cells the notes cover (overlaps allowed: separate curves may cross).
fn covered(v: &[TNote]) -> BTreeSet<(i64, u8)> {
    let mut out = BTreeSet::new();
    for n in v {
        for c in n.start / 240..n.end().div_ceil(240) {
            out.insert((c as i64, n.pitch));
        }
    }
    out
}

/// `n + 1` points of curve `f` over t = 0 … 1 (grid units).
fn samples(f: impl Fn(f64) -> (f64, f64)) -> Vec<(f64, f64)> {
    let n = 20_000;
    (0..=n).map(|i| f(f64::from(i) / f64::from(n))).collect()
}

/// The curves' box: u from 4 to just short of 20 (the box's end, less 10⁻⁶
/// of a step: a curve ending upright there stays in its last step), v
/// from 60.5 to 67.5; its centre (12, 64).
const U1: f64 = 20.0 - 1e-6;
const CU: f64 = (4.0 + U1) / 2.0;

/// The base: the quarter of the ellipse centred on (4, 67.5) with radii 16
/// and 7, from its bottom (4, 60.5) to its right (20, 67.5).
fn base_curve(t: f64) -> (f64, f64) {
    let a = -std::f64::consts::FRAC_PI_2 * (1.0 - t);
    (4.0 + (U1 - 4.0) * a.cos(), 67.5 + 7.0 * a.sin())
}

/// Each cell's centre lies within `tol` grid units of one of the points.
fn near_curve(c: &BTreeSet<(i64, u8)>, pts: &[(f64, f64)], tol: f64) {
    for &(x, r) in c {
        let (cu, cv) = (x as f64 + 0.5, f64::from(r) + 0.5);
        let d = pts
            .iter()
            .map(|&(u, v)| (u - cu).hypot(v - cv))
            .fold(f64::INFINITY, f64::min);
        assert!(d <= tol, "cell ({x}, {r}) is {d:.2} from the curve");
    }
}

/// The cells trace the points: each cell's centre within `tol` grid units
/// of one, and each point (inside rows 0 … 127, not within 0.06 of a row's
/// edge — a flattened curve's chords may touch it from either side) on a
/// cell of its row within a cell of it.
fn traces(c: &BTreeSet<(i64, u8)>, pts: &[(f64, f64)], tol: f64) {
    near_curve(c, pts, tol);
    for &(u, v) in pts {
        if !(0.0..128.0).contains(&v) || (v - v.round()).abs() < 0.06 {
            continue;
        }
        let r = v.floor() as u8;
        assert!(
            c.iter()
                .any(|&(x, rr)| rr == r && (x as f64 + 0.5 - u).abs() <= 1.0),
            "({u:.2}, {v:.2}) not drawn"
        );
    }
}

/// Each row's first and last cell of a curve crossing every row once —
/// the cells whose centres its span covers, or the cell containing its
/// middle — where its entry / exit is not within 0.06 of a half cell.
fn rows_exact(c: &BTreeSet<(i64, u8)>, pts: &[(f64, f64)]) {
    let mut span: std::collections::BTreeMap<u8, (f64, f64)> = Default::default();
    for &(u, v) in pts {
        let e = span
            .entry(v.floor() as u8)
            .or_insert((f64::INFINITY, f64::NEG_INFINITY));
        *e = (e.0.min(u), e.1.max(u));
    }
    let rows = rows_of(c);
    for (r, (a, b)) in span {
        let tie = |x: f64| ((x - x.floor()) - 0.5).abs() < 0.06;
        let got = &rows[&r];
        if tie(a) || tie(b) {
            continue;
        }
        let (s, e) = if b.round() > a.round() {
            (a.round() as i64, b.round() as i64)
        } else {
            let m = ((a + b) / 2.0).floor() as i64;
            (m, m + 1)
        };
        assert_eq!(got[0], s, "row {r} starts at {a:.3}");
        assert_eq!(*got.last().unwrap() + 1, e, "row {r} ends at {b:.3}");
    }
}

#[test]
fn arc_the_base_is_a_quarter_ellipse_rising_across_the_box() {
    let v = arc(&[]);
    let c = cells(&v);
    let pts = samples(base_curve);
    traces(&c, &pts, 1.0);
    rows_exact(&c, &pts);
    // One note per row (one brush, merged along each row).
    assert_eq!(v.len(), 8);
    assert!(c.contains(&(4, 60)) && c.contains(&(19, 67)));
    // Row 60: from the start (u = 4) to where the curve leaves the row
    // (v = 61: u = 4 + 16 · √(1 − (6.5 / 7)²) = 9.94); the upright end in
    // the box's last step.
    assert_eq!(rows_of(&c)[&60], (4..10).collect::<Vec<_>>());
    assert_eq!(rows_of(&c)[&67], vec![19]);
}

#[test]
fn arc_circular_and_elliptical_arcs_and_the_beziers_trace_their_curves() {
    use std::f64::consts::PI;
    // A circle's arc of radius 3 about the box's centre (12, 64), from 180°
    // clockwise over the top to 0°.
    let c = covered(&arc(&[
        on("c01_circular_arc"),
        num("c01_circular_arc.radius", 3.0),
    ]));
    traces(
        &c,
        &samples(|t| {
            let a = PI - PI * t;
            (CU + 3.0 * a.cos(), 64.0 + 3.0 * a.sin())
        }),
        1.0,
    );
    assert!(c.iter().all(|&(_, r)| r >= 63), "the top half only");
    // An ellipse 6 across and 3 up turned 90°: 3 across and 6 up; the
    // half from 180° clockwise.
    let c = covered(&arc(&[
        on("c02_elliptical_arc"),
        num("c02_elliptical_arc.x", 6.0),
        num("c02_elliptical_arc.y", 3.0),
        num("c02_elliptical_arc.rotation", 90.0),
    ]));
    traces(
        &c,
        &samples(|t| {
            let a = PI - PI * t;
            (CU - 3.0 * a.sin(), 64.0 + 6.0 * a.cos())
        }),
        1.0,
    );
    // Quadratic: control at the box's top-left corner (4, 67.5).
    let quad = |t: f64| {
        let u = 1.0 - t;
        (
            u * u * 4.0 + 2.0 * u * t * 4.0 + t * t * U1,
            u * u * 60.5 + 2.0 * u * t * 67.5 + t * t * 67.5,
        )
    };
    let c = cells(&arc(&[on("c03_quadratic_bezier")]));
    traces(&c, &samples(quad), 1.0);
    rows_exact(&c, &samples(quad));
    // Cubic: controls (12, 60.5) and (12, 67.5) — an S from corner to
    // corner.
    let cubic = |t: f64| {
        let u = 1.0 - t;
        let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        (
            w0 * 4.0 + (w1 + w2) * CU + w3 * U1,
            w0 * 60.5 + w1 * 60.5 + (w2 + w3) * 67.5,
        )
    };
    let c = cells(&arc(&[on("c04_cubic_bezier")]));
    traces(&c, &samples(cubic), 1.0);
    rows_exact(&c, &samples(cubic));
    // The last switched on decides: the cubic over the quadratic.
    assert_eq!(
        cells(&arc(&[on("c03_quadratic_bezier"), on("c04_cubic_bezier")])),
        c
    );
}

#[test]
fn arc_splines_pass_their_points_and_tension_straightens_or_bends() {
    use plumspirit_engine::transform::Curve;
    let pts = vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
    let at = |(x, y): (f64, f64)| (4.0 + (U1 - 4.0) * x, 60.5 + 7.0 * y);
    let near = |c: &BTreeSet<(i64, u8)>, (u, v): (f64, f64)| {
        c.iter().any(|&(x, r)| {
            (x as f64 + 0.5 - u).abs() <= 1.0 && (f64::from(r) + 0.5 - v).abs() <= 0.5
        })
    };
    // Catmull-Rom passes every point (the peak at (12, 67.5)).
    let cr = covered(&arc(&[
        on("c06_catmull_rom_smooth_path"),
        (
            "c06_catmull_rom_smooth_path.points",
            ParamValue::Curve(Curve::new(pts.clone())),
        ),
    ]));
    for &p in &pts {
        assert!(near(&cr, at(p)), "{p:?}");
    }
    // The B-spline passes its ends but only nears the peak: below it.
    let bs = covered(&arc(&[
        on("c05_multi_point_spline"),
        (
            "c05_multi_point_spline.points",
            ParamValue::Curve(Curve::new(pts.clone())),
        ),
    ]));
    assert!(near(&bs, at(pts[0])) && near(&bs, at(pts[2])));
    let top = |c: &BTreeSet<(i64, u8)>| c.iter().map(|x| x.1).max().unwrap();
    assert!(top(&bs) < top(&cr), "{} {}", top(&bs), top(&cr));
    // Tension 100 %: the straight line between the base's ends, (4, 60.5)
    // to (20, 67.5).
    let line = |t: f64| (4.0 + (U1 - 4.0) * t, 60.5 + 7.0 * t);
    let c = cells(&arc(&[
        on("c07_curvature_tension"),
        num("c07_curvature_tension.amount", 100.0),
    ]));
    traces(&c, &samples(line), 1.0);
    rows_exact(&c, &samples(line));
    // −100 %: the bend doubled — p + (p − chord) at each parameter.
    let doubled = |t: f64| {
        let (p, q) = (base_curve(t), line(t));
        (2.0 * p.0 - q.0, 2.0 * p.1 - q.1)
    };
    let c = covered(&arc(&[
        on("c07_curvature_tension"),
        num("c07_curvature_tension.amount", -100.0),
    ]));
    traces(&c, &samples(doubled), 1.0);
}

#[test]
fn arc_thickness_parallels_and_ribbons_widen_the_curve() {
    let base = cells(&arc(&[]));
    // Thickness 3: every row of the base's cells and the rows ±1.5 around
    // the curve (the flat start: rows 59 … 61 at column 5).
    let t = cells(&arc(&[on("c08_stroke_thickness")]));
    assert!(base.is_subset(&t), "{:?}", base.difference(&t));
    for r in 59u8..=61 {
        assert!(t.contains(&(5, r)), "row {r}");
    }
    // Three parallels 2 apart on the left (above / inside the bend): the
    // offset curves of the ellipse, exactly along its normals.
    let c = covered(&arc(&[on("c09_parallel_curves")]));
    let offset = |d: f64| {
        move |t: f64| {
            let a = -std::f64::consts::FRAC_PI_2 * (1.0 - t);
            let (x, y) = ((U1 - 4.0) * a.cos(), 7.0 * a.sin());
            // The travel's direction; its left normal is (−ty, tx).
            let (tx, ty) = (-(U1 - 4.0) * a.sin(), 7.0 * a.cos());
            let l = tx.hypot(ty);
            (4.0 + x - ty / l * d, 67.5 + y + tx / l * d)
        }
    };
    let mut pts = samples(offset(0.0));
    pts.extend(samples(offset(2.0)));
    pts.extend(samples(offset(4.0)));
    traces(&c, &pts, 1.0);
    // A ribbon 2 wide on the left: between the curve and its offset.
    let rb = covered(&arc(&[
        on("c25_ribbon_fill_from_curve"),
        num("c25_ribbon_fill_from_curve.width", 2.0),
    ]));
    let band: Vec<(f64, f64)> = (0..=8)
        .flat_map(|k| samples(offset(f64::from(k) / 4.0)))
        .collect();
    for &(x, r) in &rb {
        let (cu, cv) = (x as f64 + 0.5, f64::from(r) + 0.5);
        let d = band
            .iter()
            .map(|&(u, v)| (u - cu).hypot(v - cv))
            .fold(f64::INFINITY, f64::min);
        assert!(d <= 0.75, "({x}, {r}) {d:.2}");
    }
    assert!(rb.len() > base.len() + 10);
    // To the box's bottom: each column filled from row 60 up to the curve.
    let fill = cells(&arc(&[
        on("c25_ribbon_fill_from_curve"),
        ch("c25_ribbon_fill_from_curve.to", 1),
    ]));
    for x in 4i64..20 {
        let top = base.iter().filter(|c| c.0 == x).map(|c| c.1).max().unwrap();
        for r in 60..=top {
            assert!(fill.contains(&(x, r)), "({x}, {r})");
        }
    }
}

#[test]
fn arc_sampling_density_and_dashes_thin_the_curve() {
    use plumspirit_engine::transform::Curve;
    let base = covered(&arc(&[]));
    // Density 1 everywhere: a one-step note every grid unit along the
    // curve (its length: the quarter ellipse's, ≈ 18.6 units).
    let all = arc(&[
        on("c11_density_along_curve"),
        (
            "c11_density_along_curve.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 1.0), (1.0, 1.0)])),
        ),
    ]);
    assert!(all.iter().all(|n| n.dur == 240));
    let len: f64 = samples(base_curve)
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum();
    assert_eq!(all.len(), len.floor() as usize + 1, "{len}");
    // A half everywhere keeps every other one.
    let half = arc(&[
        on("c11_density_along_curve"),
        (
            "c11_density_along_curve.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 0.5), (1.0, 0.5)])),
        ),
    ]);
    assert_eq!(half.len(), all.len() / 2);
    // Adaptive: the next note one grid unit on where the curve bends at
    // radius ≤ 4 (its upright end: radius 7² / 16 = 3.06), up to 6 on where
    // it is flat (its start: radius 16² / 7 = 36.6) — fewer notes than
    // every unit, the gaps closing towards the end; each note centred on
    // its point, inside the box.
    let ad = arc(&[on("c10_adaptive_note_sampling")]);
    assert!(ad.len() < all.len() && ad.len() >= 4, "{ad:?}");
    let mut pts: Vec<(f64, f64)> = ad
        .iter()
        .map(|n| ((n.start / 240) as f64 + 0.5, f64::from(n.pitch) + 0.5))
        .collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let gap = |i: usize| (pts[i + 1].0 - pts[i].0).hypot(pts[i + 1].1 - pts[i].1);
    assert!(gap(pts.len() - 2) < gap(0), "{pts:?}");
    assert!(ad.iter().all(|n| (960..4800).contains(&n.start)));
    // Dashes 3 on / 2 off along the arc: a part of the base's cells, about
    // three fifths of them, moved by the phase.
    let d0 = covered(&arc(&[on("c15_dash_pattern")]));
    let d1 = covered(&arc(&[
        on("c15_dash_pattern"),
        num("c15_dash_pattern.phase", 2.0),
    ]));
    near_curve(&d0, &samples(base_curve), 1.0);
    near_curve(&d1, &samples(base_curve), 1.0);
    assert!(d0 != d1);
    let share = d0.len() as f64 / base.len() as f64;
    assert!((0.4..0.85).contains(&share), "{share}");
}

#[test]
fn arc_gradients_and_curvature_modulation_follow_the_curve() {
    use plumspirit_engine::transform::Curve;
    // Velocity 127 → 40 from the start (row 60, left) to the end (row 67).
    let v = arc(&[on("c12_velocity_along_curve")]);
    let first = v.iter().min_by_key(|n| (n.pitch, n.start)).unwrap();
    let last = v.iter().max_by_key(|n| (n.pitch, n.start)).unwrap();
    assert!(
        first.vel >= 120 && last.vel <= 50,
        "{} {}",
        first.vel,
        last.vel
    );
    // No two notes share a cell (the pen trims a piece next to another
    // brush) and the velocity never rises along the curve.
    let _ = cells(&v);
    let mut along: Vec<&TNote> = v.iter().collect();
    along.sort_by_key(|n| (n.pitch, n.start));
    assert!(along.windows(2).all(|w| w[1].vel <= w[0].vel));
    // Shaped by a curve: the first half stays at 127.
    let v = arc(&[
        on("c12_velocity_along_curve"),
        (
            "c12_velocity_along_curve.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 0.0), (0.5, 0.0), (1.0, 1.0)])),
        ),
    ]);
    assert!(v.iter().filter(|n| n.pitch == 60).all(|n| n.vel == 127));
    // Gate 100 → 25 %: the end's notes shorter than their cells.
    let v = arc(&[on("c13_gate_along_curve")]);
    let top = v.iter().filter(|n| n.pitch == 67).collect::<Vec<_>>();
    assert!(top.iter().all(|n| n.dur % 240 != 0));
    // Subchannel 0 → 3: 0 at the start, 3 at the end.
    let v = arc(&[on("c14_subchannel_along_curve")]);
    assert_eq!(v.iter().min_by_key(|n| (n.pitch, n.start)).unwrap().sub, 0);
    assert_eq!(v.iter().max_by_key(|n| (n.pitch, n.start)).unwrap().sub, 3);
    // Curvature: the flat start (radius 36.6: 4 / 36.6 of the way from the
    // floor of 30 % to full) quiet, the tight end (radius 3.06 ≤ 4) at full
    // velocity.
    let v = arc(&[on("c20_curvature_driven_modulation")]);
    let base_vel = f64::from(arc(&[])[0].vel);
    let first = v.iter().min_by_key(|n| (n.pitch, n.start)).unwrap();
    let last = v.iter().max_by_key(|n| (n.pitch, n.start)).unwrap();
    let want = (base_vel * (0.3 + 0.7 * 4.0 * 7.0 / (16.0 * 16.0))).round();
    assert!(
        (f64::from(first.vel) - want).abs() <= 2.0,
        "{} {want}",
        first.vel
    );
    assert_eq!(f64::from(last.vel), base_vel);
}

#[test]
fn arc_reverse_mirror_and_copies() {
    let base = cells(&arc(&[]));
    // Reverse: played backwards — mirrored in time about the box's middle
    // (u ↦ 24 − u), still inside the box.
    let rev = cells(&arc(&[on("c16_reverse_direction")]));
    let back = samples(|t| {
        let (u, v) = base_curve(t);
        (4.0 + U1 - u, v)
    });
    traces(&rev, &back, 1.0);
    rows_exact(&rev, &back);
    assert!(rev.iter().all(|c| (4..20).contains(&c.0)));
    // Its start is now at the box's end: a velocity gradient starts there.
    let v = arc(&[on("c16_reverse_direction"), on("c12_velocity_along_curve")]);
    let low = v
        .iter()
        .filter(|n| n.pitch == 60)
        .max_by_key(|n| n.start)
        .unwrap();
    assert!(low.vel >= 120, "{}", low.vel);
    // Mirror across the middle pitch, keeping the curve: row r ↦ 127 − r.
    let m = covered(&arc(&[on("c17_mirror")]));
    let mut want = base.clone();
    want.extend(base.iter().map(|&(x, r)| (x, 127 - r)));
    assert_eq!(m, want);
    // Only the mirror.
    let m = covered(&arc(&[on("c17_mirror"), ch("c17_mirror.mode", 1)]));
    assert_eq!(m, base.iter().map(|&(x, r)| (x, 127 - r)).collect());
    // Normal copies: 3 copies 4 apart, left of the line between the ends
    // (4, 60.5) → (20, 67.5).
    let c = covered(&arc(&[on("c19_normal_offset_duplication")]));
    let (dx, dy) = (U1 - 4.0, 7.0f64);
    let l = dx.hypot(dy);
    let mut pts = Vec::new();
    for k in 0..3 {
        let o = 4.0 * f64::from(k);
        pts.extend(samples(|t| {
            let (u, v) = base_curve(t);
            (u - dy / l * o, v + dx / l * o)
        }));
    }
    traces(&c, &pts, 1.0);
    // A chain of four quarter circles (radius 3 about (12, 64), from 180°
    // clockwise 90°), each continuing the last one's direction: the whole
    // circle.
    let c = covered(&arc(&[
        on("c01_circular_arc"),
        num("c01_circular_arc.radius", 3.0),
        num("c01_circular_arc.sweep", -90.0),
        on("c18_tangent_aligned_duplication"),
    ]));
    traces(
        &c,
        &samples(|t| {
            let a = std::f64::consts::TAU * t;
            (CU + 3.0 * a.cos(), 64.0 + 3.0 * a.sin())
        }),
        1.0,
    );
}

#[test]
fn arc_loops_cusps_jitter_morphs_and_branches() {
    let base = covered(&arc(&[]));
    let dist = |c: &BTreeSet<(i64, u8)>| {
        let pts = samples(base_curve);
        c.iter()
            .map(|&(x, r)| {
                pts.iter()
                    .map(|&(u, v)| (u - x as f64 - 0.5).hypot(v - f64::from(r) - 0.5))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max)
    };
    // Eight turns along ≈ 18.6 units: a rolling circle of radius 0.37 on
    // the left — cusps reach 2 · 0.37 off the curve, loops 0.37 + 0.59.
    let cusps = covered(&arc(&[
        on("c22_loop_cusp_mode"),
        ch("c22_loop_cusp_mode.mode", 1),
    ]));
    let loops = covered(&arc(&[on("c22_loop_cusp_mode")]));
    assert!(dist(&cusps) <= 0.74 + 0.75 && dist(&loops) <= 0.96 + 0.75);
    assert!(cusps != base && loops != base && loops != cusps);
    // Jitter: seeded, its ends kept, within its amount (1.5) of the curve.
    let jit = |seed: i64| {
        covered(&arc(&[
            on("c21_seeded_normal_jitter"),
            int("gen.seed", seed),
        ]))
    };
    assert_eq!(jit(4), jit(4));
    assert!((1..6).map(jit).collect::<BTreeSet<_>>().len() > 1);
    for s in 1..4 {
        let j = jit(s);
        // Its ends kept: the start's cell, the end's row near the end.
        assert!(j.contains(&(4, 60)), "seed {s}");
        assert!(j.iter().any(|&(x, r)| r == 67 && x >= 17), "seed {s}");
        assert!(dist(&j) <= 1.5 + 1.0, "seed {s}");
    }
    // Morph to the mirror in five curves: the middle one is the box's
    // middle row coordinate 64.0 — the edge between rows 63 and 64 —
    // across the box.
    let m = covered(&arc(&[on("c23_curve_a_b_morph")]));
    assert!(
        (4..20).all(|x| m.contains(&(x, 64)) || m.contains(&(x, 63))),
        "{m:?}"
    );
    assert!(base.is_subset(&m));
    // Branches: seeded, the trunk kept, smaller curves growing from it.
    let br = |seed: i64| {
        covered(&arc(&[
            on("c24_recursive_branch_curves"),
            int("gen.seed", seed),
        ]))
    };
    assert_eq!(br(2), br(2));
    assert!(base.is_subset(&br(2)) && br(2).len() > base.len() + 5);
    assert!((1..6).map(br).collect::<BTreeSet<_>>().len() > 1);
}

#[test]
fn arc_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], words: &str| match arc_try(kv) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains(words) && m.ends_with('.'), "{m}"),
        other => panic!("{words}: {:?}", other.map(|v| v.len())),
    };
    refused(
        &[
            on("c24_recursive_branch_curves"),
            int("c24_recursive_branch_curves.branches", 8),
            int("c24_recursive_branch_curves.depth", 4),
        ],
        "copies",
    );
    refused(
        &[
            on("c22_loop_cusp_mode"),
            int("c22_loop_cusp_mode.turns", 256),
            on("c09_parallel_curves"),
            int("c09_parallel_curves.count", 32),
            on("c23_curve_a_b_morph"),
            int("c23_curve_a_b_morph.count", 32),
        ],
        "points",
    );
}

#[test]
fn arc_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = arc_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.iter().all(|n| valid(n, keys)));
    }
}

// ------------------------------------------------------------ Polygons (V09-S56)
//
// What each of Polygons' 25 functions means (V09-0093, `docs/GENERATORS.md`
// "Polygons"), on the Rectangle tests' box: cells 4 … 19, rows 60 … 67, grid
// space u = tick / 240, v = row; the box's centre (12, 64), its ellipse's radii
// 8 and 4. The oracles fill polygons by the row-centre rule (even–odd crossing
// pairs, each rounded to whole cells), independently of the region code.

type Pt2 = (f64, f64);

fn poly_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.polygons")
        .unwrap()
}

fn poly_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = poly_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(60)),
            ("gen.high", ParamValue::Int(67)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn poly(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    poly_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

fn lst(k: &'static str, v: &[i64]) -> (&'static str, ParamValue) {
    (k, ParamValue::List(v.to_vec()))
}

/// The regular polygon of `n` vertices on the ellipse (rx, ry) about `c`,
/// the first at angle `a0` (radians).
fn ngon(c: Pt2, rx: f64, ry: f64, n: usize, a0: f64) -> Vec<Pt2> {
    (0..n)
        .map(|i| {
            let a = a0 + std::f64::consts::TAU * i as f64 / n as f64;
            (c.0 + rx * a.cos(), c.1 + ry * a.sin())
        })
        .collect()
}

/// The base: the hexagon on the box's ellipse, a vertex at the top.
fn hexagon() -> Vec<Pt2> {
    ngon((12.0, 64.0), 8.0, 4.0, 6, std::f64::consts::FRAC_PI_2)
}

/// Even–odd crossing pairs of the row centre `yc` with the polygon's edges
/// (an edge crosses when one end is at or below `yc` and the other above).
fn crossing_pairs(pts: &[Pt2], yc: f64) -> Vec<(f64, f64)> {
    let n = pts.len();
    let mut xs: Vec<f64> = (0..n)
        .filter_map(|k| {
            let (a, b) = (pts[k], pts[(k + 1) % n]);
            ((a.1 <= yc && yc < b.1) || (b.1 <= yc && yc < a.1))
                .then(|| a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1))
        })
        .collect();
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2).map(|p| (p[0], p[1])).collect()
}

/// The polygon's cells: each pair `[a, b]` as cells `round(a) … round(b)`,
/// at least one when `min1` (a drawn piece), none when empty otherwise (a
/// hollow interior).
fn fill_cells(pts: &[Pt2], min1: bool) -> BTreeSet<(i64, u8)> {
    let mut s = BTreeSet::new();
    for r in 0u8..=127 {
        for (a, b) in crossing_pairs(pts, f64::from(r) + 0.5) {
            let (x0, mut x1) = (a.round() as i64, b.round() as i64);
            if min1 {
                x1 = x1.max(x0 + 1);
            }
            for c in x0..x1 {
                s.insert((c, r));
            }
        }
    }
    s
}

fn fill(pts: &[Pt2]) -> BTreeSet<(i64, u8)> {
    fill_cells(pts, true)
}

/// A convex counter-clockwise polygon moved `d` out (in when negative):
/// each vertex where its two edges' moved lines meet (a mitre).
fn moved(pts: &[Pt2], d: f64) -> Vec<Pt2> {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (p, q, r) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let nrm = |a: Pt2, b: Pt2| {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l = dx.hypot(dy);
                (dy / l, -dx / l)
            };
            let (n1, n2) = (nrm(p, q), nrm(q, r));
            let k = d / (1.0 + n1.0 * n2.0 + n1.1 * n2.1);
            (q.0 + (n1.0 + n2.0) * k, q.1 + (n1.1 + n2.1) * k)
        })
        .collect()
}

/// The outline `t` units wide inside convex polygon `pts`.
fn outline_of(pts: &[Pt2], t: f64) -> BTreeSet<(i64, u8)> {
    let hollow = fill_cells(&moved(pts, -t), false);
    fill(pts).difference(&hollow).copied().collect()
}

fn dist_seg(p: Pt2, a: Pt2, b: Pt2) -> f64 {
    let d = (b.0 - a.0, b.1 - a.1);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0);
    (p.0 - a.0 - t * d.0).hypot(p.1 - a.1 - t * d.1)
}

/// The edge of `pts` nearest to `p` (the first of equals).
fn nearest_edge(pts: &[Pt2], p: Pt2) -> usize {
    let n = pts.len();
    (0..n)
        .min_by(|&i, &j| {
            dist_seg(p, pts[i], pts[(i + 1) % n]).total_cmp(&dist_seg(p, pts[j], pts[(j + 1) % n]))
        })
        .unwrap()
}

#[test]
fn poly_the_base_is_the_outline_of_the_hexagon_inscribed_in_the_box_ellipse() {
    let h = hexagon();
    assert_eq!(cells(&poly(&[on("c08_filled_polygon")])), fill(&h));
    let o = cells(&poly(&[]));
    assert_eq!(o, outline_of(&h, 1.0));
    // The top and bottom vertices' rows, each row's ends; a hollow middle.
    assert!(o.contains(&(12, 67)) && o.contains(&(12, 60)));
    assert!(o.contains(&(5, 64)) && o.contains(&(18, 64)));
    assert!(!o.contains(&(12, 64)) && !o.contains(&(8, 63)));
}

#[test]
fn poly_side_count_regular_size_radius_and_rotation_place_the_vertices() {
    use std::f64::consts::FRAC_PI_2;
    let filled = |kv: &[(&str, ParamValue)]| {
        let mut v = kv.to_vec();
        v.push(on("c08_filled_polygon"));
        cells(&poly(&v))
    };
    // A pentagon on the ellipse.
    let five = filled(&[on("c01_side_count")]);
    assert_eq!(five, fill(&ngon((12.0, 64.0), 8.0, 4.0, 5, FRAC_PI_2)));
    // Regular: the circle of the smaller side's half (4); as large as fits
    // the box — a pentagon's height is 1 + cos 36° of its radius, so 8 /
    // 1.809 = 4.42, centred (0.42 lower).
    assert_eq!(
        filled(&[on("c02_regular_polygon")]),
        fill(&ngon((12.0, 64.0), 4.0, 4.0, 6, FRAC_PI_2))
    );
    let s = 8.0 / (1.0 + 36f64.to_radians().cos());
    let dv = -(1.0 - 36f64.to_radians().cos()) / 2.0 * s;
    assert_eq!(
        filled(&[
            on("c01_side_count"),
            on("c02_regular_polygon"),
            ch("c02_regular_polygon.fit", 1)
        ]),
        fill(&ngon((12.0, 64.0 + dv), s, s, 5, FRAC_PI_2))
    );
    // Radius 3 (grid units), and turned 30° about the centre.
    assert_eq!(
        filled(&[on("c04_radius"), num("c04_radius.radius", 3.0)]),
        fill(&ngon((12.0, 64.0), 3.0, 3.0, 6, FRAC_PI_2))
    );
    let turned: Vec<Pt2> = hexagon()
        .iter()
        .map(|&(u, v)| {
            let (s, c) = 30f64.to_radians().sin_cos();
            let (x, y) = (u - 12.0, v - 64.0);
            (12.0 + x * c - y * s, 64.0 + x * s + y * c)
        })
        .collect();
    assert_eq!(
        filled(&[on("c05_rotation"), num("c05_rotation.angle", 30.0)]),
        fill(&turned)
    );
}

#[test]
fn poly_an_irregular_polygon_takes_its_vertices_in_per_mille_of_the_box() {
    // The default list (‰): (500, 1000), (1000, 620), (780, 0), (180, 80),
    // (0, 700) — clockwise, drawn counter-clockwise.
    let pts: Vec<Pt2> = [(500, 1000), (1000, 620), (780, 0), (180, 80), (0, 700)]
        .iter()
        .rev()
        .map(|&(x, y)| {
            (
                4.0 + 16.0 * f64::from(x) / 1000.0,
                60.0 + 8.0 * f64::from(y) / 1000.0,
            )
        })
        .collect();
    assert_eq!(
        cells(&poly(&[
            on("c03_irregular_polygon"),
            on("c08_filled_polygon")
        ])),
        fill(&pts)
    );
    // Fewer than three vertices: the regular polygon.
    assert_eq!(
        poly(&[
            on("c03_irregular_polygon"),
            lst("c03_irregular_polygon.points", &[10, 10, 90])
        ]),
        poly(&[])
    );
}

#[test]
fn poly_a_star_puts_a_vertex_between_every_two_at_the_inner_radius() {
    // Regular (radius 4), inner radius 50 %: a six-pointed star.
    let mut star = Vec::new();
    for i in 0..12 {
        let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * f64::from(i) / 6.0;
        let r = if i % 2 == 0 { 4.0 } else { 2.0 };
        star.push((12.0 + r * a.cos(), 64.0 + r * a.sin()));
    }
    let base = [on("c02_regular_polygon"), on("c06_star_inner_radius_mode")];
    let mut f = base.to_vec();
    f.push(on("c08_filled_polygon"));
    assert_eq!(cells(&poly(&f)), fill(&star));
    let o = cells(&poly(&base));
    assert!(o.is_subset(&fill(&star)) && !o.contains(&(12, 64)));
    // The tips' cells are drawn, the notches between them are not.
    assert!(o.contains(&(12, 67)) && o.contains(&(12, 60)));
}

#[test]
fn poly_a_star_in_the_stretched_box_puts_its_inner_vertices_on_the_inner_ellipse() {
    // The base's ellipse (radii 8, 4): tips at 90° + 60° k on it, inner
    // vertices half way round between them on the ellipse at 50 %.
    let mut star = Vec::new();
    for i in 0..12 {
        let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * f64::from(i) / 6.0;
        let k = if i % 2 == 0 { 1.0 } else { 0.5 };
        star.push((12.0 + 8.0 * k * a.cos(), 64.0 + 4.0 * k * a.sin()));
    }
    let f = cells(&poly(&[
        on("c06_star_inner_radius_mode"),
        on("c08_filled_polygon"),
    ]));
    assert_eq!(f, fill(&star));
    // A very wide box: every cell stays in the box's rows and steps.
    let wide = poly(&[
        on("c06_star_inner_radius_mode"),
        ("gen.length", ParamValue::Int(240 * 4000)),
    ]);
    assert!(
        wide.iter()
            .all(|n| (60..=67).contains(&n.pitch) && n.start >= 960 && n.end() <= 960 + 240 * 4000)
    );
}

#[test]
fn poly_outline_placement_thickness_and_filling() {
    let h = hexagon();
    let band = |out: f64, inn: f64| -> BTreeSet<(i64, u8)> {
        let hollow = fill_cells(&moved(&h, inn), false);
        fill(&moved(&h, out)).difference(&hollow).copied().collect()
    };
    let o = |p: u32| cells(&poly(&[on("c07_outline"), ch("c07_outline.placement", p)]));
    assert_eq!(o(0), band(1.0, 0.0));
    assert_eq!(o(1), band(0.5, -0.5));
    assert_eq!(o(2), band(0.0, -1.0));
    assert_eq!(cells(&poly(&[on("c09_border_thickness")])), band(0.0, -2.0));
}

#[test]
fn poly_spokes_triangles_and_scanlines_fill_the_interior() {
    let h = hexagon();
    let outline = outline_of(&h, 1.0);
    // Spokes to every vertex: the centre column (to the top and bottom
    // vertices); to every second vertex no bottom spoke.
    let s1 = cells(&poly(&[on("c10_radial_spokes")]));
    let s2 = cells(&poly(&[
        on("c10_radial_spokes"),
        int("c10_radial_spokes.every", 2),
    ]));
    for r in 61u8..67 {
        assert!(s1.contains(&(12, r)), "row {r}");
    }
    assert!(!s2.contains(&(12, 61)) && !s2.contains(&(12, 62)) && s2.contains(&(12, 65)));
    assert!(s1.is_superset(&outline) && s1.is_subset(&fill(&h)));
    // A fan from the top vertex: its middle diagonal is the centre column;
    // a subdivision adds lines.
    let t0 = cells(&poly(&[
        on("c11_triangulated_fill"),
        int("c11_triangulated_fill.levels", 0),
    ]));
    let t1 = cells(&poly(&[on("c11_triangulated_fill")]));
    for r in 61u8..67 {
        assert!(t0.contains(&(12, r)), "row {r}");
    }
    assert!(t1.is_superset(&t0) && t1.len() > t0.len() && t1.is_subset(&fill(&h)));
    // From the centre: its spokes are among the triangles' edges.
    let tc = cells(&poly(&[
        on("c11_triangulated_fill"),
        ch("c11_triangulated_fill.from", 1),
        int("c11_triangulated_fill.levels", 0),
    ]));
    assert!(tc.is_superset(&s1));
    // Scanlines every 2 rows from the lowest (60), and with phase 1.
    let full = fill(&h);
    let scan = |phase: i64| {
        cells(&poly(&[
            on("c12_scanline_fill"),
            int("c12_scanline_fill.phase", phase),
        ]))
    };
    for (phase, rows) in [(0, [62u8, 64, 66]), (1, [61, 63, 65])] {
        let want: BTreeSet<(i64, u8)> = outline
            .iter()
            .copied()
            .chain(full.iter().copied().filter(|c| rows.contains(&c.1)))
            .collect();
        assert_eq!(scan(phase), want, "phase {phase}");
    }
}

#[test]
fn poly_concentric_polygons_nest_outlines_inside() {
    let h = hexagon();
    let ring = |d: f64| -> BTreeSet<(i64, u8)> {
        let hollow = fill_cells(&moved(&h, -d - 1.0), false);
        fill(&moved(&h, -d)).difference(&hollow).copied().collect()
    };
    let one = cells(&poly(&[
        on("c13_concentric_polygons"),
        int("c13_concentric_polygons.count", 1),
        int("c13_concentric_polygons.spacing", 2),
    ]));
    let want: BTreeSet<(i64, u8)> = ring(0.0).union(&ring(2.0)).copied().collect();
    assert_eq!(one, want);
    // Even spacing: the inradius (3.84, the slanted edges') over count + 1,
    // at least 1 — three outlines at 1, 2, 3 units in.
    let even = cells(&poly(&[on("c13_concentric_polygons")]));
    let want: BTreeSet<(i64, u8)> = (0..4).flat_map(|k| ring(f64::from(k))).collect();
    assert_eq!(even, want);
}

#[test]
fn poly_per_vertex_radius_and_angle_follow_their_curves() {
    use plumspirit_engine::transform::Curve;
    let tau = std::f64::consts::TAU;
    let r = Curve::new(vec![(0.0, 1.0), (0.5, 0.45), (1.0, 1.0)]);
    let scaled: Vec<Pt2> = hexagon()
        .iter()
        .enumerate()
        .map(|(i, &(u, v))| {
            let k = r.eval(i as f64 / 6.0);
            (12.0 + (u - 12.0) * k, 64.0 + (v - 64.0) * k)
        })
        .collect();
    assert_eq!(
        cells(&poly(&[
            on("c14_per_vertex_radius"),
            on("c08_filled_polygon")
        ])),
        fill(&scaled)
    );
    // Each vertex moves along the ellipse: its angle on the ellipse's circle
    // shifted by (curve(i / 6) − ½) · 2 · 30 % of 60°.
    let a = Curve::new(vec![(0.0, 0.5), (0.25, 1.0), (0.75, 0.0), (1.0, 0.5)]);
    let turned: Vec<Pt2> = (0..6)
        .map(|i| {
            let t = (a.eval(f64::from(i) / 6.0) - 0.5) * 2.0 * 0.3 * tau / 6.0;
            let ang = std::f64::consts::FRAC_PI_2 + tau * f64::from(i) / 6.0 + t;
            (12.0 + 8.0 * ang.cos(), 64.0 + 4.0 * ang.sin())
        })
        .collect();
    assert_eq!(
        cells(&poly(&[
            on("c15_per_vertex_angle"),
            on("c08_filled_polygon")
        ])),
        fill(&turned)
    );
}

#[test]
fn poly_chamfered_and_rounded_corners_cut_the_corners() {
    let h = hexagon();
    // Each corner cut back `cut(turn)` along both edges (at most half of
    // each).
    let cut_by = |cut: &dyn Fn(f64) -> f64| -> Vec<Pt2> {
        let n = h.len();
        let mut out = Vec::new();
        for i in 0..n {
            let (p, q, r) = (h[(i + n - 1) % n], h[i], h[(i + 1) % n]);
            let (l1, l2) = ((q.0 - p.0).hypot(q.1 - p.1), (r.0 - q.0).hypot(r.1 - q.1));
            let dot = ((q.0 - p.0) * (r.0 - q.0) + (q.1 - p.1) * (r.1 - q.1)) / (l1 * l2);
            let cc = cut(dot.clamp(-1.0, 1.0).acos()).min(l1 / 2.0).min(l2 / 2.0);
            out.push((q.0 - (q.0 - p.0) / l1 * cc, q.1 - (q.1 - p.1) / l1 * cc));
            out.push((q.0 + (r.0 - q.0) / l2 * cc, q.1 + (r.1 - q.1) / l2 * cc));
        }
        out
    };
    let cut = cells(&poly(&[
        on("c16_chamfer_corners"),
        on("c08_filled_polygon"),
    ]));
    assert_eq!(cut, fill(&cut_by(&|_| 2.0)));
    assert!(cut.is_subset(&fill(&h)) && cut.len() < fill(&h).len());
    // A rounded corner's arc lies between the corner and the chord of its
    // tangent points (radius · tan(turn / 2) back along each edge).
    let round = cells(&poly(&[
        on("c17_rounded_corners"),
        on("c08_filled_polygon"),
    ]));
    let chords = fill(&cut_by(&|turn| 3.0 * (turn / 2.0).tan()));
    assert!(round.is_subset(&fill(&h)) && round.is_superset(&chords));
    assert_ne!(round, fill(&h));
}

#[test]
fn poly_per_edge_subchannels_density_and_accents_brush_the_outline() {
    let h = hexagon();
    // Subchannels by edge: [0, 1, 2, 3] cycled over the six edges; each
    // piece of a row takes its middle's nearest edge.
    let v = poly(&[on("c18_per_edge_subchannels")]);
    let inner = moved(&h, -1.0);
    for n in &v {
        let (c0, c1) = ((n.start / 240) as f64, (n.end() / 240) as f64);
        let yc = f64::from(n.pitch) + 0.5;
        // The piece: the outer crossing to the inner one on that side.
        let outer = crossing_pairs(&h, yc)[0];
        let mid = match crossing_pairs(&inner, yc).first() {
            Some(&(ia, _)) if c1 <= ia.round() + 0.5 => (outer.0 + ia) / 2.0,
            Some(&(_, ib)) if c0 >= ib.round() - 0.5 => (ib + outer.1) / 2.0,
            _ => (outer.0 + outer.1) / 2.0,
        };
        let want = (nearest_edge(&h, (mid, yc)) % 4) as u32;
        assert_eq!((n.sub, u32::from(n.ch)), (want, want), "{n:?}");
    }
    assert_eq!(cells(&v), outline_of(&h, 1.0));
    // Density [1, 2]: label the edges' parity by subchannel; even edges
    // keep every cell, odd ones about every second.
    let parity = [
        lst("c18_per_edge_subchannels.subs", &[0, 1]),
        on("c18_per_edge_subchannels"),
    ];
    let by_sub =
        |v: &[TNote], s: u32| cells(&v.iter().filter(|n| n.sub == s).copied().collect::<Vec<_>>());
    let all = poly(&parity);
    let mut d = parity.to_vec();
    d.push(on("c20_per_edge_density"));
    let thin = poly(&d);
    assert_eq!(by_sub(&thin, 0), by_sub(&all, 0));
    let (kept, was) = (by_sub(&thin, 1).len(), by_sub(&all, 1).len());
    assert!(by_sub(&thin, 1).is_subset(&by_sub(&all, 1)));
    assert!(kept * 4 >= was && kept * 4 <= was * 3, "{kept} of {was}");
    // Accents: cells whose centre lies within 1 unit of a vertex.
    let acc = poly(&[on("c19_vertex_accents")]);
    for n in &acc {
        for c in n.start / 240..n.end() / 240 {
            let p = (c as f64 + 0.5, f64::from(n.pitch) + 0.5);
            let near = h.iter().any(|q| (p.0 - q.0).hypot(p.1 - q.1) <= 1.0);
            assert_eq!(n.vel, if near { 127 } else { 100 }, "{n:?} cell {c}");
        }
    }
    assert!(acc.iter().any(|n| n.vel == 127));
    assert_eq!(cells(&acc), outline_of(&h, 1.0));
}

#[test]
fn poly_gradients_run_along_each_edge() {
    let h = hexagon();
    let v = poly(&[on("c21_velocity_gate_edge_gradients")]);
    let n6 = h.len();
    for n in &v {
        // The note's first cell: its nearest edge, its position along it.
        let c = (n.start / 240) as f64;
        let p = (c + 0.5, f64::from(n.pitch) + 0.5);
        let e = nearest_edge(&h, p);
        let (a, b) = (h[e], h[(e + 1) % n6]);
        let d = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / (d.0 * d.0 + d.1 * d.1)).clamp(0.0, 1.0);
        assert_eq!(n.vel, (127.0 + (40.0 - 127.0) * t).round() as u8, "{n:?}");
        let g = ((1.0 + (0.5 - 1.0) * t) * 100.0).round() / 100.0;
        let len = (n.dur as f64 / (240.0 * g)).round().max(1.0);
        assert_eq!(n.dur, (240.0 * len * g).round() as u64, "{n:?}");
    }
    let vels: BTreeSet<u8> = v.iter().map(|n| n.vel).collect();
    assert!(
        vels.iter().any(|&x| x >= 115) && vels.iter().any(|&x| x <= 55),
        "{vels:?}"
    );
}

#[test]
fn poly_skew_twist_and_perspective_map_the_shape() {
    let h = hexagon();
    let skewed: Vec<Pt2> = h
        .iter()
        .map(|&(u, v)| (u + (v - 64.0) * 30f64.to_radians().tan(), v))
        .collect();
    assert_eq!(
        cells(&poly(&[
            on("c22_twist_skew"),
            num("c22_twist_skew.skew", 30.0),
            num("c22_twist_skew.twist", 0.0),
            on("c08_filled_polygon"),
        ])),
        fill(&skewed)
    );
    // Twist 60° on a regular hexagon of radius 3: the vertices (all at the
    // largest radius) turn by 60°; the top one to (12 − 3 sin 60°, 65.5),
    // accented there.
    let tw = poly(&[
        on("c04_radius"),
        num("c04_radius.radius", 3.0),
        on("c22_twist_skew"),
        num("c22_twist_skew.twist", 60.0),
        on("c19_vertex_accents"),
        num("c19_vertex_accents.radius", 0.5),
    ]);
    assert!(
        tw.iter()
            .any(|n| n.pitch == 65 && n.start <= 9 * 240 && n.end() > 9 * 240 && n.vel == 127),
        "{tw:?}"
    );
    assert_ne!(
        cells(&tw),
        cells(&poly(&[on("c04_radius"), num("c04_radius.radius", 3.0)]))
    );
    // Perspective: the top half as wide as the bottom (a keystone over the
    // shape's rows 60 … 68), straight edges kept.
    let k: f64 = 0.5;
    let key: Vec<Pt2> = h
        .iter()
        .map(|&(u, v)| {
            let y = (v - 60.0) / 8.0;
            let den = 1.0 + (1.0 / k - 1.0) * y;
            (12.0 + (u - 12.0) / den, 60.0 + 8.0 * y / (k * den))
        })
        .collect();
    assert_eq!(
        cells(&poly(&[on("c23_perspective"), on("c08_filled_polygon")])),
        fill(&key)
    );
}

#[test]
fn poly_tiles_cover_the_box_on_the_polygons_own_lattice() {
    use std::f64::consts::{FRAC_PI_2, PI};
    let tiled = |sides: i64, layout: u32| {
        cells(&poly(&[
            on("c01_side_count"),
            int("c01_side_count.sides", sides),
            on("c24_tessellation_tiling"),
            num("c24_tessellation_tiling.radius", 2.0),
            ch("c24_tessellation_tiling.layout", layout),
            on("c08_filled_polygon"),
        ]))
    };
    let inside = |p: Pt2| p.0 >= 4.0 && p.0 < 20.0 && p.1 >= 60.0 && p.1 < 68.0;
    // Tiles whose centres lie in the box, filled: the union of their cells.
    let union = |centres: &[(Pt2, bool)], n: usize| -> BTreeSet<(i64, u8)> {
        let mut s = BTreeSet::new();
        for &(c, flip) in centres {
            let mut t = ngon((0.0, 0.0), 2.0, 2.0, n, FRAC_PI_2);
            if flip {
                t.iter_mut().for_each(|p| *p = (-p.0, -p.1));
            }
            let t: Vec<Pt2> = t.iter().map(|p| (p.0 + c.0, p.1 + c.1)).collect();
            s.extend(fill(&t));
        }
        s
    };
    let lattice = |t1: Pt2, t2: Pt2, extra: Option<Pt2>| {
        let mut out = Vec::new();
        for off in std::iter::once(((0.0, 0.0), false)).chain(extra.map(|d| (d, true))) {
            for i in -40i32..=40 {
                for j in -40i32..=40 {
                    let p = (
                        12.0 + off.0.0 + f64::from(i) * t1.0 + f64::from(j) * t2.0,
                        64.0 + off.0.1 + f64::from(i) * t1.1 + f64::from(j) * t2.1,
                    );
                    if inside(p) {
                        out.push((p, off.1));
                    }
                }
            }
        }
        out
    };
    // Hexagons (a vertex at the top): apothem √3, neighbours across the
    // edges at 120° and 180°.
    let a6 = 2.0 * (PI / 6.0).cos();
    let dir = |deg: f64| (deg.to_radians().cos(), deg.to_radians().sin());
    let hex = lattice(
        (2.0 * a6 * dir(120.0).0, 2.0 * a6 * dir(120.0).1),
        (2.0 * a6 * dir(180.0).0, 2.0 * a6 * dir(180.0).1),
        None,
    );
    assert!(hex.len() >= 10, "{}", hex.len());
    assert_eq!(tiled(6, 0), union(&hex, 6));
    // Triangles: up ones on the lattice of their sides (2√3), a turned one
    // across each first edge (the normal at 150°, twice the apothem 1).
    let s3 = 2.0 * 3f64.sqrt();
    let tri = lattice(
        (s3, 0.0),
        (s3 * dir(120.0).0, s3 * dir(120.0).1),
        Some((2.0 * dir(150.0).0, 2.0 * dir(150.0).1)),
    );
    assert_eq!(tiled(3, 0), union(&tri, 3));
    // A square grid of pentagons' circles (diameter 4).
    let sq = lattice((4.0, 0.0), (0.0, 4.0), None);
    assert_eq!(tiled(5, 2), union(&sq, 5));
    // Hexagonal packing: rows 2√3 apart, every other shifted by 2.
    let hp = lattice((4.0, 0.0), (2.0, 2.0 * 3f64.sqrt()), None);
    assert_eq!(tiled(5, 1), union(&hp, 5));
    // A pentagon asks for its own tiling: hexagonal packing.
    assert_eq!(tiled(5, 0), tiled(5, 1));
}

#[test]
fn poly_seeded_perturbation_and_the_morph_towards_the_ellipse() {
    let p = |seed: i64| {
        cells(&poly(&[
            on("c25_seeded_vertex_perturbation_shape_morph"),
            int("gen.seed", seed),
        ]))
    };
    assert_eq!(p(1), p(1));
    assert_ne!(p(1), p(2));
    assert_ne!(p(1), cells(&poly(&[])));
    // No moves, a whole morph: the box's ellipse (within a few cells of the
    // filled ellipse, its points one grid unit apart).
    let round = cells(&poly(&[
        on("c25_seeded_vertex_perturbation_shape_morph"),
        num("c25_seeded_vertex_perturbation_shape_morph.amount", 0.0),
        num("c25_seeded_vertex_perturbation_shape_morph.morph", 100.0),
        on("c08_filled_polygon"),
    ]));
    let e = ellipse_cells(12.0, 64.0, 8.0, 4.0);
    let diff = round.symmetric_difference(&e).count();
    assert!(diff <= 4, "{diff}");
    // Half way: between the hexagon and the ellipse.
    let half = cells(&poly(&[
        on("c25_seeded_vertex_perturbation_shape_morph"),
        num("c25_seeded_vertex_perturbation_shape_morph.amount", 0.0),
        num("c25_seeded_vertex_perturbation_shape_morph.morph", 50.0),
        on("c08_filled_polygon"),
    ]));
    let hx = fill(&hexagon());
    assert!(half.is_superset(&hx) && e.is_superset(&half), "{half:?}");
}

#[test]
fn poly_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], words: &str| match poly_try(kv) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains(words) && m.ends_with('.'), "{m}"),
        other => panic!("{words}: {:?}", other.map(|v| v.len())),
    };
    let tiles = [
        on("c24_tessellation_tiling"),
        num("c24_tessellation_tiling.radius", 1.0),
        ("gen.length", ParamValue::Int(240 * 4000)),
    ];
    refused(&tiles, "tiles");
    let mut rings = vec![
        on("c24_tessellation_tiling"),
        num("c24_tessellation_tiling.radius", 1.0),
        ("gen.length", ParamValue::Int(240 * 200)),
        on("c13_concentric_polygons"),
        int("c13_concentric_polygons.count", 64),
    ];
    refused(&rings, "rings");
    rings.truncate(3);
    rings.extend([
        on("c01_side_count"),
        int("c01_side_count.sides", 64),
        on("c11_triangulated_fill"),
        int("c11_triangulated_fill.levels", 4),
    ]);
    refused(&rings, "line segments");
    refused(
        &[
            on("c24_tessellation_tiling"),
            num("c24_tessellation_tiling.radius", 1.0),
            ("gen.length", ParamValue::Int(240 * 200)),
            on("c01_side_count"),
            int("c01_side_count.sides", 64),
            on("c17_rounded_corners"),
        ],
        "points",
    );
}

#[test]
fn poly_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = poly_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.iter().all(|n| valid(n, keys)));
    }
}

// --------------------------------------------------- Custom Shape (V09-S57)
// What each of Custom Shape's 25 functions means (V09-0094, `docs/
// GENERATORS.md` "Custom Shape"), on a box of cells 4 … 43 (u = tick / 240)
// and rows 50 … 69: 40 grid units by 20, its centre (24, 60). The oracles
// build the expected rows from the shapes' own definitions — crossing pairs
// of polygons combined interval by interval, distances to the boundary at
// the cells' centres — independently of the area code.

fn cust_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.custom")
        .unwrap()
}

fn cust_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = cust_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(50)),
            ("gen.high", ParamValue::Int(69)),
            ("gen.length", ParamValue::Int(240 * 40)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn cust(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    cust_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

/// Per mille of the box to grid space.
fn pm(x: f64, y: f64) -> Pt2 {
    (4.0 + x / 1000.0 * 40.0, 50.0 + y / 1000.0 * 20.0)
}

fn pms(v: &[i64]) -> Vec<Pt2> {
    v.chunks_exact(2)
        .map(|q| pm(q[0] as f64, q[1] as f64))
        .collect()
}

/// The base arrow.
fn arrow() -> Vec<Pt2> {
    pms(&[
        0, 250, 600, 250, 600, 0, 1000, 500, 600, 1000, 600, 750, 0, 750,
    ])
}

/// Crossings of row centre `yc` with closed contours: x and the edge's
/// direction (up +1, down −1), by the row-centre rule.
fn cust_crossings(cs: &[Vec<Pt2>], yc: f64) -> Vec<(f64, i32)> {
    let mut xs = Vec::new();
    for c in cs {
        let n = c.len();
        for k in 0..n {
            let (a, b) = (c[k], c[(k + 1) % n]);
            if (a.1 <= yc && yc < b.1) || (b.1 <= yc && yc < a.1) {
                xs.push((
                    a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1),
                    if b.1 > a.1 { 1 } else { -1 },
                ));
            }
        }
    }
    xs.sort_by(|p, q| p.0.total_cmp(&q.0));
    xs
}

/// The row's intervals inside the contours by the even–odd or non-zero
/// rule.
fn ruled(cs: &[Vec<Pt2>], yc: f64, nonzero: bool) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let (mut w, mut open) = (0, None);
    let xs = cust_crossings(cs, yc);
    let mut i = 0;
    while i < xs.len() {
        let x = xs[i].0;
        while i < xs.len() && xs[i].0 == x {
            w += xs[i].1;
            i += 1;
        }
        let inside = if nonzero { w != 0 } else { w % 2 != 0 };
        match (open, inside) {
            (None, true) => open = Some(x),
            (Some(s), false) => {
                out.push((s, x));
                open = None;
            }
            _ => {}
        }
    }
    out
}

/// Whether `p` lies inside the contours (even–odd).
fn inside_eo(cs: &[Vec<Pt2>], p: Pt2) -> bool {
    ruled(cs, p.1, false)
        .iter()
        .any(|&(a, b)| a <= p.0 && p.0 < b)
}

fn iv_union(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = a.iter().chain(b).copied().collect();
    v.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for w in v {
        match out.last_mut() {
            Some(l) if w.0 <= l.1 => l.1 = l.1.max(w.1),
            _ => out.push(w),
        }
    }
    out
}

fn iv_inter(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for &(p, q) in a {
        for &(r, s) in b {
            let (x, y) = (p.max(r), q.min(s));
            if y > x {
                out.push((x, y));
            }
        }
    }
    out.sort_by(|p, q| p.0.total_cmp(&q.0));
    out
}

fn iv_sub(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = a.to_vec();
    for &(r, s) in b {
        out = out
            .into_iter()
            .flat_map(|(p, q)| {
                let mut v = Vec::new();
                if r > p {
                    v.push((p, q.min(r)));
                }
                if s < q {
                    v.push((p.max(s), q));
                }
                v.into_iter().filter(|w| w.1 > w.0)
            })
            .collect();
    }
    out
}

/// The cells of row `r`'s pieces by the pen's rounding: each piece whole
/// cells round(a) … round(b) (at least one), after the last one.
fn round_cells(r: u8, ivs: &[(f64, f64)], out: &mut BTreeSet<(i64, u8)>) {
    let mut end = f64::NEG_INFINITY;
    for &(a, b) in ivs {
        let s = a.round().max(end);
        let e = b.round().max(a.round() + 1.0);
        if e > s {
            for c in s as i64..e as i64 {
                out.insert((c, r));
            }
            end = e;
        }
    }
}

/// The cells of the rows' intervals `f(yc)`.
fn region(f: impl Fn(f64) -> Vec<(f64, f64)>) -> BTreeSet<(i64, u8)> {
    let mut out = BTreeSet::new();
    for r in 30u8..=90 {
        round_cells(r, &f(f64::from(r) + 0.5), &mut out);
    }
    out
}

/// The cells where `pred` holds: each row scanned every 1/256 of a unit
/// over cells −10 … 60, each change refined by bisection, the pieces
/// rounded like the pen's.
fn pred_cells(pred: impl Fn(Pt2) -> bool) -> BTreeSet<(i64, u8)> {
    let mut out = BTreeSet::new();
    let (x0, x1) = (-10.0, 60.0);
    for r in 30u8..=90 {
        let yc = f64::from(r) + 0.5;
        let n = ((x1 - x0) * 256.0) as usize;
        let mut prev = pred((x0, yc));
        let mut start = prev.then_some(x0);
        let mut ivs = Vec::new();
        for i in 1..=n {
            let x = x0 + i as f64 / 256.0;
            let cur = pred((x, yc));
            if cur != prev {
                let (mut lo, mut hi) = (x - 1.0 / 256.0, x);
                for _ in 0..80 {
                    let m = (lo + hi) / 2.0;
                    if pred((m, yc)) == prev {
                        lo = m;
                    } else {
                        hi = m;
                    }
                }
                if cur {
                    start = Some(hi);
                } else {
                    // A touching point (a chord of no width; its distance
                    // equals the radius over ~1e-8 in floating point) draws
                    // nothing.
                    let s = start.take().unwrap();
                    if hi - s > 1e-6 {
                        ivs.push((s, hi));
                    }
                }
                prev = cur;
            }
        }
        round_cells(r, &ivs, &mut out);
    }
    out
}

/// Whether `p` lies within `r` of segment `a → b` (exactly `r`: only
/// above the segment's nearest point — the area code's half-open rows).
fn within(p: Pt2, a: Pt2, b: Pt2, r: f64) -> bool {
    let d = (b.0 - a.0, b.1 - a.1);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = (a.0 + t * d.0, a.1 + t * d.1);
    let dist = (p.0 - q.0).hypot(p.1 - q.1);
    dist < r || (dist == r && p.1 > q.1)
}

/// Segments of closed contours (`closed`) or open paths.
fn segs_of(cs: &[Vec<Pt2>], closed: bool) -> Vec<(Pt2, Pt2)> {
    let mut v = Vec::new();
    for c in cs {
        let n = c.len();
        let m = if closed { n } else { n - 1 };
        for k in 0..m {
            v.push((c[k], c[(k + 1) % n]));
        }
    }
    v
}

fn near_any(segs: &[(Pt2, Pt2)], p: Pt2, r: f64) -> bool {
    segs.iter().any(|&(a, b)| within(p, a, b, r))
}

/// Points mapped about the box's centre.
fn about(pts: &[Pt2], f: impl Fn(Pt2) -> Pt2) -> Vec<Pt2> {
    pts.iter()
        .map(|&(u, v)| {
            let q = f((u - 24.0, v - 60.0));
            (q.0 + 24.0, q.1 + 60.0)
        })
        .collect()
}

/// The ellipse operand: 64 points from its right, counter-clockwise.
fn cust_ellipse(c: Pt2, r: Pt2) -> Vec<Pt2> {
    (0..64)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / 64.0;
            (c.0 + r.0 * a.cos(), c.1 + r.1 * a.sin())
        })
        .collect()
}

fn filled(kv: &[(&'static str, ParamValue)]) -> BTreeSet<(i64, u8)> {
    let mut v = kv.to_vec();
    v.push(on("c08_filled_generation"));
    cells(&cust(&v))
}

#[test]
fn cust_the_base_is_the_arrow_outline_one_cell_wide_inside_it() {
    let a = arrow();
    let edges = segs_of(std::slice::from_ref(&a), true);
    let base = cust(&[]);
    assert!(base.iter().all(|n| (n.vel, n.sub) == (100, 0)));
    assert_eq!(
        cells(&base),
        pred_cells(|p| inside_eo(std::slice::from_ref(&a), p) && near_any(&edges, p, 1.0))
    );
    // Filled: the arrow by the row-centre rule, one note a row.
    let f = cust(&[on("c08_filled_generation")]);
    assert_eq!(
        cells(&f),
        region(|y| ruled(std::slice::from_ref(&a), y, false))
    );
    assert_eq!(f.len(), 20);
}

#[test]
fn cust_vertices_the_library_and_bezier_handles_shape_the_main_contour() {
    // c01: the listed vertices (a lightning bolt).
    let bolt = pms(&[
        400, 1000, 150, 450, 450, 450, 250, 0, 850, 600, 550, 600, 750, 1000,
    ]);
    assert_eq!(
        filled(&[on("c01_arbitrary_vertex_placement")]),
        region(|y| ruled(std::slice::from_ref(&bolt), y, false))
    );
    // c25: the library's named shapes (the name in any case), the star.
    let star = pms(&[
        500, 1000, 382, 662, 24, 655, 310, 438, 206, 95, 500, 300, 794, 95, 690, 438, 976, 655,
        618, 662,
    ]);
    let lib = |name: &str| {
        [
            on("c25_save_load_custom_shape_presets"),
            (
                "c25_save_load_custom_shape_presets.name",
                ParamValue::Text(name.into()),
            ),
        ]
    };
    assert_eq!(
        filled(&lib("star")),
        region(|y| ruled(std::slice::from_ref(&star), y, false))
    );
    // A shape of two contours: the ring's inner one cuts a hole.
    let ring = [
        pms(&[
            300, 0, 700, 0, 1000, 300, 1000, 700, 700, 1000, 300, 1000, 0, 700, 0, 300,
        ]),
        pms(&[
            400, 250, 600, 250, 750, 400, 750, 600, 600, 750, 400, 750, 250, 600, 250, 400,
        ]),
    ];
    assert_eq!(filled(&lib("Ring")), region(|y| ruled(&ring, y, false)));
    match cust_try(&lib("Moon")) {
        Err(TransformError::Invalid(m)) => {
            assert!(
                m.contains("Moon") && m.contains("Heart, Star") && m.ends_with('.'),
                "{m}"
            )
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    let mut bad = lib("A").to_vec();
    bad.push((
        "c25_save_load_custom_shape_presets.library",
        ParamValue::Text("A: 1,2 3,x 5,6".into()),
    ));
    assert!(matches!(cust_try(&bad), Err(TransformError::Invalid(m)) if m.contains("“3,x”")));
    // c02: every edge a cubic through the vertices (automatic handles: the
    // neighbours' chord / 6), flattened on the grid — close to the curve
    // sampled densely, and rounder than the straight arrow.
    let a = arrow();
    let n = a.len();
    let mut curve = Vec::new();
    for i in 0..n {
        let (p0, p3) = (a[i], a[(i + 1) % n]);
        let h = |k: usize| {
            let (p, q) = (a[(k + n - 1) % n], a[(k + 1) % n]);
            ((q.0 - p.0) / 6.0, (q.1 - p.1) / 6.0)
        };
        let (h0, h1) = (h(i), h((i + 1) % n));
        let (p1, p2) = ((p0.0 + h0.0, p0.1 + h0.1), (p3.0 - h1.0, p3.1 - h1.1));
        for k in 0..400 {
            let t = k as f64 / 400.0;
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            curve.push((
                w[0] * p0.0 + w[1] * p1.0 + w[2] * p2.0 + w[3] * p3.0,
                w[0] * p0.1 + w[1] * p1.1 + w[2] * p2.1 + w[3] * p3.1,
            ));
        }
    }
    let got = filled(&[on("c02_bezier_handles_per_vertex")]);
    let want = region(|y| ruled(std::slice::from_ref(&curve), y, false));
    assert!(got.symmetric_difference(&want).count() <= 4, "{got:?}");
    assert!(got.symmetric_difference(&filled(&[])).count() > 20);
    // Listed handles differ from automatic ones.
    let listed = filled(&[
        on("c02_bezier_handles_per_vertex"),
        ch("c02_bezier_handles_per_vertex.mode", 1),
    ]);
    assert_ne!(listed, got);
}

#[test]
fn cust_open_and_closed_paths_are_strokes_centred_on_their_lines() {
    let a = arrow();
    // c03: the closing edge (the last vertex back to the first) left out;
    // round ends, one unit wide (half a unit each side).
    let open: Vec<Pt2> = a.clone();
    let segs = segs_of(std::slice::from_ref(&open), false);
    assert_eq!(
        cells(&cust(&[on("c03_open_path")])),
        pred_cells(|p| near_any(&segs, p, 0.5))
    );
    // Opening after vertex 3 leaves out edge 3 (vertex 3 → 4) instead.
    let mut r = a.clone();
    r.rotate_left(3);
    let segs3 = segs_of(std::slice::from_ref(&r), false);
    assert_eq!(
        cells(&cust(&[on("c03_open_path"), int("c03_open_path.after", 3)])),
        pred_cells(|p| near_any(&segs3, p, 0.5))
    );
    // c04 with round corners: every edge, centred.
    let closed = segs_of(std::slice::from_ref(&a), true);
    let round = cells(&cust(&[
        on("c04_closed_path"),
        ch("c04_closed_path.joins", 0),
    ]));
    assert_eq!(round, pred_cells(|p| near_any(&closed, p, 0.5)));
    // Mitres reach past the acute corners; bevels stay inside the mitres.
    let mitre = cells(&cust(&[on("c04_closed_path")]));
    let bevel = cells(&cust(&[
        on("c04_closed_path"),
        ch("c04_closed_path.joins", 2),
    ]));
    assert!(bevel.is_subset(&mitre) && bevel != mitre);
    assert!(mitre.contains(&(28, 49)) && !round.contains(&(28, 49)));
    // Square ends reach half the width past the path's ends (3 wide: to
    // u = 2.5).
    let wide = |cap: u32| {
        cells(&cust(&[
            on("c03_open_path"),
            ch("c03_open_path.caps", cap),
            on("c09_stroke_thickness"),
            int("c09_stroke_thickness.size", 3),
        ]))
    };
    let (sq, butt) = (wide(2), wide(0));
    assert!(butt.is_subset(&sq) && sq.contains(&(3, 64)) && !butt.contains(&(3, 64)));
}

#[test]
fn cust_contours_holes_and_the_fill_rule_decide_the_area() {
    let a = arrow();
    let extra = [
        pms(&[50, 820, 250, 820, 250, 980, 50, 980]),
        pms(&[100, 30, 320, 30, 210, 200]),
    ];
    let mut all = vec![a.clone()];
    all.extend(extra.iter().cloned());
    assert_eq!(
        filled(&[on("c05_multiple_contours")]),
        region(|y| ruled(&all, y, false))
    );
    // Holes: cut out whatever the rule.
    let hole = pms(&[300, 380, 420, 500, 300, 620, 180, 500]);
    let want = region(|y| {
        iv_sub(
            &ruled(std::slice::from_ref(&a), y, false),
            &ruled(std::slice::from_ref(&hole), y, false),
        )
    });
    assert_eq!(filled(&[on("c06_interior_holes")]), want);
    // The hole's edges are outlined like the shape's.
    let outlined = cells(&cust(&[on("c06_interior_holes")]));
    let segs = segs_of(&[a.clone(), hole.clone()], true);
    assert_eq!(
        outlined,
        pred_cells(|p| inside_eo(std::slice::from_ref(&a), p)
            && !inside_eo(std::slice::from_ref(&hole), p)
            && near_any(&segs, p, 1.0))
    );
    // Rules on an overlapping contour (same direction): even–odd cancels
    // the overlap, non-zero keeps it.
    let over = [
        on("c05_multiple_contours"),
        lst(
            "c05_multiple_contours.points",
            &[100, 100, 500, 100, 500, 900, 100, 900],
        ),
        lst("c05_multiple_contours.sizes", &[4]),
        on("c10_configurable_fill_strategy"),
        ch("c10_configurable_fill_strategy.pattern", 0),
    ];
    let sq = pms(&[100, 100, 500, 100, 500, 900, 100, 900]);
    let both = [a.clone(), sq];
    let mut eo = over.to_vec();
    eo.push(ch("c10_configurable_fill_strategy.rule", 0));
    assert_eq!(filled(&eo), region(|y| ruled(&both, y, false)));
    assert_eq!(filled(&over), region(|y| ruled(&both, y, true)));
    assert_ne!(filled(&eo), filled(&over));
}

#[test]
fn cust_fill_patterns_draw_the_interior_and_filling_draws_between_them() {
    let a = arrow();
    let area = region(|y| ruled(std::slice::from_ref(&a), y, false));
    let outline = cells(&cust(&[]));
    let pat = |p: u32| {
        cells(&cust(&[
            on("c10_configurable_fill_strategy"),
            ch("c10_configurable_fill_strategy.pattern", p),
        ]))
    };
    // Rows every 4 from the box's lowest row; columns every 4 from its
    // start; diagonals shift a column a row; a crosshatch both ways. A
    // window cut short by the outline's piece still draws a whole cell (the
    // pen's rule): only next to the outline may a cell lie off the pattern.
    let keep = |f: &dyn Fn(i64, u8) -> bool| -> BTreeSet<(i64, u8)> {
        area.iter()
            .filter(|&&(c, r)| outline.contains(&(c, r)) || f(c, r))
            .copied()
            .collect()
    };
    let check = |got: BTreeSet<(i64, u8)>, want: BTreeSet<(i64, u8)>| {
        assert!(
            want.is_subset(&got),
            "{:?}",
            want.difference(&got).collect::<Vec<_>>()
        );
        for &(c, r) in got.difference(&want) {
            assert!(
                area.contains(&(c, r))
                    && (outline.contains(&(c - 1, r)) || outline.contains(&(c + 1, r))),
                "{c} {r}"
            );
        }
    };
    assert_eq!(pat(0), area);
    check(pat(1), keep(&|_, r| (r - 50) % 4 == 0));
    check(pat(2), keep(&|c, _| (c - 4) % 4 == 0));
    check(
        pat(3),
        keep(&|c, r| (c - 4 + i64::from(r) - 50).rem_euclid(4) == 0),
    );
    check(
        pat(4),
        keep(&|c, r| {
            (c - 4 + i64::from(r) - 50).rem_euclid(4) == 0
                || (c - 4 - i64::from(r) + 50).rem_euclid(4) == 0
        }),
    );
    // Filled with a pattern: what lies between its lines.
    let between = filled(&[
        on("c10_configurable_fill_strategy"),
        ch("c10_configurable_fill_strategy.pattern", 2),
    ]);
    let gaps = keep(&|c, _| (c - 4) % 4 != 0);
    assert!(between.is_subset(&gaps) && gaps.len() - between.len() <= 4);
}

#[test]
fn cust_outline_placement_thickness_and_filling() {
    let a = arrow();
    let one = std::slice::from_ref(&a);
    let edges = segs_of(one, true);
    let area = region(|y| ruled(one, y, false));
    let out = cells(&cust(&[on("c07_outline_generation")]));
    assert!(out.is_disjoint(&area));
    assert_eq!(
        out,
        pred_cells(|p| !inside_eo(one, p) && near_any(&edges, p, 1.0))
    );
    let centred = cells(&cust(&[
        on("c07_outline_generation"),
        ch("c07_outline_generation.placement", 1),
    ]));
    assert_eq!(centred, pred_cells(|p| near_any(&edges, p, 0.5)));
    let thick = cells(&cust(&[
        on("c09_stroke_thickness"),
        int("c09_stroke_thickness.size", 3),
    ]));
    assert_eq!(
        thick,
        pred_cells(|p| inside_eo(one, p) && near_any(&edges, p, 3.0))
    );
    // Filled without the outline: one fill, no band.
    let plain = cust(&[
        on("c08_filled_generation"),
        ("c08_filled_generation.outline", ParamValue::Bool(false)),
        on("c13_per_edge_subchannel"),
    ]);
    assert_eq!(cells(&plain), area);
    assert!(plain.iter().all(|n| n.sub == 0));
}

#[test]
fn cust_edge_and_vertex_brushes_follow_the_nearest_edge() {
    let a = arrow();
    let n7 = a.len();
    let base = cells(&cust(&[]));
    // c13: subchannels by edge [0, 1, 2, 3] cycled.
    let subs = cust(&[on("c13_per_edge_subchannel")]);
    assert_eq!(cells(&subs), base);
    for n in &subs {
        let yc = f64::from(n.pitch) + 0.5;
        let ok = (n.start / 240..n.end() / 240)
            .any(|c| (nearest_edge(&a, (c as f64 + 0.5, yc)) % 4) as u32 == n.sub);
        assert!(ok, "{n:?}");
    }
    let used: BTreeSet<u32> = subs.iter().map(|n| n.sub).collect();
    assert_eq!(used.len(), 4);
    // c14: velocity from each edge's start vertex to its end vertex
    // ([127, 30] by vertex, cycled), by the cell's position along its
    // nearest edge.
    let vel = cust(&[on("c14_per_vertex_velocity")]);
    assert_eq!(cells(&vel), base);
    let vv = [127.0, 30.0];
    for n in &vel {
        let yc = f64::from(n.pitch) + 0.5;
        for c in n.start / 240..n.end() / 240 {
            let p = (c as f64 + 0.5, yc);
            let e = nearest_edge(&a, p);
            let (s, t) = (a[e], a[(e + 1) % n7]);
            let d = (t.0 - s.0, t.1 - s.1);
            let k =
                (((p.0 - s.0) * d.0 + (p.1 - s.1) * d.1) / (d.0 * d.0 + d.1 * d.1)).clamp(0.0, 1.0);
            let (v0, v1) = (vv[e % 2], vv[((e + 1) % n7) % 2]);
            assert_eq!(n.vel, (v0 + (v1 - v0) * k).round() as u8, "{n:?} cell {c}");
        }
    }
    // c11: a note every [1, 2, 3] cells by edge: the outline thinned (a
    // dash cut short at a piece's end still draws a cell), edge 0 (the
    // bottom of the shaft) whole, edge 2 about every third cell.
    let thin = cells(&cust(&[on("c11_per_edge_density")]));
    assert!(thin.difference(&base).count() <= 2 && thin.len() * 4 < base.len() * 3);
    let on_edge = |s: &BTreeSet<(i64, u8)>, e: usize| {
        s.iter()
            .filter(|&&(c, r)| nearest_edge(&a, (c as f64 + 0.5, f64::from(r) + 0.5)) == e)
            .count()
    };
    assert_eq!(on_edge(&thin, 0), on_edge(&base, 0));
    assert!(on_edge(&thin, 2) * 2 <= on_edge(&base, 2) + 2);
    // c12: shares [100 %, 20 %] by vertex: fewer cells, none added.
    let share = cells(&cust(&[on("c12_per_vertex_density")]));
    assert!(share.is_subset(&base) && share.len() * 10 < base.len() * 9);
    // c15: the gate by the curve around the shape (from its first vertex).
    let gate = cust(&[on("c15_gate_profile")]);
    let cv = plumspirit_engine::transform::Curve::new(vec![(0.0, 1.0), (0.5, 0.2), (1.0, 1.0)]);
    let c0 = (24.0, 60.0);
    let th0 = (a[0].1 - c0.1).atan2(a[0].0 - c0.0);
    for n in &gate {
        let p = ((n.start / 240) as f64 + 0.5, f64::from(n.pitch) + 0.5);
        let s = ((p.1 - c0.1).atan2(p.0 - c0.0) - th0).rem_euclid(std::f64::consts::TAU)
            / std::f64::consts::TAU;
        let g = (cv.eval(s).clamp(0.0, 1.0) * 100.0)
            .round()
            .clamp(1.0, 100.0)
            / 100.0;
        let len = (n.dur as f64 / (240.0 * g)).round().max(1.0);
        assert_eq!(n.dur, (240.0 * len * g).round() as u64, "{n:?}");
    }
    assert!(gate.iter().any(|n| n.dur < 120));
}

#[test]
fn cust_transform_mirror_radial_and_warp_map_the_shape() {
    let a = arrow();
    // c16: scaled 80 %, turned 20°, moved a tenth of the box's width.
    let (s, c) = 20f64.to_radians().sin_cos();
    let t = about(&a, |(x, y)| {
        let (x, y) = (x * 0.8, y * 0.8);
        (x * c - y * s + 4.0, x * s + y * c)
    });
    assert_eq!(
        filled(&[on("c16_full_transform")]),
        region(|y| ruled(std::slice::from_ref(&t), y, false))
    );
    // c17: united with its mirror image about the box's centre line.
    let m = about(&a, |(x, y)| (-x, y));
    let union2 = |p: &[Pt2], q: &[Pt2], y: f64| {
        iv_union(
            &ruled(&[p.to_vec()], y, false),
            &ruled(&[q.to_vec()], y, false),
        )
    };
    assert_eq!(
        filled(&[on("c17_mirror_symmetry")]),
        region(|y| union2(&a, &m, y))
    );
    // An axis moved a quarter of the box.
    let m2 = about(&a, |(x, y)| (20.0 - x, y));
    assert_eq!(
        filled(&[
            on("c17_mirror_symmetry"),
            num("c17_mirror_symmetry.offset", 25.0)
        ]),
        region(|y| union2(&a, &m2, y))
    );
    // c18: four copies turned by quarter turns about the centre.
    let rots: Vec<Vec<Pt2>> = (0..4)
        .map(|k| {
            let (s, c) = (std::f64::consts::FRAC_PI_2 * f64::from(k)).sin_cos();
            about(&a, |(x, y)| (x * c - y * s, x * s + y * c))
        })
        .collect();
    assert_eq!(
        filled(&[on("c18_radial_symmetry")]),
        region(|y| rots.iter().fold(Vec::new(), |acc, r| iv_union(
            &acc,
            &ruled(std::slice::from_ref(r), y, false)
        )))
    );
    // c23: each edge cut into unit pieces, each point moved by the 3 × 3
    // mesh's bilinear offset (the centre node pushed 150 ‰, 100 ‰).
    let n = a.len();
    let mut warped = Vec::new();
    for i in 0..n {
        let (p, q) = (a[i], a[(i + 1) % n]);
        let k = ((q.0 - p.0).hypot(q.1 - p.1)).ceil().clamp(1.0, 64.0) as usize;
        for j in 0..k {
            let f = j as f64 / k as f64;
            let (x, y) = (p.0 + (q.0 - p.0) * f, p.1 + (q.1 - p.1) * f);
            let (u, v) = ((x - 4.0) / 40.0, (y - 50.0) / 20.0);
            // The only nonzero node is the centre (1, 1): a tent.
            let w = (1.0 - (u * 2.0 - 1.0).abs()).max(0.0) * (1.0 - (v * 2.0 - 1.0).abs()).max(0.0);
            warped.push((x + 0.15 * w * 40.0, y + 0.1 * w * 20.0));
        }
    }
    assert_eq!(
        filled(&[on("c23_mesh_warp")]),
        region(|y| ruled(std::slice::from_ref(&warped), y, false))
    );
}

#[test]
fn cust_booleans_combine_the_area_and_its_outline_follows() {
    let a = arrow();
    let one = |p: &Vec<Pt2>, y: f64| ruled(std::slice::from_ref(p), y, false);
    let uni = cust_ellipse((14.0, 60.0), (10.0, 8.0));
    let sub = cust_ellipse((36.0, 60.0), (6.0, 3.0));
    let int_ = cust_ellipse((24.0, 60.0), (20.0, 10.0));
    assert_eq!(
        filled(&[on("c19_shape_union")]),
        region(|y| iv_union(&one(&a, y), &one(&uni, y)))
    );
    assert_eq!(
        filled(&[on("c20_shape_subtract")]),
        region(|y| iv_sub(&one(&a, y), &one(&sub, y)))
    );
    assert_eq!(
        filled(&[on("c21_shape_intersection")]),
        region(|y| iv_inter(&one(&a, y), &one(&int_, y)))
    );
    // In order: union, then subtract, then intersection.
    assert_eq!(
        filled(&[
            on("c19_shape_union"),
            on("c20_shape_subtract"),
            on("c21_shape_intersection")
        ]),
        region(|y| iv_inter(
            &iv_sub(&iv_union(&one(&a, y), &one(&uni, y)), &one(&sub, y)),
            &one(&int_, y)
        ))
    );
    // The union's outline: inside the area, within a unit of the boundary
    // of the union only (the shaft's top edge inside the ellipse is gone).
    let o = cells(&cust(&[on("c19_shape_union")]));
    let area = filled(&[on("c19_shape_union")]);
    assert!(o.is_subset(&area));
    for c in 8..20 {
        assert!(!o.contains(&(c, 64)), "{c}");
    }
    assert!(o.contains(&(24, 64)) && o.contains(&(14, 67)));
    // A rectangle operand and a polygon operand.
    let rect = pms(&[400, 300, 600, 300, 600, 700, 400, 700]);
    assert_eq!(
        filled(&[
            on("c20_shape_subtract"),
            ch("c20_shape_subtract.kind", 1),
            int("c20_shape_subtract.x", 500),
            int("c20_shape_subtract.w", 200),
            int("c20_shape_subtract.h", 400),
        ]),
        region(|y| iv_sub(&one(&a, y), &one(&rect, y)))
    );
    let tri = pms(&[0, 0, 1000, 0, 500, 1000]);
    assert_eq!(
        filled(&[
            on("c21_shape_intersection"),
            ch("c21_shape_intersection.kind", 2),
            lst("c21_shape_intersection.points", &[0, 0, 1000, 0, 500, 1000]),
        ]),
        region(|y| iv_inter(&one(&a, y), &one(&tri, y)))
    );
}

#[test]
fn cust_tiles_and_morph_steps_make_copies() {
    // c22: 8 × 8 tiles 1 apart: 4 × 2 fit, centred; each the arrow built
    // for its tile.
    let tile = |cx: f64, cy: f64, flip: bool| -> Vec<Pt2> {
        [
            0, 250, 600, 250, 600, 0, 1000, 500, 600, 1000, 600, 750, 0, 750,
        ]
        .chunks_exact(2)
        .map(|q| {
            let x = (q[0] as f64 / 1000.0 - 0.5) * 8.0;
            (
                cx + if flip { -x } else { x },
                cy + (q[1] as f64 / 1000.0 - 0.5) * 8.0,
            )
        })
        .collect()
    };
    let centres: Vec<(f64, f64, bool)> = [55.5, 64.5]
        .iter()
        .enumerate()
        .flat_map(|(j, &y)| {
            [10.5, 19.5, 28.5, 37.5]
                .iter()
                .enumerate()
                .map(move |(i, &x)| (x, y, (i + j) % 2 == 1))
        })
        .collect();
    let tiles = |mirror: bool| {
        region(|y| {
            centres.iter().fold(Vec::new(), |acc, &(cx, cy, f)| {
                iv_union(&acc, &ruled(&[tile(cx, cy, mirror && f)], y, false))
            })
        })
    };
    assert_eq!(filled(&[on("c22_repeat_tile")]), tiles(false));
    assert_eq!(
        filled(&[on("c22_repeat_tile"), ch("c22_repeat_tile.layout", 2)]),
        tiles(true)
    );
    // c24: no morph is the arrow; three steps draw the arrow, a halfway
    // shape and the 50 % one; 100 % is the box's ellipse.
    let base = cells(&cust(&[]));
    assert_eq!(
        cells(&cust(&[
            on("c24_shape_a_b_morph"),
            num("c24_shape_a_b_morph.amount", 0.0)
        ])),
        base
    );
    let steps = cells(&cust(&[
        on("c24_shape_a_b_morph"),
        int("c24_shape_a_b_morph.steps", 3),
    ]));
    assert!(base.is_subset(&steps) && steps.len() > base.len());
    let round = filled(&[
        on("c24_shape_a_b_morph"),
        num("c24_shape_a_b_morph.amount", 100.0),
    ]);
    let ell = cust_ellipse((24.0, 60.0), (20.0, 10.0));
    let want = region(|y| ruled(std::slice::from_ref(&ell), y, false));
    assert!(round.symmetric_difference(&want).count() <= 6, "{round:?}");
}

#[test]
fn cust_a_preset_keeps_its_library_and_loads_the_named_shape() {
    let def = cust_def();
    let specs = (def.spec)().params;
    let lib = "Tri: 0,0 1000,0 500,1000; Box: 100,100 900,100 900,900 100,900";
    let values = with(
        defaults(def, KeyRange::Standard),
        &[
            on("c25_save_load_custom_shape_presets"),
            (
                "c25_save_load_custom_shape_presets.library",
                ParamValue::Text(lib.into()),
            ),
            (
                "c25_save_load_custom_shape_presets.name",
                ParamValue::Text("Box".into()),
            ),
        ],
    );
    let p = preset::Preset {
        tool: def.id.to_owned(),
        name: "My shapes".into(),
        values,
    };
    let text = preset::to_text(&p);
    let (back, skipped) = preset::parse(&text, def.id, &specs).unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(preset::to_text(&back), text);
    let d = doc(KeyRange::Standard, &[]);
    let c = ctx(&d, 960);
    let a = run_in(def, &d, &p.values, &c, &Limits::default())
        .unwrap()
        .notes;
    let b = run_in(def, &d, &back.values, &c, &Limits::default())
        .unwrap()
        .notes;
    assert_eq!(a, b);
    let bx = pms(&[100, 100, 900, 100, 900, 900, 100, 900]);
    let edges = segs_of(std::slice::from_ref(&bx), true);
    assert_eq!(
        cells(&cust(&[
            on("c25_save_load_custom_shape_presets"),
            (
                "c25_save_load_custom_shape_presets.library",
                ParamValue::Text(lib.into())
            ),
            (
                "c25_save_load_custom_shape_presets.name",
                ParamValue::Text("Box".into())
            ),
        ])),
        pred_cells(|q| inside_eo(std::slice::from_ref(&bx), q) && near_any(&edges, q, 1.0))
    );
}

#[test]
fn cust_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], words: &str| match cust_try(kv) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains(words) && m.ends_with('.'), "{m}"),
        other => panic!("{words}: {:?}", other.map(|v| v.len())),
    };
    refused(
        &[
            on("c22_repeat_tile"),
            num("c22_repeat_tile.w", 1.0),
            num("c22_repeat_tile.gap", 0.0),
            ("gen.length", ParamValue::Int(240 * 5000)),
        ],
        "shapes",
    );
    let mut many = vec![
        on("c01_arbitrary_vertex_placement"),
        lst(
            "c01_arbitrary_vertex_placement.points",
            &(0..64)
                .flat_map(|i| {
                    let a = std::f64::consts::TAU * f64::from(i) / 64.0;
                    let r = if i % 2 == 0 { 500.0 } else { 150.0 };
                    [(500.0 + r * a.cos()) as i64, (500.0 + r * a.sin()) as i64]
                })
                .collect::<Vec<_>>(),
        ),
        on("c02_bezier_handles_per_vertex"),
        on("c18_radial_symmetry"),
        int("c18_radial_symmetry.count", 32),
        on("c17_mirror_symmetry"),
        ch("c17_mirror_symmetry.axis", 2),
    ];
    refused(&many, "segments");
    many.truncate(2);
    assert!(cust_try(&many).is_ok());
}

#[test]
fn cust_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = cust_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.iter().all(|n| valid(n, keys)));
    }
}

#[test]
fn cust_side_densities_on_dashes_stay_bounded_and_keep_their_share() {
    // A long shaft (20,000 cells): its dashes (every 2nd cell) are more than
    // 4,096, so they are kept or dropped in groups by the vertices' shares —
    // about half here, never a cell the dashes alone would not draw.
    let long = [
        ("gen.length", ParamValue::Int(240 * 20_000)),
        on("c11_per_edge_density"),
        lst("c11_per_edge_density.every", &[2]),
    ];
    let dashes = cells(&cust(&long));
    let mut half = long.to_vec();
    half.extend([
        on("c12_per_vertex_density"),
        lst("c12_per_vertex_density.share", &[50]),
    ]);
    let kept = cells(&cust(&half));
    assert!(kept.is_subset(&dashes));
    assert!(kept.len() * 10 > dashes.len() * 3 && kept.len() * 10 < dashes.len() * 7);
    // Over a box spanning every tick with every share 0: each group is
    // tested once and none is walked — nothing is drawn, at once.
    let def = cust_def();
    let d = doc(KeyRange::Standard, &[rec(0, u64::MAX - 1, 60)]);
    let s = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.place", ParamValue::Choice(2)),
            ("gen.keep", ParamValue::Bool(false)),
            on("c11_per_edge_density"),
            lst("c11_per_edge_density.every", &[2, 3]),
            on("c12_per_vertex_density"),
            lst("c12_per_vertex_density.share", &[0]),
            on("c14_per_vertex_velocity"),
        ],
    );
    let t = std::time::Instant::now();
    match run_in(def, &d, &s, &ctx(&d, 0), &Limits::default()) {
        Ok(r) => assert!(r.notes.is_empty(), "{:?}", r.notes),
        Err(e) => assert!(e.user_message().ends_with('.'), "{e}"),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

// ------------------------------------------- Parametric Equation (V09-S58)
// What each of Parametric Equation's 25 functions means (V09-0095,
// `docs/GENERATORS.md` "Parametric Equation"), on Custom Shape's box: cells
// 4 … 43, rows 50 … 69 — grid u = 4 + 40 x (less 10⁻⁶ at the end), v from
// 50.5 (y = 0) to 69.5 (y = 1). The oracles evaluate the formulas by hand
// and draw the samples with the pen through the platform's own stroke rule
// where a pen is needed, comparing structure (breaks, windows, symmetry)
// exactly and shapes as cells.

fn par_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.parametric")
        .unwrap()
}

fn par_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = par_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(50)),
            ("gen.high", ParamValue::Int(69)),
            ("gen.length", ParamValue::Int(240 * 40)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn par(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    par_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

fn txt(k: &'static str, v: &str) -> (&'static str, ParamValue) {
    (k, ParamValue::Text(v.into()))
}

/// The cells notes start in or cover (gates may shorten them).
fn par_cells(v: &[TNote]) -> BTreeSet<(i64, u8)> {
    let mut s = BTreeSet::new();
    for n in v {
        for c in n.start / 240..(n.start + n.dur).div_ceil(240) {
            s.insert((c as i64, n.pitch));
        }
    }
    s
}

/// Whether a cell within one of `(c, r)` is drawn (the pen rounds a
/// stroke's rows and steps; a sample's own cell may be its neighbour).
fn near_cell(s: &BTreeSet<(i64, u8)>, c: i64, r: i64) -> bool {
    (-1..=1).any(|dc| (-1..=1).any(|dr| r + dr >= 0 && s.contains(&(c + dc, (r + dr) as u8))))
}

/// Notes with their brushes, as comparable tuples.
fn par_all(v: &[TNote]) -> Vec<(u64, u64, u8, u8, u32)> {
    v.iter()
        .map(|n| (n.start, n.dur, n.pitch, n.vel, n.sub))
        .collect()
}

const PAR_Y: &str = "1 - pow(1 - (a*t - floor(a*t)), 2)";

#[test]
fn par_the_base_is_a_curved_sawtooth_through_its_samples() {
    let base = par(&[]);
    let cells = par_cells(&base);
    assert!(base.iter().all(|n| (n.vel, n.sub) == (100, 0)));
    // Every sample (u = 4 + 40 t less 1e-6, v = 50.5 + 19 y) is drawn
    // in its cell or a neighbour, and nothing lies far from the samples.
    let y = |t: f64| 1.0 - (1.0 - (3.0 * t - (3.0 * t).floor())).powi(2);
    let pts: Vec<(f64, f64)> = (0..256)
        .map(|k| {
            let t = k as f64 / 255.0;
            (4.0 + 40.0 * t - 1e-6 * t, 50.5 + 19.0 * y(t))
        })
        .collect();
    for (k, &(u, v)) in pts.iter().enumerate() {
        assert!(near_cell(&cells, u.floor() as i64, v.floor() as i64), "{k}");
    }
    // The drops are joined (whole columns between the teeth).
    for r in 53..=66 {
        assert!(cells.contains(&(17, r)), "{r}");
    }
    // The base formulas given again draw the same notes.
    assert_eq!(
        par_all(&par(&[
            on("c01_x_t_formula"),
            txt("c01_x_t_formula.x", "t"),
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", PAR_Y),
        ])),
        par_all(&base)
    );
}

#[test]
fn par_modes_constants_and_maps_equal_their_formulas() {
    let same = |a: &[(&'static str, ParamValue)], b: &[(&'static str, ParamValue)]| {
        assert_eq!(par_all(&par(a)), par_all(&par(b)));
    };
    // c07: R = 1 is the box's ellipse, θ = 2π t.
    same(
        &[
            on("c07_polar_equation_mode"),
            txt("c07_polar_equation_mode.r", "1"),
        ],
        &[
            on("c01_x_t_formula"),
            txt("c01_x_t_formula.x", "0.5 + 0.5*1*cos(2*pi*t)"),
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", "0.5 + 0.5*1*sin(2*pi*t)"),
        ],
    );
    // c08: y = F(x) with x = t.
    same(
        &[
            on("c08_cartesian_mode"),
            txt("c08_cartesian_mode.f", "0.5 + 0.4*sin(2*pi*x)"),
        ],
        &[
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", "0.5 + 0.4*sin(2*pi*t)"),
        ],
    );
    // c10: a = 5 in the base's Y(t).
    same(
        &[on("c10_user_constants_variables")],
        &[
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", "1 - pow(1 - (5*t - floor(5*t)), 2)"),
        ],
    );
    // c11 (from the start), c13: x scaled, both moved.
    same(
        &[on("c11_time_scale")],
        &[
            on("c01_x_t_formula"),
            txt("c01_x_t_formula.x", "0 + (t - 0)*0.5"),
        ],
    );
    same(
        &[on("c13_x_y_offset")],
        &[
            on("c01_x_t_formula"),
            txt("c01_x_t_formula.x", "t + 0.1"),
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", &format!("{PAR_Y} + 0.1")),
        ],
    );
    // c12: the rows stay within the scaled range about the middle row.
    let rows: BTreeSet<u8> = par(&[on("c12_pitch_scale")])
        .iter()
        .map(|n| n.pitch)
        .collect();
    assert_eq!(
        (*rows.first().unwrap(), *rows.last().unwrap()),
        (
            (50.5f64 + 19.0 * 0.25).floor() as u8,
            (50.5f64 + 19.0 * 0.7499).floor() as u8
        )
    );
    // c03 / c04 on a straight line: the part of t's range.
    let line = [
        on("c02_y_t_formula"),
        txt("c02_y_t_formula.y", "0.25 + 0.5*t"),
    ];
    let mut part = line.to_vec();
    part.extend([on("c03_t_start"), on("c04_t_end")]);
    let (whole, cut) = (par_cells(&par(&line)), par_cells(&par(&part)));
    assert!(cut.iter().all(|&(c, _)| (13..=34).contains(&c)), "{cut:?}");
    assert!(
        cut.iter()
            .filter(|&&(c, _)| (15..=32).contains(&c))
            .all(|x| whole.contains(x))
    );
    // c25: the library's Sine is its formulas; names are matched in any
    // case; an unknown name or a bad formula is refused with a sentence.
    same(
        &[
            on("c25_saved_equation_presets_expression_validation"),
            txt(
                "c25_saved_equation_presets_expression_validation.name",
                "sine",
            ),
        ],
        &[
            on("c01_x_t_formula"),
            txt("c01_x_t_formula.x", "t"),
            on("c02_y_t_formula"),
            txt("c02_y_t_formula.y", "0.5 + 0.4*sin(2*pi*t)"),
        ],
    );
    match par_try(&[
        on("c25_saved_equation_presets_expression_validation"),
        txt(
            "c25_saved_equation_presets_expression_validation.name",
            "Moon",
        ),
    ]) {
        Err(TransformError::Invalid(m)) => {
            assert!(m.contains("Lissajous, Rose") && m.ends_with('.'), "{m}")
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    match par_try(&[
        on("c02_y_t_formula"),
        txt("c02_y_t_formula.y", "0.5 + sin("),
    ]) {
        Err(TransformError::Invalid(m)) => {
            assert!(m.starts_with("Y(t):") && m.contains("character"), "{m}")
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
}

#[test]
fn par_samples_adaptive_and_breaks() {
    // c05: fewer samples, the same curve's chords — every sample's cell.
    let few = par_cells(&par(&[on("c05_sample_count")]));
    let y = |t: f64| 1.0 - (1.0 - (3.0 * t - (3.0 * t).floor())).powi(2);
    for k in 0..24 {
        let t = k as f64 / 23.0;
        let (u, v) = (4.0 + 40.0 * t - 1e-6 * t, 50.5 + 19.0 * y(t));
        assert!(near_cell(&few, u.floor() as i64, v.floor() as i64), "{k}");
    }
    assert_ne!(few, par_cells(&par(&[])));
    // c06: from 16 steps, halved only near the drops (where the chord
    // leaves the curve): fewer notes on the teeth, the drops still drawn.
    let ad = par_cells(&par(&[on("c06_adaptive_sampling")]));
    assert_ne!(ad, par_cells(&par(&[])));
    assert!((53..=66).all(|r| ad.contains(&(17, r))));
    // c20: the drops are not joined; a formula without a value refuses
    // the run, or leaves a gap with c20.
    let base = par_cells(&par(&[]));
    let cut = par_cells(&par(&[on("c20_discontinuity_detection")]));
    assert!(cut.is_subset(&base) && cut.len() < base.len());
    assert!(cut.iter().filter(|&&(c, _)| c == 17).count() <= 4);
    let sq = [
        on("c02_y_t_formula"),
        txt("c02_y_t_formula.y", "sqrt(t - 0.5)"),
    ];
    match par_try(&sq) {
        Err(TransformError::Invalid(m)) => {
            assert!(m.starts_with("Y(t):") && m.contains("t = "), "{m}")
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    let mut gap = sq.to_vec();
    gap.push(on("c20_discontinuity_detection"));
    assert!(par(&gap).iter().all(|n| n.start >= 240 * 23));
}

#[test]
fn par_windows_wraps_and_rotation_keep_the_curve_where_asked() {
    // c21: the window 20 – 80 % of the box: u 12 – 36, v 54.3 – 65.7.
    let w = par_cells(&par(&[on("c21_domain_clipping")]));
    assert!(!w.is_empty());
    assert!(
        w.iter()
            .all(|&(c, r)| (11..=36).contains(&c) && (54..=65).contains(&r)),
        "{w:?}"
    );
    // c22: twice the size, wrapped or reflected: inside the box.
    for mode in [0, 1] {
        let v = par(&[
            on("c22_wrap_reflect_boundaries"),
            ch("c22_wrap_reflect_boundaries.mode", mode),
        ]);
        assert!(
            v.iter().all(|n| (50..=69).contains(&n.pitch)
                && n.start >= 960
                && n.end() <= 960 + 240 * 41)
        );
        assert_ne!(par_cells(&v), par_cells(&par(&[])));
    }
    // c14: a half turn about the box's centre (24, 60) is the base turned
    // round: cell (c, r) of one near cell (47 − c, 119 − r) of the other.
    let r = par_cells(&par(&[
        on("c14_rotation"),
        num("c14_rotation.angle", 180.0),
    ]));
    let base = par_cells(&par(&[]));
    assert_ne!(r, base);
    for &(c, row) in &r {
        assert!(near_cell(&base, 47 - c, 119 - i64::from(row)), "{c} {row}");
    }
}

#[test]
fn par_layers_morph_noise_and_densities() {
    // c09: layer 2 (subchannel 1) is its own curve.
    let v = par(&[on("c09_multiple_equation_layers")]);
    let l2: Vec<TNote> = v
        .iter()
        .filter(|n| n.sub == 1)
        .map(|n| TNote { sub: 0, ..*n })
        .collect();
    let alone = par(&[
        on("c02_y_t_formula"),
        txt("c02_y_t_formula.y", "pow(1 - (a*t - floor(a*t)), 2)"),
    ]);
    assert_eq!(par_all(&l2), par_all(&alone));
    assert_eq!(
        par_all(&v.iter().filter(|n| n.sub == 0).copied().collect::<Vec<_>>()),
        par_all(&par(&[]))
    );
    // c23: no morph is curve A; three curves contain A.
    assert_eq!(
        par_all(&par(&[
            on("c23_formula_a_b_morph"),
            num("c23_formula_a_b_morph.amount", 0.0)
        ])),
        par_all(&par(&[]))
    );
    let three = par_cells(&par(&[
        on("c23_formula_a_b_morph"),
        int("c23_formula_a_b_morph.steps", 3),
    ]));
    assert!(par_cells(&par(&[])).is_subset(&three));
    // c24: seeded — the same seed the same notes, another seed others; no
    // amount and no use of noise is the base.
    let n1 = par(&[on("c24_deterministic_noise_functions"), int("gen.seed", 1)]);
    assert_eq!(
        par_all(&n1),
        par_all(&par(&[
            on("c24_deterministic_noise_functions"),
            int("gen.seed", 1)
        ]))
    );
    assert_ne!(
        par_all(&n1),
        par_all(&par(&[
            on("c24_deterministic_noise_functions"),
            int("gen.seed", 2)
        ]))
    );
    assert_eq!(
        par_all(&par(&[
            on("c24_deterministic_noise_functions"),
            num("c24_deterministic_noise_functions.amount", 0.0),
        ])),
        par_all(&par(&[]))
    );
    // c15 / c16: one-cell notes every grid unit, a share of them.
    for id in [
        "c15_derivative_based_density",
        "c16_curvature_based_density",
    ] {
        let d = par(&[on(id)]);
        assert!(!d.is_empty() && d.iter().all(|n| n.dur <= 240), "{id}");
        assert!(par_cells(&d).len() < par_cells(&par(&[])).len(), "{id}");
    }
    // c15: the drops (where t races) keep fewer notes than a tooth's slow
    // start.
    let d = par_cells(&par(&[on("c15_derivative_based_density")]));
    assert!(d.iter().filter(|&&(c, _)| c == 17).count() <= 3);
}

#[test]
fn par_brush_formulas_set_velocity_gate_and_subchannel() {
    // c17: 40 + 87 y at each sample (y of the drawn point, box shares).
    let v = par(&[on("c17_velocity_formula")]);
    for n in &v {
        let y = (f64::from(n.pitch) + 0.5 - 50.5) / 19.0;
        let want = 40.0 + 87.0 * y;
        assert!((f64::from(n.vel) - want).abs() <= 8.0, "{n:?} {want}");
    }
    // c18: 100 % at a piece's start, 20 % at its end.
    let g = par(&[on("c18_gate_formula"), on("c20_discontinuity_detection")]);
    assert!(g.iter().any(|n| n.dur >= 230) && g.iter().any(|n| n.dur <= 100));
    // c19: floor(4 s): subchannels 0 – 3 along each piece.
    let s = par(&[on("c19_subchannel_formula")]);
    let subs: BTreeSet<u32> = s.iter().map(|n| n.sub).collect();
    assert_eq!(subs, [0, 1, 2, 3].into_iter().collect());
    // The same curve: a varying brush draws the stroke in unit pieces, each
    // rounded on its own (the pen), so cells may move to a neighbour only.
    let (sc, bc) = (par_cells(&s), par_cells(&par(&[])));
    assert!(sc.iter().all(|&(c, r)| near_cell(&bc, c, i64::from(r))));
    assert!(bc.iter().all(|&(c, r)| near_cell(&sc, c, i64::from(r))));
    assert!(sc.len().abs_diff(bc.len()) <= bc.len() / 20);
}

#[test]
fn par_caps_refuse_huge_settings_with_sentences() {
    let long = "sin(t)+".repeat(60) + "t";
    match par_try(&[
        on("c05_sample_count"),
        int("c05_sample_count.n", 16_384),
        on("c09_multiple_equation_layers"),
        int("c09_multiple_equation_layers.count", 2),
        on("c23_formula_a_b_morph"),
        int("c23_formula_a_b_morph.steps", 16),
        on("c17_velocity_formula"),
        txt("c17_velocity_formula.v", &long),
    ]) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains("steps") && m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
}

/// Shares near 0 (c15 at 200 %) over a box 2³⁶ grid units wide: every
/// sample would be visited to keep almost none — refused at once with a
/// sentence (a harness draw like it ran for more than 12 minutes).
#[test]
fn par_sparse_shares_over_a_huge_box_are_refused_before_visiting_the_samples() {
    let t = std::time::Instant::now();
    match par_try(&[
        ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
        ("gen.step", ParamValue::Choice(11)),
        ("gen.step_ticks", ParamValue::Int(1)),
        on("c15_derivative_based_density"),
        num("c15_derivative_based_density.strength", 200.0),
    ]) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains("visit") && m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

/// A velocity formula (a brush changing along the curve) on two layers of
/// 16,384 samples over a box 2³⁶ grid units wide: every segment would be
/// cut into up to 1,024 pieces —
/// refused at once with a sentence (V09-S60's piece cap; a Spirograph
/// harness draw like it held 2 GB and then counted for 277 s).
#[test]
fn par_a_brush_formula_over_a_huge_box_is_refused_before_cutting_the_curve() {
    let t = std::time::Instant::now();
    match par_try(&[
        ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
        ("gen.step", ParamValue::Choice(11)),
        ("gen.step_ticks", ParamValue::Int(1)),
        on("c05_sample_count"),
        ("c05_sample_count.n", ParamValue::Int(16_384)),
        on("c09_multiple_equation_layers"),
        on("c17_velocity_formula"),
    ]) {
        Err(TransformError::TooLarge(m)) => {
            assert!(m.contains("pieces") && m.ends_with('.'), "{m}")
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    // The same formula on the default box draws.
    assert!(!par_try(&[on("c17_velocity_formula")]).unwrap().is_empty());
}

#[test]
fn par_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = par_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(a.iter().all(|n| valid(n, keys)));
    }
}

// ----- Fractal / L-System (V09-S59)

fn lsys_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.lsystem")
        .unwrap()
}

/// Runs the L-system on the 40-step × 20-row test box (rows 50 – 69, from
/// tick 960): u = 4 + 40 x less 10⁻⁶, v = 50.5 + 19 y for box shares x, y.
fn lsys_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = lsys_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(50)),
            ("gen.high", ParamValue::Int(69)),
            ("gen.length", ParamValue::Int(240 * 40)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn lsys(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    lsys_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

const LSYS_X: &str = "F-[[X]+X]+F[+FX]-X";

/// An independent L-system: plain string rewriting of (symbol, generation)
/// pairs, then a turtle heading along time (`deg` per turn). `k` scales a
/// branch's step per level; `letters` draw with their step; `gen` scales a
/// step's time / pitch extent per generation. Segments with their class
/// (0: F / G, 1: a letter).
struct Oracle<'a> {
    axiom: &'a str,
    rules: Vec<(char, &'a str)>,
    n: u32,
    deg: f64,
    k: f64,
    letters: &'a str,
    letter_len: f64,
    gscale: (f64, f64),
    /// Only the segments at this branch depth (bracket nesting).
    only: Option<u32>,
}

type Seg = ((f64, f64), (f64, f64), u32);

impl<'a> Oracle<'a> {
    fn base() -> Oracle<'a> {
        Oracle {
            axiom: "X",
            rules: vec![('X', LSYS_X), ('F', "FF")],
            n: 5,
            deg: 22.5,
            k: 1.0,
            letters: "",
            letter_len: 1.0,
            gscale: (1.0, 1.0),
            only: None,
        }
    }

    fn segs(&self) -> Vec<Seg> {
        let mut s: Vec<(char, u32)> = self.axiom.chars().map(|c| (c, 0)).collect();
        for g in 1..=self.n {
            let mut t = Vec::new();
            for (c, o) in s {
                match self.rules.iter().find(|r| r.0 == c) {
                    Some(r) => t.extend(r.1.chars().map(|d| (d, if d == c { o } else { g }))),
                    None => t.push((c, o)),
                }
            }
            s = t;
        }
        let a = self.deg.to_radians();
        let (mut x, mut y, mut h, mut l) = (0.0f64, 0.0f64, 0.0f64, 1.0f64);
        let mut depth = 0u32;
        let mut stack = Vec::new();
        let mut out = Vec::new();
        for (c, o) in s {
            let class = if c == 'F' || c == 'G' {
                Some(0)
            } else if self.letters.contains(c) {
                Some(1)
            } else {
                None
            };
            match c {
                '+' => h += a,
                '-' => h -= a,
                '|' => h += std::f64::consts::PI,
                '[' => {
                    stack.push((x, y, h, l));
                    l *= self.k;
                    depth += 1;
                }
                ']' => {
                    if let Some(t) = stack.pop() {
                        (x, y, h, l) = t;
                        depth -= 1;
                    }
                }
                'f' | 'g' => {
                    x += l * h.cos();
                    y += l * h.sin();
                }
                _ => {
                    if let Some(cl) = class {
                        let len = if cl == 1 { l * self.letter_len } else { l };
                        let e = o as i32;
                        let (nx, ny) = (
                            x + len * h.cos() * self.gscale.0.powi(e),
                            y + len * h.sin() * self.gscale.1.powi(e),
                        );
                        if self.only.is_none_or(|d| d == depth) {
                            out.push(((x, y), (nx, ny), cl));
                        }
                        (x, y) = (nx, ny);
                    }
                }
            }
        }
        out
    }
}

/// The segments' extent (min x, min y, max x, max y).
fn lsys_extent(segs: &[Seg]) -> [f64; 4] {
    let mut e = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(a, b, _) in segs {
        for p in [a, b] {
            e = [e[0].min(p.0), e[1].min(p.1), e[2].max(p.0), e[3].max(p.1)];
        }
    }
    e
}

/// Grid points every ¹⁄₈ unit along the segments placed by `map` (turtle
/// point → box shares), those inside the box only.
fn lsys_points(segs: &[Seg], map: impl Fn((f64, f64)) -> (f64, f64)) -> Vec<(f64, f64)> {
    let grid = |p: (f64, f64)| (4.0 + 40.0 * p.0 - 1e-6 * p.0, 50.5 + 19.0 * p.1);
    let mut out = Vec::new();
    for &(a, b, _) in segs {
        let (ga, gb) = (grid(map(a)), grid(map(b)));
        let n = ((gb.0 - ga.0).hypot(gb.1 - ga.1) * 8.0).ceil().max(1.0) as usize;
        for i in 0..=n {
            let t = i as f64 / n as f64;
            let p = (ga.0 + (gb.0 - ga.0) * t, ga.1 + (gb.1 - ga.1) * t);
            if p.0 >= 4.0 && p.0 < 44.0 && p.1 >= 50.0 && p.1 < 70.0 {
                out.push(p);
            }
        }
    }
    out
}

/// Stretched over the box (the base's placement).
fn lsys_stretch(segs: &[Seg]) -> Vec<(f64, f64)> {
    let e = lsys_extent(segs);
    let (w, h) = (e[2] - e[0], e[3] - e[1]);
    lsys_points(segs, |p| {
        (
            if w > 0.0 { (p.0 - e[0]) / w } else { 0.5 },
            if h > 0.0 { (p.1 - e[1]) / h } else { 0.5 },
        )
    })
}

/// Every oracle point lies in or next to a drawn cell, and every drawn cell
/// next to an oracle point.
fn lsys_matches(notes: &[TNote], pts: &[(f64, f64)], what: &str) {
    assert!(!notes.is_empty(), "{what}: no notes");
    let cells = par_cells(notes);
    for &(u, v) in pts {
        assert!(
            near_cell(&cells, u.floor() as i64, v.floor() as i64),
            "{what}: ({u}, {v}) not drawn"
        );
    }
    let pc: BTreeSet<(i64, u8)> = pts
        .iter()
        .map(|&(u, v)| (u.floor() as i64, v.floor() as u8))
        .collect();
    for &(c, r) in &cells {
        assert!(near_cell(&pc, c, i64::from(r)), "{what}: ({c}, {r}) drawn");
    }
}

#[test]
fn lsys_the_base_is_the_fractal_plant_stretched_over_the_box() {
    let base = lsys(&[]);
    assert!(base.iter().all(|n| (n.vel, n.sub) == (100, 0)));
    lsys_matches(&base, &lsys_stretch(&Oracle::base().segs()), "base");
    // It fills the box: its first and last steps, lowest and highest rows.
    let rows: BTreeSet<u8> = base.iter().map(|n| n.pitch).collect();
    assert_eq!((*rows.first().unwrap(), *rows.last().unwrap()), (50, 69));
    assert_eq!(base.iter().map(|n| n.start).min(), Some(960));
    assert!(base.iter().any(|n| n.end() == 960 + 9600));
    // The base's own settings given again draw the same notes.
    assert_eq!(
        par_all(&lsys(&[
            on("c01_axiom"),
            txt("c01_axiom.axiom", "X"),
            on("c02_rewrite_rules"),
            txt("c02_rewrite_rules.rules", "X=F-[[X]+X]+F[+FX]-X; F=FF"),
            on("c03_iteration_depth"),
            int("c03_iteration_depth.depth", 5),
            on("c05_turn_angle"),
            num("c05_turn_angle.angle", 22.5),
        ])),
        par_all(&base)
    );
}

#[test]
fn lsys_axiom_rules_depth_and_angle_rewrite_the_string() {
    // A straight line: F → FF three times, stretched: row 60 across the box.
    let line = lsys(&[
        on("c01_axiom"),
        txt("c01_axiom.axiom", "F"),
        on("c02_rewrite_rules"),
        txt("c02_rewrite_rules.rules", "F=FF"),
        on("c03_iteration_depth"),
        int("c03_iteration_depth.depth", 3),
    ]);
    let want: BTreeSet<(i64, u8)> = (4..44).map(|c| (c, 60)).collect();
    assert_eq!(par_cells(&line), want);
    // Each of c01, c02, c03, c05 alone: the oracle with that change.
    let cases: Vec<(Vec<(&'static str, ParamValue)>, Oracle)> = vec![
        (
            vec![on("c01_axiom")],
            Oracle {
                axiom: "[-X]X[+X]",
                ..Oracle::base()
            },
        ),
        (
            vec![on("c02_rewrite_rules")],
            Oracle {
                rules: vec![('X', "F[+X]F[-X]+X"), ('F', "FF")],
                ..Oracle::base()
            },
        ),
        (
            vec![on("c03_iteration_depth")],
            Oracle {
                n: 6,
                ..Oracle::base()
            },
        ),
        (
            vec![on("c05_turn_angle")],
            Oracle {
                deg: 30.0,
                ..Oracle::base()
            },
        ),
    ];
    for (kv, o) in &cases {
        lsys_matches(&lsys(kv), &lsys_stretch(&o.segs()), kv[0].0);
    }
    // No rewrite: the axiom X draws nothing.
    assert!(
        lsys(&[
            on("c03_iteration_depth"),
            int("c03_iteration_depth.depth", 0)
        ])
        .is_empty()
    );
}

#[test]
fn lsys_branches_letters_and_generations_scale_the_steps() {
    // c06: a step per branch level of 100 % and no turn is the base.
    assert_eq!(
        par_all(&lsys(&[
            on("c06_branch_push_pop"),
            num("c06_branch_push_pop.length", 100.0),
        ])),
        par_all(&lsys(&[]))
    );
    let o = Oracle {
        k: 0.7,
        ..Oracle::base()
    };
    lsys_matches(
        &lsys(&[on("c06_branch_push_pop")]),
        &lsys_stretch(&o.segs()),
        "c06",
    );
    // c07: X draws half a step on subchannel 1.
    let v = lsys(&[on("c07_multiple_symbols")]);
    let o = Oracle {
        letters: "X",
        letter_len: 0.5,
        ..Oracle::base()
    };
    lsys_matches(&v, &lsys_stretch(&o.segs()), "c07");
    assert!(v.iter().any(|n| n.sub == 1) && v.iter().any(|n| n.sub == 0));
    // c10 / c11: each step's time / pitch extent × 75 % per generation.
    for (id, k, g) in [
        (
            "c10_time_scale_per_generation",
            "c10_time_scale_per_generation.k",
            (0.75, 1.0),
        ),
        (
            "c11_pitch_scale_per_generation",
            "c11_pitch_scale_per_generation.k",
            (1.0, 0.75),
        ),
    ] {
        let o = Oracle {
            gscale: g,
            ..Oracle::base()
        };
        lsys_matches(&lsys(&[on(id)]), &lsys_stretch(&o.segs()), id);
        assert_eq!(
            par_all(&lsys(&[on(id), num(k, 100.0)])),
            par_all(&lsys(&[]))
        );
    }
}

#[test]
fn lsys_stochastic_rules_and_weights_choose_by_the_seed() {
    let base = par_all(&lsys(&[]));
    // c08: the same rule twice is the base; the default alternative is
    // seeded — the same notes for a seed, others for another.
    assert_eq!(
        par_all(&lsys(&[
            on("c08_stochastic_rules_with_seed"),
            txt(
                "c08_stochastic_rules_with_seed.rules",
                &format!("X={LSYS_X}")
            ),
        ])),
        base
    );
    let st = |seed: i64| {
        par_all(&lsys(&[
            on("c08_stochastic_rules_with_seed"),
            int("gen.seed", seed),
        ]))
    };
    assert_eq!(st(1), st(1));
    assert_ne!(st(1), st(2));
    assert_ne!(st(1), base);
    // c09: weights 100 / 100 are the base; F's rule at 0 never fires (F
    // stays: the X rule alone); X's at 0 leaves the axiom (nothing drawn).
    let w = |a: i64, b: i64| {
        lsys(&[
            on("c09_rule_weights"),
            ("c09_rule_weights.weights", ParamValue::List(vec![a, b])),
        ])
    };
    assert_eq!(par_all(&w(100, 100)), base);
    assert_eq!(
        par_all(&w(100, 0)),
        par_all(&lsys(&[
            on("c02_rewrite_rules"),
            txt("c02_rewrite_rules.rules", &format!("X={LSYS_X}")),
        ]))
    );
    assert!(w(0, 100).is_empty());
    let half = par_all(&w(100, 50));
    assert!(half != base && half != par_all(&w(100, 0)));
}

#[test]
fn lsys_depth_brushes_thickness_and_density_follow_the_branch_depth() {
    let base = lsys(&[]);
    // c14: 120 at the trunk, 20 less per depth.
    let v = lsys(&[on("c14_velocity_per_depth")]);
    let vels: BTreeSet<u8> = v.iter().map(|n| n.vel).collect();
    assert!(vels.contains(&120) && vels.contains(&100) && vels.contains(&80));
    assert!(
        vels.iter()
            .all(|&x| x == 1 || (120 - u32::from(x)) % 20 == 0)
    );
    // c15: 100 % at the trunk, 60 % of that per depth (whole percent).
    let g = lsys(&[on("c15_gate_per_depth")]);
    assert!(g.iter().any(|n| n.dur % 240 == 0) && g.iter().any(|n| n.dur % 240 != 0));
    assert!(g.iter().any(|n| n.dur == 144) || g.iter().any(|n| n.dur % 240 == 192));
    // c16: the depth as the subchannel position.
    let s = lsys(&[on("c16_subchannel_per_depth")]);
    let subs: BTreeSet<u32> = s.iter().map(|n| n.sub).collect();
    assert!((0..=3).all(|d| subs.contains(&d)));
    // The brushes leave the cells: same cells as the base, or neighbours.
    for v in [&v, &g, &s] {
        let (a, b) = (par_cells(v), par_cells(&base));
        assert!(a.iter().all(|&(c, r)| near_cell(&b, c, i64::from(r))));
    }
    // c12: one row at 100 % is the base; 3 rows at the trunk widen it.
    assert_eq!(
        par_all(&lsys(&[
            on("c12_thickness_per_depth"),
            num("c12_thickness_per_depth.rows", 1.0),
            num("c12_thickness_per_depth.factor", 100.0),
        ])),
        par_all(&base)
    );
    assert!(par_cells(&lsys(&[on("c12_thickness_per_depth")])).len() > par_cells(&base).len());
    // c13: none kept past the trunk draws the trunk alone (placed with the
    // whole plant); 100 % keeps a note every unit.
    let e = lsys_extent(&Oracle::base().segs());
    let trunk = Oracle {
        only: Some(0),
        ..Oracle::base()
    }
    .segs();
    let pts = lsys_points(&trunk, |p| {
        ((p.0 - e[0]) / (e[2] - e[0]), (p.1 - e[1]) / (e[3] - e[1]))
    });
    let d0 = lsys(&[
        on("c13_density_per_depth"),
        num("c13_density_per_depth.factor", 0.0),
    ]);
    lsys_matches(&d0, &pts, "c13 trunk");
    let all = lsys(&[
        on("c13_density_per_depth"),
        num("c13_density_per_depth.factor", 100.0),
    ]);
    assert!(all.iter().all(|n| n.dur <= 240));
    let (a, b) = (par_cells(&all), par_cells(&base));
    assert!(a.iter().all(|&(c, r)| near_cell(&b, c, i64::from(r))));
    assert!(par_cells(&lsys(&[on("c13_density_per_depth")])).len() < a.len());
}

#[test]
fn lsys_pruning_tropism_and_noise_reshape_the_growth() {
    // c17: every branch dropped is the trunk alone — the rules without
    // their bracketed parts.
    let o = Oracle {
        rules: vec![('X', "F-+F-X"), ('F', "FF")],
        ..Oracle::base()
    };
    for kv in [
        vec![
            on("c17_branch_pruning"),
            int("c17_branch_pruning.depth", 0),
            num("c17_branch_pruning.share", 0.0),
        ],
        vec![
            on("c17_branch_pruning"),
            int("c17_branch_pruning.depth", 64),
            num("c17_branch_pruning.share", 100.0),
        ],
    ] {
        lsys_matches(&lsys(&kv), &lsys_stretch(&o.segs()), "c17");
    }
    let base = par_all(&lsys(&[]));
    // c18 / c19 at 0 are the base; on, they change it; c19 is seeded.
    assert_eq!(
        par_all(&lsys(&[
            on("c18_tropism_direction_bias"),
            num("c18_tropism_direction_bias.strength", 0.0),
        ])),
        base
    );
    assert_ne!(par_all(&lsys(&[on("c18_tropism_direction_bias")])), base);
    assert_eq!(
        par_all(&lsys(&[
            on("c19_growth_noise"),
            num("c19_growth_noise.angle", 0.0),
            num("c19_growth_noise.length", 0.0),
        ])),
        base
    );
    let nz = |seed: i64| par_all(&lsys(&[on("c19_growth_noise"), int("gen.seed", seed)]));
    assert_eq!(nz(3), nz(3));
    assert_ne!(nz(3), nz(4));
}

#[test]
fn lsys_fit_step_mirror_and_wrap_place_the_drawing() {
    let segs = Oracle::base().segs();
    let e = lsys_extent(&segs);
    let (w, h) = (e[2] - e[0], e[3] - e[1]);
    let (cx, cy) = ((e[0] + e[2]) / 2.0, (e[1] + e[3]) / 2.0);
    // c20: proportions kept, 5 % margins, centred (40 × 19 grid units).
    let s = (0.9 * 40.0 / w).min(0.9 * 19.0 / h);
    let pts = lsys_points(&segs, |p| {
        (
            0.5 + s * (p.0 - cx) / (40.0 - 1e-6),
            0.5 + s * (p.1 - cy) / 19.0,
        )
    });
    lsys_matches(&lsys(&[on("c20_bounding_box_fit")]), &pts, "c20");
    // c04: a quarter grid unit per step from the box's start, middle row,
    // cut at the box.
    let pts = lsys_points(&segs, |p| {
        (0.25 * p.0 / (40.0 - 1e-6), 0.5 + 0.25 * p.1 / 19.0)
    });
    lsys_matches(&lsys(&[on("c04_turtle_step_length")]), &pts, "c04");
    // c21: the pitch mirror about the start, stretched: symmetric rows.
    let m = par_cells(&lsys(&[on("c21_mirror_symmetry")]));
    assert!(m.iter().all(|&(c, r)| near_cell(&m, c, i64::from(119 - r))));
    // Two radial copies: symmetric about the box's centre.
    let r = par_cells(&lsys(&[
        on("c21_mirror_symmetry"),
        ch("c21_mirror_symmetry.axis", 0),
        int("c21_mirror_symmetry.copies", 2),
    ]));
    assert!(
        r.iter()
            .all(|&(c, rw)| near_cell(&r, 47 - c, i64::from(119 - rw)))
    );
    // c22: an extent of 1 is the base; 2 folds everything into the box.
    for mode in [0, 1] {
        assert_eq!(
            par_all(&lsys(&[
                on("c22_wrap_reflect"),
                ch("c22_wrap_reflect.mode", mode),
                num("c22_wrap_reflect.extent", 1.0),
            ])),
            par_all(&lsys(&[]))
        );
        let f = lsys(&[on("c22_wrap_reflect"), ch("c22_wrap_reflect.mode", mode)]);
        assert!(
            f.iter()
                .all(|n| (50..=69).contains(&n.pitch) && n.start >= 960)
        );
        assert_ne!(par_all(&f), par_all(&lsys(&[])));
    }
}

#[test]
fn lsys_joins_forests_and_generations_add_to_the_drawing() {
    let base = par_cells(&lsys(&[]));
    // c23: joins only add pieces between tips (inside the drawing's box).
    for mode in [0, 1] {
        let j = par_cells(&lsys(&[
            on("c23_branch_join"),
            ch("c23_branch_join.mode", mode),
        ]));
        assert!(base.is_subset(&j) && j.len() > base.len(), "{mode}");
    }
    // c24: two plants from X side by side without a gap.
    let segs = Oracle::base().segs();
    let e = lsys_extent(&segs);
    let w = e[2] - e[0];
    let two: Vec<Seg> = segs
        .iter()
        .flat_map(|&(a, b, c)| [(a, b, c), ((a.0 + w, a.1), (b.0 + w, b.1), c)])
        .collect();
    let f = lsys(&[
        on("c24_multi_axiom_forest"),
        int("c24_multi_axiom_forest.count", 2),
        txt("c24_multi_axiom_forest.axioms", "X; X"),
        num("c24_multi_axiom_forest.gap", 0.0),
    ]);
    lsys_matches(&f, &lsys_stretch(&two), "c24");
    // c25 evolution: generations 1 and 2 in turn, 10 % of the wider apart.
    let g1 = Oracle {
        n: 1,
        ..Oracle::base()
    }
    .segs();
    let g2 = Oracle {
        n: 2,
        ..Oracle::base()
    }
    .segs();
    let (e1, e2) = (lsys_extent(&g1), lsys_extent(&g2));
    let gap = 0.1 * (e1[2] - e1[0]).max(e2[2] - e2[0]);
    let dx2 = (e1[2] - e1[0]) + gap - e2[0];
    let mut both: Vec<Seg> = g1
        .iter()
        .map(|&(a, b, c)| ((a.0 - e1[0], a.1), (b.0 - e1[0], b.1), c))
        .collect();
    both.extend(
        g2.iter()
            .map(|&(a, b, c)| ((a.0 + dx2, a.1), (b.0 + dx2, b.1), c)),
    );
    let ev = lsys(&[
        on("c03_iteration_depth"),
        int("c03_iteration_depth.depth", 2),
        on("c25_rule_set_morph_evolution"),
        ch("c25_rule_set_morph_evolution.mode", 1),
    ]);
    lsys_matches(&ev, &lsys_stretch(&both), "c25 evolution");
    // c25 morph: nothing taken from B, or B the base's rules, is the base.
    let all = par_all(&lsys(&[]));
    assert_eq!(
        par_all(&lsys(&[
            on("c25_rule_set_morph_evolution"),
            num("c25_rule_set_morph_evolution.amount", 0.0),
        ])),
        all
    );
    assert_eq!(
        par_all(&lsys(&[
            on("c25_rule_set_morph_evolution"),
            txt(
                "c25_rule_set_morph_evolution.rules",
                "X=F-[[X]+X]+F[+FX]-X; F=FF"
            ),
        ])),
        all
    );
    assert_ne!(par_all(&lsys(&[on("c25_rule_set_morph_evolution")])), all);
}

#[test]
fn lsys_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], has: &str| match lsys_try(kv) {
        Err(TransformError::TooLarge(m)) | Err(TransformError::Invalid(m)) => {
            assert!(m.contains(has) && m.ends_with('.'), "{m}")
        }
        other => panic!("{has}: {:?}", other.map(|v| v.len())),
    };
    let f = |rules: &str, depth: i64| {
        vec![
            on("c01_axiom"),
            txt("c01_axiom.axiom", "F"),
            on("c02_rewrite_rules"),
            txt("c02_rewrite_rules.rules", rules),
            on("c03_iteration_depth"),
            int("c03_iteration_depth.depth", depth),
        ]
    };
    // 4¹² symbols; 3¹² segments × 4 mirror copies.
    refused(&f("F=FFFF", 12), "symbols");
    let mut m = f("F=FFF", 12);
    m.extend([on("c21_mirror_symmetry"), ch("c21_mirror_symmetry.axis", 3)]);
    refused(&m, "segments");
    // Wrapping a huge drawing: too many points.
    refused(
        &[
            on("c03_iteration_depth"),
            int("c03_iteration_depth.depth", 8),
            on("c04_turtle_step_length"),
            num("c04_turtle_step_length.step", 64.0),
            on("c22_wrap_reflect"),
            num("c22_wrap_reflect.extent", 16.0),
        ],
        "points",
    );
    // Malformed rules and letters.
    refused(
        &[
            on("c02_rewrite_rules"),
            txt("c02_rewrite_rules.rules", "X=F["),
        ],
        "open",
    );
    refused(
        &[
            on("c02_rewrite_rules"),
            txt("c02_rewrite_rules.rules", "XY=F"),
        ],
        "one symbol",
    );
    refused(
        &[
            on("c07_multiple_symbols"),
            txt("c07_multiple_symbols.letters", "F"),
        ],
        "F",
    );
}

/// Shares near 0 over a box 2³⁶ grid units wide: refused at once with a
/// sentence instead of visiting every sample.
#[test]
fn lsys_sparse_density_over_a_huge_box_is_refused_before_visiting_the_samples() {
    let t = std::time::Instant::now();
    match lsys_try(&[
        ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
        ("gen.step", ParamValue::Choice(11)),
        ("gen.step_ticks", ParamValue::Int(1)),
        on("c13_density_per_depth"),
        num("c13_density_per_depth.factor", 1.0),
    ]) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains("visit") && m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

#[test]
fn lsys_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = lsys_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.iter().all(|n| valid(n, keys)));
    }
}

// ----- Harmonic / Spirograph (V09-S60)

const TAU: f64 = std::f64::consts::TAU;

fn spiro_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.spirograph")
        .unwrap()
}

/// Runs the spirograph on the 40-step × 20-row test box (see `lsys_try`).
fn spiro_try(kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = spiro_def();
    let d = doc(KeyRange::Standard, &[]);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(50)),
            ("gen.high", ParamValue::Int(69)),
            ("gen.length", ParamValue::Int(240 * 40)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn spiro(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    spiro_try(kv).unwrap_or_else(|e| panic!("{e}"))
}

/// `n` + 1 points of curve `f` over `t` from 0 to `t1`, as segments.
fn spiro_curve(f: impl Fn(f64) -> (f64, f64), t1: f64, n: usize) -> Vec<Seg> {
    let p: Vec<(f64, f64)> = (0..=n).map(|i| f(t1 * i as f64 / n as f64)).collect();
    p.windows(2).map(|w| (w[0], w[1], 0)).collect()
}

/// The base: r = 3 rolling along time, pen at 5, five turns.
fn spiro_line(r: f64, d: f64, turns: f64, ph: f64) -> Vec<Seg> {
    spiro_curve(
        |t| (r * t - d * (t + ph).sin(), r - d * (t + ph).cos()),
        TAU * turns,
        4000,
    )
}

/// Turned by `a` about the origin.
fn spiro_turn(segs: &[Seg], a: f64) -> Vec<Seg> {
    let (s, c) = a.sin_cos();
    let t = |p: (f64, f64)| (p.0 * c - p.1 * s, p.0 * s + p.1 * c);
    segs.iter().map(|&(a, b, k)| (t(a), t(b), k)).collect()
}

#[test]
fn spiro_the_base_is_a_circle_rolling_along_time_stretched_over_the_box() {
    let base = spiro(&[]);
    assert!(base.iter().all(|n| (n.vel, n.sub) == (100, 0)));
    lsys_matches(
        &base,
        &lsys_stretch(&spiro_line(3.0, 5.0, 5.0, 0.0)),
        "base",
    );
    let rows: BTreeSet<u8> = base.iter().map(|n| n.pitch).collect();
    assert_eq!((*rows.first().unwrap(), *rows.last().unwrap()), (50, 69));
    assert_eq!(base.iter().map(|n| n.start).min(), Some(960));
}

#[test]
fn spiro_trochoids_and_sizes_follow_the_classic_formulas() {
    let (big, r, d) = (5.0f64, 3.0f64, 5.0f64);
    // c01: rolling outside the fixed circle, closed after 3 passes.
    let epi = spiro_curve(
        |t| {
            let k = (big + r) / r;
            (
                (big + r) * t.cos() - d * (k * t).cos(),
                (big + r) * t.sin() - d * (k * t).sin(),
            )
        },
        TAU * 3.0,
        8000,
    );
    lsys_matches(
        &spiro(&[on("c01_epitrochoid_mode")]),
        &lsys_stretch(&epi),
        "c01",
    );
    // c02: inside.
    let hypo = spiro_curve(
        |t| {
            let k = (big - r) / r;
            (
                (big - r) * t.cos() + d * (k * t).cos(),
                (big - r) * t.sin() - d * (k * t).sin(),
            )
        },
        TAU * 3.0,
        8000,
    );
    lsys_matches(
        &spiro(&[on("c02_hypotrochoid_mode")]),
        &lsys_stretch(&hypo),
        "c02",
    );
    // c03 R = 7: 7 / 3 turns a pass, 3 passes; c04 r = 2: 5 / 2 turns a
    // pass, 2 passes; c05 d = 1: a wave; c06 1.5 passes; c07 the pen 90°
    // ahead; c08 the base turned 30° before the stretch.
    for (kv, segs) in [
        (vec![on("c03_spirograph_r")], spiro_line(3.0, 5.0, 7.0, 0.0)),
        (vec![on("c04_spirograph_r")], spiro_line(2.0, 5.0, 5.0, 0.0)),
        (
            vec![on("c05_pen_distance_d")],
            spiro_line(3.0, 1.0, 5.0, 0.0),
        ),
        (vec![on("c06_revolutions")], spiro_line(3.0, 5.0, 2.5, 0.0)),
        (vec![on("c07_phase")], spiro_line(3.0, 5.0, 5.0, TAU / 4.0)),
        (
            vec![on("c08_rotation")],
            spiro_turn(&spiro_line(3.0, 5.0, 5.0, 0.0), 30f64.to_radians()),
        ),
    ] {
        lsys_matches(&spiro(&kv), &lsys_stretch(&segs), kv[0].0);
    }
}

#[test]
fn spiro_lissajous_frequencies_and_phases() {
    let liss = |a: f64, b: f64, px: f64, py: f64| {
        spiro_curve(|t| ((a * t + px).sin(), (b * t + py).sin()), TAU, 6000)
    };
    let q = TAU / 4.0;
    lsys_matches(
        &spiro(&[on("c09_lissajous_mode")]),
        &lsys_stretch(&liss(3.0, 2.0, q, 0.0)),
        "c09",
    );
    let deg = |x: f64| x.to_radians();
    for (kv, segs) in [
        (vec![on("c10_x_frequency")], liss(5.0, 2.0, q, 0.0)),
        (vec![on("c11_y_frequency")], liss(3.0, 4.0, q, 0.0)),
        (vec![on("c12_x_phase")], liss(3.0, 2.0, deg(45.0), 0.0)),
        (vec![on("c13_y_phase")], liss(3.0, 2.0, q, deg(30.0))),
    ] {
        let mut kv = kv;
        kv.push(on("c09_lissajous_mode"));
        lsys_matches(&spiro(&kv), &lsys_stretch(&segs), kv[0].0);
    }
    // In the base the frequency multiplies the pen's x angle.
    let segs = spiro_curve(
        |t| (3.0 * t - 5.0 * (5.0 * t).sin(), 3.0 - 5.0 * t.cos()),
        TAU * 5.0,
        20000,
    );
    lsys_matches(
        &spiro(&[on("c10_x_frequency")]),
        &lsys_stretch(&segs),
        "c10 base",
    );
}

#[test]
fn spiro_fourier_sums_and_harmonics() {
    let four = |c: &[(f64, f64, f64)]| {
        let c = c.to_vec();
        spiro_curve(
            move |t| {
                c.iter().fold((0.0, 0.0), |s, &(f, a, p)| {
                    (s.0 + a * (f * t + p).cos(), s.1 + a * (f * t + p).sin())
                })
            },
            TAU,
            6000,
        )
    };
    lsys_matches(
        &spiro(&[on("c14_fourier_harmonic_sum")]),
        &lsys_stretch(&four(&[
            (1.0, 1.0, 0.0),
            (-3.0, 1.0 / 3.0, 0.0),
            (5.0, 0.2, 0.0),
        ])),
        "c14",
    );
    let deg = |x: f64| x.to_radians();
    lsys_matches(
        &spiro(&[
            on("c14_fourier_harmonic_sum"),
            on("c15_harmonic_amplitudes"),
            on("c16_harmonic_phases"),
        ]),
        &lsys_stretch(&four(&[
            (1.0, 1.0, deg(30.0)),
            (-3.0, 0.4, deg(90.0)),
            (5.0, 0.2, deg(45.0)),
        ])),
        "c14 + c15 + c16",
    );
    // In the base: the pen's offset as harmonics 1, 2, 3 (100, 40, 20 %).
    let segs = spiro_curve(
        |t| {
            let (mut sx, mut cy) = (0.0, 0.0);
            for (k, a) in [(1.0, 1.0), (2.0, 0.4), (3.0, 0.2)] {
                sx += a * (k * t).sin();
                cy += a * (k * t).cos();
            }
            (3.0 * t - 5.0 * sx, 3.0 - 5.0 * cy)
        },
        TAU * 5.0,
        12000,
    );
    lsys_matches(
        &spiro(&[on("c15_harmonic_amplitudes")]),
        &lsys_stretch(&segs),
        "c15 base",
    );
}

#[test]
fn spiro_modulations_orbits_sectors_and_morph() {
    let base = spiro_line(3.0, 5.0, 5.0, 0.0);
    // At depth 0 the modulations leave the base.
    for (id, k) in [
        ("c17_radial_modulation", "c17_radial_modulation.depth"),
        ("c18_angular_modulation", "c18_angular_modulation.depth"),
    ] {
        lsys_matches(&spiro(&[on(id), num(k, 0.0)]), &lsys_stretch(&base), id);
    }
    // c17: the offset scaled by 1 + 0.3 sin(2π 6 u) along the curve.
    let m = spiro_curve(
        |t| {
            let s = 1.0 + 0.3 * (TAU * 6.0 * t / (TAU * 5.0)).sin();
            (3.0 * t - 5.0 * s * t.sin(), 3.0 - 5.0 * s * t.cos())
        },
        TAU * 5.0,
        8000,
    );
    lsys_matches(
        &spiro(&[on("c17_radial_modulation")]),
        &lsys_stretch(&m),
        "c17",
    );
    // c23: two equal orbits are the base; c24: two sectors are symmetric
    // about the box's centre.
    lsys_matches(
        &spiro(&[
            on("c23_multi_orbit_superposition"),
            int("c23_multi_orbit_superposition.count", 2),
            num("c23_multi_orbit_superposition.factor", 100.0),
            num("c23_multi_orbit_superposition.phase", 0.0),
        ]),
        &lsys_stretch(&base),
        "c23",
    );
    let s = par_cells(&spiro(&[
        on("c24_symmetry_sector_replication"),
        int("c24_symmetry_sector_replication.sectors", 2),
    ]));
    assert!(
        s.iter()
            .all(|&(c, r)| near_cell(&s, 47 - c, i64::from(119 - r)))
    );
    // c25: a share of 0 is the base; 100 % is the preset (Rose: epi 4 1 2).
    lsys_matches(
        &spiro(&[
            on("c25_harmonic_preset_morph"),
            num("c25_harmonic_preset_morph.amount", 0.0),
        ]),
        &lsys_stretch(&base),
        "c25 0 %",
    );
    let rose = spiro_curve(
        |t| {
            (
                5.0 * t.cos() - 2.0 * (5.0 * t).cos(),
                5.0 * t.sin() - 2.0 * (5.0 * t).sin(),
            )
        },
        TAU,
        6000,
    );
    lsys_matches(
        &spiro(&[
            on("c25_harmonic_preset_morph"),
            num("c25_harmonic_preset_morph.amount", 100.0),
        ]),
        &lsys_stretch(&rose),
        "c25 100 %",
    );
}

#[test]
fn spiro_brushes_follow_curvature_radius_phase_and_layers() {
    let base = spiro(&[]);
    let near = |v: &[TNote]| {
        let (a, b) = (par_cells(v), par_cells(&base));
        a.iter().all(|&(c, r)| near_cell(&b, c, i64::from(r)))
    };
    // c19: notes every unit, fewer than the stroke's cells.
    let k = spiro(&[on("c19_density_by_curvature")]);
    assert!(near(&k) && k.iter().all(|n| n.dur <= 240));
    assert!(par_cells(&k).len() < par_cells(&base).len());
    // c20: 40 near the centre, 127 at the farthest (the box's corners).
    let v = spiro(&[on("c20_velocity_by_radius")]);
    assert!(near(&v));
    let mid = v
        .iter()
        .filter(|n| (58..=61).contains(&n.pitch))
        .map(|n| n.vel)
        .min();
    let edge = v
        .iter()
        .filter(|n| n.pitch == 50 || n.pitch == 69)
        .map(|n| n.vel)
        .max();
    assert!(mid.unwrap() < 80 && edge.unwrap() > 100, "{mid:?} {edge:?}");
    // c21: gates between 25 % and 100 %.
    let g = spiro(&[on("c21_gate_by_harmonic_phase")]);
    assert!(near(&g) && g.iter().any(|n| n.dur % 240 != 0));
    // c22: five turns over four layers: subchannels 0 – 3.
    let s = spiro(&[on("c22_subchannel_by_harmonic_layer")]);
    let subs: BTreeSet<u32> = s.iter().map(|n| n.sub).collect();
    assert_eq!(subs, [0, 1, 2, 3].into_iter().collect());
    assert!(near(&s));
}

#[test]
fn spiro_caps_refuse_huge_settings_with_sentences() {
    let refused = |kv: &[(&str, ParamValue)], has: &str| match spiro_try(kv) {
        Err(TransformError::TooLarge(m)) | Err(TransformError::Invalid(m)) => {
            assert!(m.contains(has) && m.ends_with('.'), "{m}")
        }
        other => panic!("{has}: {:?}", other.map(|v| v.len())),
    };
    refused(
        &[
            on("c03_spirograph_r"),
            num("c03_spirograph_r.r", 100.0),
            on("c04_spirograph_r"),
            num("c04_spirograph_r.r", 0.1),
            on("c06_revolutions"),
            num("c06_revolutions.n", 64.0),
        ],
        "turn",
    );
    refused(
        &[
            on("c14_fourier_harmonic_sum"),
            ("c14_fourier_harmonic_sum.freqs", ParamValue::List(vec![0])),
        ],
        "frequency",
    );
    refused(
        &[
            on("c25_harmonic_preset_morph"),
            txt("c25_harmonic_preset_morph.name", "Nope"),
        ],
        "Nope",
    );
    refused(
        &[
            on("c25_harmonic_preset_morph"),
            txt("c25_harmonic_preset_morph.library", "A: spiral 1 2 3"),
        ],
        "mode",
    );
}

/// Shares near 0 over a box 2³⁶ grid units wide: refused at once with a
/// sentence instead of visiting every sample.
#[test]
fn spiro_sparse_density_over_a_huge_box_is_refused_before_visiting_the_samples() {
    let t = std::time::Instant::now();
    match spiro_try(&[
        ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
        ("gen.step", ParamValue::Choice(11)),
        ("gen.step_ticks", ParamValue::Int(1)),
        on("c19_density_by_curvature"),
        num("c19_density_by_curvature.floor", 0.0),
        num("c19_density_by_curvature.strength", 200.0),
    ]) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains("visit") && m.ends_with('.'), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

/// A gate along the curve (c21) over a box 2³⁶ grid units wide: every
/// segment would be cut into up to 1,024 pieces — refused at once with a
/// sentence (a harness draw like it held 2 GB, then counted for 277 s).
#[test]
fn spiro_a_gate_along_the_curve_over_a_huge_box_is_refused_before_cutting_it() {
    let t = std::time::Instant::now();
    match spiro_try(&[
        ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
        ("gen.step", ParamValue::Choice(11)),
        ("gen.step_ticks", ParamValue::Int(1)),
        on("c21_gate_by_harmonic_phase"),
    ]) {
        Err(TransformError::TooLarge(m)) => {
            assert!(m.contains("pieces") && m.ends_with('.'), "{m}")
        }
        other => panic!("{:?}", other.map(|v| v.len())),
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    assert!(
        !spiro_try(&[on("c21_gate_by_harmonic_phase")])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn spiro_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = spiro_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.iter().all(|n| valid(n, keys)));
    }
}

// ----- Procedural Growth / Topology (V09-S61)

fn grow_def() -> &'static ToolDef {
    generators()
        .into_iter()
        .find(|t| t.id == "gen.growth")
        .unwrap()
}

/// Runs the growth on the 40-step × 20-row test box (see `lsys_try`) of a
/// document holding `notes` on track 0.
fn grow_in(notes: &[NoteRecord], kv: &[(&str, ParamValue)]) -> Result<Vec<TNote>, TransformError> {
    let def = grow_def();
    let d = doc(KeyRange::Standard, notes);
    let base = with(
        defaults(def, KeyRange::Standard),
        &[
            ("gen.low", ParamValue::Int(50)),
            ("gen.high", ParamValue::Int(69)),
            ("gen.length", ParamValue::Int(240 * 40)),
        ],
    );
    let mut r = run_in(def, &d, &with(base, kv), &ctx(&d, 960), &Limits::default())?.notes;
    r.retain(|n| n.start >= 960 || notes.is_empty());
    r.sort_by_key(|n| (n.pitch, n.start, n.dur, n.vel, n.sub));
    Ok(r)
}

fn grow(kv: &[(&str, ParamValue)]) -> Vec<TNote> {
    grow_in(&[], kv).unwrap_or_else(|e| panic!("{e}"))
}

/// The domain laid on the box by a mask that excludes nothing (a rectangle
/// without inset): u = 4 + 40 x, v = 50.5 + 19 y.
fn on_box() -> Vec<(&'static str, ParamValue)> {
    vec![
        on("c23_boundary_mask"),
        ch("c23_boundary_mask.shape", 1),
        num("c23_boundary_mask.inset", 0.0),
    ]
}

fn plus_box(kv: &[(&'static str, ParamValue)]) -> Vec<(&'static str, ParamValue)> {
    let mut v = on_box();
    v.extend(kv.iter().cloned());
    v
}

/// The cells' extent: (first column, last column, lowest row, highest row).
fn grow_extent(v: &[TNote]) -> (i64, i64, u8, u8) {
    let c = par_cells(v);
    (
        c.iter().map(|x| x.0).min().unwrap(),
        c.iter().map(|x| x.0).max().unwrap(),
        c.iter().map(|x| x.1).min().unwrap(),
        c.iter().map(|x| x.1).max().unwrap(),
    )
}

#[test]
fn grow_the_base_branches_from_one_seed_and_is_stretched_over_the_box() {
    let base = grow(&[]);
    assert!(base.iter().all(|n| (n.vel, n.sub) == (100, 0)));
    assert_eq!(grow_extent(&base), (4, 43, 50, 69));
    assert_eq!(par_all(&base), par_all(&grow(&[])));
    assert_ne!(par_all(&base), par_all(&grow(&[int("gen.seed", 2)])));
    // Laid on the box, the seed's first step starts at (0.02, 0.5).
    let one = grow(&plus_box(&[
        on("c02_iterations"),
        int("c02_iterations.n", 1),
    ]));
    let cells = par_cells(&one);
    assert!(near_cell(&cells, 4, 60), "{cells:?}");
    assert!(cells.len() <= 4, "{cells:?}");
}

#[test]
fn grow_seeds_iterations_steps_and_branches() {
    // c01: three seeds (20,250), (20,750), (500,500): their first steps.
    let s = par_cells(&grow(&plus_box(&[
        on("c01_seed_points"),
        on("c02_iterations"),
        int("c02_iterations.n", 1),
    ])));
    for (u, v) in [(4.8, 55.25), (4.8, 64.75), (24.0, 60.0)] {
        assert!(near_cell(&s, u as i64, v as i64), "{u} {v}: {s:?}");
    }
    // c02 / c03: more iterations or longer steps reach farther along time.
    let reach = |kv: &[(&'static str, ParamValue)]| grow_extent(&grow(&plus_box(kv))).1;
    let base = reach(&[]);
    assert!(reach(&[on("c02_iterations")]) >= base);
    assert!(reach(&[on("c03_growth_step")]) > base);
    // c04 / c05: more branches make more cells; no branching is one line.
    let cells = |kv: &[(&'static str, ParamValue)]| par_cells(&grow(&plus_box(kv))).len();
    assert!(cells(&[on("c04_branch_probability")]) > cells(&[]));
    let line = grow(&plus_box(&[
        on("c04_branch_probability"),
        num("c04_branch_probability.p", 0.0),
    ]));
    let cols: BTreeSet<i64> = par_cells(&line).iter().map(|c| c.0).collect();
    assert_eq!(
        cols.len(),
        par_cells(&line)
            .iter()
            .map(|c| c.0)
            .collect::<BTreeSet<_>>()
            .len()
    );
    assert_ne!(
        par_all(&grow(&[on("c05_branch_angle_range")])),
        par_all(&grow(&[]))
    );
}

#[test]
fn grow_fields_steer_the_tips() {
    // c06: an attractor at the far end pulls the growth towards it; c07: a
    // repeller at the seed's row pushes it off that row.
    let far = |kv: &[(&'static str, ParamValue)]| grow_extent(&grow(&plus_box(kv))).1;
    assert!(
        far(&[
            on("c06_attraction_field"),
            num("c06_attraction_field.strength", 100.0),
        ]) >= far(&[])
    );
    let rep = grow(&plus_box(&[
        on("c07_repulsion_field"),
        (
            "c07_repulsion_field.points",
            ParamValue::List(vec![200, 500]),
        ),
        num("c07_repulsion_field.strength", 100.0),
        num("c07_repulsion_field.radius", 20.0),
    ]));
    // Nothing grows on the repeller itself (u = 12, v = 60).
    assert!(!par_cells(&rep).contains(&(12, 60)));
    // c21: notes of the track draw the tips; growth slows away from them.
    let notes = [rec(960 + 240 * 30, 240 * 8, 66)];
    let with_notes = grow_in(&notes, &plus_box(&[on("c21_external_track_field")])).unwrap();
    let without = grow(&plus_box(&[on("c21_external_track_field")]));
    assert_ne!(par_all(&with_notes), par_all(&without));
    assert!(par_cells(&without).len() < par_cells(&grow(&plus_box(&[]))).len());
    // c22: a bright image grows faster than a dark one.
    let img = |t: &str| {
        par_cells(&grow(&plus_box(&[
            on("c22_image_field"),
            txt("c22_image_field.image", t),
        ])))
        .len()
    };
    assert!(img("9999/9999") > img("0000/0000"));
    assert_ne!(
        par_all(&grow(&[on("c22_image_field")])),
        par_all(&grow(&[]))
    );
}

#[test]
fn grow_neighbours_merges_loops_and_density() {
    let base = par_all(&grow(&[]));
    for id in [
        "c08_neighbor_radius",
        "c09_density_target",
        "c10_collision_avoidance",
        "c11_branch_merge",
        "c12_loop_formation",
    ] {
        let a = grow(&[on(id), on("c04_branch_probability")]);
        assert_eq!(
            par_all(&a),
            par_all(&grow(&[on(id), on("c04_branch_probability")])),
            "{id}"
        );
        assert!(a.iter().all(|n| valid(n, KeyRange::Standard)), "{id}");
        assert_ne!(par_all(&grow(&[on(id)])), base, "{id}");
    }
}

#[test]
fn grow_aging_pruning_and_evolving_brushes() {
    let base = grow(&[]);
    // Laid on the box (no stretch): c13 / c14 remove segments — fewer
    // cells; c15 makes older parts thicker — more cells.
    let n = |kv: &[(&'static str, ParamValue)]| par_cells(&grow(&plus_box(kv))).len();
    let b = n(&[on("c04_branch_probability")]);
    assert!(n(&[on("c04_branch_probability"), on("c13_aging_decay")]) < b);
    assert!(n(&[on("c04_branch_probability"), on("c14_branch_pruning")]) < b);
    assert!(n(&[on("c04_branch_probability"), on("c15_thickness_evolution")]) > b);
    // c16: velocities from 127 (oldest) to 40 (newest); c17 gates.
    let v = grow(&[on("c16_velocity_evolution")]);
    let vels: BTreeSet<u8> = v.iter().map(|n| n.vel).collect();
    assert!(
        *vels.first().unwrap() < 80 && *vels.last().unwrap() > 110,
        "{vels:?}"
    );
    let g = grow(&[on("c17_gate_evolution")]);
    assert!(g.iter().any(|n| n.dur % 240 != 0));
    // The base keeps one velocity and full gates.
    assert_eq!(
        base.iter().map(|n| n.vel).collect::<BTreeSet<u8>>().len(),
        1
    );
    assert!(base.iter().all(|n| n.dur % 240 == 0));
    // c18: species by seed and branch order: subchannels 0 – 2.
    let s = grow(&[on("c18_subchannel_species"), on("c04_branch_probability")]);
    let subs: BTreeSet<u32> = s.iter().map(|n| n.sub).collect();
    assert_eq!(subs, [0, 1, 2].into_iter().collect());
    // c19 / c20 are seeded.
    for id in ["c19_mutation_rate", "c20_directional_noise"] {
        let a = |seed: i64| {
            par_all(&grow(&[
                on(id),
                on("c04_branch_probability"),
                int("gen.seed", seed),
            ]))
        };
        assert_eq!(a(3), a(3), "{id}");
        assert_ne!(a(3), a(4), "{id}");
    }
}

#[test]
fn grow_masks_symmetry_and_stages() {
    // c23: inside the diamond inscribed in the box.
    let m = grow(&[
        on("c23_boundary_mask"),
        ch("c23_boundary_mask.shape", 2),
        on("c04_branch_probability"),
    ]);
    for (c, r) in par_cells(&m) {
        let (x, y) = (
            (c as f64 + 0.5 - 4.0) / 40.0,
            (f64::from(r) + 0.5 - 50.5) / 19.0,
        );
        assert!((x - 0.5).abs() + (y - 0.5).abs() <= 0.5 + 0.06, "{c} {r}");
    }
    // c24: the pitch mirror stretched over the box: symmetric rows.
    let s = par_cells(&grow(&[on("c24_symmetry_mirrored_growth")]));
    assert!(s.iter().all(|&(c, r)| near_cell(&s, c, i64::from(119 - r))));
    // c25: one saved stage is part of the last stage's growth (the same
    // growth, fewer iterations), laid on the box.
    let last = par_cells(&grow(&plus_box(&[])));
    let one = par_cells(&grow(&plus_box(&[
        on("c25_evolution_timeline_saved_growth_stages"),
        ch("c25_evolution_timeline_saved_growth_stages.mode", 1),
    ])));
    assert!(one.is_subset(&last) && one.len() < last.len());
    assert_ne!(
        par_all(&grow(&[on("c25_evolution_timeline_saved_growth_stages")])),
        par_all(&grow(&[]))
    );
}

#[test]
fn grow_with_every_function_on_stays_valid_bounded_and_deterministic() {
    let def = grow_def();
    let spec = (def.spec)();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let d = doc(keys, &[]);
        let mut s = defaults(def, keys);
        for f in &spec.functions {
            s.set(f.id, ParamValue::Bool(true));
        }
        let c = ctx(&d, 960);
        let a = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        let b = run_in(def, &d, &s, &c, &Limits::default()).unwrap().notes;
        assert_eq!(a, b);
        assert!(a.iter().all(|n| valid(n, keys)));
    }
}

/// An evolving gate (c17: a brush changing along each piece) on a dense
/// growth over a box 2³⁶ grid units wide: every segment would be cut into up
/// to 1,024 pieces — refused with a sentence before anything is drawn
/// (V09-S60's piece cap), also when the growth waits for its External Track
/// Field's notes (c21: refused by the run once the source is known).
#[test]
fn grow_an_evolving_gate_over_a_huge_box_is_refused_before_cutting_the_strokes() {
    let def = grow_def();
    let d = doc(KeyRange::Standard, &[]);
    let t = std::time::Instant::now();
    for field in [false, true] {
        let mut kv = vec![
            ("gen.length", ParamValue::Int(MAX_LENGTH as i64)),
            ("gen.step", ParamValue::Choice(11)),
            ("gen.step_ticks", ParamValue::Int(1)),
            on("c02_iterations"),
            ("c02_iterations.n", ParamValue::Int(1000)),
            on("c04_branch_probability"),
            num("c04_branch_probability.p", 100.0),
            on("c17_gate_evolution"),
        ];
        if field {
            kv.push(on("c21_external_track_field"));
        }
        let s = with(defaults(def, KeyRange::Standard), &kv);
        match run_in(def, &d, &s, &ctx(&d, 960), &Limits::default()) {
            Err(TransformError::TooLarge(m)) => {
                assert!(m.contains("pieces") && m.ends_with('.'), "{m}")
            }
            other => panic!("field {field}: {:?}", other.map(|v| v.notes.len())),
        }
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    assert!(!grow(&[on("c17_gate_evolution")]).is_empty());
}
