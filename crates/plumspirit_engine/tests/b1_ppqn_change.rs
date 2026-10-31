//! B1-S42 (contract §014-B, B1-0103 / B1-0104): Change PPQN (Experimental).
//! 960 → 30,720 → 960 is the identity (the exported file is byte-identical),
//! 960 → 480 keeps every position within half a new tick and every time in
//! seconds within half a new tick's duration, nested multi-track clips keep
//! their windows, the 32,767 boundary (the largest standard PPQN), .lup
//! round trips, Undo / Redo, and Preserve seconds keeps the timeline.

mod common;

use common::*;
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::editor::clips::{add_clip, duplicate_clips, resize_clip};
use plumspirit_engine::editor::ppqn::{PpqnMode, PpqnReport, prepare_change_ppqn};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{export_to_bytes_with, plan_export, validate_exported_bytes};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::{
    Bpm, ClipId, ClipLayout, ContentRef, Division, PlumDocument, RawEvent, RawKind, TempoMap,
    TempoValue,
};
use plumspirit_engine::native::lup::{project_from_bytes, project_to_bytes};

/// Three tracks (one a subtrack), a controller lane, a tempo ramp, a meter
/// change, a chapter, a loop, a tick correction, an explicit end, an owned
/// clip with a window placed twice (linked) on two tracks.
fn song(ppqn: u32) -> PlumDocument {
    let q = u64::from(ppqn);
    let mut d = PlumDocument::new_project("ppqn".into(), Bpm::DEFAULT, ppqn);
    d.add_track().unwrap();
    d.add_track().unwrap();
    let mut h = History::default();
    for i in 0..3usize {
        let recs: Vec<_> = (0..200u64)
            .map(|k| rec(k * q / 4 + i as u64, q / 4 + k % 7, 40 + (k % 40) as u8))
            .collect();
        let t = d.track_mut(i).unwrap();
        t.notes.insert_sorted_batch(&recs, false).unwrap();
        for k in 0..50u64 {
            t.events
                .push(RawEvent {
                    tick: k * q / 2 + 1,
                    anchor: 0,
                    kind: RawKind::Channel {
                        status: 0xB0,
                        data: [7, (k % 128) as u8],
                    },
                })
                .unwrap();
        }
    }
    d.track_mut(0).unwrap().end_of_track_tick = Some(200 * q);
    let c = d.conductor();
    let e = c
        .prepare_set_tempo(
            None,
            4 * q,
            TempoValue::Bpm(Bpm::from_integer(90).unwrap()),
            Interp::Linear,
        )
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit: e,
            label: "t",
        },
    )
    .unwrap();
    let e = d
        .conductor()
        .prepare_set_tempo(None, 8 * q + 1, TempoValue::Micros(400_000), Interp::Step)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit: e,
            label: "t",
        },
    )
    .unwrap();
    let e = d
        .conductor()
        .prepare_set_signature(None, 8 * q, 3, 2)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit: e,
            label: "m",
        },
    )
    .unwrap();
    let e = d
        .conductor()
        .prepare_set_marker(None, 2 * q + 3, Some(6 * q + 5), "Verse")
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit: e,
            label: "k",
        },
    )
    .unwrap();
    // An owned clip on track 1 with notes, its window cut on both sides,
    // and a linked copy on track 2.
    let op = add_clip(&d, 1, 60 * q, 8 * q).unwrap();
    h.apply(&mut d, op).unwrap();
    let content = d.clips().contents()[0].id;
    for k in 0..32u64 {
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: NoteTarget::Content { host: 1, content },
                record: rec(k * q / 4 + 1, q / 8, 70 + (k % 12) as u8),
            },
        )
        .unwrap();
    }
    let id = d.clips().placements_of(ContentRef::Owned(content))[0].id;
    let op = resize_clip(&d, id, 60 * q + q / 2 + 1, q / 2 + 1, Some(5 * q + 3)).unwrap();
    h.apply(&mut d, op).unwrap();
    let op = duplicate_clips(&d, &[(id, 2, 90 * q + 7)], true).unwrap();
    h.apply(&mut d, op).unwrap();
    d.project_mut().looping.range = Some((q + 1, 9 * q + 3));
    d.validate_clips().unwrap();
    d
}

fn change(d: &mut PlumDocument, h: &mut History, to: u32, mode: PpqnMode) -> PpqnReport {
    let (tb, r) = prepare_change_ppqn(d, to, mode).unwrap();
    h.apply(
        d,
        Operation::SetTimebase {
            timebase: Box::new(tb),
            label: "Change PPQN",
        },
    )
    .unwrap();
    d.validate_clips().unwrap();
    for t in d.tracks() {
        t.notes.validate().unwrap();
    }
    r
}

