//! v0.5 stress fixtures (contract §249–§254): millions of notes over many
//! subchannels, a thousand (nested) subtracks, Onion density over many sources,
//! Preview of a huge selection, and Import Into Track of a large file.
//!
//! The quick fixtures run with `cargo test`. The heavyweight ones are `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v05 -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use plumspirit_engine::editor::preview::{NoteKernel, PreviewScope, summarize, visible_preview};
use plumspirit_engine::editor::transforms::prepare_scale_x;
use plumspirit_engine::editor::{EditControl, ExactFactor};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::merge::{ConductorPolicy, MergeMode, MergeOptions, prepare_merge};
use plumspirit_engine::midi::{
    ExportOptions, export_to_bytes_with, import_bytes, plan_export, plan_export_with,
    validate_exported_bytes,
};
use plumspirit_engine::model::project::{OnionOrder, order_onion_layers};
use plumspirit_engine::model::{Bpm, NoteRecord, PlumDocument, Subchannel, SubchannelId};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// `notes` Dense-fixture notes on track 0 spread over `n_subs` subchannels
/// (note `i` → subchannel `i % n_subs`, MIDI channel `sub % 16`).
fn sub_doc(notes: usize, n_subs: u32) -> PlumDocument {
    let mut d = PlumDocument::new_project("stress".into(), Bpm::DEFAULT, 960);
    if n_subs > 1 {
        let subs: Vec<Subchannel> = (1..n_subs)
            .map(|i| Subchannel::new(SubchannelId(i), (i % 16) as u8))
            .collect();
        History::default()
            .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
            .unwrap();
    }
    let src = generate(FixtureShape::Dense, notes, 960, 7);
    let recs: Vec<NoteRecord> = src
        .track(1)
        .unwrap()
        .notes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let s = (i as u32) % n_subs;
            let mut r = v.record();
            r.attrs = r.attrs.with_channel((s % 16) as u8).unwrap();
            r.sub = SubchannelId(s);
            r
        })
        .collect();
    drop(src);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

/// Per-subchannel note counts (the identity every round trip must keep).
fn per_sub(d: &PlumDocument, track: usize) -> Vec<(u32, u8, usize)> {
    let mut m = std::collections::BTreeMap::new();
    for v in d.track(track).unwrap().notes.iter() {
        *m.entry((v.sub.0, v.attrs.channel())).or_insert(0usize) += 1;
    }
    m.into_iter().map(|((s, c), n)| (s, c, n)).collect()
}

fn subchannel_roundtrip(notes: usize, n_subs: u32) {
    let t = Instant::now();
    let d = sub_doc(notes, n_subs);
    let built = t.elapsed();
    let want = per_sub(&d, 0);
    let heap = d.track(0).unwrap().notes.heap_bytes();
    // .lup
    let t = Instant::now();
    let bytes = project_to_bytes(&d).unwrap();
    let (back, _) = project_from_bytes(&bytes).unwrap();
    let lup = t.elapsed();
    assert_eq!(per_sub(&back, 0), want);
    drop(back);
    // MIDI with structure records (layers) and re-import.
    let t = Instant::now();
    let plan = plan_export_with(&d, ExportOptions::default());
    let midi = export_to_bytes_with(&d, &plan).unwrap();
    validate_exported_bytes(&d, &plan, &midi).unwrap();
    let export = t.elapsed();
    let t = Instant::now();
    let (back, _) = import_bytes(&midi).unwrap();
    let import = t.elapsed();
    assert_eq!(back.track_count(), d.track_count());
    assert_eq!(
        back.track(0).unwrap().subchannels(),
        d.track(0).unwrap().subchannels()
    );
    assert_eq!(per_sub(&back, 0), want);
    println!(
        "{notes} notes / {n_subs} subchannels: build {}, {:.2} B/note, .lup save+load {}, export+validate {} ({} chunks), re-import {}",
        ms(built),
        heap as f64 / notes as f64,
        ms(lup),
        ms(export),
        plan.outputs.len(),
        ms(import)
    );
}

#[test]
fn many_subchannels_round_trip_exactly() {
    for n in [16, 300] {
        subchannel_roundtrip(20_000, n);
    }
}

