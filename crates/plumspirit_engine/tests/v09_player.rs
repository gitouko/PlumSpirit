//! V09-S17 step 2 (V09-0040, `docs/PLAYER.md` P1, P4): the MIDI Player
//! counts with the project's analysis index (`Scope::Project`) — and that
//! index must count exactly the notes of the export snapshot the Player
//! shows. Checked against a brute-force walk of
//! `ExportSnapshot::for_each_note` on every golden-corpus project, at seeded
//! ticks: total notes, notes started, notes ended (the Player's Passed),
//! polyphony, NPS, and the peaks up to the playhead.
//!
//! V09-S17 step 3 (V09-0041 / V09-0042, P3): the Player draws each track's
//! notes from the project's own stores (placements and the track's
//! correction applied inside the tile fill) — and those tiles must equal the
//! tiles of the track's export notes materialised in a store (the reference
//! only this test builds), at seeded levels and tiles of every corpus project
//! and after edits.

mod common;

use common::corpus::{apply, corpus, props};
use plumspirit_engine::analysis::canonical::window_lo;
use plumspirit_engine::analysis::profile::PeakProfile;
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing};
use plumspirit_engine::midi::{ExportOptions, ExportSnapshot, WideKeys};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::TrackProps;
use plumspirit_engine::model::{NoteRecord, NoteStore, NoteView};
use plumspirit_engine::view::tiles::{
    self, Outline, Reading, Scratch, SourcePlace, TileColors, TileOptions,
};

const SECOND: u64 = 1_000_000_000;

/// The snapshot the Player shows (P1): the default export, and in a
/// 256-Key Mode project every key kept.
fn player_snapshot(d: &PlumDocument) -> std::sync::Arc<ExportSnapshot> {
    let wide_keys = if d.project().keys == KeyRange::Extended256 {
        WideKeys::Experimental
    } else {
        ExportOptions::default().wide_keys
    };
    let options = ExportOptions {
        wide_keys,
        ..ExportOptions::default()
    };
    // The app's Player takes its snapshot with the engine's one definition.
    assert_eq!(ExportOptions::for_player(d.project().keys), options);
    ExportSnapshot::take(d, options)
}

/// The snapshot's notes as (start, end), sorted by start.
fn snapshot_notes(s: &ExportSnapshot) -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    s.for_each_note(|n| v.push((n.start, n.end)));
    v.sort_unstable();
    v
}

/// Every check of P4 at the ticks `ticks`. Returns the number of checks.
fn check(name: &str, d: &PlumDocument, ticks: &[u64]) -> usize {
    let s = player_snapshot(d);
    let notes = snapshot_notes(&s);
    let index = AnalysisIndex::for_document(d);
    let timing = Timing::for_document(d);
    let scope = Scope::Project;
    assert_eq!(index.notes_in(&scope), notes.len() as u64, "{name}: total");
    assert_eq!(s.note_count(), notes.len() as u64, "{name}");
    let profile = PeakProfile::build(&index, &scope, &timing, SECOND, None).unwrap();
    let starts: Vec<u64> = notes.iter().map(|n| n.0).collect();
    let mut ends: Vec<u64> = notes.iter().map(|n| n.1).collect();
    ends.sort_unstable();
    let le = |v: &[u64], t: u64| v.partition_point(|&x| x <= t) as u64;
    // Polyphony at x: started at or before x, not yet ended (end > x).
    let poly_at = |x: u64| le(&starts, x) - le(&ends, x);
    let nps_at = |x: u64| {
        let lo = window_lo(&timing, timing.nanos_at(x), SECOND);
        le(&starts, x) - lo.map_or(0, |k| le(&starts, k))
    };
    // Peaks up to each distinct start tick (poly only rises at a start;
    // NPS is measured at starts, as the canonical definition says).
    let mut distinct: Vec<u64> = starts.clone();
    distinct.dedup();
    let mut peak_poly = Vec::with_capacity(distinct.len());
    let mut peak_nps = Vec::with_capacity(distinct.len());
    let (mut pp, mut pn) = (0u64, 0u64);
    for &x in &distinct {
        pp = pp.max(poly_at(x));
        pn = pn.max(nps_at(x));
        peak_poly.push(pp);
        peak_nps.push(pn);
    }
    let peak_so_far = |v: &[u64], t: u64| -> u64 {
        let i = distinct.partition_point(|&x| x <= t);
        if i == 0 { 0 } else { v[i - 1] }
    };
    let mut checked = 0;
    for &t in ticks {
        let what = format!("{name} at tick {t}");
        let m = index.metrics(&scope, &timing, t, SECOND);
        assert_eq!(m.total_notes, notes.len() as u64, "{what}: total");
        assert_eq!(m.notes_passed, le(&starts, t), "{what}: started");
        assert_eq!(
            index.ends_in(&scope, None, t).0,
            le(&ends, t),
            "{what}: Passed (ended)"
        );
        assert_eq!(m.poly, poly_at(t), "{what}: polyphony");
        assert_eq!(m.nps, nps_at(t), "{what}: NPS");
        assert_eq!(
            profile.peak_poly_so_far(&index, &scope, t),
            peak_so_far(&peak_poly, t),
            "{what}: peak polyphony so far"
        );
        assert_eq!(
            profile.peak_nps_so_far(&index, &scope, &timing, t),
            peak_so_far(&peak_nps, t),
            "{what}: peak NPS so far"
        );
        checked += 1;
    }
    checked
}

