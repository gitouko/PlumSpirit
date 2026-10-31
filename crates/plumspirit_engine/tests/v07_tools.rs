//! v0.7 generic tool harness (docs/v0_7/TEST_MATRIX.md, T-H-*): every tool of
//! the transformation registry is run on a rich fixture (several tracks, a
//! subtrack, several subchannels, stacked duplicates, pitch and tick extremes)
//! with its defaults, with each advanced function enabled, and with seeded
//! random parameters. Invariants: valid output, determinism, lazy preview ==
//! full run, commit + undo + redo exact, preview never mutates the document,
//! every advanced function materially changes the result for some parameters.

mod tool_support;

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};
use plumspirit_engine::transform::param::{ADV_KEYS, ParamKind, ParamValue};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{
    Ctx, Limits, Output, ParamSet, Source, TNote, ToolDef, TrackScope, TransformError, commit,
    registry, run_kernel,
};
use tool_support::random_value;

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

/// Track 0 (4 subchannels) with a subtrack (index 1) and track 2. Chords,
/// stacks, extremes. Everything selected.
fn fixture(extra: usize) -> PlumDocument {
    fixture_in(extra, KeyRange::Standard, 0, false)
}

/// [`fixture`] in a project of key range `keys` with every pitch moved up by
/// `shift` semitones (v0.9 256-Key Mode); `wide_extremes` adds notes on keys
/// 128, 200, 254 and 255 (a stacked pair on 255).
fn fixture_in(extra: usize, keys: KeyRange, shift: u8, wide_extremes: bool) -> PlumDocument {
    let mut d = PlumDocument::new_project("v07".into(), Bpm::DEFAULT, 960);
    d.project_mut().keys = keys;
    let mut h = History::default();
    let subs: Vec<Subchannel> = (1..4)
        .map(|i| Subchannel::new(SubchannelId(i), i as u8))
        .collect();
    h.apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    d.add_track().unwrap();
    let mut recs: Vec<Vec<NoteRecord>> = vec![Vec::new(), Vec::new(), Vec::new()];
    // Chords on beats, four subchannels.
    for bar in 0..4u64 {
        for (k, p) in [48u8, 52, 55, 60].iter().enumerate() {
            recs[0].push(rec(bar * 3840, 1920, *p + bar as u8, 90, k as u8, k as u32));
        }
        // A melody.
        for s in 0..8u64 {
            recs[0].push(rec(
                bar * 3840 + s * 480,
                240,
                72 + (s % 5) as u8,
                60 + s as u8,
                0,
                (s % 4) as u32,
            ));
        }
    }
    // Stacked duplicates.
    for _ in 0..3 {
        recs[0].push(rec(960, 480, 64, 100, 0, 0));
    }
    // Extremes: tick 0, pitch 0 and 127, gate 1, very long note.
    recs[0].push(rec(0, 1, 0, 1, 0, 0));
    recs[0].push(rec(5, 1, 127, 127, 0, 0));
    recs[0].push(rec(7, 1 << 20, 64, 64, 0, 1));
    // Fragments (Join): a touching chain in two groups, an overlapping pair,
    // a gapped chain and a touching pair across subchannels.
    for (st, du, v, g) in [
        (20_000u64, 100u64, 40u8, 1u32),
        (20_100, 200, 80, 1),
        (20_300, 50, 120, 2),
        (20_350, 150, 60, 2),
    ] {
        let mut r = rec(st, du, 90, v, 2, 2);
        r.group = g;
        recs[0].push(r);
    }
    recs[0].push(rec(21_000, 400, 91, 100, 2, 2));
    recs[0].push(rec(21_100, 100, 91, 30, 2, 2));
    for k in 0..3u64 {
        recs[0].push(rec(22_000 + k * 130, 100, 92, 70 + k as u8, 3, 3));
    }
    for k in 0..70u64 {
        recs[0].push(rec(24_000 + k * 30, 30, 94, 50 + (k % 50) as u8, 0, 0));
    }
    recs[0].push(rec(23_000, 100, 93, 50, 1, 1));
    recs[0].push(rec(23_100, 100, 93, 90, 2, 2));
    // A note of subchannel 2 over the subtrack's row 40 (subchannel-sensitive
    // masking).
    recs[0].push(rec(900, 400, 40, 70, 2, 2));
    // Track 3 overlaps the subtrack's rows (Clipping Mask union / subtract).
    recs[2].push(rec(100, 1500, 40, 77, 0, 0));
    recs[2].push(rec(500, 800, 44, 77, 0, 0));
    // Subtrack and second track.
    for i in 0..16u64 {
        recs[1].push(rec(i * 240, 200, 36 + (i % 12) as u8, 80, 9, 0));
        recs[2].push(rec(i * 240 + 60, 120, 84 + (i % 7) as u8, 70, 2, 0));
    }
    // Bulk notes for size checks.
    let mut r = Rng::new(11);
    for i in 0..extra as u64 {
        recs[2].push(rec(
            i * 37 + r.range_i64(0, 30) as u64,
            r.range_i64(1, 2000) as u64,
            r.range_i64(20, 108) as u8,
            r.range_i64(1, 127) as u8,
            0,
            0,
        ));
    }
    // The fragments' groups 1 and 2, and a group 3 nested in 1 (no notes).
    let g1 = d.project_mut().groups.add(Default::default());
    let g2 = d.project_mut().groups.add(Default::default());
    let g3 = d
        .project_mut()
        .groups
        .add(plumspirit_engine::model::GroupInfo {
            parent: g1,
            ..Default::default()
        });
    assert_eq!((g1, g2, g3), (1, 2, 3));
    if wide_extremes {
        for (st, p) in [(30u64, 128u8), (40, 200), (50, 254), (60, 255), (60, 255)] {
            recs[0].push(rec(st, 480, 0, 90, 0, 1));
            let r = recs[0].last_mut().unwrap();
            r.attrs = r.attrs.with_pitch_wide(p).unwrap();
        }
    }
    for (t, mut v) in recs.into_iter().enumerate() {
        if shift > 0 {
            for r in &mut v {
                if r.attrs.pitch() <= 127 {
                    let p = r.attrs.pitch() + shift;
                    r.attrs = r.attrs.with_pitch_wide(p).unwrap();
                }
            }
        }
        v.sort_by_key(|n| n.start_tick);
        let tr = d.track_mut(t).unwrap();
        tr.notes.insert_sorted_batch(&v, true).unwrap();
    }
    d
}

