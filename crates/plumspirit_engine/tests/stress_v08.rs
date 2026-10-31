//! v0.8 stress (contract §261–262 "v0.8 suites", adversarial review): the
//! built-in plugin tools (HZ-Chop, SAFOR) and the revamped Paint through the
//! Note Transformation Platform on a million notes — preflight bound, preview
//! changes nothing, exact undo / redo; a million-note Image to MIDI paste into
//! Subchannels and into new Subtracks (one undo step each, exact); and the
//! persistent history: million-note steps journaled, the project "restarted",
//! every step undone from disk and redone to exact identities.
//!
//! The quick versions run with `cargo test`. The heavyweight ones are
//! `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v08 -- --ignored --nocapture --test-threads=1
//! ```

use plumspirit_engine::editor::clipboard::Clipboard;
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId, Track, TrackId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use plumspirit_engine::transform::image_midi::{self as im, Rgba, Settings};
use plumspirit_engine::transform::paste::{self, PasteMode, PasteSpec};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{collect_with, commit};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, TrackScope, TransformError, TransformRun, run_kernel, tool,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The Dense fixture keeps its notes in track 1.
const ART: usize = 1;

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1e3)
}

/// The Dense fixture (12 notes on every 1/16, random keys: doubled notes and
/// overlaps for SAFOR) with Subchannels 1–15 on the art track (for Paint).
fn fixture(notes: usize) -> PlumDocument {
    let mut d = generate(FixtureShape::Dense, notes, 960, 8);
    let have = d.track(ART).unwrap().subchannels().clone();
    let subs: Vec<Subchannel> = (1..16u32)
        .filter(|&i| !have.contains(SubchannelId(i)))
        .map(|i| Subchannel::new(SubchannelId(i), (i % 16) as u8))
        .collect();
    if !subs.is_empty() {
        let mut setup = History::default();
        setup
            .apply(&mut d, Operation::AddSubchannels { track: ART, subs })
            .unwrap();
    }
    d.track_mut(ART).unwrap().notes.select_all();
    d
}

/// Runs tool `id` with `set` on the selection of the art track (preview =
/// the run; it must leave the document unchanged and stay within the
/// preflight estimate).
fn run(
    d: &PlumDocument,
    id: &str,
    set: &[(&str, ParamValue)],
) -> Result<(TransformRun, Duration), TransformError> {
    let def = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let sc = [TrackScope {
        index: ART,
        all: false,
        content: None,
    }];
    let before = d.state_fingerprint();
    let t0 = Instant::now();
    let src = collect_with(def, d, &sc, &ps, 1 << 27)?;
    let mut ctx = Ctx::simple(960);
    ctx.groups = std::sync::Arc::new(d.project().groups.clone());
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&ps, &b)?;
    let est = k.estimate(&src.stats).notes;
    let limits = Limits::for_system_memory(None);
    let r = run_kernel(k.as_ref(), &src, def.boundary(&ps), &limits, None)?;
    let took = t0.elapsed();
    assert!(
        r.notes.len() as u64 <= est,
        "{id}: {} notes out, estimated at most {est}",
        r.notes.len()
    );
    assert_eq!(
        d.state_fingerprint(),
        before,
        "{id}: the preview changed the document"
    );
    Ok((r, took))
}

/// Commit (one undo step), undo, redo, undo — each state exact.
fn round_trip(
    d: &mut PlumDocument,
    h: &mut History,
    id: &str,
    set: &[(&str, ParamValue)],
) -> Result<String, TransformError> {
    let before = d.state_fingerprint();
    let undo_len = h.undo_len();
    let (r, t_run) = run(d, id, set)?;
    let out = r.notes.len();
    let t1 = Instant::now();
    commit(h, d, &r, "stress")?;
    let t_apply = t1.elapsed();
    drop(r);
    assert_eq!(h.undo_len(), undo_len + 1, "{id}: not one undo step");
    let after = d.state_fingerprint();
    assert_ne!(after, before, "{id}: nothing changed");
    let t2 = Instant::now();
    h.undo(d).unwrap();
    let t_undo = t2.elapsed();
    assert_eq!(d.state_fingerprint(), before, "{id}: undo is not exact");
    let t3 = Instant::now();
    h.redo(d).unwrap();
    let t_redo = t3.elapsed();
    assert_eq!(d.state_fingerprint(), after, "{id}: redo is not exact");
    h.undo(d).unwrap();
    d.track_mut(ART).unwrap().notes.select_all();
    Ok(format!(
        "{out} out; run {}, apply {}, undo {}, redo {}",
        ms(t_run),
        ms(t_apply),
        ms(t_undo),
        ms(t_redo)
    ))
}

