//! V09-S18 (V09-0053, `docs/INSPECTOR.md` I5): the MIDI Inspector's chart
//! columns hold the exact highest NPS and polyphony over their tick ranges,
//! and the highest column is the song's exact peak — never a sampled value.
//! Checked against a brute-force evaluation at every event tick of the
//! export snapshot's notes (the notes the Inspector reports, I1) on every
//! golden-corpus project, with columns laid out in song time the way the
//! Inspector lays them out and with seeded random (also empty) columns.

mod common;

use common::corpus::{corpus, note};
use plumspirit_engine::analysis::canonical::window_lo;
use plumspirit_engine::analysis::profile::{PeakProfile, window_peak_nps};
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing};
use plumspirit_engine::midi::{ExportOptions, ExportSnapshot};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::project::Bpm;

const SECOND: u64 = 1_000_000_000;

/// The notes the Inspector reports as (start, end): the Player's snapshot.
fn notes_of(d: &PlumDocument) -> (std::sync::Arc<ExportSnapshot>, Vec<(u64, u64)>) {
    let s = ExportSnapshot::take(d, ExportOptions::for_player(d.project().keys));
    let mut v = Vec::new();
    s.for_each_note(|n| v.push((n.start, n.end)));
    v.sort_unstable();
    (s, v)
}

struct Brute {
    starts: Vec<u64>,
    ends: Vec<u64>,
    events: Vec<u64>,
}

impl Brute {
    fn new(notes: &[(u64, u64)]) -> Brute {
        let starts: Vec<u64> = notes.iter().map(|n| n.0).collect();
        let mut ends: Vec<u64> = notes.iter().map(|n| n.1).collect();
        ends.sort_unstable();
        let mut events: Vec<u64> = starts.iter().chain(ends.iter()).copied().collect();
        events.sort_unstable();
        events.dedup();
        Brute {
            starts,
            ends,
            events,
        }
    }
    fn le(v: &[u64], t: u64) -> u64 {
        v.partition_point(|&x| x <= t) as u64
    }
    /// Started at or before `x`, not yet ended (ends first at a tick).
    fn poly(&self, x: u64) -> u64 {
        Self::le(&self.starts, x) - Self::le(&self.ends, x)
    }
    /// Starts in the trailing second `(window_lo, x]` of song time.
    fn nps(&self, timing: &Timing, x: u64) -> u64 {
        let lo = window_lo(timing, timing.nanos_at(x), SECOND);
        Self::le(&self.starts, x) - lo.map_or(0, |k| Self::le(&self.starts, k))
    }
    /// Highest (NPS, poly) over the ticks `[a, b]`: at `a` and at every event
    /// tick inside (both step functions only change at events — NPS also
    /// falls between starts, never rises).
    fn column(&self, timing: &Timing, a: u64, b: u64) -> (u64, u64) {
        let mut best = (self.nps(timing, a), self.poly(a));
        let i = self.events.partition_point(|&x| x <= a);
        for &x in self.events[i..].iter().take_while(|&&x| x <= b) {
            best.0 = best.0.max(self.nps(timing, x));
            best.1 = best.1.max(self.poly(x));
        }
        best
    }
    /// The song's peaks over every event tick.
    fn peaks(&self, timing: &Timing) -> (u64, u64) {
        self.events.iter().fold((0, 0), |b, &x| {
            (b.0.max(self.nps(timing, x)), b.1.max(self.poly(x)))
        })
    }
}

/// The tick range column `k` of `bounds` covers (the last one inclusive).
fn range(bounds: &[u64], k: usize) -> (u64, u64) {
    let a = bounds[k];
    let b = if k + 2 == bounds.len() {
        bounds[k + 1].max(a)
    } else {
        bounds[k + 1].saturating_sub(1).max(a)
    };
    (a, b)
}

/// Columns over the song's duration in seconds, as the Inspector lays them
/// out: `cols` equal time spans from 0 to the last event.
fn time_bounds(timing: &Timing, last: u64, cols: u64) -> Vec<u64> {
    let dur = timing.nanos_at(last);
    let mut v: Vec<u64> = (0..=cols)
        .map(|k| timing.tick_at_nanos((u128::from(dur) * u128::from(k) / u128::from(cols)) as u64))
        .collect();
    *v.last_mut().unwrap() = last;
    v
}

fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
    let mut x = seed | 1;
    move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n.max(1)
    }
}

