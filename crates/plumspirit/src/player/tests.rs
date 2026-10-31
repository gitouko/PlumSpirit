//! The Player's tile family (v0.9 V09-S17 step 4a): drawn by the dense
//! renderer, its tiles equal `tiles::build_tile_read` of the export
//! snapshot's sources as the engine's Player tests define them
//! (`tests/v09_player.rs` `player_sources`, which are proved there equal to
//! the tiles of the materialised export notes).

use super::*;
use crate::piano_roll::Geometry;
use crate::piano_roll::dense::{DenseRenderer, Role};
use crate::piano_roll::viewport::RollView;
use egui::{Pos2, Rect};
use plumspirit_engine::editor::clips::{
    add_clip, delete_clips, duplicate_clips, move_clips, resize_clip, set_muted,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::track::{GatePercent, TrackProps};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
};
use plumspirit_engine::view::tiles::{self, Scratch, TileOut};
use plumspirit_engine::view::{PitchAxis, TimeAxis};

fn note(start: u64, len: u64, pitch: u8, ch: u8) -> NoteRecord {
    NoteRecord::new(
        start,
        len,
        NoteAttrs::new_wide(pitch, 1 + pitch % 127, ch, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Three tracks of chords and runs on several subchannels, one muted note,
/// same-key overlaps; `wide`: a 256-Key Mode project with keys above 127.
fn song(wide: bool) -> PlumDocument {
    let q = 960u64;
    let mut d = PlumDocument::new_project("player".into(), Bpm::parse("150").unwrap(), 960);
    if wide {
        d.project_mut().keys = KeyRange::Extended256;
    }
    d.add_track().unwrap();
    d.add_track().unwrap();
    for i in 0..3usize {
        let top = if wide { 220 } else { 100 };
        let mut recs: Vec<NoteRecord> = (0..400u64)
            .map(|k| {
                let mut r = note(
                    k * q / 8,
                    q / 4 + (k % 5) * q / 16,
                    24 + ((k * 7 + i as u64 * 11) % (top - 24)) as u8,
                    i as u8,
                );
                r.sub = plumspirit_engine::model::SubchannelId((k % 3) as u32);
                r
            })
            .collect();
        recs.push(note(3 * q, q, 71, i as u8));
        recs.push(note(3 * q + q / 8, q, 71, i as u8));
        recs.sort_by_key(|r| r.start_tick);
        if i == 1 {
            recs[5].attrs = recs[5].attrs.with_muted(true);
        }
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    d
}

fn apply(d: &mut PlumDocument, op: Operation) {
    History::default().apply(d, op).unwrap();
    d.validate_clips().unwrap();
}

fn primary(d: &PlumDocument, track: usize) -> ClipId {
    ClipId::Primary(d.track(track).unwrap().id())
}

fn props(d: &mut PlumDocument, track: usize, props: TrackProps) {
    let track = d.track(track).unwrap().id();
    apply(
        d,
        Operation::TrackProps {
            track,
            props,
            end: None,
            label: "Track Properties",
        },
    );
}

/// Clips: track 1 moved, linked copies (one muted), a trimmed window, an
/// independent copy and an owned clip with notes, the primary of track 3
/// deleted while its linked copy stays.
fn clip_song(wide: bool) -> PlumDocument {
    let q = 960u64;
    let mut d = song(wide);
    let (p0, p1) = (primary(&d, 0), primary(&d, 1));
    let op = move_clips(&d, &[(p0, 0, q)]).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p0, 2, 20 * q)], true).unwrap();
    apply(&mut d, op);
    let op = resize_clip(&d, p1, q / 2, q, Some(5 * q)).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p1, 0, 30 * q)], false).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p0, 1, 40 * q)], true).unwrap();
    apply(&mut d, op);
    let muted = d
        .clips()
        .explicit_placements()
        .iter()
        .map(|p| p.id)
        .filter(|id| matches!(id, ClipId::Placed(_)))
        .max_by_key(|id| match id {
            ClipId::Placed(n) => *n,
            ClipId::Primary(_) => 0,
        })
        .unwrap();
    let op = set_muted(&d, &[muted], true).unwrap();
    apply(&mut d, op);
    let op = add_clip(&d, 2, 50 * q, 4 * q).unwrap();
    apply(&mut d, op);
    let id = d.clips().contents().last().unwrap().id;
    let recs: Vec<NoteRecord> = (0..12u64)
        .map(|k| note(k * 240, 200, 80 + k as u8, 9))
        .collect();
    d.notes_of_mut(ContentRef::Owned(id))
        .unwrap()
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let p2 = primary(&d, 2);
    let op = delete_clips(&d, &[p2]).unwrap();
    apply(&mut d, op);
    assert!(!d.clips_are_identity());
    d
}