fn on() -> ParamValue {
    ParamValue::Bool(true)
}

fn ch(v: u32) -> ParamValue {
    ParamValue::Choice(v)
}

/// (tool id, description, parameter values).
type Case = (&'static str, &'static str, Vec<(&'static str, ParamValue)>);

fn plugin_tools_round_trip(notes: usize) {
    let mut d = fixture(notes);
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    let cases: Vec<Case> = vec![
        // Fixed key 0 (8.18 Hz): about two chops per note; the step grows them.
        (
            "hz_chop",
            "fixed key, step +5",
            vec![
                ("inote", ParamValue::Int(0)),
                ("fstep", ParamValue::Float(5.0)),
            ],
        ),
        (
            "hz_chop",
            "High Precision, replace",
            vec![
                ("inote", ParamValue::Int(12)),
                ("fstep", ParamValue::Float(-3.0)),
                ("mode", ch(1)),
                ("replace", on()),
            ],
        ),
        ("safor", "overlaps", vec![("mode", ch(0))]),
        ("safor", "sustains + overlaps", vec![("mode", ch(1))]),
        (
            "safor",
            "velocity threshold",
            vec![("mode", ch(2)), ("threshold", ParamValue::Int(40))],
        ),
        ("subpaint", "feature 1", vec![("every", ch(4))]),
        (
            "subpaint",
            "walk + momentum",
            vec![("every", ch(4)), ("a1", on()), ("a13", on())],
        ),
        (
            "subpaint",
            "density + fan + regions",
            vec![("a7", on()), ("a8", on()), ("a9", on())],
        ),
        ("subpaint", "contrast", vec![("a10", on())]),
        (
            "subpaint",
            "symmetry + field",
            vec![("a11", on()), ("a14", on())],
        ),
    ];
    let start = d.state_fingerprint();
    for (id, what, set) in &cases {
        match round_trip(&mut d, &mut h, id, set) {
            Ok(line) => println!("{id:>8} {what:<24} on {notes} notes: {line}"),
            Err(TransformError::Unchanged) => {
                panic!("{id} ({what}): the parameters must change the fixture")
            }
            Err(e) => panic!("{id} ({what}): {}", e.user_message()),
        }
    }
    assert_eq!(d.state_fingerprint(), start);
}

#[test]
fn ten_thousand_notes_round_trip_exactly_through_the_plugin_tools_and_paint() {
    plugin_tools_round_trip(10_000);
}

#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn a_million_notes_round_trip_exactly_through_the_plugin_tools_and_paint() {
    plugin_tools_round_trip(1_000_000);
}

/// A picture of `w` × 128 pixels in eight colours, every pixel opaque.
fn stripes(w: u32) -> Rgba {
    let mut px = Vec::with_capacity(w as usize * 128);
    for y in 0..128u32 {
        for x in 0..w {
            let [r, g, b] = color((x / 7 + y / 5) % 8);
            px.push([r, g, b, 255]);
        }
    }
    Rgba::new(w, 128, px).unwrap()
}

/// Colour `c` (0–7) of the picture: distinct in every channel.
fn color(c: u32) -> [u8; 3] {
    [
        (c * 30 + 10) as u8,
        (250 - c * 30) as u8,
        ((c * 70) % 251) as u8,
    ]
}

fn palette() -> Vec<[u8; 3]> {
    (0..8).map(color).collect()
}

fn image_paste(w: u32) {
    let s = Settings {
        first_key: 0,
        last_key: 127,
        time_pixels: Some(w),
        ticks_per_pixel: 30,
        palette: palette(),
        pixel_notes: true,
        ..Default::default()
    };
    let t = Instant::now();
    let img = im::prepare(&stripes(w), &s).unwrap();
    let art = im::convert(&img, &s, 4_000_000).unwrap();
    let t_convert = t.elapsed();
    let n = art.notes.len();
    assert_eq!(n, w as usize * 128, "one note per pixel");
    let counts = art.counts();
    assert_eq!(counts.len(), 8);
    assert_eq!(counts.iter().sum::<u64>(), n as u64);

    let mut d = fixture(10_000);
    let dest = d.track(ART).unwrap().id();
    let tracks_before = d.track_count();
    let notes_before = d.track(ART).unwrap().notes.len();
    let before = d.state_fingerprint();
    let base_fp = d.fingerprint();
    let mut h = History::new(HistoryLimits::for_system_memory(None));

    // Colours → Subchannels 20 … 27 of the art track.
    let mut spec = PasteSpec::new(dest, 3840);
    spec.sub_offset = 20;
    let t = Instant::now();
    let p = paste::plan(&d, &art, &spec);
    assert!(p.error.is_none(), "{:?}", p.error);
    let op = paste::build(&d, &art, &spec, &p, "Paste Artwork").unwrap();
    h.apply(&mut d, op).unwrap();
    let t_sub = t.elapsed();
    assert_eq!(h.undo_len(), 1);
    let tr = d.track(ART).unwrap();
    assert_eq!(tr.notes.len(), notes_before + n);
    for c in 0..8u32 {
        assert!(tr.subchannels().contains(SubchannelId(20 + c)));
    }
    let after_sub = d.state_fingerprint();
    let t = Instant::now();
    h.undo(&mut d).unwrap();
    let t_undo = t.elapsed();
    assert_eq!(
        d.state_fingerprint(),
        before,
        "undo of the subchannel paste"
    );
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after_sub);
    h.undo(&mut d).unwrap();

    // Colours → eight new Subtracks of the art track.
    spec.mode = PasteMode::Subtracks;
    spec.sub_offset = 0;
    let t = Instant::now();
    let p = paste::plan(&d, &art, &spec);
    assert!(p.error.is_none(), "{:?}", p.error);
    let op = paste::build(&d, &art, &spec, &p, "Paste Artwork").unwrap();
    h.apply(&mut d, op).unwrap();
    let t_trk = t.elapsed();
    assert_eq!(d.track_count(), tracks_before + 8);
    let mut pasted: Vec<u64> = (0..d.track_count())
        .filter_map(|i| d.track(i))
        .filter(|t| t.parent() == Some(dest))
        .map(|t| t.notes.len() as u64)
        .collect();
    pasted.sort_unstable();
    let mut want = counts.clone();
    want.sort_unstable();
    assert_eq!(pasted, want, "one Subtrack per colour with its notes");
    assert_eq!(d.track(ART).unwrap().notes.len(), notes_before);
    let after_trk = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    // Track ids are never reused: undo removes the subtracks and their notes
    // exactly; the next free track id stays advanced (by design), so the
    // musical fingerprint and the track list are compared.
    assert_eq!(d.track_count(), tracks_before);
    assert_eq!(d.fingerprint(), base_fp, "undo of the subtrack paste");
    h.redo(&mut d).unwrap();
    assert_eq!(
        d.state_fingerprint(),
        after_trk,
        "redo of the subtrack paste"
    );
    h.undo(&mut d).unwrap();
    assert_eq!(d.fingerprint(), base_fp);
    println!(
        "Image to MIDI {w} × 128 px → {n} notes: convert {}, paste to subchannels {} (undo {}), to subtracks {}",
        ms(t_convert),
        ms(t_sub),
        ms(t_undo),
        ms(t_trk)
    );
}