/// Every check on one document; returns the number of columns checked.
fn check(name: &str, d: &PlumDocument, seed: u64) -> usize {
    let (s, notes) = notes_of(d);
    if s.plan().rescale.is_some() {
        return 0; // file ticks at another resolution; the index works in project ticks
    }
    let idx = AnalysisIndex::for_document(d);
    let timing = Timing::for_document(d);
    let scope = Scope::Project;
    let prof = PeakProfile::build(&idx, &scope, &timing, SECOND, None).unwrap();
    let brute = Brute::new(&notes);
    let last = brute.events.last().copied().unwrap_or(0);
    let (pn, pp) = brute.peaks(&timing);
    // Whole-song peaks: the profile's answer at the last tick.
    assert_eq!(
        prof.peak_nps_so_far(&idx, &scope, &timing, last),
        pn,
        "{name}: peak NPS"
    );
    assert_eq!(
        prof.peak_poly_so_far(&idx, &scope, last),
        pp,
        "{name}: peak poly"
    );
    let mut checked = 0;
    let mut r = rng(seed);
    let mut layouts: Vec<Vec<u64>> = [1u64, 7, 64, 2_048]
        .iter()
        .map(|&c| time_bounds(&timing, last, c))
        .collect();
    for _ in 0..6 {
        let n = 2 + r(120) as usize;
        let mut b: Vec<u64> = (0..n).map(|_| r(last + 3)).collect();
        b.sort_unstable();
        if r(2) == 0 {
            b[0] = 0;
        }
        layouts.push(b);
    }
    for (li, bounds) in layouts.iter().enumerate() {
        let cols = prof.chart_columns(&idx, &scope, &timing, bounds);
        assert_eq!(cols.len(), bounds.len() - 1, "{name}");
        for (k, &c) in cols.iter().enumerate() {
            let (a, b) = range(bounds, k);
            assert_eq!(
                c,
                brute.column(&timing, a, b),
                "{name}: layout {li}, column {k} = ticks [{a}, {b}] (NPS, poly)"
            );
            checked += 1;
        }
        // Columns from 0 to the last event: the highest one is the peak.
        if bounds[0] == 0 && *bounds.last().unwrap() == last && li < 4 {
            let top = cols
                .iter()
                .fold((0, 0), |m, c| (m.0.max(c.0), m.1.max(c.1)));
            assert_eq!(
                top,
                (pn, pp),
                "{name}: layout {li}: highest column == the peaks"
            );
        }
    }
    // Arbitrary ranges straight through the between-queries.
    for _ in 0..40 {
        let a = r(last + 2);
        let b = a + r(last / 3 + 2);
        let lo = if r(5) == 0 { None } else { Some(a) };
        let want = match lo {
            Some(a) => brute.column(&timing, a, b),
            None => brute.column(&timing, 0, b),
        };
        assert_eq!(
            prof.peak_nps_between(&idx, &scope, &timing, lo, b),
            want.0,
            "{name}: NPS in ({lo:?}, {b}]"
        );
        assert_eq!(
            window_peak_nps(&idx, &scope, &timing, SECOND, lo, b),
            want.0,
            "{name}: window NPS in ({lo:?}, {b}]"
        );
        assert_eq!(
            prof.peak_poly_between(&idx, &scope, lo, b),
            want.1,
            "{name}: poly in ({lo:?}, {b}]"
        );
    }
    checked
}

#[test]
fn chart_columns_hold_the_exact_peaks_of_their_ranges_on_the_whole_corpus() {
    let mut projects = 0;
    let mut columns = 0;
    for (k, (name, d)) in corpus().into_iter().enumerate() {
        let c = check(&name, &d, 0x5180 + k as u64);
        if c > 0 {
            projects += 1;
            columns += c;
        }
    }
    assert!(projects >= 20, "{projects}");
    assert!(columns > 20_000, "{columns} columns");
    eprintln!("{projects} projects, {columns} columns checked");
}

/// A dense song large enough for many profile blocks (the between-queries
/// combine whole blocks with partial sweeps), with a tempo ramp and steps.
#[test]
fn chart_columns_combine_whole_blocks_exactly_on_a_long_dense_song() {
    let mut d = PlumDocument::new_project("dense".into(), Bpm::parse("150").unwrap(), 96);
    let mut recs = Vec::new();
    let mut r = rng(77);
    for k in 0..40_000u64 {
        let t = k * 3 + r(3);
        recs.push(note(t, 1 + r(400), (20 + r(90)) as u8, (k % 16) as u8));
    }
    recs.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let c = check("dense", &d, 3);
    assert!(c > 2_000, "{c}");
}

