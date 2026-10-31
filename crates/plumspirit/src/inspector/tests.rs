//! The Inspector's values and charts against brute force over the export
//! snapshot's notes (V09-0051 / 0052 / 0053 / 0057, `docs/INSPECTOR.md`).

use super::*;
use crate::app::analysis::AnalysisService;
use plumspirit_engine::analysis::canonical::window_lo;
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::TrackProps;
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, TempoValue};
use std::time::{Duration, Instant};

fn note(start: u64, len: u64, pitch: u8, ch: u8) -> NoteRecord {
    let attrs = if pitch > 127 {
        NoteAttrs::new_wide(pitch, 100, ch, 0, NoteOffKind::NoteOff).unwrap()
    } else {
        NoteAttrs::new(pitch, 100, ch, 0, NoteOffKind::NoteOff).unwrap()
    };
    NoteRecord::new(start, len, attrs)
}

fn add(d: &mut PlumDocument, track: usize, mut recs: Vec<NoteRecord>) {
    recs.sort_by_key(|n| n.start_tick);
    d.track_mut(track)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
}

/// Three tracks of chords and runs, a long note, back-to-back notes, a
/// corrected track, a tempo ramp 100 → 200 BPM and a step to 90.
fn song() -> PlumDocument {
    let mut d = PlumDocument::new_project("i".into(), Bpm::parse("100").unwrap(), 480);
    d.add_track().unwrap();
    d.add_track().unwrap();
    for t in 0..3usize {
        let recs = (0..600u64)
            .map(|k| {
                note(
                    k * 40 + t as u64 * 7,
                    30 + (k % 7) * 40,
                    (36 + (k * 5 + t as u64) % 50) as u8,
                    t as u8,
                )
            })
            .collect();
        add(&mut d, t, recs);
    }
    add(
        &mut d,
        0,
        vec![
            note(1_000, 20_000, 100, 0),
            note(2_000, 480, 101, 0),
            note(2_480, 480, 101, 0),
        ],
    );
    let bpm = |s: &str| TempoValue::Bpm(Bpm::parse(s).unwrap());
    let edit = d
        .conductor()
        .prepare_tempo_points(
            &[
                (0, bpm("100"), Interp::Linear),
                (9_600, bpm("200"), Interp::Step),
                (19_200, bpm("90"), Interp::Step),
            ],
            true,
        )
        .unwrap();
    History::default()
        .apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
    let id = d.track(2).unwrap().id();
    History::default()
        .apply(
            &mut d,
            Operation::TrackProps {
                track: id,
                props: TrackProps {
                    tick_correction: 333,
                    ..TrackProps::default()
                },
                end: None,
                label: "Track Properties",
            },
        )
        .unwrap();
    d
}

/// Brute force over the snapshot's notes: (notes, keys above 127, max NPS,
/// max poly) by the canonical definitions at every event tick.
fn brute(prep: &Prepared) -> (u64, u64, u64, u64) {
    let mut notes = Vec::new();
    let mut wide = 0;
    prep.snapshot.for_each_note(|n| {
        notes.push((n.start, n.end));
        wide += u64::from(n.key > 127);
    });
    let starts: Vec<u64> = {
        let mut v: Vec<u64> = notes.iter().map(|n| n.0).collect();
        v.sort_unstable();
        v
    };
    let mut ends: Vec<u64> = notes.iter().map(|n| n.1).collect();
    ends.sort_unstable();
    let le = |v: &[u64], t: u64| v.partition_point(|&x| x <= t) as u64;
    let timing = &prep.timing;
    let (mut nps, mut poly) = (0, 0);
    for &t in starts.iter().chain(ends.iter()) {
        let lo = window_lo(timing, timing.nanos_at(t), WINDOW_NS);
        nps = nps.max(le(&starts, t) - lo.map_or(0, |k| le(&starts, k)));
        poly = poly.max(le(&starts, t) - le(&ends, t));
    }
    (notes.len() as u64, wide, nps, poly)
}