fn same_layout(a: &ClipLayout, b: &ClipLayout) {
    assert_eq!(a.placements.len(), b.placements.len());
    assert!(
        a.placements
            .iter()
            .zip(&b.placements)
            .all(|(x, y)| x.same_except_revision(y)),
        "{a:?} vs {b:?}"
    );
    assert_eq!(
        (
            &a.removed_primaries,
            &a.contents,
            a.next_clip,
            a.next_content
        ),
        (
            &b.removed_primaries,
            &b.contents,
            b.next_clip,
            b.next_content
        )
    );
}

fn export(d: &PlumDocument) -> Vec<u8> {
    let plan = plan_export(d);
    let bytes = export_to_bytes_with(d, &plan).unwrap();
    validate_exported_bytes(d, &plan, &bytes).unwrap();
    bytes
}

#[test]
fn going_960_to_30720_and_back_is_exact_and_exports_the_same_bytes() {
    let mut d = song(960);
    let before = export(&d);
    let fp = d.state_fingerprint();
    let layout = d.clips().layout();
    let mut h = History::default();
    let r = change(&mut d, &mut h, 30_720, PpqnMode::Beats);
    assert!(r.is_exact(), "{r:?}");
    assert_eq!(d.division(), Division::ppqn(30_720));
    assert_eq!(
        d.project().looping.range,
        Some((32 * (960 + 1), 32 * (9 * 960 + 3)))
    );
    let r = change(&mut d, &mut h, 960, PpqnMode::Beats);
    assert!(r.is_exact(), "{r:?}");
    assert_eq!(d.state_fingerprint(), fp);
    same_layout(&d.clips().layout(), &layout);
    assert_eq!(export(&d), before, "byte-identical after the round trip");
    // Undo both, redo both: each state exports as it did.
    h.undo(&mut d).unwrap();
    assert_eq!(d.division(), Division::ppqn(30_720));
    h.undo(&mut d).unwrap();
    assert_eq!(export(&d), before);
    h.redo(&mut d).unwrap();
    h.redo(&mut d).unwrap();
    assert_eq!(export(&d), before);
}

#[test]
fn going_960_to_480_keeps_positions_within_half_a_tick_and_times_within_one() {
    let old = song(960);
    let mut d = song(960);
    let mut h = History::default();
    let r = change(&mut d, &mut h, 480, PpqnMode::Beats);
    assert!(r.rounded_ticks > 0 && r.max_error_ticks() <= 0.5, "{r:?}");
    let (mo, mn) = (
        TempoMap::for_document(&old, 48_000),
        TempoMap::for_document(&d, 48_000),
    );
    // A position moves by at most half a new tick and the tempo map's
    // rounded points by at most half a tick each: within one new tick at the
    // slowest tempo (90 BPM, 2/3 s per quarter note).
    let tick_s = (60.0 / 90.0) / 480.0 + 1e-9;
    for i in 0..3 {
        let (a, b) = (&old.tracks()[i].notes, &d.tracks()[i].notes);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!(
                (i128::from(y.start_tick) * 960 - i128::from(x.start_tick) * 480).abs() <= 480,
                "{} → {}",
                x.start_tick,
                y.start_tick
            );
            assert!(
                (i128::from(y.end_tick()) * 960 - i128::from(x.end_tick()) * 480).abs() <= 480
                    || y.duration_ticks == 1
            );
            let (so, sn) = (
                mo.seconds_at_tick(x.start_tick),
                mn.seconds_at_tick(y.start_tick),
            );
            assert!((so - sn).abs() <= tick_s, "{so} vs {sn}");
            assert_eq!(x.attrs, y.attrs);
        }
        let ea: Vec<_> = old.tracks()[i].events.events().to_vec();
        let eb: Vec<_> = d.tracks()[i].events.events().to_vec();
        assert_eq!(ea.len(), eb.len());
        for (x, y) in ea.iter().zip(&eb) {
            assert_eq!(x.kind, y.kind);
            assert_eq!(y.tick, (x.tick * 480 * 2 + 960) / (2 * 960));
        }
    }
    // The conductor: ramp start, the 400,000 µs point at 7,681 → 3,841, 3/4 at 3,840.
    let t = d.conductor().tempo_points();
    assert_eq!(t[1].tick, 4 * 480);
    assert_eq!(t[1].interp, Interp::Linear);
    assert_eq!(t[2].tick, 3841);
    assert_eq!(t[2].value, TempoValue::Micros(400_000));
    assert_eq!(d.conductor().time_signatures()[1].tick, 3840);
    let m = &d.conductor().markers()[0];
    assert_eq!((m.start, m.end), (962, Some(2883)));
    assert_eq!(d.tracks()[0].end_of_track_tick, Some(200 * 480));
    export(&d);
}

