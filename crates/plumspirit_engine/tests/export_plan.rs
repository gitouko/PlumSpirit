//! MIDI export of native-only values: exact project tempo (substituted or
//! injected, with rounding disclosed) and PPQN above the MIDI header limit
//! (rescaled, validated, never truncated or wrapped).

mod common;

use common::*;
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{
    self, export_to_bytes_with, import_bytes, plan_export, semantic::semantic_from_bytes,
    validate_exported_bytes,
};
use plumspirit_engine::model::{Bpm, PlumDocument, TempoFidelity, TempoValue};

fn tempo_events(bytes: &[u8]) -> Vec<(usize, u64, u32)> {
    let s = semantic_from_bytes(bytes).unwrap();
    let mut out = Vec::new();
    for (ti, t) in s.tracks.iter().enumerate() {
        for (tick, ev) in &t.other {
            if ev[0] == 0xFF && ev[1] == 0x51 {
                // canonical bytes: FF 51 len(u32 BE) payload
                let p = &ev[6..];
                out.push((ti, *tick, u32::from_be_bytes([0, p[0], p[1], p[2]])));
            }
        }
    }
    out
}

fn set_start_tempo(doc: &mut PlumDocument, bpm: &str) {
    let edit = doc
        .conductor()
        .prepare_set_start_tempo(TempoValue::Bpm(Bpm::parse(bpm).unwrap()))
        .unwrap();
    History::default()
        .apply(
            doc,
            Operation::Conductor {
                edit,
                label: "Change Tempo",
            },
        )
        .unwrap();
}

#[test]
fn new_project_tempo_is_written_to_the_first_track() {
    let mut doc = PlumDocument::new_project("T".into(), Bpm::parse("150").unwrap(), 960);
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 960, 60), rec(960, 960, 62)], false)
        .unwrap();
    let plan = plan_export(&doc);
    assert!(!plan.needs_approval());
    assert_eq!(plan.tempo.events, 1);
    assert_eq!(plan.signatures, 1);
    let bytes = export_to_bytes_with(&doc, &plan).unwrap();
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    assert_eq!(tempo_events(&bytes), vec![(0, 0, 400_000)]);
    // Re-import: notes identical; the tempo and time signature become the
    // Conductor again (not raw track events).
    let (back, _) = import_bytes(&bytes).unwrap();
    assert_eq!(back.note_count(), 2);
    assert_eq!(back.tracks()[0].events.tempos().count(), 0);
    assert_eq!(back.conductor().start_tempo(), TempoValue::Micros(400_000));
    assert_eq!(back.meter_events(0), vec![(0, 4, 2)]);
}

#[test]
fn file_tempos_are_adopted_and_the_last_same_tick_tempo_wins() {
    let tr0 = {
        let mut t = TrackBytes::new();
        t.meta(0, 0x51, &[0x07, 0xA1, 0x20]) // 500,000 µs
            .meta(0, 0x51, &[0x06, 0x1A, 0x80]) // 400,000 µs (last at tick 0 wins)
            .meta(480, 0x51, &[0x0F, 0x42, 0x40]); // 1,000,000 µs later: kept
        t.eot(960)
    };
    let tr1 = {
        let mut t = TrackBytes::new();
        t.on(0, 0, 60, 100).off(480, 0, 60, 0);
        t.eot(960)
    };
    let (mut doc, summary) = import_bytes(&smf(1, 480, &[tr0, tr1])).unwrap();
    // One effective tempo per tick, chosen by track/event order; the shadowed
    // event is reported, not played twice.
    let points: Vec<_> = doc
        .conductor()
        .tempo_points()
        .iter()
        .map(|p| (p.tick, p.value))
        .collect();
    assert_eq!(
        points,
        vec![
            (0, TempoValue::Micros(400_000)),
            (480, TempoValue::Micros(1_000_000))
        ]
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.user_message().contains("never took effect")),
        "{:?}",
        summary.warnings
    );
    // Unchanged export: the tempo map that plays, nothing rounded.
    let plan = plan_export(&doc);
    assert!(plan.tempo.lossy.is_empty());
    assert!(!plan.needs_approval());
    let bytes = export_to_bytes_with(&doc, &plan).unwrap();
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    assert_eq!(
        tempo_events(&bytes),
        vec![(0, 0, 400_000), (0, 480, 1_000_000)]
    );
    // Changing the start tempo replaces only the tick-0 tempo; the change at 480
    // stays. The decimal BPM is rounded to whole microseconds and disclosed.
    set_start_tempo(&mut doc, "123.456");
    let plan = plan_export(&doc);
    assert_eq!(plan.tempo.lossy.len(), 1);
    assert_eq!(plan.tempo.lossy[0].midi.fidelity, TempoFidelity::Rounded);
    assert!(!plan.needs_approval());
    assert_eq!(plan.disclosures().len(), 1);
    let bytes = export_to_bytes_with(&doc, &plan).unwrap();
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    assert_eq!(
        tempo_events(&bytes),
        vec![(0, 0, 486_003), (0, 480, 1_000_000)]
    );
}