/// Corrections on two tracks: a tick / key shift with a long gate, and a
/// negative shift from measure 2 with a key range and a short gate.
fn corrected(mut d: PlumDocument) -> PlumDocument {
    props(
        &mut d,
        0,
        TrackProps {
            tick_correction: 37,
            key_correction: 5,
            gate: GatePercent::from_hundredths(15_000).unwrap(),
            ..TrackProps::default()
        },
    );
    props(
        &mut d,
        2,
        TrackProps {
            tick_correction: -20,
            correction_from_measure: 2,
            key_range: (40, 90),
            gate: GatePercent::from_hundredths(5_000).unwrap(),
            ..TrackProps::default()
        },
    );
    d
}

fn cases() -> Vec<(&'static str, PlumDocument)> {
    vec![
        ("song", song(false)),
        ("song corrected", corrected(song(false))),
        ("clips", clip_song(false)),
        ("clips corrected", corrected(clip_song(false))),
        ("256 keys clips corrected", corrected(clip_song(true))),
    ]
}

/// The engine's rule (`tests/v09_player.rs` `player_sources`), every source
/// kept: a track's own store where it is, or one source per unmuted
/// placement.
type Rule<'a> = (
    Vec<&'a NoteStore>,
    Vec<SourcePlace>,
    Vec<Reading>,
    Vec<usize>,
);

fn rule(s: &ExportSnapshot) -> Rule<'_> {
    let doc = s.document();
    let (mut st, mut pl, mut rd, mut tr) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..doc.tracks().len() {
        let c = Reading::Played(s.plan().correction(i).copied());
        if s.plan().placed(i).is_none() {
            st.push(&doc.tracks()[i].notes);
            pl.push(SourcePlace::IDENTITY);
            rd.push(c);
            tr.push(i);
            continue;
        }
        for pc in doc.placed_contents(i) {
            if !pc.placement.muted {
                st.push(pc.notes);
                pl.push(SourcePlace::of(&pc.placement));
                rd.push(c);
                tr.push(i);
            }
        }
    }
    (st, pl, rd, tr)
}

/// A view of ticks `[0, span)` over 1,200 px.
fn geometry(span: u64, keys: KeyRange) -> Geometry {
    let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1300.0, 1400.0));
    let view = RollView {
        time: TimeAxis::new(0, 1200.0 / span as f64),
        pitch: PitchAxis {
            scroll_rows: 0.0,
            row_height: 5.0,
            top: keys.max_pitch(),
        },
    };
    Geometry::new(full, view, 1.0)
}

fn song_end(s: &ExportSnapshot) -> u64 {
    let mut end = 1;
    s.for_each_note(|n| end = end.max(n.end));
    end
}

/// Draws the Player family of `s` once (`sync`: tiles built inline; else on
/// the worker, then settled) and returns its built tiles (level, index,
/// content hash).
fn drawn(s: &ExportSnapshot, span: u64, sync: bool) -> (FamilyId, Vec<(i32, i64, u64)>) {
    let keys = s.document().project().keys;
    let palette = Arc::new(NotePalette::default());
    let src = Sources::of(s);
    let (id, fam) = family(&src, keys, &palette, true);
    let ctx = egui::Context::default();
    let mut d = DenseRenderer::new(sync);
    let g = geometry(span, keys);
    let mut fam = Some(fam);
    for k in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_frame();
            d.draw(&p, ui.ctx(), &g, Role::Player, id, || fam.take().unwrap());
        });
        out.textures_delta.clear();
        if k < 2 {
            d.settle(&ctx);
        }
    }
    assert_eq!(
        d.stats[Role::Player as usize].ready,
        d.stats[Role::Player as usize].visible_tiles
    );
    (id, d.tile_hashes(id))
}