#[test]
fn nested_multi_track_clips_keep_their_windows_and_links() {
    let mut d = song(960);
    let before: Vec<_> = d.clips().explicit_placements().to_vec();
    let mut h = History::default();
    change(&mut d, &mut h, 480, PpqnMode::Beats);
    let after = d.clips().explicit_placements();
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(after) {
        assert_eq!((a.id, a.track, a.content), (b.id, b.track, b.content));
        let r = |t: u64| (t * 2 * 480 + 960) / (2 * 960);
        assert_eq!(b.start, r(a.start));
        assert_eq!(b.offset, r(a.offset));
        assert_eq!(b.length, a.length.map(|l| r(a.offset + l) - r(a.offset)));
    }
    // Both instances still show one content (linked), placed on tracks 2 and 3.
    let content = d.clips().contents()[0].id;
    let p = d.clips().placements_of(ContentRef::Owned(content));
    assert_eq!(p.len(), 2);
    assert_ne!(p[0].track, p[1].track);
    assert!(matches!(p[0].id, ClipId::Placed(_)));
    // The content's notes: 32, rescaled.
    assert_eq!(d.clips().content(content).unwrap().notes.len(), 32);
    h.undo(&mut d).unwrap();
    let back = d.clips().explicit_placements();
    assert_eq!(back.len(), before.len());
    assert!(
        back.iter()
            .zip(&before)
            .all(|(a, b)| a.same_except_revision(b))
    );
}

#[test]
fn the_32767_boundary_is_standard_and_larger_stays_native() {
    for to in [32_767u32, 32_768] {
        let mut d = song(960);
        let fp = d.state_fingerprint();
        let mut h = History::default();
        let r = change(&mut d, &mut h, to, PpqnMode::Beats);
        assert_eq!(r.collisions, 0, "going up never collides");
        assert_eq!(r.lengthened, 0);
        let plan = plan_export(&d);
        assert_eq!(plan.rescale.is_some(), to > 32_767, "{to}");
        assert_eq!(
            r.lines()
                .iter()
                .any(|l| l.contains("a MIDI file can store")),
            to > 32_767
        );
        export(&d);
        // .lup keeps the native PPQN.
        let (back, _) = project_from_bytes(&project_to_bytes(&d).unwrap()).unwrap();
        assert_eq!(back.division(), Division::ppqn(to));
        assert_eq!(back.state_fingerprint(), d.state_fingerprint());
        // Up then down again is the identity (an upscale error is below half
        // an old tick).
        change(&mut d, &mut h, 960, PpqnMode::Beats);
        assert_eq!(d.state_fingerprint(), fp, "{to}");
    }
}

#[test]
fn preserve_seconds_keeps_ticks_and_the_time_of_every_note() {
    let old = song(960);
    let mut d = song(960);
    let mut h = History::default();
    let r = change(&mut d, &mut h, 1_000, PpqnMode::Seconds);
    assert!(r.seconds_mode);
    for i in 0..3 {
        assert_eq!(notes_of(&old, i), notes_of(&d, i));
    }
    same_layout(&d.clips().layout(), &old.clips().layout());
    let (mo, mn) = (
        TempoMap::for_document(&old, 48_000),
        TempoMap::for_document(&d, 48_000),
    );
    for v in d.tracks()[0].notes.iter() {
        let (a, b) = (
            mo.seconds_at_tick(v.start_tick),
            mn.seconds_at_tick(v.start_tick),
        );
        // Tempos are exact here except whole microseconds (400,000 × 1000/960
        // = 416,666.7 → 416,667: 8 × 10⁻⁷ relative).
        assert!((a - b).abs() <= a * 1e-6 + 1e-9, "{a} vs {b}");
    }
    assert_eq!(r.rounded_tempos, 1, "{r:?}");
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), old.state_fingerprint());
    export(&d);
}