/// 0, every note start and end of a few notes and their neighbours, the
/// song end and beyond, and seeded random ticks.
fn ticks_of(s: &ExportSnapshot, seed: u64) -> Vec<u64> {
    let notes = snapshot_notes(s);
    let last = notes.iter().map(|n| n.1).max().unwrap_or(0);
    let mut v = vec![0, 1, last.saturating_sub(1), last, last + 1, last + 10_000];
    let mut x = seed | 1;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n.max(1)
    };
    for _ in 0..12 {
        if let Some(&(a, b)) = notes.get(rnd(notes.len() as u64) as usize) {
            v.extend([a.saturating_sub(1), a, a + 1, b.saturating_sub(1), b, b + 1]);
        }
    }
    for _ in 0..20 {
        v.push(rnd(last + 2));
    }
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn the_players_statistics_are_the_export_snapshots_notes_on_the_whole_corpus() {
    let mut projects = 0;
    let mut checks = 0;
    let mut skipped_rescale = 0;
    for (k, (name, d)) in corpus().into_iter().enumerate() {
        let s = player_snapshot(&d);
        if s.plan().rescale.is_some() {
            // The file's ticks are at another resolution; the Player works
            // in the project's ticks (P2). Totals still agree.
            assert_eq!(
                AnalysisIndex::for_document(&d).notes_in(&Scope::Project),
                s.note_count(),
                "{name}: total"
            );
            skipped_rescale += 1;
            continue;
        }
        checks += check(&name, &d, &ticks_of(&s, 0x0040 + k as u64));
        projects += 1;
    }
    assert!(
        projects >= 20 && skipped_rescale >= 1,
        "{projects} {skipped_rescale}"
    );
    assert!(checks > 1_000, "{checks} checks");
    eprintln!(
        "{projects} projects, {checks} positions checked, {skipped_rescale} rescaled (totals only)"
    );
}

#[test]
fn the_players_statistics_follow_edits_of_corrections_and_clips() {
    // An edit of every kind that changes what the Player shows: a tick
    // correction, a key correction with a key range, a gate, a clip moved.
    let base = || {
        corpus()
            .into_iter()
            .find(|(n, _)| n == "clips")
            .expect("the clip project")
            .1
    };
    let mut d = base();
    let s = player_snapshot(&d);
    check("clips", &d, &ticks_of(&s, 1));
    props(
        &mut d,
        0,
        TrackProps {
            tick_correction: 95,
            ..TrackProps::default()
        },
    );
    let s = player_snapshot(&d);
    check("tick correction", &d, &ticks_of(&s, 2));
    props(
        &mut d,
        1,
        TrackProps {
            key_correction: -7,
            key_range: (40, 80),
            correction_from_measure: 2,
            ..TrackProps::default()
        },
    );
    let s = player_snapshot(&d);
    check("key correction and range", &d, &ticks_of(&s, 3));
    props(
        &mut d,
        2,
        TrackProps {
            gate: plumspirit_engine::model::track::GatePercent::from_hundredths(25_000).unwrap(),
            ..TrackProps::default()
        },
    );
    let s = player_snapshot(&d);
    check("gate 250 %", &d, &ticks_of(&s, 4));
    let p = common::corpus::primary(&d, 1);
    let op = plumspirit_engine::editor::clips::move_clips(&d, &[(p, 2, 7_777)]).unwrap();
    apply(&mut d, op);
    let s = player_snapshot(&d);
    check("clip moved", &d, &ticks_of(&s, 5));
}

