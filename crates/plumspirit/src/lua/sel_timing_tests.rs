//! B1-0031 / B1-0032: the Selection API and the Timing / Math / Analysis
//! API.

use super::handles::ProjectInfo;
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::sync::Arc;

fn note(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

/// 120 BPM, 4/4, 960 PPQN; track 1: four quarter notes on key 60 at beats
/// 1 - 4, then a chord of 3 notes at bar 2 and one note overlapping the
/// key-60 note before it.
fn project() -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let mut recs = vec![
        note(0, 960, 60),
        note(960, 960, 60),
        note(1920, 960, 60),
        note(2880, 960, 60),
        note(3840, 480, 60),
        note(3840, 480, 64),
        note(3840, 480, 67),
        note(4000, 480, 60),
    ];
    recs.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn host_for(doc: &Arc<PlumDocument>, h: &mut History, info: ProjectInfo) -> Host {
    h.observe_ids(doc);
    let host = Host::new(
        Limits::default(),
        [Capability::ReadProject, Capability::EditNotes],
    )
    .unwrap();
    host.set_world(Some(
        World::new(Arc::clone(doc), h.incarnations().snapshot()).with_info(info),
    ));
    host
}

fn run(host: &Host, src: &str) -> Plain {
    host.run("st.lua", src).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn timing_reads_the_tempo_map_and_meter_exactly() {
    let (doc, mut h) = project();
    let host = host_for(&doc, &mut h, ProjectInfo::default());
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:tempo_at(5000)
           assert(t.bpm == "120" and t.micros == 500000 and t.exact)
           local s = p:time_signature_at(5000)
           assert(s.numerator == 4 and s.denominator == 4 and s.tick == 0)
           local b = p:bar_beat(3840 + 960 + 5)
           assert(b.bar == 2 and b.beat == 2 and b.tick == 5, b.bar .. ":" .. b.beat .. ":" .. b.tick)
           assert(p:tick_of(2) == 3840 and p:tick_of(2, 3) == 3840 + 1920 and p:tick_of(1, 1, 7) == 7)
           assert(p:tick_at_seconds(0.5) == 960 and p:tick_at_seconds(0) == 0)
           assert(math.abs(p:seconds_at(p:tick_at_seconds(1.25)) - 1.25) < 1e-6)
           assert(p:note_length("1/4") == 960 and p:note_length("1/8t") == 320)
           assert(p:note_length("1/16.") == 360 and p:note_length("1") == 3840 and p:note_length("2") == 7680)
           local ok, err = pcall(function() return p:note_length("1/128t") end)
           assert(ok and p:note_length("1/128t") == 20)
           ok, err = pcall(function() return p:note_length("1/7") end)
           assert(not ok and tostring(err):find("whole number of ticks", 1, true), tostring(err))
           assert(plum.timing.quantize(1000, 240) == 960)
           assert(plum.timing.quantize(1080, 240) == 1200, "halves go up")
           assert(plum.timing.quantize(1000, 240, "floor") == 960)
           assert(plum.timing.quantize(961, 240, "ceil") == 1200)
           assert(plum.timing.quantize(960, 240, "ceil") == 960)
           return true"#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn analysis_gives_exact_peaks_and_says_when_it_estimates() {
    let (doc, mut h) = project();
    let host = host_for(&doc, &mut h, ProjectInfo::default());
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local a = t:notes():analysis()
           assert(a.exact and a.notes == 8 and a.counted == 8)
           assert(a.peak_poly == 4, a.peak_poly)          -- the chord + the overlapping key 60
           assert(a.peak_poly_tick == 4000, a.peak_poly_tick)
           assert(a.first_tick == 0 and a.last_tick == 4480)
           assert(a.peak_nps >= 5, a.peak_nps)            -- 5 starts within one second of tick 4000
           local chord = t:notes{ start_tick = 3840, end_tick = 3841 }:analysis()
           assert(chord.notes == 3 and chord.peak_poly == 3 and chord.peak_unique_keys == 3)
           local e = t:notes{ start_tick = 0, end_tick = 1000 }:estimate()
           assert(e.exact == false and e.notes >= 2)
           assert(t:notes{ start_tick = 0, end_tick = 1000 }:count() == 2)
           local o = t:notes{ keys = 60 }:overlaps()
           assert(o.exact and o.notes == 1, o.notes)
           local m = p:metrics_at(4000)
           assert(m.exact and m.poly == 4 and m.notes_passed == 8 and m.total_notes == 8, m.poly)
           return true"#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn note_selection_is_set_in_a_transaction_without_an_undo_step() {
    let (mut doc, mut h) = project();
    let before = doc.export_fingerprint();
    let host = host_for(&doc, &mut h, ProjectInfo::default());
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local tx = plum.transaction{ label = "Select" }
           assert(tx:select_notes(t:notes{ keys = 60 }) == 6)
           assert(p:selected_note_count() == 6)
           assert(tx:select_notes(t:notes{ keys = 64 }, "add") == 7)
           assert(tx:select_notes(t:notes{ start_tick = 0, end_tick = 1000 }, "remove") == 5)
           assert(tx:select_notes(t:notes{ keys = { 64, 67 } }, "toggle") == 5)
           assert(plum.selection.notes(t):count() == 5)
           local a = plum.selection.analytics()
           assert(a.notes == 5 and a.exact)
           tx:commit()
           return p:selected_note_count()"#,
    );
    assert_eq!(r, Plain::Integer(5));
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.track(0).unwrap().notes.selected_count(), 5);
    assert!(!h.can_undo(), "a selection is no Undo step");
    assert_eq!(doc.export_fingerprint(), before, "the music is unchanged");
    // Clearing it.
    let host = host_for(&doc, &mut h, ProjectInfo::default());
    run(
        &host,
        "local tx = plum.transaction{} tx:clear_selection(plum.project.current():track(1)) tx:commit()",
    );
    let mut commits = host.take_commits().unwrap();
    commits.apply(&mut h, &mut doc).unwrap();
    assert_eq!(doc.track(0).unwrap().notes.selected_count(), 0);
}