/// The service's index, timing and project profile once caught up.
fn analysed(
    svc: &mut AnalysisService,
    d: &PlumDocument,
) -> (Arc<AnalysisIndex>, Arc<Timing>, Arc<PeakProfile>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        svc.sync(d, 1);
        if let (Some(i), Some(t)) = (svc.index().cloned(), svc.timing().cloned())
            && let Some(p) = svc.profile(&Scope::Project, WINDOW_NS)
        {
            return (i, t, p);
        }
        assert!(Instant::now() < deadline, "analysis never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn check_values(name: &str, d: &PlumDocument) -> InspectorValues {
    let prep = Prepared::of(d);
    let mut svc = AnalysisService::default();
    let (idx, timing, prof) = analysed(&mut svc, d);
    let mut insp = Inspector::default();
    let n = insp.tempo_changes(&prep);
    let v = values(d, &prep, &idx, &timing, Some(&prof), n);
    let (notes, wide, nps, poly) = brute(&prep);
    assert_eq!(v.notes, notes, "{name}: notes = the file's Note Ons");
    assert_eq!(v.notes, prep.snapshot.note_count(), "{name}");
    assert_eq!(v.wide_keys, wide, "{name}: keys above 127");
    assert_eq!(v.max_nps, Some(nps), "{name}: max NPS");
    assert_eq!(v.max_poly, Some(poly), "{name}: max poly");
    assert_eq!(v.stored, d.stored_note_count() as u64, "{name}");
    assert_eq!(v.end_tick, prep.snapshot.end_tick(), "{name}");
    assert_eq!(v.duration_ns, prep.length_ns, "{name}: the Player's length");
    assert_eq!(
        v.tempo_changes,
        prep.snapshot.tempo_changes().len(),
        "{name}"
    );
    // The chart over the song: its highest column is the peaks.
    let mut chart = Chart {
        inline: true,
        ..Chart::default()
    };
    for cols in [1usize, 37, 500, 5_000] {
        chart.sync(&idx, &prof, &timing, v.end_tick, cols);
        assert_eq!(chart.columns.len(), cols.min(MAX_COLUMNS), "{name}");
        let top = chart
            .columns
            .iter()
            .fold((0, 0), |m, c| (m.0.max(c.0), m.1.max(c.1)));
        assert_eq!(top, (nps, poly), "{name}: {cols} columns, highest column");
    }
    v
}

#[test]
fn the_inspector_values_are_the_exported_files_exact_numbers() {
    let d = song();
    let v = check_values("song", &d);
    assert_eq!(v.timing, "480 PPQN");
    assert_eq!(
        (
            v.bpm_start.as_str(),
            v.bpm_low.as_str(),
            v.bpm_high.as_str()
        ),
        ("100", "90", "200")
    );
    assert!(v.duration_ns > 0 && v.notes == v.stored);

    // Muted notes: stored, not exported.
    let mut d = song();
    let muted: Vec<NoteRecord> = (0..7u64)
        .map(|k| {
            let mut n = note(3_000 + k * 10, 50, 70, 1);
            n.attrs = n.attrs.with_muted(true);
            n
        })
        .collect();
    add(&mut d, 1, muted);
    let v = check_values("muted", &d);
    assert_eq!(
        v.stored - v.notes,
        7,
        "muted notes are stored, not exported"
    );

    // 256-Key Mode: keys above 127 counted, never truncated.
    let mut d = song();
    d.project_mut().keys = KeyRange::Extended256;
    add(
        &mut d,
        1,
        vec![
            note(500, 100, 128, 0),
            note(600, 100, 200, 1),
            note(700, 100, 255, 2),
        ],
    );
    let v = check_values("256 keys", &d);
    assert_eq!(v.wide_keys, 3);
}

#[test]
fn the_chart_after_a_small_edit_recomputes_only_the_columns_it_touches() {
    let mut d = song();
    let mut svc = AnalysisService::default();
    let (idx, timing, prof) = analysed(&mut svc, &d);
    let end = Prepared::of(&d).snapshot.end_tick();
    let mut chart = Chart {
        inline: false,
        ..Chart::default()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while chart.sync(&idx, &prof, &timing, end, 600) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!((chart.full_builds, chart.updates), (1, 0));
    // A short burst in the middle: the profile is updated at once and only
    // the touched columns are recomputed, in the frame.
    add(
        &mut d,
        1,
        (0..30).map(|k| note(9_000, 60, 40 + k, 1)).collect(),
    );
    let (idx2, timing2, prof2) = analysed(&mut svc, &d);
    assert!(
        Arc::ptr_eq(&timing, &timing2),
        "a note edit keeps the tempo map"
    );
    assert!(!chart.sync(&idx2, &prof2, &timing2, end, 600), "no worker");
    assert_eq!((chart.full_builds, chart.updates), (1, 1));
    assert_eq!(
        chart.columns,
        prof2.chart_columns(&idx2, &Scope::Project, &timing2, chart.bounds())
    );
}

#[test]
fn the_on_save_check_agrees_with_a_fresh_analysis_and_names_a_difference() {
    let d = song();
    let idx = Arc::new(AnalysisIndex::for_document(&d));
    let timing = Arc::new(Timing::for_document(&d));
    let profile =
        Arc::new(PeakProfile::build(&idx, &Scope::Project, &timing, WINDOW_NS, None).unwrap());
    let live = Live {
        index: Arc::clone(&idx),
        profile: Arc::clone(&profile),
        timing: Arc::clone(&timing),
    };
    assert_eq!(check(&d, &live), Ok(()));
    // The live state of another version (a burst added): named.
    let mut other = song();
    add(
        &mut other,
        0,
        (0..40).map(|k| note(5_000, 2_000, 30 + k, 0)).collect(),
    );
    let e = check(&other, &live).unwrap_err();
    assert!(e.starts_with("notes "), "{e}");
    // Same count, other peaks: the peak is named.
    let mut moved = song();
    let s = &mut moved.track_mut(0).unwrap().notes;
    s.clear_selection();
    s.select_in_region(1_000, 1_000, 100, 100);
    s.set_selected_durations(80_000).unwrap();
    let e = check(&moved, &live).unwrap_err();
    assert!(e.contains("polyphony") || e.contains("chart column"), "{e}");
}

/// B1-0108: the visible window zooms about the pointer, pans, stays inside
/// the song and at least `MIN_SPAN` wide; the vertical view likewise.
#[test]
fn the_chart_window_zooms_about_the_pointer_and_stays_inside_the_song() {
    let w = Window::default();
    assert!(w.is_full());
    let z = w.zoomed(4.0, 0.25);
    assert!((z.span() - 0.25).abs() < 1e-12);
    // The point under the pointer (a quarter in) stays a quarter in.
    assert!((z.lo + 0.25 * z.span() - 0.25).abs() < 1e-12);
    let p = z.panned(10.0);
    assert!(
        (p.hi - 1.0).abs() < 1e-12 && (p.span() - 0.25).abs() < 1e-12,
        "{p:?}"
    );
    let p = z.panned(-10.0);
    assert_eq!(p.lo, 0.0);
    let tiny = w.zoomed(1e12, 0.5);
    assert!((tiny.span() - MIN_SPAN).abs() < 1e-15, "{tiny:?}");
    assert_eq!(
        z.zoomed(1e-9, 0.5),
        Window::default(),
        "zooming out stops at the song"
    );
    let a = w.around(0.999, 0.1);
    assert!((a.hi - 1.0).abs() < 1e-12 && (a.span() - 0.1).abs() < 1e-12);
    // Vertical: zoom 4 at the bottom shows the lowest quarter; the pivot
    // stays; zooming out returns to the full range.
    let y = YView::default().zoomed(4.0, 0.0);
    assert_eq!(y.range(0.0, 100.0), (0.0, 25.0));
    let y = YView::default().zoomed(2.0, 0.5);
    let (a, b) = y.range(0.0, 100.0);
    assert!(
        (a - 25.0).abs() < 1e-9 && (b - 75.0).abs() < 1e-9,
        "{a} {b}"
    );
    assert_eq!(y.zoomed(0.25, 0.5), YView::default());
    assert_eq!(YView::default().range(10.0, 20.0), (10.0, 20.0));
}

/// B1-0108: column bounds over a window — the whole song on the time axis
/// is the v0.9 layout; the musical axis spaces ticks evenly; a zoomed
/// window lies inside the song; tick ↔ fraction round trips.
#[test]
fn window_bounds_follow_the_axis_and_round_trip() {
    let d = song();
    let mut svc = AnalysisService::default();
    let (_, timing, _) = analysed(&mut svc, &d);
    let end = Prepared::of(&d).snapshot.end_tick();
    assert_eq!(
        bounds_in(&timing, end, Window::default(), 300),
        bounds(&timing, end, 300)
    );
    let m = bounds_in(
        &timing,
        end,
        Window {
            axis: Axis::Musical,
            lo: 0.0,
            hi: 1.0,
        },
        4,
    );
    assert_eq!(
        m,
        (0..=4)
            .map(|k| (end as f64 * k as f64 / 4.0).round() as u64)
            .collect::<Vec<_>>()
    );
    let z = Window::default().zoomed(8.0, 0.5);
    let b = bounds_in(&timing, end, z, 100);
    assert_eq!(b.len(), 101);
    assert!(b.windows(2).all(|w| w[0] <= w[1]));
    assert!(b[0] > 0 && b[100] < end, "{} .. {}", b[0], b[100]);
    for axis in [Axis::Time, Axis::Musical] {
        for t in [0, 1, end / 3, end / 2, end] {
            let f = fraction_of(&timing, axis, end, t);
            let back = tick_of(&timing, axis, end, f);
            assert!(back.abs_diff(t) <= 1, "{axis:?} {t} -> {f} -> {back}");
        }
    }
}

/// B1-0108: two separate bursts (peaks) in one song: a coarse chart may
/// merge them into one bar, but zoomed to each, the bars' maximum is that
/// burst's own exact peak (from the profile over its span, not from
/// sampling) — neither is lost.
#[test]
fn zooming_tells_two_peaks_apart_with_exact_column_maxima() {
    let mut d = PlumDocument::new_project("peaks".into(), Bpm::parse("120").unwrap(), 480);
    // Burst A: 40 notes at once at tick 10,000; burst B: 25 at 10,960.
    add(
        &mut d,
        0,
        (0..40).map(|k| note(10_000, 100, 30 + k, 0)).collect(),
    );
    add(
        &mut d,
        0,
        (0..25).map(|k| note(10_960, 100, 30 + k, 0)).collect(),
    );
    add(&mut d, 0, vec![note(0, 10, 60, 0), note(40_000, 10, 60, 0)]);
    let mut svc = AnalysisService::default();
    let (idx, timing, prof) = analysed(&mut svc, &d);
    let end = Prepared::of(&d).snapshot.end_tick();
    let scope = Scope::Project;
    let mut chart = Chart {
        inline: true,
        ..Chart::default()
    };
    // Four bars over the whole song: both bursts in one bar (the higher wins).
    chart.sync_in(&idx, &prof, &timing, end, 4, Window::default());
    let polys: Vec<u64> = chart.columns.iter().map(|c| c.1).collect();
    assert!(polys.contains(&40), "{polys:?}");
    assert!(!polys.contains(&25), "B hidden at this zoom: {polys:?}");
    for (lo_t, hi_t, want) in [(9_900u64, 10_200u64, 40u64), (10_900, 11_200, 25)] {
        let w = Window {
            axis: Axis::Musical,
            lo: lo_t as f64 / end as f64,
            hi: hi_t as f64 / end as f64,
        };
        chart.sync_in(&idx, &prof, &timing, end, 64, w);
        assert_eq!(chart.window(), Some(w));
        let top = chart.columns.iter().map(|c| c.1).max().unwrap();
        assert_eq!(top, want, "zoomed to {lo_t}..{hi_t}");
        assert_eq!(
            top,
            prof.peak_poly_between(
                &idx,
                &scope,
                Some(chart.bounds()[0]),
                *chart.bounds().last().unwrap()
            )
        );
        // Every column is the exact maximum over its own span.
        assert_eq!(
            chart.columns,
            prof.chart_columns(&idx, &scope, &timing, chart.bounds())
        );
    }
}

/// B1-0108 measurement: computing the columns of a zoomed window costs what
/// the whole-song chart costs (same column count, exact maxima).
#[test]
#[ignore = "measurement: cargo test --release -p plumspirit zoomed_chart_cost -- --ignored --nocapture"]
fn zoomed_chart_cost() {
    use plumspirit_engine::fixtures::{FixtureShape, generate};
    for n in [1_000_000usize, 10_000_000] {
        let d = generate(FixtureShape::Dense, n, 960, 5);
        let mut svc = AnalysisService::default();
        let (idx, timing, prof) = analysed(&mut svc, &d);
        let end = idx.last_tick(&Scope::Project);
        let mut times = Vec::new();
        for w in [
            Window::default(),
            Window::default().zoomed(16.0, 0.4),
            Window::default().zoomed(4096.0, 0.7),
            Window {
                axis: Axis::Musical,
                lo: 0.25,
                hi: 0.5,
            },
        ] {
            let b = bounds_in(&timing, end, w, 1_000);
            let mut best = f64::MAX;
            for _ in 0..3 {
                let t = Instant::now();
                let c = prof.chart_columns(&idx, &Scope::Project, &timing, &b);
                best = best.min(t.elapsed().as_secs_f64() * 1e3);
                assert_eq!(c.len(), 1_000);
            }
            times.push(format!(
                "{:?} {:.4}-{:.4}: {best:.1} ms",
                w.axis, w.lo, w.hi
            ));
        }
        println!(
            "zoomed chart cost, {n} notes, 1,000 columns: {}",
            times.join("; ")
        );
    }
}