#[test]
fn the_player_familys_tiles_are_build_tile_read_of_the_export_snapshot() {
    for (name, doc) in cases() {
        let s = snapshot(&doc);
        let keys = doc.project().keys;
        let span = song_end(&s);
        let (_, got) = drawn(&s, span, true);
        assert!(got.len() >= 3, "{name}: {got:?}");
        let (stores, places, reads, tracks) = rule(&s);
        let palette = Arc::new(NotePalette::default());
        let colors = colors(
            Arc::clone(&palette),
            true,
            &track_bases(s.document()),
            &tracks,
        );
        let opt = tile_options(keys);
        let mut sc = Scratch::default();
        let mut notes = 0;
        for &(level, index, hash) in &got {
            let want = tiles::build_tile_read(
                &stores,
                &places,
                &reads,
                &colors,
                level,
                index,
                opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            notes += want.notes;
            assert_eq!(
                hash,
                TileOut::hash(&want),
                "{name}: level {level} tile {index}"
            );
        }
        assert!(notes > 400, "{name}: the tiles show the notes ({notes})");
        // The worker builds the same pixels.
        let (_, threaded) = drawn(&s, span, false);
        assert_eq!(threaded, got, "{name}: worker == inline");
    }
}

/// v1.0-beta1 B1-0142 golden frames: the Player's tile pixels for each case
/// (every tile of a whole-song view, folded with FNV-1a over (level, index,
/// pixel hash)). A deliberate visual change updates these numbers in the same
/// commit and says why (`docs/PLAYER.md` "PFA behaviour matrix").
const GOLDEN_FRAMES: [(&str, u64); 5] = [
    ("song", 0xb0df_3e9d_4990_f54b),
    ("song corrected", 0x3c4d_16b9_4464_8240),
    ("clips", 0x0dd6_11c5_9fb3_61af),
    ("clips corrected", 0xdddc_a57c_55e4_35e8),
    ("256 keys clips corrected", 0x15f8_d8ed_734c_e5ab),
];

#[test]
fn the_player_frames_match_their_golden_pixels() {
    let mut got = Vec::new();
    for (name, doc) in cases() {
        let s = snapshot(&doc);
        let (_, tiles) = drawn(&s, song_end(&s), true);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (level, index, hash) in tiles {
            for w in [level as u64, index as u64, hash] {
                for b in w.to_le_bytes() {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        got.push((name, h));
    }
    let want: Vec<(&str, u64)> = GOLDEN_FRAMES.to_vec();
    assert_eq!(got, want, "golden Player frames: {got:#018x?}");
}

#[test]
fn the_players_sources_share_linked_contents_and_leave_out_muted_clips() {
    let d = clip_song(false);
    let s = snapshot(&d);
    let src = Sources::of(&s);
    let (stores, places, reads, tracks) = rule(&s);
    assert_eq!(src.tracks, tracks);
    // Same sources as the rule (no empty one in this project), in order.
    assert_eq!(src.len(), stores.len());
    assert_eq!(src.places, places);
    assert_eq!(src.readings, reads);
    for (a, b) in src.stores.iter().zip(&stores) {
        assert_eq!(a.revision(), b.revision());
        assert_eq!(a.len(), b.len());
    }
    // Linked placements share one store; nothing is copied per note.
    let unique: std::collections::HashSet<*const NoteStore> =
        src.stores.iter().map(Arc::as_ptr).collect();
    assert!(
        unique.len() < src.len(),
        "{} of {}",
        unique.len(),
        src.len()
    );
    // The muted linked copy is not a source.
    let muted: Vec<SourcePlace> = d
        .clips()
        .explicit_placements()
        .iter()
        .filter(|p| p.muted)
        .map(SourcePlace::of)
        .collect();
    assert_eq!(muted.len(), 1);
    assert!(!src.places.contains(&muted[0]));
}

#[test]
fn the_player_family_id_changes_with_what_its_tiles_show() {
    let palette = Arc::new(NotePalette::default());
    let id_of = |d: &PlumDocument, dark: bool, p: &Arc<NotePalette>| {
        let s = snapshot(d);
        family(&Sources::of(&s), d.project().keys, p, dark).0
    };
    let base = corrected(clip_song(false));
    let id = id_of(&base, true, &palette);
    // Taken again without a change: the same id (every tile kept).
    assert_eq!(id_of(&base, true, &palette), id);
    // Theme and palette.
    assert_ne!(id_of(&base, false, &palette), id);
    let seeded = Arc::new(NotePalette::new(crate::ui::theme::NoteColorPrefs {
        seed: 7,
        ..Default::default()
    }));
    assert_ne!(id_of(&base, true, &seeded), id);
    // A correction by value.
    let mut d = base.clone();
    props(
        &mut d,
        0,
        TrackProps {
            tick_correction: 38,
            key_correction: 5,
            gate: GatePercent::from_hundredths(15_000).unwrap(),
            ..TrackProps::default()
        },
    );
    assert_ne!(id_of(&d, true, &palette), id);
    // A clip moved.
    let mut d = base.clone();
    let p0 = primary(&d, 0);
    let op = move_clips(&d, &[(p0, 0, 2 * 960)]).unwrap();
    apply(&mut d, op);
    assert_ne!(id_of(&d, true, &palette), id);
    // A note added.
    let mut d = base.clone();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(7, 30, 60, 1)], false)
        .unwrap();
    assert_ne!(id_of(&d, true, &palette), id);
    // The key range.
    let mut d = base.clone();
    d.project_mut().keys = KeyRange::Extended256;
    assert_ne!(id_of(&d, true, &palette), id);
}

// ---- step 4b: settings and the snapshot kept current ----

#[test]
fn player_settings_fall_back_field_by_field() {
    let mut p = Vec::new();
    let bad = serde_json::json!({"enabled": "yes", "template": 5, "fall_seconds": 1000.0});
    let got = PlayerSettings::from_json(&bad, &mut p);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(!got.enabled);
    assert_eq!(got.template, stats::DEFAULT_TEMPLATE);
    // B2-F20: no upper limit any more — 1,000 s is kept as written.
    assert_eq!(got.fall_seconds, 1000.0);
    // B2-F20: any finite positive falling time is kept exactly (an f32-era
    // value, a microsecond, ten hours, the extremes); 0, negatives, NaN-like
    // text and non-numbers fall back to 2 s with a problem line.
    for f in [
        7.25,
        1e-6,
        36_000.0,
        f64::MIN_POSITIVE,
        f64::MAX,
        0.123_456_789_012_345,
    ] {
        let mut p = Vec::new();
        let got = PlayerSettings::from_json(&serde_json::json!({ "fall_seconds": f }), &mut p);
        assert_eq!(got.fall_seconds.to_bits(), f.to_bits(), "{f}");
        assert!(p.is_empty());
        let back = PlayerSettings::from_json(&got.to_json(), &mut p);
        assert_eq!(back.fall_seconds.to_bits(), f.to_bits(), "round trip {f}");
    }
    for bad in [
        serde_json::json!(0.0),
        serde_json::json!(-2.0),
        serde_json::json!("2"),
        serde_json::json!(null),
    ] {
        let mut p = Vec::new();
        let got = PlayerSettings::from_json(&serde_json::json!({ "fall_seconds": bad }), &mut p);
        assert_eq!(got.fall_seconds, DEFAULT_FALL_SECONDS);
        assert_eq!(p, ["player.fall_seconds is invalid; using the default"]);
    }
    assert!(!valid_fall_seconds(f64::INFINITY) && !valid_fall_seconds(f64::NAN));
    let mut p = Vec::new();
    let got = PlayerSettings::from_json(&serde_json::json!([]), &mut p);
    assert_eq!(got, PlayerSettings::default());
    assert_eq!(p.len(), 1);
    let s = PlayerSettings {
        enabled: true,
        template: "{passed}".into(),
        fall_seconds: 0.25,
        mapping: crate::player::rain::Mapping::Tick,
        gradient: 40,
        glow: 65,
    };
    let mut p = Vec::new();
    assert_eq!(PlayerSettings::from_json(&s.to_json(), &mut p), s);
    assert!(p.is_empty());
    // v1.0-beta1 (B1-0143): the effects are off by default, clamped to
    // 100 %, and an invalid value falls back on its own.
    assert_eq!(
        (
            PlayerSettings::default().gradient,
            PlayerSettings::default().glow
        ),
        (0, 0)
    );
    let mut p = Vec::new();
    let got = PlayerSettings::from_json(&serde_json::json!({"gradient": 250, "glow": "x"}), &mut p);
    assert_eq!((got.gradient, got.glow), (100, 0));
    assert_eq!(p.len(), 1, "{p:?}");
}

#[test]
fn the_player_takes_a_snapshot_only_when_the_exported_file_changes() {
    let mut d = corrected(clip_song(false));
    let mut v = PlayerView::new(true);
    assert!(v.current().is_none());
    assert!(!v.sync(&d));
    assert_eq!(v.taken, 1);
    let first = v.current().unwrap().snapshot.key();
    let palette = Arc::new(NotePalette::default());
    let id_of = |v: &PlayerView, d: &PlumDocument| {
        family(
            &v.current().unwrap().sources,
            d.project().keys,
            &palette,
            true,
        )
        .0
    };
    let id = id_of(&v, &d);
    // Nothing changed: nothing is taken (frames cost the export key only).
    for _ in 0..5 {
        assert!(!v.sync(&d));
    }
    assert_eq!(v.taken, 1);
    // Mute / Solo change only the sound (P2): the same snapshot.
    d.track_mut(1).unwrap().mute = true;
    d.track_mut(2).unwrap().solo = true;
    assert!(!v.sync(&d));
    assert_eq!(v.taken, 1);
    // A tempo edit changes the file (a new snapshot) but not one tile.
    let edit = d
        .conductor()
        .prepare_set_start_tempo(plumspirit_engine::model::TempoValue::Bpm(
            Bpm::parse("90").unwrap(),
        ))
        .unwrap();
    apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Change Tempo",
        },
    );
    assert!(!v.sync(&d));
    assert_eq!(v.taken, 2);
    assert_ne!(v.current().unwrap().snapshot.key(), first);
    assert_eq!(id_of(&v, &d), id, "a tempo edit keeps every tile");
    // A note edit: a new snapshot and new tiles.
    let taken = v.taken;
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(5, 10, 64, 1)], false)
        .unwrap();
    assert!(!v.sync(&d));
    assert_eq!(v.taken, taken + 1);
    assert_ne!(id_of(&v, &d), id);
}