#[test]
fn ui_selection_requests_are_read_back_and_handed_to_the_app() {
    let (doc, mut h) = project();
    let t1 = doc.track(0).unwrap().id();
    let t2 = doc.track(1).unwrap().id();
    let info = ProjectInfo {
        active_track: Some(t1),
        selected_tracks: vec![t1],
        time_selection: Some((0, 960)),
        ..ProjectInfo::default()
    };
    let host = host_for(&doc, &mut h, info);
    let r = run(
        &host,
        r#"local p = plum.project.current()
           assert(plum.selection.active_track() == p:track(1))
           assert(#plum.selection.tracks() == 1)
           local ts = plum.selection.time()
           assert(ts.start_tick == 0 and ts.end_tick == 960)
           assert(#plum.selection.clips() == 0)
           plum.selection.set_tracks({ p:track(1), p:track(2) }, { active = p:track(2) })
           plum.selection.set_time(1920, 3840)
           assert(plum.selection.active_track() == p:track(2))
           assert(#plum.selection.tracks() == 2 and plum.selection.time().start_tick == 1920)
           local ok, err = pcall(function() plum.selection.set_time(10, 5) end)
           assert(not ok and tostring(err):find("ends after it starts", 1, true))
           return true"#,
    );
    assert_eq!(r, Plain::Boolean(true));
    let ui = host.take_ui_selection().expect("a request");
    assert_eq!(ui.active_track, Some(Some(t2)));
    assert_eq!(ui.tracks, Some(vec![t1, t2]));
    assert_eq!(ui.time, Some(Some((1920, 3840))));
    assert_eq!(ui.clips, None);
    // A failed run asks for nothing.
    host.run("f.lua", "plum.selection.set_time(0, 1) error('no')")
        .unwrap_err();
    assert!(host.take_ui_selection().is_none());
}
