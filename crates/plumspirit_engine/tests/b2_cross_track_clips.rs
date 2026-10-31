//! v1.0-beta2 B2-F16 (slice B2-S13): tools that read another track read what
//! that track shows in song time — every placement of it, its content's notes
//! in the placement's window at the placement's song ticks — moved onto the
//! edited notes' ticks (`Ctx::shift`); results written to such a track land
//! where the song shows them; the "track below / above" is counted from the
//! track the edited clip sits on (`Ctx::active`), never from its content's
//! track, so source and destination never swap.

use plumspirit_engine::editor::clips::move_clips;
use plumspirit_engine::history::History;
use plumspirit_engine::model::{
    Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlacedLane, PlacedNotes, PlumDocument,
    SubchannelId,
};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_in;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, Source, TrackScope, TransformRun, commit, run_kernel, tool,
};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// `n` tracks; track `i` gets `notes[i]` (track 0's selected).
fn doc(notes: &[&[NoteRecord]]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b2".into(), Bpm::DEFAULT, 960);
    while d.track_count() < notes.len() {
        d.add_track().unwrap();
    }
    for (i, n) in notes.iter().enumerate() {
        let mut v = n.to_vec();
        v.sort_by_key(|r| r.start_tick);
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, i == 0)
            .unwrap();
    }
    d
}

/// Moves track `from`'s own clip (its primary) to song tick `at` on track `to`.
fn mv(d: &mut PlumDocument, from: usize, to: usize, at: u64) {
    let id = ClipId::Primary(d.track(from).unwrap().id());
    let op = move_clips(d, &[(id, to, at)]).unwrap();
    History::default().apply(d, op).unwrap();
    d.validate_clips().unwrap();
}

/// Runs tool `id` on track `content`'s selected notes, edited on track
/// `active` whose content tick 0 is song tick `shift` (as the Piano Roll
/// passes it).
fn run_on(
    d: &PlumDocument,
    id: &str,
    content: usize,
    active: usize,
    shift: i128,
    set: &[(&str, ParamValue)],
) -> Result<(Source, TransformRun), String> {
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let sc = [TrackScope {
        index: content,
        all: false,
        content: None,
    }];
    let mut ctx = Ctx::simple(960);
    ctx.shift = shift;
    ctx.active = Some(active);
    let src = collect_in(t, d, &sc, &ps, 1 << 24, Some(&ctx)).map_err(|e| e.user_message())?;
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).map_err(|e| e.user_message())?;
    let r = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
        .map_err(|e| e.user_message())?;
    Ok((src, r))
}

type Key = (u64, u64, u8);

fn sorted(mut v: Vec<Key>) -> Vec<Key> {
    v.sort_unstable();
    v
}

/// What track `i` plays, in song ticks (every placement, its window).
fn song(d: &PlumDocument, i: usize) -> Vec<Key> {
    let lanes: Vec<PlacedLane> = d.placed_contents(i).iter().map(PlacedLane::of).collect();
    sorted(
        PlacedNotes::new(&lanes)
            .map(|r| (r.start_tick, r.duration_ticks, r.attrs.pitch()))
            .collect(),
    )
}

/// The run's notes for the slot of the track with id of track `i`, moved by
/// `shift` into song ticks.
fn preview_song(d: &PlumDocument, r: &TransformRun, i: usize, shift: i128) -> Vec<Key> {
    let id = d.track(i).unwrap().id();
    let slot = r.tracks.iter().position(|t| t.id == id).unwrap() as u16;
    sorted(
        r.track_notes(slot)
            .iter()
            .map(|n| ((i128::from(n.start) + shift) as u64, n.dur, n.pitch))
            .collect(),
    )
}

/// The notes `after` has that `before` lacks (multisets).
fn added(before: &[Key], after: &[Key]) -> Vec<Key> {
    let mut b = before.to_vec();
    let mut out = Vec::new();
    for k in after {
        if let Some(p) = b.iter().position(|x| x == k) {
            b.remove(p);
        } else {
            out.push(*k);
        }
    }
    sorted(out)
}