/// The tools' context for `d` (960 PPQN, the document's note groups).
fn ctx_of(d: &PlumDocument) -> Ctx {
    ctx_in(d, 0)
}

/// [`ctx_of`] with the stroke moved up by `shift` semitones and the
/// document's key range (v0.9 256-Key Mode).
fn ctx_in(d: &PlumDocument, shift: u8) -> Ctx {
    let mut c = Ctx::simple(960);
    c.keys = d.project().keys;
    c.groups = std::sync::Arc::new(d.project().groups.clone());
    // A pointer stroke for the stroke tools (Draw, Paint, Select lassos).
    c.stroke = Some(std::sync::Arc::new(tool_support::stroke(shift)));
    c
}

fn scopes(d: &PlumDocument) -> Vec<TrackScope> {
    (0..d.track_count())
        .filter(|&i| d.track(i).unwrap().notes.selected_count() > 0)
        .map(|index| TrackScope {
            index,
            all: false,
            content: None,
        })
        .collect()
}

struct Outcome {
    notes: Vec<TNote>,
    /// Controller points and the new group table (v0.7 events / groups output).
    events: Vec<plumspirit_engine::transform::output::TEvent>,
    groups: Option<plumspirit_engine::model::GroupTable>,
}

impl Outcome {
    fn same(&self, o: &Outcome) -> bool {
        self.notes == o.notes && self.events == o.events && self.groups == o.groups
    }
}

fn run(d: &PlumDocument, tool: &ToolDef, set: &ParamSet) -> Result<Outcome, TransformError> {
    run_in(d, tool, set, 0)
}

/// [`run`] with the stroke moved up by `shift` semitones ([`ctx_in`]).
fn run_in(
    d: &PlumDocument,
    tool: &ToolDef,
    set: &ParamSet,
    shift: u8,
) -> Result<Outcome, TransformError> {
    let sc = scopes(d);
    let src = collect_for(tool, d, &sc, 10_000_000)?;
    let ctx = ctx_in(d, shift);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = tool.kernel(set, &b)?;
    let est = k.estimate(&src.stats);
    let r = run_kernel(
        k.as_ref(),
        &src,
        tool.boundary(set),
        &Limits::default(),
        None,
    )?;
    // The preflight is an upper bound of the output (contract §25, §141).
    assert!(
        r.notes.len() as u64 <= est.notes + src.locked.len() as u64,
        "{}: {} notes out, preflight estimated at most {}",
        tool.id,
        r.notes.len(),
        est.notes
    );
    Ok(Outcome {
        notes: r.notes,
        events: r.events,
        groups: r.groups,
    })
}

/// Tools whose results may use subchannels the track lacks (created on apply).
const CREATES_SUBCHANNELS: &[&str] = &["note_props", "subpaint", "paste_repeat_notes"];

fn check_valid(d: &PlumDocument, tool: &str, notes: &[TNote]) {
    let sc = scopes(d);
    for w in notes.windows(2) {
        assert!(
            (w[0].track, w[0].start) <= (w[1].track, w[1].start),
            "{tool}: unsorted"
        );
    }
    for n in notes {
        assert!(
            d.project().keys.contains(n.pitch)
                && (1..=127).contains(&n.vel)
                && n.ch <= 15
                && n.rel <= 127,
            "{tool}: {n:?}"
        );
        assert!(n.dur >= 1, "{tool}: zero length");
        assert!(n.start.checked_add(n.dur).is_some(), "{tool}: overflow");
        let t = d.track(sc[usize::from(n.track)].index).unwrap();
        // v0.8 §161: a Subchannel Offset may reach subchannels the track does
        // not have yet; applying creates them in the same undo step
        // (`v08_editing::subchannel_offset_…`).
        assert!(
            CREATES_SUBCHANNELS.contains(&tool) || t.subchannels().contains(SubchannelId(n.sub)),
            "{tool}: unknown subchannel {}",
            n.sub
        );
    }
}

fn base_set(tool: &ToolDef) -> ParamSet {
    ParamSet::defaults(&(tool.spec)().params)
}

/// The defaults in a project of key range `keys` (keyed defaults of 127 are
/// its top key).
fn base_set_in(tool: &ToolDef, keys: KeyRange) -> ParamSet {
    ParamSet::defaults_in(&(tool.spec)().params, keys)
}