// ---- step 3: the Player's tiles are the tiles of the export notes ----

/// Colours that show what a reading changes (key, subchannel, source) with
/// two opacities (stacking); the Player's outline darkness 90 (P7).
struct Identity;
impl TileColors for Identity {
    fn color(&self, s: usize, n: &NoteView) -> ([u8; 4], bool) {
        let h = (s as u32)
            .wrapping_mul(0x9E37_79B1)
            .wrapping_add(n.sub.0.wrapping_mul(0x85EB_CA6B))
            .wrapping_add(u32::from(n.attrs.pitch()).wrapping_mul(0xC2B2_AE35));
        let c = h.to_le_bytes();
        let a = if n.attrs.velocity() > 64 { 255 } else { 160 };
        ([c[0], c[1], c[2], a], n.selected)
    }
}

/// Track `i`'s sources as the Player reads them (P3): its own store where
/// it is, or one source per unmuted placement — the export plan's lanes.
fn player_sources(s: &ExportSnapshot, i: usize) -> Vec<(&NoteStore, SourcePlace)> {
    let doc = s.document();
    if s.plan().placed(i).is_none() {
        return vec![(&doc.tracks()[i].notes, SourcePlace::IDENTITY)];
    }
    doc.placed_contents(i)
        .into_iter()
        .filter(|pc| !pc.placement.muted)
        .map(|pc| (pc.notes, SourcePlace::of(&pc.placement)))
        .collect()
}

/// The export's notes of one source (`export_notes_as_stored`'s composition
/// for one lane), materialised — the test-only reference.
fn reference(s: &ExportSnapshot, i: usize, store: &NoteStore, pl: SourcePlace) -> Vec<NoteRecord> {
    use plumspirit_engine::model::correction::{CorrectedNotes, CorrectedRuns};
    use plumspirit_engine::model::{EventStore, PlacedLane, PlacedNotes};
    let c = s.plan().correction(i).copied();
    if s.plan().placed(i).is_none() {
        let t = &s.document().tracks()[i];
        let via_export: Vec<NoteRecord> =
            plumspirit_engine::midi::export::export_notes_as_stored(t, i, s.plan())
                .map(|v| v.record())
                .collect();
        let direct: Vec<NoteRecord> = match c {
            Some(c) => CorrectedNotes::new(store.snapshot(), c).collect(),
            None => store.iter_audible().map(|v| v.record()).collect(),
        };
        assert_eq!(via_export, direct);
        return via_export;
    }
    let lane = [PlacedLane::new(
        store.snapshot(),
        EventStore::default(),
        pl.window.0,
        pl.window.1,
        pl.shift,
    )];
    match c {
        Some(c) => CorrectedRuns::from_runs(
            PlacedNotes::range(&lane, 0, c.from_tick),
            PlacedNotes::range(&lane, c.from_tick, u64::MAX),
            c,
        )
        .collect(),
        None => PlacedNotes::new(&lane)
            .filter(|r| !r.attrs.muted())
            .collect(),
    }
}