#[test]
fn a_sixty_five_thousand_pixel_picture_pastes_into_subchannels_and_subtracks_exactly() {
    image_paste(512);
}

#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn a_million_pixel_picture_pastes_into_subchannels_and_subtracks_exactly() {
    image_paste(8192);
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-s08-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn reopen(file: &Path, jpath: &Path) -> (PlumDocument, History) {
    let (doc, _) = load_project(file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(jpath, path_key(file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&doc)).unwrap();
    (doc, h)
}

/// Million-note steps (SAFOR, a paste of `notes` notes, Paint, HZ-Chop)
/// journaled; the project is saved half-way and "restarted" at the end; the
/// unsaved steps are restored, every step undoes from disk to the exact
/// earlier state and redoes to the exact later one.
fn history_restart(notes: usize) {
    let d0 = dir(&format!("restart-{notes}"));
    let file = d0.join("song.lup");
    let jpath = d0.join("song.plh");
    let mut doc = fixture(notes);
    save_project(&doc, &file, LupControl::default()).unwrap();
    // The app's smallest undo memory (64 MB) and a RAM window of two steps:
    // older steps live on disk only.
    let mut h = History::new(HistoryLimits {
        max_bytes: 64 * 1024 * 1024,
        max_entries: 2,
    });
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&doc))).unwrap();

    let mut states = vec![(identity(&doc), doc.state_fingerprint())];
    let push = |doc: &PlumDocument, v: &mut Vec<(u64, u64)>| {
        v.push((identity(doc), doc.state_fingerprint()))
    };
    let t = Instant::now();
    // 1. SAFOR (sustains and overlaps).
    let (r, _) = run(&doc, "safor", &[("mode", ch(1))]).unwrap();
    commit(&mut h, &mut doc, &r, "SAFOR").unwrap();
    drop(r);
    push(&doc, &mut states);
    // 2. Paste `notes` notes at the end of the art (a second track).
    let end = doc.track(ART).unwrap().notes.max_end().unwrap_or(0);
    let big: Vec<NoteRecord> = (0..notes as u64)
        .map(|k| {
            NoteRecord::new(
                k * 3,
                90,
                NoteAttrs::new(20 + (k % 90) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            )
        })
        .collect();
    let mut scratch = Track::new(TrackId(99_999));
    scratch.notes.insert_sorted_batch(&big, false).unwrap();
    drop(big);
    scratch.notes.select_in_region(0, u64::MAX, 0, 127);
    let clip = Clipboard::copy_selected(&scratch).unwrap().unwrap();
    drop(scratch);
    let p = clip.prepare_paste(&doc, 0, end).unwrap();
    h.apply(
        &mut doc,
        Operation::Prepared {
            edit: p,
            label: "Paste",
        },
    )
    .unwrap();
    push(&doc, &mut states);
    // Saved here (the saved state survives the restart).
    save_project(&doc, &file, LupControl::default()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, identity(&doc));
    let saved_at = states.len() - 1;
    // 3. Paint, 4. HZ-Chop — not saved.
    doc.track_mut(ART).unwrap().notes.select_all();
    let (r, _) = run(&doc, "subpaint", &[("every", ch(4)), ("a1", on())]).unwrap();
    commit(&mut h, &mut doc, &r, "Paint").unwrap();
    drop(r);
    push(&doc, &mut states);
    doc.track_mut(ART).unwrap().notes.select_all();
    let (r, _) = run(&doc, "hz_chop", &[("inote", ParamValue::Int(0))]).unwrap();
    commit(&mut h, &mut doc, &r, "HZ-Chop").unwrap();
    drop(r);
    push(&doc, &mut states);
    let t_steps = t.elapsed();
    assert!(!h.is_at_saved_state());
    let bytes = std::fs::metadata(&jpath).unwrap().len();
    drop(h);
    drop(doc);

    // Restart: the file holds state 2; the journal brings 3 and 4 back.
    let t = Instant::now();
    let (mut doc, mut h) = reopen(&file, &jpath);
    let t_reopen = t.elapsed();
    assert_eq!(identity(&doc), states[saved_at].0);
    let t = Instant::now();
    while h.can_redo() {
        h.redo(&mut doc).unwrap();
    }
    let t_redo_unsaved = t.elapsed();
    let last = states.len() - 1;
    assert_eq!(
        (identity(&doc), doc.state_fingerprint()),
        states[last],
        "the unsaved steps are redone exactly after the restart"
    );
    let t = Instant::now();
    for i in (0..last).rev() {
        h.undo(&mut doc).unwrap();
        assert_eq!(
            (identity(&doc), doc.state_fingerprint()),
            states[i],
            "undo from disk reaches state {i} exactly"
        );
        assert_eq!(h.is_at_saved_state(), i == saved_at);
    }
    assert!(!h.can_undo());
    let t_undo_all = t.elapsed();
    let t = Instant::now();
    for (i, want) in states.iter().enumerate().skip(1) {
        h.redo(&mut doc).unwrap();
        assert_eq!(
            (identity(&doc), doc.state_fingerprint()),
            *want,
            "redo reaches state {i} exactly"
        );
    }
    let t_redo_all = t.elapsed();
    println!(
        "history restart, {notes}-note fixture: 4 steps {} ({} journal bytes); reopen {}; redo unsaved {}; undo all from disk {}; redo all {}",
        ms(t_steps),
        bytes,
        ms(t_reopen),
        ms(t_redo_unsaved),
        ms(t_undo_all),
        ms(t_redo_all)
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&d0);
}

#[test]
fn twenty_thousand_note_steps_survive_a_restart_and_undo_exactly_from_disk() {
    history_restart(20_000);
}

#[test]
#[ignore = "heavyweight: run in release with --ignored"]
fn million_note_steps_survive_a_restart_and_undo_exactly_from_disk() {
    history_restart(1_000_000);
}