/// The registry, or only the tools named in `PLUMSPIRIT_HARNESS_TOOLS`
/// (comma-separated ids) — a tool expansion step sweeps its own tool with
/// many draws: `PLUMSPIRIT_HARNESS_TOOLS=articulate
/// PLUMSPIRIT_HARNESS_DRAWS=300 cargo test --release … --test v07_tools`.
fn harness_tools() -> impl Iterator<Item = &'static ToolDef> {
    let only: Option<Vec<String>> = std::env::var("PLUMSPIRIT_HARNESS_TOOLS")
        .ok()
        .map(|v| v.split(',').map(|t| t.trim().to_owned()).collect());
    registry()
        .iter()
        .filter(move |t| only.as_ref().is_none_or(|o| o.iter().any(|x| x == t.id)))
}

fn randomized(tool: &ToolDef, seed: u64, adv_on: Option<usize>) -> ParamSet {
    randomized_in(tool, seed, adv_on, KeyRange::Standard)
}

/// [`randomized`] with pitches drawn from the key range `keys`.
fn randomized_in(tool: &ToolDef, seed: u64, adv_on: Option<usize>, keys: KeyRange) -> ParamSet {
    let spec = (tool.spec)();
    let mut r = Rng::new(seed);
    let mut set = ParamSet::defaults_in(&spec.params, keys);
    for p in &spec.params {
        if p.key.starts_with("b.") || ADV_KEYS.contains(&p.key) {
            continue;
        }
        if let Some(v) = random_value(p, &mut r, keys) {
            set.set(p.key, v);
        }
    }
    for (i, k) in ADV_KEYS.iter().enumerate() {
        set.set(k, ParamValue::Bool(adv_on == Some(i)));
    }
    // Keep pitch results inside the range so runs are comparable.
    set.set("b.pitch", ParamValue::Choice(1));
    set
}

#[test]
fn every_tool_runs_with_defaults_and_each_advanced_function_alone() {
    let d = fixture(0);
    for tool in registry() {
        let out = run(&d, tool, &base_set(tool)).unwrap_or_else(|e| panic!("{}: {e}", tool.id));
        check_valid(&d, tool.id, &out.notes);
        for (i, key) in ADV_KEYS
            .iter()
            .enumerate()
            .take((tool.spec)().advanced.len())
        {
            let mut set = base_set(tool);
            set.set(key, ParamValue::Bool(true));
            set.set("b.pitch", ParamValue::Choice(1));
            let out = run(&d, tool, &set)
                .unwrap_or_else(|e| panic!("{} advanced {}: {e}", tool.id, i + 1));
            check_valid(&d, tool.id, &out.notes);
        }
    }
}

#[test]
fn every_advanced_function_materially_changes_the_result_for_some_parameters() {
    let d = fixture(0);
    for tool in registry() {
        for (i, key) in ADV_KEYS
            .iter()
            .enumerate()
            .take((tool.spec)().advanced.len())
        {
            // v0.8 plugins (HZ-Chop, SAFOR) have no advanced functions.
            if i >= (tool.spec)().advanced.len() {
                continue;
            }
            let mut changed = false;
            for draw in 0..12u64 {
                let mut on = randomized(tool, draw * 31 + i as u64, Some(i));
                // V09-S34: every other draw switches the functions declared
                // by id off — a new grid (a frame or elastic grid, an onset
                // magnet) can replace what a frozen function shapes (the
                // groove), which would hide it in most random draws.
                if draw % 2 == 0 {
                    for f in &(tool.spec)().functions {
                        on.set(f.id, ParamValue::Bool(false));
                    }
                }
                let mut off = on.clone();
                off.set(key, ParamValue::Bool(false));
                match (run(&d, tool, &on), run(&d, tool, &off)) {
                    (Ok(a), Ok(b)) if !a.same(&b) => {
                        changed = true;
                        break;
                    }
                    _ => {}
                }
            }
            assert!(
                changed,
                "{}: advanced function {} never changed the result",
                tool.id,
                i + 1
            );
        }
    }
}