#[test]
fn refusals_change_nothing() {
    let d = song(960);
    assert!(prepare_change_ppqn(&d, 960, PpqnMode::Beats).is_err());
    assert!(prepare_change_ppqn(&d, 0, PpqnMode::Beats).is_err());
    assert!(prepare_change_ppqn(&d, 2_147_483_648, PpqnMode::Beats).is_err());
    // A tick beyond 64 bits at the new PPQN is refused before anything changes.
    let mut big = PlumDocument::new_project("big".into(), Bpm::DEFAULT, 1);
    big.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(u64::MAX / 8, 1, 60)], false)
        .unwrap();
    let fp = big.state_fingerprint();
    assert!(prepare_change_ppqn(&big, 960, PpqnMode::Beats).is_err());
    assert_eq!(big.state_fingerprint(), fp);
    // Preserve seconds refuses a tempo a MIDI file cannot store.
    let mut fast = PlumDocument::new_project("fast".into(), Bpm::DEFAULT, 960);
    let e = fast
        .conductor()
        .prepare_set_tempo(None, 0, TempoValue::Micros(1), Interp::Step)
        .unwrap();
    History::default()
        .apply(
            &mut fast,
            Operation::Conductor {
                edit: e,
                label: "t",
            },
        )
        .unwrap();
    // 1 µs × 400 / 960 rounds to 0 µs: faster than 60,000,000 BPM.
    assert!(prepare_change_ppqn(&fast, 400, PpqnMode::Seconds).is_err());
    assert!(prepare_change_ppqn(&fast, 480, PpqnMode::Seconds).is_ok());
}

/// The persistent undo history restarts after a Change PPQN step (it holds
/// every note store, RAM only): after a restart the saved file opens at the
/// new PPQN, undo cannot go back past the step, and the steps after it are
/// still there.
#[test]
fn the_journal_restarts_after_a_ppqn_change() {
    use plumspirit_engine::history::HistoryLimits;
    use plumspirit_engine::history::journal::{Journal, identity, path_key};
    use plumspirit_engine::native::{LupControl, load_project, save_project};
    let d0 = std::env::temp_dir().join(format!("plumspirit-ppqn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d0);
    std::fs::create_dir_all(&d0).unwrap();
    let (file, jpath) = (d0.join("song.lup"), d0.join("song.plh"));
    let mut doc = song(960);
    save_project(&doc, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&doc)),
    )
    .unwrap();
    h.apply(&mut doc, Operation::SetName("before".into()))
        .unwrap();
    change(&mut doc, &mut h, 480, PpqnMode::Beats);
    h.apply(&mut doc, Operation::SetName("after".into()))
        .unwrap();
    assert_eq!(h.undo_len(), 3, "all three steps undo in this session");
    save_project(&doc, &file, LupControl::default()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, identity(&doc));
    let end = doc.state_fingerprint();
    drop(h);
    let (mut doc, _) = load_project(&file, LupControl::default()).unwrap();
    assert_eq!(doc.division(), Division::ppqn(480));
    assert_eq!(doc.state_fingerprint(), end);
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&doc)).unwrap();
    assert_eq!(h.undo_len(), 1, "only the step after the PPQN change");
    h.undo(&mut doc).unwrap();
    assert_eq!(doc.project().name, "before");
    assert_eq!(doc.division(), Division::ppqn(480));
    let _ = std::fs::remove_dir_all(&d0);
}

/// B1-0103 cost: 1,000,000 notes in 16 tracks with a controller lane each,
/// prepared (the Preview) and swapped in / out (Apply, Undo). Release:
/// `cargo test --release -p plumspirit_engine --test b1_ppqn_change change_ppqn_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn change_ppqn_cost() {
    let mut d = PlumDocument::new_project("cost".into(), Bpm::DEFAULT, 960);
    for _ in 1..16 {
        d.add_track().unwrap();
    }
    for i in 0..16usize {
        let recs: Vec<_> = (0..62_500u64)
            .map(|k| rec(k * 97 + i as u64, 120 + k % 50, 30 + (k % 64) as u8))
            .collect();
        let t = d.track_mut(i).unwrap();
        t.notes.insert_sorted_batch(&recs, false).unwrap();
        for k in 0..5_000u64 {
            t.events
                .push(RawEvent {
                    tick: k * 1_201,
                    anchor: 0,
                    kind: RawKind::Channel {
                        status: 0xB0,
                        data: [1, (k % 128) as u8],
                    },
                })
                .unwrap();
        }
    }
    assert_eq!(d.note_count(), 1_000_000);
    for to in [480u32, 30_720] {
        let mut times = Vec::new();
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let (tb, r) = prepare_change_ppqn(&d, to, PpqnMode::Beats).unwrap();
            let prep = t0.elapsed();
            assert_eq!(r.notes, 1_000_000);
            let mut h = History::default();
            let t1 = std::time::Instant::now();
            h.apply(
                &mut d,
                Operation::SetTimebase {
                    timebase: Box::new(tb),
                    label: "Change PPQN",
                },
            )
            .unwrap();
            let swap = t1.elapsed();
            h.undo(&mut d).unwrap();
            times.push((prep, swap));
        }
        times.sort();
        println!(
            "960 -> {to}: prepare {:?} / {:?} / {:?} (median), swap {:?}",
            times[0].0, times[1].0, times[2].0, times[1].1
        );
    }
}