#[test]
fn the_player_snapshot_worker_keeps_the_last_one_drawing_and_drops_stale_results() {
    let mut d = corrected(clip_song(false));
    let mut v = PlayerView::new(false);
    let wait = |v: &mut PlayerView, d: &PlumDocument| {
        for _ in 0..5000 {
            if !v.sync(d) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the snapshot never arrived");
    };
    assert!(v.sync(&d), "taken on the worker");
    wait(&mut v, &d);
    let key = d.export_key();
    assert_eq!(v.current().unwrap().snapshot.key(), key);
    // An edit: the old snapshot keeps drawing until the new one is ready.
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(9, 10, 60, 0)], false)
        .unwrap();
    assert!(v.sync(&d));
    assert_eq!(v.current().unwrap().snapshot.key(), key);
    // Another edit before it arrives: only the newest document is shown.
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(11, 10, 61, 0)], false)
        .unwrap();
    wait(&mut v, &d);
    assert_eq!(v.current().unwrap().snapshot.key(), d.export_key());
    let notes = v.current().unwrap().snapshot.note_count();
    assert_eq!(notes, snapshot(&d).note_count());
    v.clear();
    assert!(v.current().is_none());
}

/// L-H03: a raised cancel stops the snapshot (nothing is sent), a
/// replaced or dropped job is cancelled, and `release` returns at once
/// for a cancelled job; an uncancelled one gives exactly `Prepared::of`.
#[test]
fn a_cancelled_player_snapshot_stops_and_a_dropped_job_is_cancelled() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let d = corrected(clip_song(false));
    assert!(Prepared::of_until(&d, &AtomicBool::new(true)).is_none());
    let whole = Prepared::of_until(&d, &AtomicBool::new(false)).unwrap();
    assert_eq!(whole.snapshot.key(), Prepared::of(&d).snapshot.key());
    assert_eq!(whole.sources.lit_masks, Prepared::of(&d).sources.lit_masks);
    let mut v = PlayerView::new(false);
    assert!(v.sync(&d), "taken on the worker");
    let flag = std::sync::Arc::clone(&v.job.as_ref().unwrap().cancel);
    assert!(!flag.load(Ordering::Relaxed));
    v.cancel_job();
    assert!(flag.load(Ordering::Relaxed), "cancel_job raises the flag");
    let t = std::time::Instant::now();
    v.release();
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
    let mut v = PlayerView::new(false);
    assert!(v.sync(&d));
    let flag = std::sync::Arc::clone(&v.job.as_ref().unwrap().cancel);
    v.clear();
    assert!(flag.load(Ordering::Relaxed), "a dropped job is canceled");
}