#[test]
fn out_of_range_tempo_needs_approval() {
    for (bpm, micros) in [("0.5", 0xFF_FFFF), ("1e9", 1)] {
        let doc = PlumDocument::new_project("T".into(), Bpm::parse(bpm).unwrap(), 96);
        let plan = plan_export(&doc);
        assert!(plan.needs_approval(), "{bpm}");
        assert_eq!(plan.tempo.lossy[0].midi.micros, micros);
        let bytes = export_to_bytes_with(&doc, &plan).unwrap();
        validate_exported_bytes(&doc, &plan, &bytes).unwrap();
        assert_eq!(tempo_events(&bytes), vec![(0, 0, micros)]);
        // The project keeps its exact BPM.
        assert_eq!(
            doc.conductor().start_tempo(),
            TempoValue::Bpm(Bpm::parse(bpm).unwrap())
        );
    }
}

#[test]
fn high_ppqn_rescales_exactly_when_possible() {
    let mut doc = PlumDocument::new_project("R".into(), Bpm::DEFAULT, 96_000);
    // All positions on a 1/16 grid (24,000 ticks) → exact at a smaller PPQN.
    let recs: Vec<_> = (0..500u64)
        .map(|i| rec(i * 24_000, 24_000, (i % 128) as u8))
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let plan = plan_export(&doc);
    let r = plan.rescale.clone().unwrap();
    assert!(r.exact);
    assert!(r.to_ppqn <= 32_767);
    assert_eq!(96_000 % r.to_ppqn, 0);
    assert!(
        plan.needs_approval(),
        "a changed timebase is always confirmed"
    );
    let bytes = export_to_bytes_with(&doc, &plan).unwrap();
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    let s = semantic_from_bytes(&bytes).unwrap();
    assert_eq!(u32::from(s.division), r.to_ppqn);
    let (back, _) = import_bytes(&bytes).unwrap();
    // Positions scale exactly: note k at k · 24,000 · to/from.
    let k = u64::from(r.to_ppqn) / 4;
    let starts: Vec<u64> = back.tracks()[0]
        .notes
        .iter()
        .map(|n| n.start_tick)
        .collect();
    assert_eq!(starts, (0..500u64).map(|i| i * k).collect::<Vec<_>>());
    // Musical time is identical: same number of quarter notes.
    assert_eq!(
        back.duration_ticks() * 96_000 / u64::from(r.to_ppqn),
        doc.duration_ticks()
    );
}

#[test]
fn high_ppqn_lossy_rescale_keeps_order_and_validates() {
    let mut doc = PlumDocument::new_project("R".into(), Bpm::DEFAULT, 1_000_003);
    // Irregular positions, including same-pitch neighbours that merge.
    let mut recs = Vec::new();
    let mut t = 0u64;
    for i in 0..3000u64 {
        t += 1 + (i * 7919) % 50_000;
        recs.push(rec(t, 1 + (i * 104_729) % 90_000, 40 + (i % 3) as u8));
    }
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let plan = plan_export(&doc);
    let r = plan.rescale.clone().unwrap();
    assert_eq!(r.to_ppqn, 30_720);
    assert!(!r.exact && r.notes_moved > 0);
    assert!(!plan.disclosures().is_empty());
    let bytes = export_to_bytes_with(&doc, &plan).unwrap();
    // The validator checks every event (tick and bytes) in original order.
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    let (back, _) = import_bytes(&bytes).unwrap();
    assert_eq!(back.note_count(), 3000);
    // Every note start is within half a target tick of its exact position.
    for (a, b) in back.tracks()[0]
        .notes
        .iter()
        .zip(doc.tracks()[0].notes.iter())
    {
        let exact = b.start_tick as f64 * 30_720.0 / 1_000_003.0;
        assert!((a.start_tick as f64 - exact).abs() <= 0.5 + 1e-9);
    }
}

#[test]
fn ppqn_never_truncates_the_header() {
    // The largest native PPQN: the header must never contain a wrapped value.
    let doc = PlumDocument::new_project("R".into(), Bpm::DEFAULT, 2_147_483_647);
    let plan = plan_export(&doc);
    let bytes = midi::export_to_bytes(&doc).unwrap();
    let division = u16::from_be_bytes([bytes[12], bytes[13]]);
    assert_eq!(division, plan.division_word);
    assert!(division > 0 && division <= 0x7FFF);
    // Nothing constrains an empty project, so a regular resolution is chosen.
    assert_eq!(division, 30_720);
    assert!(plan.rescale.as_ref().unwrap().exact);
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
}