#[test]
fn random_parameters_give_valid_deterministic_results() {
    let d = fixture(0);
    // `PLUMSPIRIT_HARNESS_DRAWS` widens the sweep (release sweeps before a
    // release use a few hundred).
    let draws: u64 = std::env::var("PLUMSPIRIT_HARNESS_DRAWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for tool in harness_tools() {
        for draw in 0..draws {
            let adv = (draw % 11) as usize;
            let set = randomized(tool, 1000 + draw, (adv < 10).then_some(adv));
            match (run(&d, tool, &set), run(&d, tool, &set)) {
                (Ok(a), Ok(b)) => {
                    check_valid(&d, tool.id, &a.notes);
                    assert_eq!(a.notes, b.notes, "{}: not deterministic", tool.id);
                    assert!(a.same(&b), "{}: events / groups not deterministic", tool.id);
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{}", tool.id),
                (a, b) => panic!(
                    "{}: nondeterministic success {:?} {:?}",
                    tool.id,
                    a.is_ok(),
                    b.is_ok()
                ),
            }
        }
    }
}

/// Every Seed control matters: for each tool with a seed, some setting
/// (defaults, one advanced function, or random parameters) gives a different
/// result for a different seed (T-H-SEED; a seed that never changes anything
/// would be a dead control).
#[test]
fn every_seed_control_changes_the_result_for_some_setting() {
    let d = fixture(0);
    let mut dead = Vec::new();
    for tool in registry() {
        let spec = (tool.spec)();
        let seeds: Vec<&str> = spec
            .params
            .iter()
            .filter(|p| matches!(p.kind, ParamKind::Seed))
            .map(|p| p.key)
            .collect();
        for key in seeds {
            let mut sets = vec![base_set(tool)];
            for (adv, key_adv) in ADV_KEYS.iter().enumerate() {
                let mut s = base_set(tool);
                s.set(key_adv, ParamValue::Bool(true));
                s.set("b.pitch", ParamValue::Choice(1));
                sets.push(s);
                for draw in 0..4u64 {
                    sets.push(randomized(tool, 500 + draw * 17 + adv as u64, Some(adv)));
                }
            }
            let differs = sets.iter().any(|set| {
                let mut a = set.clone();
                a.set(key, ParamValue::Int(1));
                let mut b = set.clone();
                b.set(key, ParamValue::Int(987_654));
                match (run(&d, tool, &a), run(&d, tool, &b)) {
                    (Ok(x), Ok(y)) => !x.same(&y),
                    _ => false,
                }
            });
            if !differs {
                dead.push(format!("{}.{key}", tool.id));
            }
        }
    }
    assert!(dead.is_empty(), "seed controls without effect: {dead:?}");
}

#[test]
fn lazy_viewport_preview_equals_the_full_run_for_pointwise_tools() {
    let d = fixture(3000);
    let sc = scopes(&d);
    let ctx = ctx_of(&d);
    for tool in registry() {
        for draw in 0..6u64 {
            let set = randomized(tool, 77 + draw, (draw < 5).then_some((draw * 2) as usize));
            let src = collect_for(tool, &d, &sc, 10_000_000).unwrap();
            let b = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let Ok(k) = tool.kernel(&set, &b) else {
                continue;
            };
            let Some(p) = k.as_point() else { continue };
            let Ok(full) = run_kernel(
                k.as_ref(),
                &src,
                tool.boundary(&set),
                &Limits::default(),
                None,
            ) else {
                continue;
            };
            if full.stats.shifted_by > 0 {
                continue;
            }
            for (t0, t1) in [
                (0u64, 1000u64),
                (3000, 9000),
                (10_000, 10_500),
                (0, u64::MAX),
            ] {
                let (a, bb) = p.source_window(t0, t1);
                let mut out = Output::new(tool.boundary(&set), usize::MAX);
                for n in &src.notes {
                    if n.start <= bb && n.end() >= a {
                        p.map(n, &mut out);
                    }
                }
                let (mut lazy, _) = out.finish().unwrap();
                lazy.retain(|n| n.start <= t1 && n.end() >= t0);
                let mut want: Vec<TNote> = full
                    .notes
                    .iter()
                    .copied()
                    .filter(|n| n.start <= t1 && n.end() >= t0)
                    .collect();
                // Group ids a kernel generates are renumbered into the table
                // by the full run (registration); the drawn preview does not
                // depend on them.
                for n in lazy.iter_mut().chain(want.iter_mut()) {
                    n.group = 0;
                }
                want.sort();
                lazy.sort();
                assert_eq!(
                    lazy, want,
                    "{}: lazy preview differs in [{t0}, {t1}]",
                    tool.id
                );
            }
        }
    }
}

/// Every note of every track (sorted), for note-only comparisons.
fn notes_of(d: &PlumDocument) -> Vec<Vec<NoteRecord>> {
    (0..d.track_count())
        .map(|i| {
            let mut v: Vec<NoteRecord> = d
                .track(i)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect();
            v.sort_by_key(|r| {
                (
                    r.start_tick,
                    r.duration_ticks,
                    r.attrs.raw(),
                    r.sub.0,
                    r.group,
                )
            });
            v
        })
        .collect()
}

#[test]
fn apply_undo_redo_are_exact_and_preview_never_mutates() {
    for tool in registry() {
        let mut d = fixture(200);
        let mut h = History::default();
        let before = d.state_fingerprint();
        let notes_before = notes_of(&d);
        let sc = scopes(&d);
        let src = collect_for(tool, &d, &sc, 10_000_000).unwrap();
        let ctx = ctx_of(&d);
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        // The random draw of seed 5 — or, when the preflight refuses it
        // (TooLarge: e.g. the geometry's Split × Fill bound times many
        // copies; V09-S40 step 6), the next seed's: a refusal says nothing
        // about apply / undo, and which draw fits depends on how many
        // parameters the tool has. A generator may refuse at build (its
        // analytic caps — S54: Circles' ring / point / line caps). A draw
        // needing an input this document cannot give (S50: Tumours' picture
        // mask without a picture file, a mask track without notes) is
        // refused with a sentence and skipped the same way.
        let skip = |e: &TransformError| match e {
            TransformError::TooLarge(_) => true,
            TransformError::Invalid(m) => {
                assert!(m.ends_with('.'), "{}: not a sentence: {m}", tool.id);
                true
            }
            _ => false,
        };
        let r = (5..37)
            .find_map(|seed| {
                let set = randomized(tool, seed, Some(0));
                let k = match tool.kernel(&set, &b) {
                    Ok(k) => k,
                    Err(e) if skip(&e) => return None,
                    Err(e) => panic!("{}: {e}", tool.id),
                };
                match run_kernel(
                    k.as_ref(),
                    &src,
                    tool.boundary(&set),
                    &Limits::default(),
                    None,
                ) {
                    Ok(r) => Some(r),
                    Err(e) if skip(&e) => None,
                    Err(e) => panic!("{}: {e}", tool.id),
                }
            })
            .unwrap_or_else(|| panic!("{}: every draw refused", tool.id));
        // Preview (the run above) changed nothing; cancelling = dropping it.
        assert_eq!(
            d.state_fingerprint(),
            before,
            "{}: preview mutated",
            tool.id
        );
        assert_eq!(h.undo_len(), 0);
        match commit(&mut h, &mut d, &r, "Tool") {
            Ok(()) => {}
            // A result equal to the selection is not applied (no undo step).
            Err(TransformError::Unchanged) => {
                assert_eq!(d.state_fingerprint(), before, "{}", tool.id);
                assert_eq!(h.undo_len(), 0, "{}", tool.id);
                continue;
            }
            Err(e) => panic!("{}: {e}", tool.id),
        }
        assert_eq!(h.undo_len(), 1, "{}: one undo step", tool.id);
        let after = d.state_fingerprint();
        // The document holds exactly the run's notes in the scoped tracks.
        for (slot, st) in r.tracks.iter().enumerate() {
            let t = d.track(st.index).unwrap();
            let mut got: Vec<(u64, u64, u8, u32)> = Vec::new();
            t.notes.for_each_selected(|v| {
                got.push((v.start_tick, v.duration_ticks, v.attrs.pitch(), v.sub.0))
            });
            let mut want: Vec<(u64, u64, u8, u32)> = r
                .track_notes(slot as u16)
                .iter()
                .map(|n| (n.start, n.dur, n.pitch, n.sub))
                .collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "{}: committed notes differ", tool.id);
        }
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), before, "{}: undo inexact", tool.id);
        h.redo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), after, "{}: redo inexact", tool.id);
        // Save / reopen keeps the result exactly (contract §150).
        let (back, _) = project_from_bytes(&project_to_bytes(&d).unwrap()).unwrap();
        assert_eq!(
            back.state_fingerprint(),
            after,
            "{}: save / reopen differs",
            tool.id
        );
        // A run whose notes were applied is stale afterwards (a run that only
        // writes controller points over unchanged notes stays valid).
        if notes_of(&d) != notes_before {
            assert_eq!(
                commit(&mut h, &mut d, &r, "Tool"),
                Err(TransformError::Stale),
                "{}",
                tool.id
            );
        }
    }
}