// ---- step 4c: the rainfall draw role ----

/// One note per key `0..keys`, note `k` from tick `k × 120` for 240 ticks.
fn ladder(keys: usize) -> PlumDocument {
    let mut d = PlumDocument::new_project("ladder".into(), Bpm::parse("120").unwrap(), 960);
    if keys > 128 {
        d.project_mut().keys = KeyRange::Extended256;
    }
    let recs: Vec<NoteRecord> = (0..keys)
        .map(|k| note(k as u64 * 120, 240, k as u8, 0))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

/// The meshes of one rainfall frame: (texture, vertices).
type Meshes = Vec<(egui::TextureId, Vec<egui::epaint::Vertex>)>;

/// Draws the Player family of `d` as rainfall over ticks `[t0, t1)` at
/// `level` into a 1,000 × 600 area (`bend`: a split of the time axis at
/// that tick, where the falling speed doubles) and returns the frame's
/// meshes, the L4 / stand-in shapes and the stats.
fn rain_frame(
    d: &mut DenseRenderer,
    ctx: &egui::Context,
    doc: &PlumDocument,
    (t0, t1): (u64, u64),
    bend: Option<f64>,
) -> (Meshes, usize, dense::DenseStats) {
    rain_frame_shaded(d, ctx, doc, (t0, t1), bend, 0.0)
}

/// [`rain_frame`] with the Player's note gradient (`Columns::shade`).
fn rain_frame_shaded(
    d: &mut DenseRenderer,
    ctx: &egui::Context,
    doc: &PlumDocument,
    (t0, t1): (u64, u64),
    bend: Option<f64>,
    shade: f32,
) -> (Meshes, usize, dense::DenseStats) {
    let keys = doc.project().keys;
    let s = snapshot(doc);
    let src = Sources::of(&s);
    let palette = Arc::new(NotePalette::default());
    let (id, fam) = family(&src, keys, &palette, true);
    let clip = Rect::from_min_size(Pos2::new(50.0, 20.0), egui::vec2(1000.0, 600.0));
    let spans = crate::player::keys::spans(keys.keys(), clip.left(), clip.right());
    let cols = crate::player::keys::columns(&spans, keys.keys(), 1.0, 1.0);
    let (a, b) = (t0 as f64, t1 as f64);
    // Linear in ticks, or twice as fast above the bend.
    let y = move |t: f64| -> f32 {
        let u = match bend {
            Some(m) if t > m => ((m - a) + 2.0 * (t - m)) / ((m - a) + 2.0 * (b - m)),
            _ => match bend {
                Some(m) => (t - a) / ((m - a) + 2.0 * (b - m)),
                None => (t - a) / (b - a),
            },
        };
        clip.bottom() - (u * 600.0) as f32
    };
    let bends: Vec<f64> = bend.into_iter().collect();
    let columns = dense::Columns {
        cols: &cols,
        y: &y,
        bends: &bends,
        clip,
        shade,
    };
    let level = tiles::level_for(600.0 / (b - a));
    let mut fam = Some(fam);
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let p = ui.ctx().layer_painter(egui::LayerId::background());
        d.begin_frame();
        d.draw_rainfall(
            &p,
            ui.ctx(),
            id,
            || fam.take().unwrap(),
            &columns,
            (t0, t1),
            level,
        );
    });
    out.textures_delta.clear();
    let mut meshes = Vec::new();
    let mut rects = 0;
    for c in &out.shapes {
        match &c.shape {
            egui::Shape::Mesh(m) => meshes.push((m.texture_id, m.vertices.clone())),
            egui::Shape::Rect(_) => rects += 1,
            _ => {}
        }
    }
    (meshes, rects, d.stats[Role::Player as usize])
}