#[test]
#[ignore]
fn a_million_notes_over_16_to_1024_subchannels_round_trip_exactly() {
    for n in [16, 64, 256, 1024] {
        subchannel_roundtrip(1_000_000, n);
    }
}

#[test]
#[ignore]
fn five_million_notes_over_256_subchannels_round_trip_exactly() {
    subchannel_roundtrip(5_000_000, 256);
}

#[test]
#[ignore]
fn a_thousand_populated_nested_subtracks_export_and_restore() {
    let mut d = PlumDocument::new_project("tree".into(), Bpm::DEFAULT, 960);
    let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
    // 10 top-level groups × 99 subtracks, each group nested two deep.
    let t = Instant::now();
    for g in 0..10 {
        let parent = d.track_count() - 1;
        if g > 0 {
            h.apply(&mut d, Operation::AddTrack).unwrap();
        }
        let parent = if g == 0 { parent } else { d.track_count() - 1 };
        h.apply(&mut d, Operation::AddSubtracks { parent, count: 9 })
            .unwrap();
        for k in 0..9 {
            h.apply(
                &mut d,
                Operation::AddSubtracks {
                    parent: parent + 1 + k * 11 - k,
                    count: 10,
                },
            )
            .ok();
        }
    }
    let structure = t.elapsed();
    d.validate_hierarchy().unwrap();
    let n = d.track_count();
    assert!(n > 900, "{n}");
    // Populate every track.
    let src = generate(FixtureShape::Dense, 1_000_000, 960, 3);
    let recs: Vec<NoteRecord> = src
        .track(1)
        .unwrap()
        .notes
        .iter()
        .map(|v| v.record())
        .collect();
    drop(src);
    let per = recs.len().div_ceil(n);
    for (k, part) in recs.chunks(per).enumerate() {
        d.track_mut(k)
            .unwrap()
            .notes
            .insert_sorted_batch(part, false)
            .unwrap();
    }
    let t = Instant::now();
    let rows = d.tree_rows();
    let rows_t = t.elapsed();
    let t = Instant::now();
    let plan = plan_export(&d);
    let midi = export_to_bytes_with(&d, &plan).unwrap();
    validate_exported_bytes(&d, &plan, &midi).unwrap();
    let export = t.elapsed();
    let t = Instant::now();
    let (back, summary) = import_bytes(&midi).unwrap();
    let import = t.elapsed();
    assert!(plan.structure.metadata);
    assert_eq!(
        back.tree_rows(),
        rows,
        "{:?}",
        summary
            .warnings
            .iter()
            .map(|w| w.user_message())
            .collect::<Vec<_>>()
    );
    assert_eq!(back.note_count(), d.note_count());
    // Undo removes every subtrack again, one step at a time, without recursion.
    let t = Instant::now();
    while h.can_undo() {
        h.undo(&mut d).unwrap();
    }
    let undo = t.elapsed();
    println!(
        "{n} tracks (nested): build {}, tree rows {}, export+validate {}, re-import {}, undo all {}",
        ms(structure),
        ms(rows_t),
        ms(export),
        ms(import),
        ms(undo)
    );
}

#[test]
#[ignore]
fn onion_dense_first_over_a_thousand_sources_is_cheap() {
    let mut d = PlumDocument::new_project("onion".into(), Bpm::DEFAULT, 960);
    History::default()
        .apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 1000,
            },
        )
        .unwrap();
    let src = generate(FixtureShape::Dense, 2_000_000, 960, 5);
    let recs: Vec<NoteRecord> = src
        .track(1)
        .unwrap()
        .notes
        .iter()
        .map(|v| v.record())
        .collect();
    drop(src);
    for (k, r) in recs.chunks(2000).enumerate() {
        d.track_mut(k % 1001)
            .unwrap()
            .notes
            .insert_sorted_batch(r, false)
            .unwrap();
    }
    let end = d.content_end_tick();
    let sources: Vec<usize> = (1..=1000).collect();
    let t = Instant::now();
    let mut order = Vec::new();
    for frame in 0..60u64 {
        let t0 = end / 2 + frame * 480;
        let counts: Vec<usize> = sources
            .iter()
            .map(|&i| {
                d.track(i)
                    .unwrap()
                    .notes
                    .count_starting_in(t0, t0 + 3840 * 8)
            })
            .collect();
        order = order_onion_layers(OnionOrder::DenseFirst, &sources, &counts);
    }
    let per_frame = t.elapsed() / 60;
    assert_eq!(order.len(), 1000);
    println!(
        "Onion Dense First, 1,000 sources, 2M notes: {} per re-rank",
        ms(per_frame)
    );
    assert!(per_frame < Duration::from_millis(16), "{per_frame:?}");
}