#[test]
fn an_unchanged_result_records_no_undo_step() {
    // Transpose by 0 (its defaults) and Paint without a stroke return the
    // selection itself: Apply reports Unchanged, no undo step, no change.
    let mut d = fixture(0);
    let mut h = History::default();
    let before = d.state_fingerprint();
    for id in ["transpose", "paint"] {
        let tool = plumspirit_engine::transform::tool(id).unwrap();
        let set = base_set(tool);
        let sc = scopes(&d);
        let src = collect_for(tool, &d, &sc, 10_000_000).unwrap();
        let mut ctx = ctx_of(&d);
        ctx.stroke = None;
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let k = tool.kernel(&set, &b).unwrap();
        let r = run_kernel(
            k.as_ref(),
            &src,
            tool.boundary(&set),
            &Limits::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            commit(&mut h, &mut d, &r, "Tool"),
            Err(TransformError::Unchanged),
            "{id}"
        );
        assert!(TransformError::Unchanged.to_string().ends_with('.'));
        assert_eq!(h.undo_len(), 0, "{id}");
        assert_eq!(d.state_fingerprint(), before, "{id}");
    }
    // A real change still applies as one step.
    let tool = plumspirit_engine::transform::tool("transpose").unwrap();
    let mut set = base_set(tool);
    set.set("semis", ParamValue::Int(1));
    set.set("b.pitch", ParamValue::Choice(1));
    let r = run(&d, tool, &set).unwrap();
    assert!(!r.notes.is_empty());
    let sc = scopes(&d);
    let src = collect_for(tool, &d, &sc, 10_000_000).unwrap();
    let ctx = ctx_of(&d);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = tool.kernel(&set, &b).unwrap();
    let r = run_kernel(
        k.as_ref(),
        &src,
        tool.boundary(&set),
        &Limits::default(),
        None,
    )
    .unwrap();
    commit(&mut h, &mut d, &r, "Tool").unwrap();
    assert_eq!(h.undo_len(), 1);
}