/// v1.0-beta1 B1-0143: the note gradient tints each column's right edge
/// (left edge full colour); off, every vertex is white (the v0.9 frame).
#[test]
fn the_note_gradient_shades_columns_left_to_right_and_off_draws_flat() {
    let doc = ladder(128);
    let ctx = egui::Context::default();
    let span = 128 * 120 + 240;
    for (shade, right) in [(0.0f32, 255u8), (0.4, 153), (1.0, 0)] {
        let mut d = DenseRenderer::new(true);
        let _ = rain_frame_shaded(&mut d, &ctx, &doc, (0, span), None, shade);
        let (meshes, _, st) = rain_frame_shaded(&mut d, &ctx, &doc, (0, span), None, shade);
        assert_eq!(st.ready, st.visible_tiles);
        assert!(!meshes.is_empty());
        for (_, vs) in &meshes {
            for q in vs.chunks(4) {
                // Vertices: (x0, top), (x1, top), (x1, bottom), (x0, bottom).
                assert_eq!(q[0].color, egui::Color32::WHITE);
                assert_eq!(q[3].color, egui::Color32::WHITE);
                assert_eq!(q[1].color, egui::Color32::from_gray(right), "{shade}");
                assert_eq!(q[2].color, egui::Color32::from_gray(right), "{shade}");
            }
        }
    }
}