#[test]
fn back_to_back_notes_never_make_a_false_one_tick_peak() {
    // Two notes on the same key end-to-start at tick 480, then a chord.
    let mut d = PlumDocument::new_project("ties".into(), Bpm::parse("120").unwrap(), 480);
    let recs = vec![
        note(0, 480, 60, 0),
        note(480, 480, 60, 0),
        note(960, 0, 62, 0), // zero length: never sounds
        note(1_440, 240, 64, 0),
        note(1_440, 240, 67, 0),
    ];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let prof = PeakProfile::build(&idx, &Scope::Project, &timing, SECOND, None).unwrap();
    let bounds = time_bounds(&timing, 1_680, 8);
    let cols = prof.chart_columns(&idx, &Scope::Project, &timing, &bounds);
    // The column holding tick 480 shows one sounding note, not two.
    let k = bounds
        .windows(2)
        .position(|w| w[0] <= 480 && 480 < w[1])
        .unwrap();
    assert_eq!(cols[k].1, 1, "{cols:?}");
    assert_eq!(
        cols.iter().map(|c| c.1).max(),
        Some(2),
        "the chord is the peak"
    );
    // And every column of every layout against the brute force.
    assert!(check("ties", &d, 9) > 2_000);
}

// ---- step 2b: the ranged profile update (INSPECTOR.md I6) ----

/// Every answer a profile gives, for comparing two profiles of one index:
/// peaks so far at seeded ticks, chart columns of several layouts and
/// between-queries over seeded ranges.
fn answers(idx: &AnalysisIndex, scope: &Scope, timing: &Timing, prof: &PeakProfile) -> Vec<u64> {
    let last = idx.last_tick(scope);
    let mut v = vec![prof.events];
    let mut r = rng(last ^ 0xA5);
    let mut ticks = vec![0, 1, last.saturating_sub(1), last, last + 1];
    ticks.extend((0..60).map(|_| r(last + 2)));
    for &t in &ticks {
        v.push(prof.peak_nps_so_far(idx, scope, timing, t));
        v.push(prof.peak_poly_so_far(idx, scope, t));
    }
    for c in [1u64, 13, 512] {
        for (n, p) in prof.chart_columns(idx, scope, timing, &time_bounds(timing, last, c)) {
            v.push(n);
            v.push(p);
        }
    }
    for _ in 0..30 {
        let a = r(last + 2);
        let b = a + r(last / 4 + 2);
        v.push(prof.peak_nps_between(idx, scope, timing, Some(a), b));
        v.push(prof.peak_poly_between(idx, scope, Some(a), b));
    }
    // Short ranges everywhere (a few blocks each): stale block maxima show.
    for k in 0..150u64 {
        let a = last * k / 150;
        let b = a + last / 60 + 1;
        v.push(prof.peak_nps_between(idx, scope, timing, Some(a), b));
        v.push(prof.peak_poly_between(idx, scope, Some(a), b));
    }
    v
}

/// Applies `edit`, then checks that the previous profile updated with the
/// index's delta answers exactly like a profile built from scratch (for
/// the project and for a one-track scope). Returns the updated profiles
/// (chained into the next edit) and whether the update path was taken.
fn step(
    what: &str,
    d: &mut PlumDocument,
    state: &mut (AnalysisIndex, PeakProfile, PeakProfile),
    edit: impl FnOnce(&mut PlumDocument),
) -> bool {
    edit(d);
    let timing = Timing::for_document(d);
    let inputs = plumspirit_engine::analysis::TrackInput::all(d);
    let idx = AnalysisIndex::build(&inputs, Some(&state.0), None).unwrap();
    let one = Scope::Tracks(std::iter::once(d.track(0).unwrap().id()).collect());
    let mut ranged = false;
    let mut next = Vec::new();
    for (scope, prev) in [(Scope::Project, &state.1), (one, &state.2)] {
        let built = PeakProfile::build(&idx, &scope, &timing, SECOND, None).unwrap();
        let updated = match idx.delta_since(&state.0, &scope) {
            Some(delta) => {
                ranged |= !delta.is_empty();
                let up = prev.update(&idx, &scope, &timing, &delta, None).unwrap();
                // The chart after the edit from the chart before it: only
                // the columns a dirty range touches are computed again.
                let end = state.0.last_tick(&scope).max(idx.last_tick(&scope)) + 500;
                let fixed: Vec<u64> = (0..=97u64).map(|k| k * end / 97).collect();
                let before = prev.chart_columns(&state.0, &scope, &timing, &fixed);
                let dirty = up.dirty_ranges(&delta, &timing);
                assert_eq!(
                    up.chart_columns_update(&idx, &scope, &timing, &fixed, &before, &dirty),
                    built.chart_columns(&idx, &scope, &timing, &fixed),
                    "{what}: updated chart columns ({scope:?})"
                );
                if delta.is_empty() {
                    assert_eq!(prev.update_blocks(&delta, &timing), 0, "{what}");
                }
                up
            }
            None => built.clone(),
        };
        assert_eq!(
            answers(&idx, &scope, &timing, &updated),
            answers(&idx, &scope, &timing, &built),
            "{what}: the updated profile ({scope:?}) answers like a fresh build"
        );
        next.push(updated);
    }
    let one = next.pop().unwrap();
    let project = next.pop().unwrap();
    *state = (idx, project, one);
    ranged
}