#[test]
fn empty_selections_and_single_notes_are_handled() {
    let mut d = fixture(0);
    for i in 0..d.track_count() {
        d.track_mut(i).unwrap().notes.clear_selection();
    }
    for tool in registry() {
        let sc: Vec<TrackScope> = vec![TrackScope {
            index: 0,
            all: false,
            content: None,
        }];
        let src = Source::collect(&d, &sc, 1000).unwrap();
        assert!(src.notes.is_empty());
        let ctx = ctx_of(&d);
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let set = base_set(tool);
        let needs_selection = (tool.spec)().needs_selection;
        let result = tool.kernel(&set, &b).and_then(|k| {
            run_kernel(
                k.as_ref(),
                &src,
                tool.boundary(&set),
                &Limits::default(),
                None,
            )
        });
        match result {
            // Tools that work on a selection produce nothing from an empty one
            // (the toolbar says "Select notes first"); generators may create notes.
            Ok(r) => assert!(
                !needs_selection || (r.notes.is_empty() && r.events.is_empty()),
                "{}: output from an empty selection",
                tool.id
            ),
            Err(e) => {
                let m = e.to_string();
                assert!(m.ends_with('.'), "{}: not a sentence: {m}", tool.id);
            }
        }
    }
    // One note.
    let mut d = PlumDocument::new_project("one".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert(rec(960, 480, 60, 100, 0, 0), true)
        .unwrap();
    for tool in registry() {
        for draw in 0..5 {
            let set = randomized(tool, 300 + draw, Some(draw as usize));
            if let Ok(o) = run(&d, tool, &set) {
                check_valid(&d, tool.id, &o.notes);
            }
        }
    }
}

/// Runs every tool on `d` (everything selected) with its defaults, each
/// advanced function alone and `draws` random parameter sets: each run must
/// refuse with a sentence or give valid notes that commit and undo exactly —
/// no overflow, wrap-around or panic. Returns the failures.
fn extreme_runs(case: &str, d: &PlumDocument, draws: u64) -> Vec<String> {
    let draws: u64 = std::env::var("PLUMSPIRIT_HARNESS_DRAWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(draws);
    let mut failures: Vec<String> = Vec::new();
    for tool in harness_tools() {
        let mut sets = vec![base_set(tool)];
        for key in ADV_KEYS {
            let mut s = base_set(tool);
            s.set(key, ParamValue::Bool(true));
            s.set("b.pitch", ParamValue::Choice(1));
            sets.push(s);
        }
        for draw in 0..draws {
            let adv = (draw % 11) as usize;
            sets.push(randomized(tool, 70 + draw, (adv < 10).then_some(adv)));
        }
        // `PLUMSPIRIT_HARNESS_CASE=i` runs only set i (a slow or failing
        // case named by `PLUMSPIRIT_HARNESS_TIMES`, which prints a line when
        // a case starts and when it ends).
        let only_case: Option<usize> = std::env::var("PLUMSPIRIT_HARNESS_CASE")
            .ok()
            .and_then(|v| v.parse().ok());
        let times = std::env::var_os("PLUMSPIRIT_HARNESS_TIMES").is_some();
        for (i, set) in sets.iter().enumerate() {
            if only_case.is_some_and(|c| c != i) {
                continue;
            }
            if only_case.is_some() {
                dump_case(tool, set);
            }
            if times {
                eprintln!("{case}\t{}\t{i}\tstart", tool.id);
            }
            let clock = std::time::Instant::now();
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match run(d, tool, set) {
                    Ok(o) => {
                        check_valid(d, tool.id, &o.notes);
                        // The result commits and undoes exactly.
                        let mut d2 = d.clone();
                        let mut h2 = History::default();
                        let before = d2.state_fingerprint();
                        let sc = scopes(&d2);
                        let src = collect_for(tool, &d2, &sc, 10_000_000).unwrap();
                        let ctx = ctx_of(&d2);
                        let b = BuildCtx {
                            ctx: &ctx,
                            stats: &src.stats,
                            tracks: &src.tracks,
                        };
                        let k = tool.kernel(set, &b).unwrap();
                        let r = run_kernel(
                            k.as_ref(),
                            &src,
                            tool.boundary(set),
                            &Limits::default(),
                            None,
                        )
                        .unwrap();
                        match commit(&mut h2, &mut d2, &r, "Tool") {
                            Ok(()) => {
                                h2.undo(&mut d2).unwrap();
                                assert_eq!(d2.state_fingerprint(), before, "undo inexact");
                            }
                            Err(TransformError::Unchanged) => {
                                assert_eq!(d2.state_fingerprint(), before, "unchanged");
                            }
                            // V09-S52: a result larger than the undo memory
                            // (a generator can make millions of notes) is
                            // refused by the history with a sentence, and
                            // nothing changes.
                            Err(TransformError::History(m)) if m.contains("undo memory") => {
                                assert!(m.ends_with('.'), "not a sentence: {m}");
                                assert_eq!(
                                    d2.state_fingerprint(),
                                    before,
                                    "refused commit changed"
                                );
                            }
                            Err(e) => panic!("commit: {e}"),
                        }
                    }
                    Err(e) => {
                        let m = e.to_string();
                        assert!(m.ends_with('.'), "not a sentence: {m}");
                    }
                }
            }));
            if times {
                eprintln!("{case}\t{}\t{i}\t{}", tool.id, clock.elapsed().as_millis());
            }
            if let Err(p) = out {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                failures.push(format!("{case}: {} (set {i}): {msg}", tool.id));
            }
        }
    }
    failures
}

/// Prints a selected harness case as preset text (the settings to reproduce
/// it in the app or in a test).
fn dump_case(tool: &ToolDef, set: &ParamSet) {
    let p = plumspirit_engine::transform::preset::Preset {
        tool: tool.id.to_owned(),
        name: "harness case".to_owned(),
        values: set.clone(),
    };
    eprintln!("{}", plumspirit_engine::transform::preset::to_text(&p));
}

/// Notes at the far end of the tick range: chords, stacks and a note ending
/// exactly at `u64::MAX` (contract §150 "extreme ticks").
fn far_end_doc() -> PlumDocument {
    let top = u64::MAX;
    let mut d = PlumDocument::new_project("far".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(1), 1)],
        },
    )
    .unwrap();
    let mut v = Vec::new();
    for k in 0..24u64 {
        v.push(rec(
            top - 60_000 + k * 1900,
            1000,
            40 + (k % 30) as u8,
            90,
            (k % 2) as u8,
            (k % 2) as u32,
        ));
    }
    for p in [60u8, 64, 67] {
        v.push(rec(top - 3000, 2000, p, 100, 0, 0));
    }
    v.push(rec(top - 1000, 1000, 72, 100, 0, 0));
    v.push(rec(top - 1, 1, 73, 100, 1, 1));
    v.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