#[test]
fn rainfall_columns_sample_their_keys_row_white_keys_first() {
    for keys in [128usize, 256] {
        let doc = ladder(keys);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        let span = keys as u64 * 120 + 240;
        let _ = rain_frame(&mut d, &ctx, &doc, (0, span), None);
        let (meshes, _, st) = rain_frame(&mut d, &ctx, &doc, (0, span), None);
        assert_eq!(st.ready, st.visible_tiles, "{keys}: {st:?}");
        assert!(!meshes.is_empty());
        let clip = Rect::from_min_size(Pos2::new(50.0, 20.0), egui::vec2(1000.0, 600.0));
        let spans = crate::player::keys::spans(keys, clip.left(), clip.right());
        let cols = crate::player::keys::columns(&spans, keys, 1.0, 1.0);
        for (_, vs) in &meshes {
            assert_eq!(vs.len() % 4, 0);
            let quads: Vec<&[egui::epaint::Vertex]> = vs.chunks(4).collect();
            // One quad per column (no bends), in the columns' order.
            assert_eq!(quads.len(), cols.len(), "{keys}");
            for (q, &(row, x0, x1)) in quads.iter().zip(&cols) {
                let v = (row as f32 + 0.5) / keys as f32;
                assert!(
                    q.iter().all(|p| (p.uv.y - v).abs() < 1e-6),
                    "row {row}: V at its center"
                );
                assert!((q[0].pos.x - x0).abs() < 1e-4 && (q[1].pos.x - x1).abs() < 1e-4);
                // Later ticks higher: the top edge has the larger U.
                assert!(q[0].pos.y < q[3].pos.y && q[0].uv.x > q[3].uv.x);
            }
            // White columns come before every black one.
            let first_black = cols
                .iter()
                .position(|&(r, _, _)| crate::player::keys::is_black(keys - 1 - r))
                .unwrap();
            assert!(
                cols[..first_black]
                    .iter()
                    .all(|&(r, _, _)| !crate::player::keys::is_black(keys - 1 - r))
            );
        }
        // Inside the clip, the window only (the playhead up).
        for (_, vs) in &meshes {
            assert!(
                vs.iter()
                    .all(|p| p.pos.y >= clip.top() - 1e-3 && p.pos.y <= clip.bottom() + 1e-3)
            );
        }
    }
}