/// Every track's tiles read in place == the tiles of its materialised export
/// notes, at the whole-song level and `extra` seeded (level, tile) pairs.
/// Returns (tiles compared, notes drawn, corrected tracks).
fn check_tiles(name: &str, d: &PlumDocument, seed: u64, extra: usize) -> (usize, usize, usize) {
    let s = player_snapshot(d);
    let keys = d.project().keys;
    let opt = TileOptions {
        outline: Some(Outline::new(255, 90)),
        keys,
        ..TileOptions::default()
    };
    let mut x = seed | 1;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n.max(1)
    };
    let (mut compared, mut drawn, mut corrected) = (0, 0, 0);
    let mut sc = Scratch::default();
    for i in 0..d.tracks().len() {
        let sources = player_sources(&s, i);
        let c = s.plan().correction(i).copied();
        corrected += usize::from(c.is_some());
        let refs: Vec<NoteStore> = sources
            .iter()
            .map(|&(st, pl)| {
                NoteStore::from_sorted_records(&reference(&s, i, st, pl), false).unwrap()
            })
            .collect();
        // The lanes together are exactly what the track exports.
        let mut all: Vec<NoteRecord> = refs
            .iter()
            .flat_map(|r| r.iter().map(|v| v.record()))
            .collect();
        let mut exported: Vec<NoteRecord> =
            plumspirit_engine::midi::export::export_notes_as_stored(&d.tracks()[i], i, s.plan())
                .map(|v| v.record())
                .collect();
        let key = |r: &NoteRecord| (r.start_tick, r.attrs.raw(), r.duration_ticks, r.sub.0);
        all.sort_by_key(key);
        exported.sort_by_key(key);
        assert_eq!(all, exported, "{name} track {i}: lanes = export");
        let end = all
            .iter()
            .map(|r| r.start_tick + r.duration_ticks)
            .max()
            .unwrap_or(0)
            .max(1);
        let stores: Vec<&NoteStore> = sources.iter().map(|&(st, _)| st).collect();
        let places: Vec<SourcePlace> = sources.iter().map(|&(_, pl)| pl).collect();
        let reads = vec![Reading::Played(c); sources.len()];
        let ref_src: Vec<&NoteStore> = refs.iter().collect();
        let whole = tiles::level_for(2048.0 / end as f64);
        let mut at = Vec::new();
        let (a, b) = tiles::tile_range(whole, 0, end);
        at.extend((a..=b).map(|k| (whole, k)));
        for _ in 0..extra {
            let level = whole + rnd(28) as i32;
            let (a, b) = tiles::tile_range(level, 0, end);
            at.push((level, a + rnd((b - a + 1) as u64) as i64));
        }
        for (level, index) in at {
            let got = tiles::build_tile_read(
                &stores,
                &places,
                &reads,
                &Identity,
                level,
                index,
                opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            // v1.0-beta1 (B1-0141): the Player paints in PFA order across its
            // sources, so the reference stores are read the same way.
            let want = tiles::build_tile_read(
                &ref_src,
                &vec![SourcePlace::IDENTITY; ref_src.len()],
                &vec![Reading::Played(None); ref_src.len()],
                &Identity,
                level,
                index,
                opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            assert!(
                got == want,
                "{name} track {i} ({} sources, correction {c:?}): level {level} tile {index}: {} vs {} notes",
                sources.len(),
                got.notes,
                want.notes
            );
            compared += 1;
            drawn += got.notes;
        }
    }
    (compared, drawn, corrected)
}

#[test]
fn the_players_tiles_are_the_tiles_of_the_export_notes_on_the_whole_corpus() {
    let (mut tiles_n, mut notes, mut corrected, mut projects) = (0, 0, 0, 0);
    for (k, (name, d)) in corpus().into_iter().enumerate() {
        let (t, n, c) = check_tiles(&name, &d, 0x0300 + k as u64, 12);
        tiles_n += t;
        notes += n;
        corrected += c;
        projects += 1;
    }
    assert!(projects >= 20, "{projects}");
    assert!(
        tiles_n > 1_000 && notes > 10_000,
        "{tiles_n} tiles, {notes} notes"
    );
    eprintln!(
        "{projects} projects, {tiles_n} tiles, {notes} notes drawn, {corrected} corrected tracks"
    );
}

#[test]
fn the_players_tiles_follow_edits_of_corrections_and_clips() {
    // The step-2 edits, plus a tick correction that moves notes before tick
    // 0 from a later measure (the corrected run merges into the earlier one).
    let mut d = corpus()
        .into_iter()
        .find(|(n, _)| n == "clips")
        .expect("the clip project")
        .1;
    let mut total = (0, 0, 0);
    let mut add = |t: (usize, usize, usize)| {
        total = (total.0 + t.0, total.1 + t.1, total.2 + t.2);
    };
    add(check_tiles("clips", &d, 11, 24));
    props(
        &mut d,
        0,
        TrackProps {
            tick_correction: 95,
            ..TrackProps::default()
        },
    );
    add(check_tiles("tick correction", &d, 12, 24));
    props(
        &mut d,
        1,
        TrackProps {
            key_correction: -7,
            key_range: (40, 80),
            correction_from_measure: 2,
            ..TrackProps::default()
        },
    );
    add(check_tiles("key correction and range", &d, 13, 24));
    props(
        &mut d,
        2,
        TrackProps {
            gate: plumspirit_engine::model::track::GatePercent::from_hundredths(25_000).unwrap(),
            ..TrackProps::default()
        },
    );
    add(check_tiles("gate 250 %", &d, 14, 24));
    let p = common::corpus::primary(&d, 1);
    let op = plumspirit_engine::editor::clips::move_clips(&d, &[(p, 2, 7_777)]).unwrap();
    apply(&mut d, op);
    add(check_tiles("clip moved", &d, 15, 24));
    props(
        &mut d,
        1,
        TrackProps {
            tick_correction: -5_000,
            correction_from_measure: 3,
            gate: plumspirit_engine::model::track::GatePercent::from_hundredths(40_000).unwrap(),
            ..TrackProps::default()
        },
    );
    add(check_tiles("backwards past earlier notes", &d, 16, 24));
    assert!(total.2 >= 6, "{total:?}");
    assert!(total.1 > 1_000, "{total:?}");
}

// ---- step 5: the length shown with the time (P10) ----

/// The file's End of Track as the written file holds it (the semantic form
/// built from the writer's streams, which `tests/v09_export_plan.rs` proves
/// equal to the bytes), in file ticks.
fn file_end(s: &ExportSnapshot) -> u64 {
    s.semantic()
        .tracks
        .iter()
        .filter_map(|t| t.end_of_track)
        .max()
        .unwrap_or(0)
}

/// `end_tick` == the written file's latest End of Track, and no note of the
/// file ends after it.
fn check_length(name: &str, d: &PlumDocument) -> u64 {
    let s = player_snapshot(d);
    let end = s.end_tick();
    assert_eq!(s.plan().map_tick(end), file_end(&s), "{name}: length");
    let mut last_note = 0;
    s.for_each_note(|n| last_note = last_note.max(n.end));
    assert!(
        s.plan().map_tick(end) >= last_note,
        "{name}: a note ends after the length"
    );
    end
}

#[test]
fn the_players_length_is_the_written_files_end_of_track_on_the_whole_corpus() {
    let mut projects = 0;
    for (name, d) in corpus() {
        check_length(&name, &d);
        projects += 1;
    }
    assert!(projects >= 20, "{projects}");
    // Edits that move the end: a tick correction, a gate, a clip moved past
    // the end, and a correction that moves notes before tick 0.
    let mut d = corpus()
        .into_iter()
        .find(|(n, _)| n == "clips")
        .expect("the clip project")
        .1;
    let before = check_length("clips", &d);
    props(
        &mut d,
        0,
        TrackProps {
            tick_correction: 95,
            ..TrackProps::default()
        },
    );
    check_length("tick correction", &d);
    props(
        &mut d,
        2,
        TrackProps {
            gate: plumspirit_engine::model::track::GatePercent::from_hundredths(40_000).unwrap(),
            ..TrackProps::default()
        },
    );
    check_length("gate 400 %", &d);
    let p = common::corpus::primary(&d, 1);
    let far = before + 50_000;
    let op = plumspirit_engine::editor::clips::move_clips(&d, &[(p, 1, far)]).unwrap();
    apply(&mut d, op);
    let moved = check_length("clip moved past the end", &d);
    assert!(moved > far, "{moved} <= {far}");
    props(
        &mut d,
        1,
        TrackProps {
            tick_correction: -5_000_000,
            ..TrackProps::default()
        },
    );
    check_length("notes moved before 0", &d);
    // A new empty project ends where its Conductor does (tick 0).
    let empty = PlumDocument::new_project(
        "empty".into(),
        plumspirit_engine::model::project::Bpm::parse("120").unwrap(),
        960,
    );
    assert_eq!(check_length("new project", &empty), 0);
}