#[test]
fn notes_at_the_far_end_of_the_tick_range_never_overflow() {
    let failures = extreme_runs("far end", &far_end_doc(), 8);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The longest possible gate: a note spanning the whole tick range next to
/// notes at tick 0 and at the far end (spatial tools work at their grid cap).
#[test]
fn a_note_spanning_the_whole_tick_range_never_overflows() {
    let mut d = far_end_doc();
    let t = d.track_mut(0).unwrap();
    t.notes
        .insert(rec(0, u64::MAX - 1, 50, 80, 0, 0), true)
        .unwrap();
    t.notes.insert(rec(0, 1, 51, 80, 1, 1), true).unwrap();
    let failures = extreme_runs("whole range", &d, 2);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn explosive_expansions_are_refused_before_allocating() {
    // One note of 2^40 ticks: arpeggiating or chopping it at 1/16 would create
    // billions of notes — refused by the preflight, never attempted.
    let mut d = PlumDocument::new_project("big".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert(rec(0, 1 << 40, 60, 100, 0, 0), true)
        .unwrap();
    for id in ["arpeggiate", "chop"] {
        let tool = plumspirit_engine::transform::tool(id).unwrap();
        match run(&d, tool, &base_set(tool)) {
            Err(TransformError::TooLarge(m)) => assert!(m.contains("limit"), "{m}"),
            other => panic!(
                "{id}: expected a refusal, got {:?}",
                other.map(|o| o.notes.len())
            ),
        }
    }
}

#[test]
fn a_large_selection_transforms_quickly() {
    let d = fixture(100_000);
    for tool in registry() {
        let t = std::time::Instant::now();
        let set = randomized(tool, 9, None);
        let _ = run(&d, tool, &set);
        let ms = t.elapsed().as_millis();
        // Debug build budget; release numbers are in PERFORMANCE.md.
        assert!(ms < 20_000, "{} took {ms} ms on 100k notes", tool.id);
    }
}

/// Release sweep of the preflight bound over many random settings, reporting
/// every tool that exceeds it (instead of stopping at the first):
/// `PLUMSPIRIT_HARNESS_DRAWS=300 cargo test --release -p plumspirit_engine
/// --test v07_tools preflight_sweep -- --ignored --nocapture`.
#[test]
#[ignore = "sweep: run with --release --ignored --nocapture"]
fn preflight_sweep() {
    let d = fixture(0);
    let draws: u64 = std::env::var("PLUMSPIRIT_HARNESS_DRAWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let sc = scopes(&d);
    let ctx = ctx_of(&d);
    let mut bad = Vec::new();
    for tool in harness_tools() {
        let mut worst: Option<(usize, u64, Option<usize>, u64)> = None;
        for draw in 0..draws {
            let adv = (draw % 11) as usize;
            let adv = (adv < 10).then_some(adv);
            let set = randomized(tool, 5000 + draw, adv);
            let Ok(src) = collect_for(tool, &d, &sc, 10_000_000) else {
                continue;
            };
            let b = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let Ok(k) = tool.kernel(&set, &b) else {
                continue;
            };
            let est = k.estimate(&src.stats).notes + src.locked.len() as u64;
            let Ok(r) = run_kernel(
                k.as_ref(),
                &src,
                tool.boundary(&set),
                &Limits::default(),
                None,
            ) else {
                continue;
            };
            let n = r.notes.len();
            if n as u64 > est && worst.is_none_or(|w| n as u64 * w.1 > w.0 as u64 * est) {
                worst = Some((n, est, adv, draw));
            }
        }
        if let Some(w) = worst {
            let set = randomized(tool, 5000 + w.3, w.2);
            let mut vals: Vec<String> = set.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
            vals.sort();
            bad.push(format!(
                "{}: {} out > {} estimated (advanced {:?}, draw {})\n    {}",
                tool.id,
                w.0,
                w.1,
                w.2.map(|a| a + 1),
                w.3,
                vals.join(" ")
            ));
        }
    }
    for b in &bad {
        println!("{b}");
    }
    assert!(
        bad.is_empty(),
        "{} tool(s) exceed their preflight",
        bad.len()
    );
}

/// v0.9 256-Key Mode (V09-S20): every tool runs in a 256-key project — the
/// fixture moved up 64 keys plus notes on keys 128, 200, 254 and 255 — with
/// its defaults, with each advanced function and with seeded random
/// parameters: no panic, every result inside 0–255, deterministic, the
/// preflight an upper bound.
#[test]
fn every_tool_runs_in_a_256_key_project() {
    let wide = KeyRange::Extended256;
    let d = fixture_in(0, wide, 64, true);
    let draws: u64 = std::env::var("PLUMSPIRIT_HARNESS_DRAWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let mut failures = Vec::new();
    for tool in harness_tools() {
        let r = std::panic::catch_unwind(|| {
            let out = run_in(&d, tool, &base_set_in(tool, wide), 64)
                .unwrap_or_else(|e| panic!("{}: {e}", tool.id));
            check_valid(&d, tool.id, &out.notes);
            for (i, key) in ADV_KEYS
                .iter()
                .enumerate()
                .take((tool.spec)().advanced.len())
            {
                let mut set = base_set_in(tool, wide);
                set.set(key, ParamValue::Bool(true));
                set.set("b.pitch", ParamValue::Choice(1));
                let out = run_in(&d, tool, &set, 64)
                    .unwrap_or_else(|e| panic!("{} advanced {}: {e}", tool.id, i + 1));
                check_valid(&d, tool.id, &out.notes);
            }
            let only_draw: Option<u64> = std::env::var("PLUMSPIRIT_HARNESS_CASE")
                .ok()
                .and_then(|v| v.parse().ok());
            for draw in 0..draws {
                if only_draw.is_some_and(|c| c != draw) {
                    continue;
                }
                let adv = (draw % 11) as usize;
                let set = randomized_in(tool, 1000 + draw, (adv < 10).then_some(adv), wide);
                if only_draw.is_some() {
                    dump_case(tool, &set);
                }
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match (run_in(&d, tool, &set, 64), run_in(&d, tool, &set, 64)) {
                        (Ok(a), Ok(b)) => {
                            check_valid(&d, tool.id, &a.notes);
                            assert!(a.same(&b), "{}: not deterministic", tool.id);
                        }
                        (Err(a), Err(b)) => assert_eq!(a, b, "{}", tool.id),
                        (a, b) => panic!(
                            "{}: nondeterministic success {:?} {:?}",
                            tool.id,
                            a.is_ok(),
                            b.is_ok()
                        ),
                    }
                }));
                if let Err(e) = r {
                    let msg = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    panic!("draw {draw}: {msg}");
                }
            }
        });
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failures.push(format!("{}: {}", tool.id, msg));
        }
    }
    assert!(
        failures.is_empty(),
        "{} tools fail in 256-Key Mode:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A standard selection gives the same result in a 256-key project (V09-S20,
/// contract §242 "Normal project: same behavior"): every tool with its
/// defaults and with each advanced function, results above 127 aside (the
/// wider keyboard keeps notes a standard project discards). The only
/// differences are the functions defined over the whole keyboard: the
/// pitch-dependent velocity curve and slice count run from the lowest to the
/// highest key, and Contour Trace's offset contour meets the keyboard's top
/// edge at 127 only in a standard project.
#[test]
fn a_standard_selection_gives_the_same_result_on_the_256_key_keyboard() {
    let std = fixture(0);
    let wide = fixture_in(0, KeyRange::Extended256, 0, false);
    let by_design = [("level_scale", 6), ("slice", 6)];
    let mut diffs = Vec::new();
    for tool in registry() {
        for adv in 0..=(tool.spec)().advanced.len() {
            let mut set = base_set(tool);
            if adv > 0 {
                set.set(ADV_KEYS[adv - 1], ParamValue::Bool(true));
            }
            set.set("b.pitch", ParamValue::Choice(0));
            let same = match (run(&std, tool, &set), run(&wide, tool, &set)) {
                (Ok(a), Ok(b)) => {
                    let kept: Vec<TNote> =
                        b.notes.iter().copied().filter(|n| n.pitch <= 127).collect();
                    a.notes == kept && a.events == b.events && a.groups == b.groups
                }
                (Err(a), Err(b)) => a == b,
                _ => false,
            };
            if !same && !by_design.contains(&(tool.id, adv)) {
                diffs.push(format!("{} (advanced {adv})", tool.id));
            }
        }
    }
    assert!(
        diffs.is_empty(),
        "differ on the 256-key keyboard: {diffs:?}"
    );
}

/// Octave registers reach the 256-key range (V09-S20): generators placed by
/// an octave number give, ten octaves higher, exactly the standard result
/// moved up 120 keys.
#[test]
fn octave_registers_reach_the_256_key_range() {
    let std = fixture(0);
    let wide = fixture_in(0, KeyRange::Extended256, 120, false);
    for (id, key, adv) in [
        ("chord_progression", "octave", 0usize),
        ("randomize", "octave", 0),
        ("chord_stamp", "a5.octave", 5),
    ] {
        let tool = registry().iter().find(|t| t.id == id).unwrap();
        let mut set = base_set(tool);
        if adv > 0 {
            set.set(ADV_KEYS[adv - 1], ParamValue::Bool(true));
        }
        set.set("b.pitch", ParamValue::Choice(0));
        let Some(ParamValue::Int(oct)) = set.get_raw(key).cloned() else {
            panic!("{id}: no {key}");
        };
        let mut high = set.clone();
        high.set(key, ParamValue::Int(oct + 10));
        let (in_range, _) = (tool.spec)()
            .params
            .iter()
            .find(|p| p.key == key)
            .unwrap()
            .range_in(KeyRange::Extended256)
            .map(|r| (r.0 <= oct + 10 && oct + 10 <= r.1, r))
            .unwrap();
        assert!(
            in_range,
            "{id}: octave {} outside the 256-key range",
            oct + 10
        );
        let a = run(&std, tool, &set).unwrap();
        let b = run_in(&wide, tool, &high, 120).unwrap();
        let mut moved = a.notes.clone();
        for n in &mut moved {
            n.pitch += 120;
        }
        let kept: Vec<TNote> = b
            .notes
            .iter()
            .copied()
            .filter(|n| (120..=247).contains(&n.pitch))
            .collect();
        assert!(!moved.is_empty(), "{id}: no result");
        assert_eq!(moved, kept, "{id}: not the standard result ten octaves up");
    }
}