#[test]
fn rainfall_quads_split_at_bends_and_stay_continuous() {
    let doc = ladder(128);
    let ctx = egui::Context::default();
    let mut d = DenseRenderer::new(true);
    let span = 128 * 120 + 240;
    let bend = 7_000.0;
    let _ = rain_frame(&mut d, &ctx, &doc, (0, span), Some(bend));
    let (meshes, _, _) = rain_frame(&mut d, &ctx, &doc, (0, span), Some(bend));
    let split = meshes
        .iter()
        .find(|(_, vs)| vs.len() == 128 * 4 * 2)
        .expect("the tile holding the bend has two segments per column");
    for col in split.1.chunks(8) {
        let (lower, upper) = (&col[0..4], &col[4..8]);
        // The lower segment's top edge is the upper one's bottom edge.
        assert!((lower[0].pos.y - upper[3].pos.y).abs() < 1e-3);
        assert!((lower[0].uv.x - upper[3].uv.x).abs() < 1e-6);
        assert!(upper[0].pos.y < upper[3].pos.y && lower[0].pos.y < lower[3].pos.y);
        // Above the bend a tick takes twice the height.
        let px_per_u =
            |q: &[egui::epaint::Vertex]| (q[3].pos.y - q[0].pos.y) / (q[0].uv.x - q[3].uv.x);
        assert!((px_per_u(upper) / px_per_u(lower) - 2.0).abs() < 1e-3);
    }
}

#[test]
fn rainfall_is_never_blank_and_idle_frames_repeat_exactly_within_the_cache_budget() {
    let doc = corrected(clip_song(false));
    let ctx = egui::Context::default();
    let mut d = DenseRenderer::new(false);
    let end = song_end(&snapshot(&doc));
    // First frame: nothing built yet — the L4 band, never blank.
    let (meshes, rects, st) = rain_frame(&mut d, &ctx, &doc, (0, end / 3), None);
    assert!(meshes.len() + rects > 0, "{st:?}");
    d.settle(&ctx);
    let (m1, _, st1) = rain_frame(&mut d, &ctx, &doc, (0, end / 3), None);
    assert_eq!(st1.ready, st1.visible_tiles, "{st1:?}");
    assert_eq!(st1.coarse, 0);
    assert_ne!(st1.picture, 0);
    for _ in 0..4 {
        let (m2, _, st2) = rain_frame(&mut d, &ctx, &doc, (0, end / 3), None);
        assert_eq!(st2.picture, st1.picture, "idle frames draw the same tiles");
        assert_eq!(st2.pending, 0);
        let pos = |m: &Meshes| -> Vec<(u32, u32, u32)> {
            m.iter()
                .flat_map(|(_, vs)| {
                    vs.iter()
                        .map(|v| (v.pos.x.to_bits(), v.pos.y.to_bits(), v.uv.x.to_bits()))
                })
                .collect()
        };
        assert_eq!(pos(&m2), pos(&m1));
    }
    // Moving the window reuses the tiles it still shows; the cache stays
    // within its budget.
    for k in 0..30u64 {
        let a = k * end / 40;
        let _ = rain_frame(&mut d, &ctx, &doc, (a, a + end / 3), None);
        d.settle(&ctx);
    }
    let st = d.stats[Role::Player as usize];
    assert!(st.cache_bytes <= dense::BUDGET_BYTES, "{st:?}");
}

#[test]
fn the_players_colours_are_one_identity_per_track_and_subchannel() {
    let palette = Arc::new(NotePalette::default());
    // Three tracks: one subchannel, three subchannels, one subchannel.
    let pc = dense::PlayedColors::new(Arc::clone(&palette), true, vec![(0, 1), (1, 3), (4, 1)]);
    assert_eq!(pc.id(0, 0), 0);
    assert_eq!(pc.id(1, 0), 1);
    assert_eq!(pc.id(1, 2), 3);
    assert_eq!(pc.id(1, 5), 3, "beyond the table: modulo the track's count");
    assert_eq!(pc.id(2, 0), 4);
    let mut seen = std::collections::HashSet::new();
    for (s, sub) in [(0, 0), (1, 0), (1, 1), (1, 2), (2, 0)] {
        let f = pc.fill(s, sub);
        let want = palette.entry(pc.id(s, sub), 127, true).fill;
        assert_eq!(
            f,
            [want.r(), want.g(), want.b(), 255],
            "the palette's velocity-127 fill, opaque"
        );
        assert!(seen.insert(f), "every (track, subchannel) its own color");
    }
    // Track bases follow the document's tracks and subchannel tables.
    let d = song(false);
    let bases = track_bases(&d);
    assert_eq!(bases.len(), d.tracks().len());
    let mut next = 0;
    for (t, &(b, n)) in d.tracks().iter().zip(&bases) {
        assert_eq!(b, next);
        assert_eq!(n, t.subchannels().max_id().0 + 1);
        next += n;
    }
}