/// Applies `r` and checks the base track `base` gained exactly the preview
/// (in song ticks), one undo step restoring the document exactly.
fn apply_equals_preview(d: &PlumDocument, r: &TransformRun, base: usize, shift: i128) {
    let mut dd = d.clone();
    let mut h = History::default();
    let fp = dd.state_fingerprint();
    let before = song(&dd, base);
    commit(&mut h, &mut dd, r, "Clipping Mask").unwrap();
    assert_eq!(
        added(&before, &song(&dd, base)),
        preview_song(d, r, base, shift),
        "Apply writes what Preview shows, where the song shows it"
    );
    assert_eq!(h.undo_len(), 1);
    h.undo(&mut dd).unwrap();
    assert_eq!(dd.state_fingerprint(), fp);
}

const ART: [(u64, u64, u8); 3] = [(0, 2000, 60), (0, 2000, 61), (0, 2000, 62)];
/// The mask's shape in its own ticks.
const MASK: [(u64, u64, u8); 3] = [(500, 1000, 60), (500, 300, 61), (1200, 300, 61)];

fn recs(v: &[(u64, u64, u8)], dt: u64) -> Vec<NoteRecord> {
    v.iter().map(|&(s, d, p)| rec(s + dt, d, p)).collect()
}

#[test]
fn clipping_mask_reads_a_moved_mask_clip_in_song_time_and_apply_equals_preview() {
    // Unmoved: the art and the mask share the timeline.
    let d0 = doc(&[&recs(&ART, 0), &recs(&MASK, 0)]);
    let (_, r0) = run_on(&d0, "clip_mask", 0, 0, 0, &[]).unwrap();
    assert_eq!(
        preview_song(&d0, &r0, 1, 0),
        sorted(vec![(500, 1000, 60), (500, 300, 61), (1200, 300, 61)])
    );
    // The mask's clip moved to song 1,000: the mask now covers song
    // [1,500, 2,500) — only the art's [1,500, 2,000) lies under it.
    let mut d = d0.clone();
    mv(&mut d, 1, 1, 1_000);
    let (_, r) = run_on(&d, "clip_mask", 0, 0, 0, &[]).unwrap();
    assert_eq!(
        preview_song(&d, &r, 1, 0),
        sorted(vec![(1_500, 500, 60), (1_500, 300, 61)]),
        "moving the mask clip changes the result"
    );
    assert_ne!(preview_song(&d, &r, 1, 0), preview_song(&d0, &r0, 1, 0));
    apply_equals_preview(&d, &r, 1, 0);
}

#[test]
fn clipping_mask_on_an_edited_clip_moved_to_7680_keeps_the_notes_under_the_mask_in_song_time() {
    // The art's clip at song 7,680 (edited there: shift 7,680); the mask
    // track unmoved with its notes at song 8,180 …: in the art's ticks the
    // mask is the same shape at 500.
    let mut d = doc(&[&recs(&ART, 0), &recs(&MASK, 7_680)]);
    mv(&mut d, 0, 0, 7_680);
    let (_, r) = run_on(&d, "clip_mask", 0, 0, 7_680, &[]).unwrap();
    assert_eq!(
        preview_song(&d, &r, 1, 7_680),
        sorted(vec![(8_180, 1000, 60), (8_180, 300, 61), (8_880, 300, 61)])
    );
    apply_equals_preview(&d, &r, 1, 7_680);
    // Replace the art: the selection becomes the clipped notes, in its own
    // (content) ticks.
    let (_, rep) = run_on(
        &d,
        "clip_mask",
        0,
        0,
        7_680,
        &[("result", ParamValue::Choice(1))],
    )
    .unwrap();
    assert_eq!(
        preview_song(&d, &rep, 0, 0),
        sorted(vec![(500, 1000, 60), (500, 300, 61), (1200, 300, 61)])
    );
}