fn edits(name: &str, d: &mut PlumDocument, seed: u64) -> usize {
    let timing = Timing::for_document(d);
    let idx = AnalysisIndex::for_document(d);
    let one = Scope::Tracks(std::iter::once(d.track(0).unwrap().id()).collect());
    let p = PeakProfile::build(&idx, &Scope::Project, &timing, SECOND, None).unwrap();
    let p1 = PeakProfile::build(&idx, &one, &timing, SECOND, None).unwrap();
    let mut state = (idx, p, p1);
    let mut r = rng(seed);
    let mut ranged = 0;
    let tracks = d.track_count();
    for round in 0..5 {
        let last = state.0.last_tick(&Scope::Project).max(960);
        let t = r(tracks as u64) as usize;
        let what = format!("{name} round {round}");
        let k = r(5);
        let ok = match k {
            0 => step(&format!("{what}: insert"), d, &mut state, |d| {
                let mut recs: Vec<_> = (0..1 + r(40))
                    .map(|_| note(r(last + 500), r(300), (30 + r(60)) as u8, (r(16)) as u8))
                    .collect();
                recs.sort_by_key(|n| n.start_tick);
                d.track_mut(t)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&recs, false)
                    .unwrap();
            }),
            1 => step(&format!("{what}: delete a region"), d, &mut state, |d| {
                let a = r(last);
                let s = &mut d.track_mut(t).unwrap().notes;
                s.clear_selection();
                s.select_in_region(a, a + r(last / 6 + 10), 0, 127);
                s.remove_selected();
            }),
            2 => step(&format!("{what}: durations"), d, &mut state, |d| {
                let a = r(last);
                let s = &mut d.track_mut(t).unwrap().notes;
                s.clear_selection();
                s.select_in_region(a, a + r(last / 8 + 10), 0, 127);
                let _ = s.set_selected_durations(1 + r(700));
                s.clear_selection();
            }),
            3 => step(&format!("{what}: selection only"), d, &mut state, |d| {
                let s = &mut d.track_mut(t).unwrap().notes;
                s.select_all();
            }),
            _ => step(&format!("{what}: tick correction"), d, &mut state, |d| {
                common::corpus::props(
                    d,
                    t,
                    plumspirit_engine::model::track::TrackProps {
                        tick_correction: r(400) as i64 - 200,
                        ..Default::default()
                    },
                );
            }),
        };
        ranged += usize::from(ok);
    }
    ranged
}

#[test]
fn the_ranged_profile_update_answers_like_a_full_build_after_edits_on_the_corpus() {
    let mut ranged = 0;
    let mut projects = 0;
    for (k, (name, mut d)) in corpus().into_iter().enumerate() {
        if d.track_count() == 0 {
            continue;
        }
        ranged += edits(&name, &mut d, 0x0B00 + k as u64);
        projects += 1;
    }
    // A clip moved past the end and back, on the clip project.
    let mut d = corpus().into_iter().find(|(n, _)| n == "clips").unwrap().1;
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let one = Scope::Tracks(std::iter::once(d.track(0).unwrap().id()).collect());
    let mut state = (
        idx,
        PeakProfile::build(
            &AnalysisIndex::for_document(&d),
            &Scope::Project,
            &timing,
            SECOND,
            None,
        )
        .unwrap(),
        PeakProfile::build(
            &AnalysisIndex::for_document(&d),
            &one,
            &timing,
            SECOND,
            None,
        )
        .unwrap(),
    );
    let p = common::corpus::primary(&d, 1);
    for at in [9_999u64, 0, 123] {
        ranged += usize::from(step(
            &format!("clip moved to {at}"),
            &mut d,
            &mut state,
            |d| {
                let op = plumspirit_engine::editor::clips::move_clips(d, &[(p, 1, at)]).unwrap();
                common::corpus::apply(d, op);
            },
        ));
    }
    assert!(projects >= 20, "{projects}");
    assert!(ranged >= 30, "{ranged} ranged updates");
    eprintln!("{projects} projects, {ranged} ranged updates checked");
}