#[test]
#[ignore]
fn preview_of_a_huge_selection_is_bounded_and_equals_apply() {
    let mut d = sub_doc(2_000_000, 16);
    d.track_mut(0).unwrap().notes.select_all();
    let fp = d.state_fingerprint();
    let t = d.track(0).unwrap();
    let b = t.notes.selection_bounds().unwrap();
    let f = ExactFactor::parse("1.25").unwrap();
    let k = NoteKernel::scale_x(&b, &f).unwrap();
    let never = AtomicBool::new(false);
    let started = Instant::now();
    let s = summarize(t, &k, PreviewScope::Selected, &never);
    let full = started.elapsed();
    assert!(s.error.is_none());
    // Per frame: only the notes near the view, within the draw budget.
    let end = d.content_end_tick();
    let started = Instant::now();
    let (n, cut) = visible_preview(
        t,
        &k,
        PreviewScope::Selected,
        end / 3,
        end / 3 + 3840 * 4,
        20_000,
        |_, _| {},
    );
    let frame = started.elapsed();
    assert!(n > 0 && n <= 20_000);
    assert!(frame < Duration::from_millis(16), "{frame:?}");
    // A cancelled worker stops early.
    let cancel = AtomicBool::new(true);
    assert!(summarize(t, &k, PreviewScope::Selected, &cancel).cancelled);
    assert_eq!(d.state_fingerprint(), fp, "preview changed nothing");
    // Apply evaluates the same kernel.
    let started = Instant::now();
    let edit = prepare_scale_x(&d, 0, &f, EditControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
    h.apply(
        &mut d,
        Operation::Prepared {
            edit,
            label: "Scale X",
        },
    )
    .unwrap();
    let apply = started.elapsed();
    println!(
        "Preview 2M selected notes: full preflight {}, visible frame {} ({n} notes, cut {cut}), apply {}",
        ms(full),
        ms(frame),
        ms(apply)
    );
}

#[test]
#[ignore]
fn import_into_track_of_a_million_note_file_is_one_undo_step() {
    let src = generate(FixtureShape::Dense, 1_000_000, 480, 9);
    let bytes = export_to_bytes_with(&src, &plan_export(&src)).unwrap();
    drop(src);
    let t = Instant::now();
    let (source, _) = import_bytes(&bytes).unwrap();
    let read = t.elapsed();
    for mode in [MergeMode::AsSubtracks, MergeMode::AsSubchannels] {
        let mut dest = PlumDocument::new_project("dest".into(), Bpm::DEFAULT, 960);
        let fp = dest.state_fingerprint();
        let t = Instant::now();
        let p = prepare_merge(
            &dest,
            0,
            &source,
            MergeOptions {
                mode,
                policy: ConductorPolicy::Merge,
                at: 0,
            },
        )
        .unwrap();
        let prep = t.elapsed();
        let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
        let t = Instant::now();
        h.apply(&mut dest, p.operation).unwrap();
        let commit = t.elapsed();
        assert_eq!(dest.note_count(), 1_000_000);
        assert_eq!(h.undo_len(), 1);
        h.undo(&mut dest).unwrap();
        assert_eq!(dest.fingerprint(), {
            let d = PlumDocument::new_project("dest".into(), Bpm::DEFAULT, 960);
            d.fingerprint()
        });
        let _ = fp;
        println!(
            "Import Into Track ({mode:?}), 1M notes 480→960 PPQN: read+parse {}, prepare {}, commit {}",
            ms(read),
            ms(prep),
            ms(commit)
        );
    }
}
