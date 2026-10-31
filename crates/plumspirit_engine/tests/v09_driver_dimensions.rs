//! v0.9 V09-0143 (contract §098 "other tool-specific dimension", §232
//! "tools can bind it differently"): the Subchannel / Pitch Offset Drivers
//! of a tool that declares a driver dimension (`definition::driver_dimension`)
//! can be driven by the ordinal its kernel gives each note it makes — the
//! copy, repeat, stroke, step, piece … — through one shared mechanism
//! (`Output::set_dim`, `drivers::Input::Tool`).
//!
//! Each tool is checked on a fixture whose ordinals are known by
//! construction: the run with the Pitch Offset Driver by the tool's
//! dimension (a step of +1 semitone per unit) equals the run without it with
//! every note moved up by its expected ordinal.

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::transform::definition::{self, driver_dimension};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TNote, TrackScope, run_kernel, tool};
use std::collections::BTreeSet;

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// One track (four subchannels) holding `notes`, all selected.
fn doc(notes: &[(u64, u64, u8)]) -> PlumDocument {
    let mut d = PlumDocument::new_project("dims".into(), Bpm::DEFAULT, 960);
    let subs: Vec<Subchannel> = (1..4)
        .map(|i| Subchannel::new(SubchannelId(i), i as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    let mut v: Vec<NoteRecord> = notes.iter().map(|&(s, l, p)| rec(s, l, p)).collect();
    v.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

fn set(id: &str, pairs: &[(&str, ParamValue)]) -> ParamSet {
    let def = definition::find(id).unwrap();
    let mut s = ParamSet::defaults(&def.specs());
    for (k, v) in pairs {
        assert!(def.param(k).is_some(), "{id} has no {k}");
        s.set(k, v.clone());
    }
    s
}

/// Tool `id` with `s` on `d`: the result notes and whether the kernel
/// offered a per-note (lazy) preview.
fn run(d: &PlumDocument, id: &str, s: &ParamSet) -> (Vec<TNote>, bool) {
    run_in(d, id, s, &|_| {})
}

/// [`run`] with the context changed by `tweak` (a stroke, a group table).
fn run_in(
    d: &PlumDocument,
    id: &str,
    s: &ParamSet,
    tweak: &dyn Fn(&mut Ctx),
) -> (Vec<TNote>, bool) {
    let td = tool(id).unwrap();
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(td, d, &sc, 1_000_000).unwrap();
    let mut c = Ctx::simple(960);
    c.keys = d.project().keys;
    tweak(&mut c);
    let b = BuildCtx {
        ctx: &c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = td.kernel(s, &b).unwrap();
    let lazy = k.as_point().is_some();
    let r = run_kernel(k.as_ref(), &src, td.boundary(s), &Limits::default(), None).unwrap();
    (r.notes, lazy)
}

const ON: ParamValue = ParamValue::Bool(true);

/// The Pitch (or Subchannel) Offset Driver by the tool's dimension: `step`
/// per unit.
fn by_dim(pitch: bool, step: i64) -> Vec<(&'static str, ParamValue)> {
    if pitch {
        vec![
            ("m.pitch", ON),
            ("m.pitch.by", ParamValue::Choice(6)),
            ("m.pitch.map", ParamValue::Choice(0)),
            ("m.pitch.step", ParamValue::Int(step)),
            ("m.pitch.every.n", ParamValue::Int(1)),
        ]
    } else {
        vec![
            ("m.sub", ON),
            ("m.sub.by", ParamValue::Choice(6)),
            ("m.sub.map", ParamValue::Choice(0)),
            ("m.sub.step", ParamValue::Int(step)),
            ("m.sub.every.n", ParamValue::Int(1)),
        ]
    }
}

fn key(n: &TNote) -> (u16, u64, u8, u64, u32, u8, u8, u8, u32, u8) {
    (
        n.track, n.start, n.pitch, n.dur, n.sub, n.vel, n.ch, n.rel, n.group, n.flags,
    )
}

fn sorted(mut v: Vec<TNote>) -> Vec<TNote> {
    v.sort_by_key(key);
    v
}

/// Dense rank of each note's distance in time from `s0` (0 = the nearest
/// distinct distance): the ordinal of a single source note's (or chord's)
/// copies, repeats, strokes, steps or pieces.
fn rank_from(s0: u64) -> impl Fn(&[TNote]) -> Vec<u32> {
    move |notes: &[TNote]| {
        let ds: BTreeSet<u64> = notes.iter().map(|n| n.start.abs_diff(s0)).collect();
        let ds: Vec<u64> = ds.into_iter().collect();
        notes
            .iter()
            .map(|n| ds.binary_search(&n.start.abs_diff(s0)).unwrap() as u32)
            .collect()
    }
}

/// The check: with the Pitch Offset Driver by the tool's dimension at +1
/// per unit, the result is the plain result with each note `oracle`'s
/// ordinal higher; at +0 it is the plain result. Returns the largest
/// ordinal seen (so a case can require several).
fn check(
    id: &str,
    d: &PlumDocument,
    pairs: &[(&str, ParamValue)],
    oracle: &dyn Fn(&[TNote]) -> Vec<u32>,
) -> u32 {
    check_in(id, d, pairs, oracle, &|_| {})
}

/// [`check`] with the context changed by `tweak`.
fn check_in(
    id: &str,
    d: &PlumDocument,
    pairs: &[(&str, ParamValue)],
    oracle: &dyn Fn(&[TNote]) -> Vec<u32>,
    tweak: &dyn Fn(&mut Ctx),
) -> u32 {
    let run = |d: &PlumDocument, id: &str, s: &ParamSet| run_in(d, id, s, tweak);
    assert!(driver_dimension(id).is_some(), "{id} declares no dimension");
    let (plain, _) = run(d, id, &set(id, pairs));
    assert!(!plain.is_empty(), "{id}: no notes");
    let dims = oracle(&plain);
    let want = sorted(
        plain
            .iter()
            .zip(&dims)
            .map(|(n, &k)| TNote {
                pitch: n.pitch + k as u8,
                ..*n
            })
            .collect(),
    );
    let mut with: Vec<(&str, ParamValue)> = pairs.to_vec();
    with.extend(by_dim(true, 1));
    let (got, lazy) = run(d, id, &set(id, &with));
    assert!(!lazy, "{id}: the tool's dimension needs the whole run");
    assert_eq!(sorted(got), want, "{id}: dims {dims:?}");
    let mut zero: Vec<(&str, ParamValue)> = pairs.to_vec();
    zero.extend(by_dim(true, 0));
    assert_eq!(
        sorted(run(d, id, &set(id, &zero)).0),
        sorted(plain),
        "{id}: +0"
    );
    dims.into_iter().max().unwrap_or(0)
}

#[test]
fn the_menu_offers_the_tools_dimension_only_where_it_has_one() {
    use plumspirit_engine::transform::param::ParamKind;
    let mut with = 0;
    for def in definition::definitions() {
        let Some(p) = def.param("m.pitch.by") else {
            assert!(
                driver_dimension(def.id).is_none(),
                "{}: a dimension without the offset drivers",
                def.id
            );
            continue;
        };
        let ParamKind::Choice(opts) = &p.spec.kind else {
            panic!("{}: m.pitch.by is not a menu", def.id);
        };
        let sub = def.param("m.sub.by").unwrap();
        let ParamKind::Choice(sopts) = &sub.spec.kind else {
            panic!("{}", def.id);
        };
        assert_eq!(opts, sopts, "{}: both drivers share the inputs", def.id);
        match driver_dimension(def.id) {
            Some(dim) => {
                with += 1;
                assert_eq!(opts.len(), 7, "{}", def.id);
                assert_eq!(opts[6], dim.name, "{}", def.id);
                assert_eq!(
                    &opts[..6],
                    plumspirit_engine::transform::drivers::Input::NAMES
                );
                assert!(dim.help.starts_with(dim.name), "{}: {}", def.id, dim.help);
            }
            None => assert_eq!(opts.len(), 6, "{}", def.id),
        }
    }
    assert!(with >= 6, "{with}");
}

#[test]
fn duplicate_copies_are_its_dimension() {
    let d = doc(&[(0, 240, 60)]);
    let m = check(
        "duplicate",
        &d,
        &[("a1", ON), ("a1.count", ParamValue::Int(3))],
        &rank_from(0),
    );
    assert_eq!(m, 3);
}

#[test]
fn repeat_repeats_flam_strokes_and_chop_pieces_are_their_dimensions() {
    // One long note: Repeat's repeats and Chop's pieces follow in time.
    let d = doc(&[(3840, 1920, 60)]);
    assert!(check("repeat", &d, &[], &rank_from(3840)) >= 3);
    assert!(check("chop", &d, &[], &rank_from(3840)) >= 3);
    // Flam: three strokes before the note (0 = the note, 1 = the nearest).
    let m = check(
        "flam",
        &d,
        &[("a1", ON), ("a1.count", ParamValue::Int(3))],
        &rank_from(3840),
    );
    assert_eq!(m, 3);
}

#[test]
fn strum_positions_and_arpeggio_steps_are_their_dimensions() {
    let chord = [
        (3840, 1920, 60),
        (3840, 1920, 64),
        (3840, 1920, 67),
        (3840, 1920, 72),
    ];
    let d = doc(&chord);
    assert_eq!(check("strum", &d, &[], &rank_from(3840)), 3);
    assert!(check("arpeggiate", &d, &[], &rank_from(3840)) >= 3);
}

/// The Subchannel Offset Driver reads the same dimension: Duplicate's copy
/// k lands k subchannels further (the track's four, cycled).
#[test]
fn the_subchannel_driver_reads_the_same_dimension() {
    let d = doc(&[(0, 240, 60)]);
    let mut pairs = vec![("a1", ON), ("a1.count", ParamValue::Int(3))];
    pairs.extend(by_dim(false, 1));
    let (got, _) = run(&d, "duplicate", &set("duplicate", &pairs));
    let mut by_start: Vec<(u64, u32)> = got.iter().map(|n| (n.start, n.sub)).collect();
    by_start.sort_unstable();
    let subs: Vec<u32> = by_start.iter().map(|x| x.1).collect();
    assert_eq!(subs, vec![0, 1, 2, 3], "{got:?}");
}

/// The same ordinals on the tools' v0.9 kernels (a new function on that
/// leaves the timing alone hands each tool over to its v0.9 kernel).
#[test]
fn the_v09_kernels_give_the_same_ordinals() {
    let one = doc(&[(3840, 1920, 60)]);
    let chord = doc(&[
        (3840, 1920, 60),
        (3840, 1920, 64),
        (3840, 1920, 67),
        (3840, 1920, 72),
    ]);
    let r = rank_from(3840);
    assert_eq!(
        check(
            "duplicate",
            &one,
            &[("a1", ON), ("a1.count", ParamValue::Int(3)), ("n04", ON)],
            &r
        ),
        3
    );
    assert!(check("repeat", &one, &[("n01", ON)], &r) >= 2);
    assert_eq!(
        check(
            "flam",
            &one,
            &[("a1", ON), ("a1.count", ParamValue::Int(3)), ("n04", ON)],
            &r
        ),
        3
    );
    assert!(check("chop", &one, &[("n07", ON)], &r) >= 3);
    assert_eq!(check("strum", &chord, &[("n07", ON)], &r), 3);
    assert!(check("arpeggiate", &chord, &[("n04", ON)], &r) >= 3);
}

/// Like [`rank_from`] over the notes the tool made: a kept source note
/// (pitch `keep`) is 0, the others are ranked by their start (0 = the
/// earliest distinct start).
fn rank_made(keep: u8) -> impl Fn(&[TNote]) -> Vec<u32> {
    move |notes: &[TNote]| {
        let ss: BTreeSet<u64> = notes
            .iter()
            .filter(|n| n.pitch != keep)
            .map(|n| n.start)
            .collect();
        let ss: Vec<u64> = ss.into_iter().collect();
        notes
            .iter()
            .map(|n| {
                if n.pitch == keep {
                    0
                } else {
                    ss.binary_search(&n.start).unwrap() as u32
                }
            })
            .collect()
    }
}

/// A stroke along time: one point every 240 ticks, its pitch stepping
/// `rise` semitones a point (cycled over three points).
fn rising_stroke(rise: f64) -> plumspirit_engine::transform::Stroke {
    use plumspirit_engine::transform::{Stroke, StrokePoint};
    Stroke {
        points: (0..16u64)
            .map(|k| StrokePoint {
                tick: k * 240,
                pitch: 60.0 + rise * (k % 3) as f64,
                t: k as f64 * 0.01,
            })
            .collect(),
    }
}

#[test]
fn chords_tones_variants_steps_cells_and_glide_steps_are_their_dimensions() {
    // Chord Progression: four chords after one another; the kept selected
    // note (pitch 100) is 0.
    let far = doc(&[(0, 240, 100)]);
    assert_eq!(check("chord_progression", &far, &[], &rank_made(100)), 3);
    // Chord Stamp: one note stamped into a chord, tone by tone upwards.
    let one = doc(&[(0, 480, 60)]);
    let by_pitch = |notes: &[TNote]| -> Vec<u32> {
        let ps: BTreeSet<u8> = notes.iter().map(|n| n.pitch).collect();
        let ps: Vec<u8> = ps.into_iter().collect();
        notes
            .iter()
            .map(|n| ps.binary_search(&n.pitch).unwrap() as u32)
            .collect()
    };
    assert!(check("chord_stamp", &one, &[], &by_pitch) >= 2);
    // Riff: three variants stacked in the track's subchannels 0, 1, 2.
    let by_sub = |notes: &[TNote]| -> Vec<u32> { notes.iter().map(|n| n.sub).collect() };
    assert_eq!(
        check(
            "riff",
            &far,
            &[("n08", ON), ("n08.count", ParamValue::Int(3))],
            &by_sub
        ),
        2
    );
    // Draw along a stroke climbing in time; Paint along a level one (one
    // cell a column, so the order along the stroke is the order in time).
    let stroke = |c: &mut Ctx| c.stroke = Some(std::sync::Arc::new(rising_stroke(1.0)));
    let level = |c: &mut Ctx| c.stroke = Some(std::sync::Arc::new(rising_stroke(0.0)));
    let far = doc(&[(100_000, 240, 100)]);
    assert!(check_in("draw", &far, &[], &rank_made(100), &stroke) >= 10);
    assert!(check_in("paint", &far, &[], &rank_made(100), &level) >= 10);
    // Slide [a10]: each glide drawn as chromatic steps, then the note.
    let line = doc(&[(0, 960, 60), (960, 960, 66), (1920, 960, 58)]);
    let within = |notes: &[TNote]| -> Vec<u32> {
        notes
            .iter()
            .map(|n| {
                // The steps of the note sounding in this 960-tick slot.
                let ss: BTreeSet<u64> = notes
                    .iter()
                    .filter(|m| m.start / 960 == n.start / 960)
                    .map(|m| m.start)
                    .collect();
                ss.iter().position(|&s| s == n.start).unwrap() as u32
            })
            .collect()
    };
    assert!(check("slide", &line, &[("a10", ON)], &within) >= 3);
}

/// Group: each existing group's place in time (1 = the earliest),
/// ungrouped notes 0.
#[test]
fn groups_in_time_order_are_groups_dimension() {
    use plumspirit_engine::model::{GroupInfo, GroupTable};
    let mut table = GroupTable::default();
    let a = table.add(GroupInfo::default());
    let b = table.add(GroupInfo::default());
    let mut d = PlumDocument::new_project("dims".into(), Bpm::DEFAULT, 960);
    let notes = [
        (1920u64, 60u8, a),
        (2400, 62, a),
        (0, 64, b),
        (480, 65, b),
        (4000, 67, 0),
    ];
    let mut v: Vec<NoteRecord> = notes
        .iter()
        .map(|&(s, p, g)| NoteRecord {
            group: g,
            ..rec(s, 240, p)
        })
        .collect();
    v.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    // Each output group's place by its first note (a function may give the
    // ungrouped note a group of its own); group 0 is 0.
    let want = move |ns: &[TNote]| -> Vec<u32> {
        let mut first: std::collections::BTreeMap<u32, u64> = Default::default();
        for n in ns.iter().filter(|n| n.group != 0) {
            let e = first.entry(n.group).or_insert(u64::MAX);
            *e = (*e).min(n.start);
        }
        let mut by: Vec<(u64, u32)> = first.iter().map(|(&g, &s)| (s, g)).collect();
        by.sort_unstable();
        ns.iter()
            .map(|n| match n.group {
                0 => 0,
                g => by.iter().position(|x| x.1 == g).unwrap() as u32 + 1,
            })
            .collect()
    };
    let _ = (a, b);
    let groups = move |c: &mut Ctx| c.groups = std::sync::Arc::new(table.clone());
    let keep = [("action", ParamValue::Choice(2))];
    assert_eq!(check_in("group", &d, &keep, &want, &groups), 2);
    // The v0.9 kernel (a new function on that keeps the groups' places).
    let mut nine = keep.to_vec();
    nine.push(("n01", ON));
    assert!(check_in("group", &d, &nine, &want, &groups) >= 2);
}

/// Each note of tool `id`'s result with its driver dimension, read through
/// the Subchannel Offset Driver by the dimension (+1 subchannel a unit,
/// unbounded): the driven run equals the plain one but for the subchannels,
/// which differ by exactly the dimension. Notes are paired by everything
/// but the subchannel (equal ones in subchannel order).
fn dims_of(
    id: &str,
    d: &PlumDocument,
    pairs: &[(&str, ParamValue)],
    tweak: &dyn Fn(&mut Ctx),
) -> Vec<(TNote, u32)> {
    type K = (u16, u64, u8, u64, u8, u8, u8, u32, u8);
    let k = |n: &TNote| -> K {
        (
            n.track, n.start, n.pitch, n.dur, n.vel, n.ch, n.rel, n.group, n.flags,
        )
    };
    let (plain, _) = run_in(d, id, &set(id, pairs), tweak);
    let mut with: Vec<(&str, ParamValue)> = pairs.to_vec();
    with.extend(by_dim(false, 1));
    with.push(("m.sub.wrap", ParamValue::Choice(4)));
    let (got, lazy) = run_in(d, id, &set(id, &with), tweak);
    assert!(!lazy, "{id}");
    let group = |v: &[TNote]| {
        let mut m: std::collections::BTreeMap<K, Vec<u32>> = Default::default();
        for n in v {
            m.entry(k(n)).or_default().push(n.sub);
        }
        for s in m.values_mut() {
            s.sort_unstable();
        }
        m
    };
    let (gp, gd) = (group(&plain), group(&got));
    assert_eq!(
        gp.keys().collect::<Vec<_>>(),
        gd.keys().collect::<Vec<_>>(),
        "{id}: the driver changed more than subchannels"
    );
    let mut out = Vec::new();
    for (key, ps) in &gp {
        let ds = &gd[key];
        assert_eq!(ps.len(), ds.len(), "{id} {key:?}");
        // Several equal notes: pair each plain subchannel with a driven one
        // at or above it (the dimension is never negative).
        let mut ds = ds.clone();
        for &p in ps {
            let j = ds.iter().position(|&x| x >= p).expect("a driven note");
            let x = ds.remove(j);
            let n = plain.iter().find(|n| k(n) == *key && n.sub == p).unwrap();
            out.push((*n, x - p));
        }
    }
    out
}

fn with_dim(v: &[(TNote, u32)], d: u32) -> Vec<TNote> {
    sorted(v.iter().filter(|x| x.1 == d).map(|x| x.0).collect())
}

fn max_dim(v: &[(TNote, u32)]) -> u32 {
    v.iter().map(|x| x.1).max().unwrap_or(0)
}

/// Trail copies, echoes and copies of the geometry and field tools: the
/// copy at the full transform (or the note itself) is exactly the run
/// without the copies, the untransformed trail end is the source.
#[test]
fn geometry_and_field_copies_are_their_dimensions() {
    let d = doc(&[(0, 480, 60), (1920, 480, 72), (3840, 240, 66)]);
    let src = sorted(run(&d, "skew", &set("skew", &[])).0.clone());
    let none = |_: &mut Ctx| {};
    let plain = |id: &str, pairs: &[(&str, ParamValue)]| sorted(run(&d, id, &set(id, pairs)).0);
    // Skew [a10]: copy 0 where the note was … copy 3 fully skewed.
    let src_notes = sorted(
        [(0u64, 480u64, 60u8), (1920, 480, 72), (3840, 240, 66)]
            .iter()
            .map(|&(s, l, p)| TNote {
                start: s,
                dur: l,
                pitch: p,
                vel: 100,
                rel: 64,
                ..TNote::default()
            })
            .collect(),
    );
    let _ = src;
    let v = dims_of(
        "skew",
        &d,
        &[("a10", ON), ("a10.copies", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 3), plain("skew", &[]));
    assert_eq!(with_dim(&v, 0), src_notes);
    // Twirl [a9] (v0.8 kernel) and with a new function (v0.9 kernel).
    let v = dims_of(
        "twirl",
        &d,
        &[("a9", ON), ("a9.copies", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 3), plain("twirl", &[]));
    assert_eq!(with_dim(&v, 0), src_notes);
    // Perspective [a10]: echo 0 is the note.
    let v = dims_of(
        "perspective",
        &d,
        &[("a10", ON), ("a10.count", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 0), plain("perspective", &[]));
    // Shear n09: copy 0 is the sheared note.
    let v = dims_of(
        "shear",
        &d,
        &[("n09", ON), ("n09.copies", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 0), plain("shear", &[]));
    // Pulse Field [a10]: echo 0 is the pulsed note.
    let v = dims_of(
        "pulse_field",
        &d,
        &[("a10", ON), ("a10.count", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 0), plain("pulse_field", &[]));
    // Flow Field [a7]: the last trail copy is the whole flow.
    let v = dims_of(
        "flow_field",
        &d,
        &[("a7", ON), ("a7.count", ParamValue::Int(4))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    assert_eq!(with_dim(&v, 3), plain("flow_field", &[]));
    // Vortex Trails [a1]: trail 0 is the original.
    let v = dims_of(
        "vortex_trails",
        &d,
        &[("a1", ON), ("a1.count", ParamValue::Int(4))],
        &none,
    );
    assert!(max_dim(&v) >= 3, "{}", max_dim(&v));
    assert_eq!(with_dim(&v, 0), src_notes);
    // Kaleidoscope: sector 0 is the original sector.
    let v = dims_of(
        "kaleidoscope",
        &d,
        &[("a1", ON), ("a1.n", ParamValue::Int(6))],
        &none,
    );
    assert_eq!(max_dim(&v), 5);
    assert_eq!(with_dim(&v, 0), src_notes);
    // Trails: generation 0 is the source.
    let v = dims_of("trails", &d, &[], &none);
    assert_eq!(max_dim(&v), 6);
    assert_eq!(with_dim(&v, 0), src_notes);
}

/// Orbit copies and particles: one note of each per placement / particle
/// (the defaults sample each copy as one note).
#[test]
fn orbit_copies_and_particles_count_one_note_each() {
    let d = doc(&[(1920, 480, 64)]);
    let none = |_: &mut Ctx| {};
    for id in ["orbit", "particle_burst"] {
        let v = dims_of(id, &d, &[], &none);
        let m = max_dim(&v);
        assert!(m >= 3, "{id}: {m}");
        for k in 1..=m {
            assert_eq!(with_dim(&v, k).len(), 1, "{id}: {k}");
        }
    }
}

/// Tumours' stages in sequence, Note Outline's art and outline, Warp
/// Transform's tile rings.
#[test]
fn stages_outlines_tiles_and_shapes_are_their_dimensions() {
    let none = |_: &mut Ctx| {};
    // Tumours c25 in sequence: stage k lies after stage k − 1.
    let art = doc(&[(0, 480, 60), (480, 480, 62), (960, 480, 64)]);
    let v = dims_of(
        "tumours",
        &art,
        &[
            ("c25_evolution_morph", ON),
            ("c25_evolution_morph.stages", ParamValue::Int(3)),
        ],
        &none,
    );
    assert_eq!(max_dim(&v), 2);
    for k in 1..=2 {
        let prev = with_dim(&v, k - 1).iter().map(|n| n.end()).max().unwrap();
        let first = with_dim(&v, k).iter().map(|n| n.start).min().unwrap();
        assert!(first >= prev, "stage {k}: {first} < {prev}");
    }
    // Note Outline: the kept art is 0, the outline 1.
    let v = dims_of("note_outline", &art, &[], &none);
    assert_eq!(max_dim(&v), 1);
    assert_eq!(with_dim(&v, 0).len(), 3);
    assert!(with_dim(&v, 1).len() > 3);
    // Warp Transform, Fill / Duplicate tiled: ring 0 is one tile.
    let v = dims_of(
        "warp_transform",
        &art,
        &[
            ("density", ParamValue::Choice(1)),
            ("wrap_x", ParamValue::Choice(1)),
            ("scale_x", ParamValue::Float(50.0)),
        ],
        &none,
    );
    assert!(max_dim(&v) >= 1, "{}", max_dim(&v));
}

/// The ordinals of single functions of otherwise one-for-one tools:
/// Humanize's ensemble players, Flip's appended mirror, Distort's shards.
#[test]
fn players_mirror_copies_and_shards_are_their_dimensions() {
    let none = |_: &mut Ctx| {};
    let line = doc(&[
        (0, 480, 60),
        (960, 480, 64),
        (1920, 480, 67),
        (2880, 480, 71),
    ]);
    let plain = |id: &str, pairs: &[(&str, ParamValue)]| sorted(run(&line, id, &set(id, pairs)).0);
    // Humanize n06: player 0 is the humanized note.
    let v = dims_of(
        "humanize",
        &line,
        &[("n06", ON), ("n06.players", ParamValue::Int(3))],
        &none,
    );
    assert_eq!(max_dim(&v), 3);
    for k in 1..=3 {
        assert_eq!(with_dim(&v, k).len(), 4, "player {k}");
    }
    assert_eq!(with_dim(&v, 0).len(), 4);
    // Flip n01: the selection (0) and its appended mirror (1).
    let v = dims_of("flip", &line, &[("n01", ON)], &none);
    assert_eq!(max_dim(&v), 1);
    let src: Vec<TNote> = plain("transpose", &[]);
    assert_eq!(with_dim(&v, 0), src);
    assert_eq!(with_dim(&v, 1).len(), 4);
    // Distort n08: each note flies with its shard.
    let wide = doc(&[
        (0, 240, 40),
        (7680, 240, 90),
        (0, 240, 90),
        (7680, 240, 40),
        (3840, 240, 65),
    ]);
    let v = dims_of(
        "distort",
        &wide,
        &[("n08", ON), ("n08.shards", ParamValue::Int(5))],
        &none,
    );
    assert!((1..=4).contains(&max_dim(&v)), "{}", max_dim(&v));
}