#[test]
fn the_ranged_profile_update_is_exact_on_a_long_dense_song_with_many_blocks() {
    // 960 PPQN at 120 BPM with a note every two ticks: about seven profile
    // blocks per second, so a changed start's one-second window and a long
    // note's sounding range reach well past the blocks holding their ticks.
    let mut d = PlumDocument::new_project("dense".into(), Bpm::parse("120").unwrap(), 960);
    d.add_track().unwrap();
    let mut r = rng(91);
    for t in 0..2u64 {
        let mut recs: Vec<_> = (0..40_000u64)
            .map(|k| {
                note(
                    k * 4 + t * 2 + r(2),
                    1 + r(200),
                    (20 + r(90)) as u8,
                    (k % 16) as u8,
                )
            })
            .collect();
        recs.sort_by_key(|n| n.start_tick);
        d.track_mut(t as usize)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let one = Scope::Tracks(std::iter::once(d.track(0).unwrap().id()).collect());
    let p = PeakProfile::build(&idx, &Scope::Project, &timing, SECOND, None).unwrap();
    let p1 = PeakProfile::build(&idx, &one, &timing, SECOND, None).unwrap();
    let mut state = (idx, p, p1);
    let mut ranged = 0;
    // A burst of 400 starts at one tick (NPS +400 for a second) with long
    // notes (poly +400 for 40,000 ticks), then a burst of short ones, then
    // the long burst removed, then durations changed under a region.
    for (what, at, len) in [
        ("long burst", 50_000u64, 40_000u64),
        ("short burst", 120_001, 7),
    ] {
        ranged += usize::from(step(what, &mut d, &mut state, |d| {
            let recs: Vec<_> = (0..400u64)
                .map(|k| note(at, len, (k % 120) as u8, 0))
                .collect();
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }));
    }
    ranged += usize::from(step("long burst removed", &mut d, &mut state, |d| {
        let s = &mut d.track_mut(1).unwrap().notes;
        s.clear_selection();
        s.select_in_region(50_000, 50_000, 0, 127);
        assert!(s.remove_selected() >= 400);
    }));
    ranged += usize::from(step("durations", &mut d, &mut state, |d| {
        let s = &mut d.track_mut(0).unwrap().notes;
        s.clear_selection();
        s.select_in_region(70_000, 71_000, 0, 127);
        s.set_selected_durations(9_000).unwrap();
        s.clear_selection();
    }));
    ranged += edits("dense", &mut d, 5);
    assert!(ranged >= 5, "{ranged}");
}

#[test]
fn updated_chart_columns_are_exact_down_to_one_tick_columns() {
    // One column per tick: every dirty range starts and ends on a column, so
    // a column touched only at its first or last tick must be recomputed.
    for seed in 1..12u64 {
        let mut r = rng(seed);
        // 123 BPM at 24 PPQN: a second is 49.2 ticks, so a start's window
        // ends inside a tick and the window's last tick still counts it.
        let mut d = PlumDocument::new_project("ticks".into(), Bpm::parse("123").unwrap(), 24);
        let mut recs: Vec<_> = (0..150)
            .map(|_| note(r(1_500), r(20), (40 + r(40)) as u8, 0))
            .collect();
        recs.sort_by_key(|n| n.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let timing = Timing::for_document(&d);
        let scope = Scope::Project;
        let idx0 = AnalysisIndex::for_document(&d);
        let p0 = PeakProfile::build(&idx0, &scope, &timing, SECOND, None).unwrap();
        let bounds: Vec<u64> = (0..=1_800u64).collect();
        let before = p0.chart_columns(&idx0, &scope, &timing, &bounds);
        let mut add: Vec<_> = (0..1 + r(4))
            .map(|_| note(r(1_500), r(12), 50, 1))
            .collect();
        add.sort_by_key(|n| n.start_tick);
        let s = &mut d.track_mut(0).unwrap().notes;
        s.insert_sorted_batch(&add, false).unwrap();
        let a = r(1_400);
        s.clear_selection();
        s.select_in_region(a, a + r(100), 0, 127);
        s.remove_selected();
        let idx1 = AnalysisIndex::build(
            &plumspirit_engine::analysis::TrackInput::all(&d),
            Some(&idx0),
            None,
        )
        .unwrap();
        let delta = idx1.delta_since(&idx0, &scope).unwrap();
        let built = PeakProfile::build(&idx1, &scope, &timing, SECOND, None).unwrap();
        let dirty = built.dirty_ranges(&delta, &timing);
        assert_eq!(
            built.chart_columns_update(&idx1, &scope, &timing, &bounds, &before, &dirty),
            built.chart_columns(&idx1, &scope, &timing, &bounds),
            "seed {seed}: {dirty:?}"
        );
    }
}