#[test]
fn the_track_below_is_counted_from_the_track_the_edited_clip_sits_on() {
    // Track 1's clip moved onto track 2 (index 1) and edited there: the
    // content is track 1's (index 0), the clip sits on index 1, the track
    // below is index 2 — the mask. Never index 1 (the clip's own track).
    let mut d = doc(&[&recs(&ART, 0), &[], &recs(&MASK, 0)]);
    mv(&mut d, 0, 1, 0);
    let (_, r) = run_on(&d, "clip_mask", 0, 1, 0, &[]).unwrap();
    assert_eq!(
        preview_song(&d, &r, 2, 0),
        sorted(vec![(500, 1000, 60), (500, 300, 61), (1200, 300, 61)])
    );
    apply_equals_preview(&d, &r, 2, 0);
}

/// The auxiliary layer tool `id` prepared (its first layer), in song ticks.
fn aux_song(d: &PlumDocument, id: &str, shift: i128, set: &[(&str, ParamValue)]) -> Vec<Key> {
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let mut ctx = Ctx::simple(960);
    ctx.shift = shift;
    ctx.active = Some(0);
    let src = collect_in(t, d, &sc, &ps, 1 << 24, Some(&ctx)).unwrap();
    sorted(
        src.aux
            .first()
            .map(|v| {
                v.iter()
                    .map(|n| ((i128::from(n.start) + shift) as u64, n.dur, n.pitch))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[test]
fn every_prepare_hook_that_reads_another_track_reads_its_placed_notes_in_song_time() {
    // Track 1 (index 1) holds the source shape; its clip moved to song 3,000;
    // the edited track's clip at song 1,000 (shift 1,000).
    let mut d = doc(&[&recs(&ART, 0), &recs(&MASK, 0)]);
    mv(&mut d, 1, 1, 3_000);
    mv(&mut d, 0, 0, 1_000);
    let want = song(&d, 1);
    assert_eq!(want[0].0, 3_500);
    let list = |k: &str| (k.to_string(), ParamValue::List(vec![2]));
    let cases: Vec<(&str, Vec<(String, ParamValue)>)> = vec![
        ("clip_mask", vec![]),
        ("displacement_map", vec![]),
        (
            "tumours",
            vec![
                (
                    "c24_mask_boundary_constraint".into(),
                    ParamValue::Bool(true),
                ),
                (
                    "c24_mask_boundary_constraint.mode".into(),
                    ParamValue::Choice(2),
                ),
            ],
        ),
        (
            "gen.growth",
            vec![
                ("c21_external_track_field".into(), ParamValue::Bool(true)),
                list("c21_external_track_field.tracks"),
            ],
        ),
    ];
    for (id, set) in cases {
        let set: Vec<(&str, ParamValue)> =
            set.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        assert_eq!(aux_song(&d, id, 1_000, &set), want, "{id}");
    }
}

#[test]
fn every_reference_track_reader_reads_the_track_as_the_song_shows_it() {
    // The edited clip at song 1,000 (shift 1,000, art on ticks 0 - 2,000);
    // track 2's clip moved to song 1,500: its notes on the edited ticks are
    // at 1,000 / 1,700 — inside every reader's window — while its raw store
    // says 500 / 1,200.
    let mut d = doc(&[&recs(&ART, 0), &recs(&MASK, 0)]);
    mv(&mut d, 1, 1, 1_500);
    mv(&mut d, 0, 0, 1_000);
    let reference = song(&d, 1);
    assert_eq!(reference[0].0, 2_000);
    let every = sorted(song(&d, 0).into_iter().chain(reference.clone()).collect());
    let b = |k: &str| (k.to_string(), ParamValue::Bool(true));
    let c = |k: &str, v: u32| (k.to_string(), ParamValue::Choice(v));
    let i = |k: &str, v: i64| (k.to_string(), ParamValue::Int(v));
    type Case = (&'static str, Vec<(String, ParamValue)>, bool);
    let cases: Vec<Case> = vec![
        ("snap_scale", vec![b("n05"), i("n05.num", 2)], false),
        (
            "articulate",
            vec![
                b("n10"),
                c("n10.ref", 2),
                i("n10.num", 2),
                i("n10.window", 4_000),
            ],
            false,
        ),
        ("chop", vec![b("n10"), c("n10.ref", 3)], true),
        (
            "quantize",
            vec![
                b("n09"),
                c("n09.ref", 2),
                i("n09.num", 2),
                i("n09.dist", 4_000),
            ],
            false,
        ),
        (
            "randomize",
            vec![b("n11"), c("n11.in", 2), c("mode", 1)],
            true,
        ),
        (
            "warp",
            vec![b("n02"), c("n02.ref", 0), i("n02.num", 2)],
            false,
        ),
        (
            "shear",
            vec![b("n01"), c("n01.of", 2), i("n01.track", 2)],
            false,
        ),
        (
            "wave",
            vec![b("n03"), c("n03.at", 4), i("n03.ref", 2)],
            false,
        ),
        (
            "subpaint",
            vec![b("n05"), c("n05.ref", 0), i("n05.num", 2)],
            false,
        ),
        (
            "duplicate",
            vec![b("n03"), c("n03.at", 2), i("n03.ref", 2)],
            false,
        ),
    ];
    for (id, set, all_tracks) in cases {
        let set: Vec<(&str, ParamValue)> =
            set.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let want = if all_tracks { &every } else { &reference };
        assert_eq!(&aux_song(&d, id, 1_000, &set), want, "{id}");
    }
}

#[test]
fn duplicate_to_tracks_pastes_into_the_clip_the_target_track_plays_where_the_song_shows_it() {
    // The edited clip at song 1,000; the target (next) track's clip moved to
    // song 500. Duplicate "To tracks" (offset 1) puts the copies on the
    // target at the song ticks of the originals (its content ticks 500 …).
    let mut d = doc(&[&recs(&ART, 0), &recs(&[(0, 100, 40)], 0)]);
    mv(&mut d, 1, 1, 500);
    mv(&mut d, 0, 0, 1_000);
    let set = [
        ("n01", ParamValue::Bool(true)),
        ("n01.targets", ParamValue::Choice(0)),
        ("n01.tracks", ParamValue::List(vec![1])),
    ];
    let (_, r) = run_on(&d, "duplicate", 0, 0, 1_000, &set).unwrap();
    let mut dd = d.clone();
    let mut h = History::default();
    let before = song(&dd, 1);
    commit(&mut h, &mut dd, &r, "Duplicate").unwrap();
    let got = added(&before, &song(&dd, 1));
    assert_eq!(
        got,
        sorted(vec![
            (1_000, 2000, 60),
            (1_000, 2000, 61),
            (1_000, 2000, 62)
        ]),
        "the copies sit at the originals' song ticks"
    );
    // A copy before the target clip's start cannot be stored there: refused
    // with a sentence, nothing changed.
    let mut far = d.clone();
    mv(&mut far, 1, 1, 9_000);
    let (_, r) = run_on(&far, "duplicate", 0, 0, 1_000, &set).unwrap();
    let fp = far.state_fingerprint();
    let e = commit(&mut History::default(), &mut far, &r, "Duplicate").unwrap_err();
    assert!(
        e.user_message().contains("before the start of the clip"),
        "{}",
        e.user_message()
    );
    assert_eq!(far.state_fingerprint(), fp);
}

#[test]
fn overlap_prevention_compares_results_with_the_destination_clip_in_song_time() {
    // The art's clip at song 7,680; the mask track unmoved with its notes at
    // song 8,180 …: the clipped pieces added to the mask track coincide with
    // the mask's own notes there, so the General Note Overlap Prevention
    // (against the notes already there) removes every piece.
    let mut d = doc(&[&recs(&ART, 0), &recs(&MASK, 7_680)]);
    mv(&mut d, 0, 0, 7_680);
    let (src, mut r) = run_on(&d, "clip_mask", 0, 0, 7_680, &[]).unwrap();
    assert_eq!(preview_song(&d, &r, 1, 7_680).len(), 3);
    let g = plumspirit_engine::transform::overlap::General::default();
    plumspirit_engine::transform::overlap::prevent_in_run(&d, &src, &mut r, g);
    assert_eq!(preview_song(&d, &r, 1, 7_680), Vec::<Key>::new());
    assert_eq!(r.stats.overlaps_removed, 3);
}

/// v1.0-beta2 B2-A02: a multi-track scope with frames (`Ctx::scope_frames`):
/// the edited clip of track 1 at song 7,680, track 2 at the identity with a
/// note at song 0 and one at 7,680. Both scoped tracks meet on the edited
/// notes' ticks; Apply stores each result in its own track's ticks; the note
/// before the edited clip's start is not in a whole-track scope and stays
/// as it is; selected, it refuses the run with a sentence.
#[test]
fn framed_scopes_meet_on_the_edited_ticks_and_keep_notes_before_the_clip() {
    let mut d = doc(&[&[rec(0, 480, 60)], &[rec(0, 480, 50), rec(7_680, 480, 51)]]);
    mv(&mut d, 0, 0, 7_680);
    let t = tool("transpose").unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    ps.set("semis", ParamValue::Int(1));
    let scopes = |all: bool| {
        [0, 1].map(|index| TrackScope {
            index,
            all,
            content: None,
        })
    };
    let mut ctx = Ctx::simple(960);
    ctx.shift = 7_680;
    ctx.active = Some(0);
    ctx.host = Some(0);
    ctx.scope_frames = std::sync::Arc::new(vec![0, -7_680]);
    let src = collect_in(t, &d, &scopes(true), &ps, 1 << 24, Some(&ctx)).unwrap();
    let got: Vec<(u64, u8, u16)> = src
        .notes
        .iter()
        .map(|n| (n.start, n.pitch, n.track))
        .collect();
    assert_eq!(
        got,
        vec![(0, 51, 1), (0, 60, 0)],
        "one axis; the earlier note left out"
    );
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    let r = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None).unwrap();
    let mut dd = d.clone();
    let mut h = History::default();
    let fp = dd.state_fingerprint();
    commit(&mut h, &mut dd, &r, "Transpose").unwrap();
    let own = |d: &PlumDocument, i: usize| -> Vec<(u64, u8)> {
        d.track(i)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.attrs.pitch()))
            .collect()
    };
    assert_eq!(own(&dd, 0), vec![(0, 61)], "the edited clip's content");
    assert_eq!(
        own(&dd, 1),
        vec![(0, 50), (7_680, 52)],
        "track 2 in its own ticks"
    );
    assert_eq!(h.undo_len(), 1);
    h.undo(&mut dd).unwrap();
    assert_eq!(dd.state_fingerprint(), fp);
    // Selected notes before the clip's start cannot take part.
    let mut sel = d.clone();
    sel.track_mut(1).unwrap().notes.select_all();
    let e = collect_in(t, &sel, &scopes(false), &ps, 1 << 24, Some(&ctx)).unwrap_err();
    assert!(
        e.user_message()
            .contains("before the start of the clip being edited"),
        "{}",
        e.user_message()
    );
    // Without frames (v1.0-beta1 callers, every clip at the identity) the
    // scopes read their stores as they are.
    let plain = collect_in(t, &d, &scopes(true), &ps, 1 << 24, None).unwrap();
    assert_eq!(plain.notes.len(), 3);
}
