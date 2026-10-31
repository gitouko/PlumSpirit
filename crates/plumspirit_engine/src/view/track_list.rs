//! The Track List's view model (v0.9, V09-S08, `docs/TRACK_LIST.md`): the
//! clips of a range of rows inside a tick range as rectangles, and the hit
//! test the gestures use. Per visible clip — never per note; a clip is a few
//! dozen bytes.

use crate::model::{ClipId, ClipPlacement, ContentId, ContentRef, PlumDocument};
use std::collections::{HashMap, HashSet};

/// One clip as the Track List draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipRect {
    pub id: ClipId,
    /// Index of the track (row) it sits on.
    pub track: usize,
    /// Song ticks `[start, end)` it occupies (at least one tick wide).
    pub start: u64,
    pub end: u64,
    /// Its window is empty (a track without notes or events): the Track
    /// List does not draw it.
    pub empty: bool,
    /// The clip's name, else its content's (owned contents); `None`: the UI
    /// shows the track's name.
    pub name: Option<String>,
    /// 0xRRGGBB, the clip's colour, else its content's; `None`: the track's.
    pub color: Option<u32>,
    pub muted: bool,
    /// Its content is shown by more than one clip (a linked instance).
    pub linked: bool,
    /// Its content is owned by the clip table (an independent copy or an
    /// added clip), not a track's own notes.
    pub owned: bool,
    /// The index of the track whose own notes it shows (`None`: an owned
    /// content). Differs from `track` for a track's clip moved to another
    /// row or a linked instance there: the UI names and colours the clip
    /// after this track.
    pub content_track: Option<usize>,
    /// Its content and the content tick at its start (V09-S09: the mini
    /// notes read them every frame, so they live here, not in a placement
    /// lookup).
    pub content: ContentRef,
    pub offset: u64,
}

/// What a pointer position hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipHit {
    Body,
    /// Within the edge tolerance of the left edge: trim.
    LeftEdge,
    /// Within the edge tolerance of the right edge: resize.
    RightEdge,
}

/// Every clip of a document as the Track List draws it, per track, built
/// once per document revision (V09-S08: the app caches it on
/// `PlumDocument::analysis_revision`, which covers every placement and every
/// content's revision). O(tracks + placements + contents) to build, with a
/// sort per row; a viewport query is O(log clips of the row + clips shown).
#[derive(Clone, Debug, Default)]
pub struct TrackListModel {
    rows: Vec<Row>,
    clips: usize,
}

/// One track's clips in draw order (by start; at equal starts the primary
/// first, then by id — later ones are drawn on top) and the running maximum
/// of their ends (the binary search for the first clip that can reach a
/// viewport's start).
#[derive(Clone, Debug, Default)]
struct Row {
    rects: Vec<ClipRect>,
    max_end: Vec<u64>,
}

impl TrackListModel {
    pub fn build(doc: &PlumDocument) -> TrackListModel {
        let c = doc.clips();
        let index: HashMap<crate::model::TrackId, usize> = doc
            .tracks()
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id(), i))
            .collect();
        // Content ends: what a window without an explicit length reaches.
        let mut ends: HashMap<ContentRef, u64> = HashMap::new();
        for t in doc.tracks() {
            ends.insert(ContentRef::Track(t.id()), t.content_only_end_tick());
        }
        let mut owned: HashMap<ContentId, (Option<String>, Option<u32>)> = HashMap::new();
        for o in c.contents() {
            ends.insert(ContentRef::Owned(o.id), o.end_tick());
            owned.insert(o.id, (o.name.clone(), o.color));
        }
        // How many clips show each content (implicit primaries included).
        let explicit_primaries: HashSet<crate::model::TrackId> = c
            .explicit_placements()
            .iter()
            .filter_map(|p| match p.id {
                ClipId::Primary(t) => Some(t),
                ClipId::Placed(_) => None,
            })
            .collect();
        let removed: HashSet<crate::model::TrackId> =
            c.removed_primaries().iter().copied().collect();
        let mut placements: Vec<ClipPlacement> = Vec::new();
        for t in doc.tracks() {
            let id = t.id();
            if !explicit_primaries.contains(&id) && !removed.contains(&id) {
                placements.push(ClipPlacement::primary(id));
            }
        }
        placements.extend(c.explicit_placements().iter().cloned());
        let mut counts: HashMap<ContentRef, u32> = HashMap::new();
        for p in &placements {
            *counts.entry(p.content).or_default() += 1;
        }
        let mut rows: Vec<Row> = vec![Row::default(); doc.track_count()];
        let mut clips = 0;
        for p in placements {
            let Some(&track) = index.get(&p.track) else {
                continue;
            };
            // A missing content shows an empty window (as `clip_span`).
            let end = ends.get(&p.content).copied().unwrap_or(0);
            let (start, stop) = p.song_span(end);
            let own = match p.content {
                ContentRef::Owned(id) => owned.get(&id),
                ContentRef::Track(_) => None,
            };
            rows[track].rects.push(ClipRect {
                id: p.id,
                track,
                start,
                end: stop.max(start.saturating_add(1)),
                empty: stop <= start,
                name: p.name.or_else(|| own.and_then(|o| o.0.clone())),
                color: p.color.or_else(|| own.and_then(|o| o.1)),
                muted: p.muted,
                linked: counts.get(&p.content).copied().unwrap_or(0) > 1,
                owned: own.is_some(),
                content_track: match p.content {
                    ContentRef::Track(id) => index.get(&id).copied(),
                    ContentRef::Owned(_) => None,
                },
                content: p.content,
                offset: p.offset,
            });
            clips += 1;
        }
        for r in &mut rows {
            r.rects.sort_by_key(|x| {
                (
                    x.start,
                    !matches!(x.id, ClipId::Primary(_)),
                    match x.id {
                        ClipId::Primary(t) => t.0,
                        ClipId::Placed(n) => n,
                    },
                )
            });
            let mut m = 0;
            r.max_end = r
                .rects
                .iter()
                .map(|x| {
                    m = m.max(x.end);
                    m
                })
                .collect();
        }
        TrackListModel { rows, clips }
    }

    /// Number of clips in the document.
    pub fn clip_count(&self) -> usize {
        self.clips
    }

    /// Number of tracks (rows) in the model.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Every clip of track `track` in draw order.
    pub fn row(&self, track: usize) -> &[ClipRect] {
        self.rows.get(track).map_or(&[][..], |r| &r.rects)
    }

    /// The clips of track `track` that overlap song ticks `[t0, t1)`, in
    /// draw order.
    pub fn visible(&self, track: usize, t0: u64, t1: u64) -> impl Iterator<Item = &ClipRect> {
        let (rects, from) = match self.rows.get(track) {
            // The first clip whose running end passes t0: none before it can
            // reach the viewport.
            Some(r) => (&r.rects[..], r.max_end.partition_point(|&e| e <= t0)),
            None => (&[][..], 0),
        };
        rects[from..]
            .iter()
            .take_while(move |r| r.start < t1)
            .filter(move |r| r.end > t0)
    }

    /// The clip with id `id` (O(clips)).
    pub fn find(&self, id: ClipId) -> Option<&ClipRect> {
        self.rows.iter().flat_map(|r| &r.rects).find(|r| r.id == id)
    }
}

/// The clips of tracks `rows` that overlap song ticks `[t0, t1)`, row by
/// row, each row in draw order (see [`TrackListModel`]).
pub fn clip_rects(
    doc: &PlumDocument,
    rows: std::ops::Range<usize>,
    t0: u64,
    t1: u64,
) -> Vec<ClipRect> {
    let m = TrackListModel::build(doc);
    let mut out = Vec::new();
    for track in rows.start..rows.end.min(doc.track_count()) {
        out.extend(m.visible(track, t0, t1).cloned());
    }
    out
}

/// The clip under (`track`, `tick`) among `rects` (in draw order): the last
/// drawn (topmost) one wins;
/// `edge` ticks around either end grab the edge (never more than a third of
/// the clip, so a short clip keeps a body).
pub fn hit(rects: &[ClipRect], track: usize, tick: u64, edge: u64) -> Option<(ClipId, ClipHit)> {
    rects
        .iter()
        .rev()
        .find(|r| r.track == track && tick >= r.start && tick < r.end)
        .map(|r| {
            let e = edge.min((r.end - r.start) / 3);
            let part = if tick < r.start.saturating_add(e) {
                ClipHit::LeftEdge
            } else if tick >= r.end.saturating_sub(e) {
                ClipHit::RightEdge
            } else {
                ClipHit::Body
            };
            (r.id, part)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::test_support::{base, clipped, flatten};

    #[test]
    fn rectangles_follow_the_placements() {
        let d = clipped();
        let all = clip_rects(&d, 0..d.track_count(), 0, u64::MAX);
        let (t0, t1, t2) = (
            d.track(0).unwrap().id(),
            d.track(1).unwrap().id(),
            d.track(2).unwrap().id(),
        );
        // Track 1: its moved, windowed primary (start 1000, 2400 ticks).
        let p0 = all.iter().find(|r| r.id == ClipId::Primary(t0)).unwrap();
        assert_eq!((p0.track, p0.start, p0.end), (0, 1000, 3400));
        assert!(p0.linked, "track 1's content is also linked onto track 3");
        assert!(!p0.owned);
        // Track 2: its primary is removed; three owned clips, one muted.
        assert!(!all.iter().any(|r| r.id == ClipId::Primary(t1)));
        let owned: Vec<&ClipRect> = all.iter().filter(|r| r.track == 1).collect();
        assert_eq!(owned.len(), 3);
        assert!(owned.iter().all(|r| r.owned && r.linked));
        assert_eq!(owned.iter().filter(|r| r.muted).count(), 1);
        // Track 3: named identity primary + the linked instance of track 1.
        let named = all.iter().find(|r| r.id == ClipId::Primary(t2)).unwrap();
        assert_eq!(named.name.as_deref(), Some("Named"));
        assert!(!named.linked);
        let link = all
            .iter()
            .find(|r| r.track == 2 && r.id != ClipId::Primary(t2))
            .unwrap();
        assert_eq!(link.start, 7000);
        // It shows track 1's notes: named and coloured after track 1.
        assert_eq!((link.track, link.content_track), (2, Some(0)));
        assert_eq!(named.content_track, Some(2));
        assert!(owned.iter().all(|r| r.content_track.is_none()));
        // A window: only overlapping clips.
        let some = clip_rects(&d, 1..2, 0, 3000);
        // The clip at 200 overlaps; the muted one starts at 3000, the end.
        assert_eq!(some.len(), 1);
        assert_eq!((some[0].track, some[0].start), (1, 200));
        assert_eq!(clip_rects(&d, 1..2, 0, 3001).len(), 2);
    }

    #[test]
    fn hits_find_bodies_and_edges() {
        let d = clipped();
        let rects = clip_rects(&d, 0..d.track_count(), 0, u64::MAX);
        let t0 = d.track(0).unwrap().id();
        assert_eq!(
            hit(&rects, 0, 2000, 30),
            Some((ClipId::Primary(t0), ClipHit::Body))
        );
        assert_eq!(
            hit(&rects, 0, 1010, 30),
            Some((ClipId::Primary(t0), ClipHit::LeftEdge))
        );
        assert_eq!(
            hit(&rects, 0, 3390, 30),
            Some((ClipId::Primary(t0), ClipHit::RightEdge))
        );
        assert_eq!(hit(&rects, 0, 999, 30), None);
        assert_eq!(hit(&rects, 0, 3400, 30), None);
    }

    #[test]
    fn a_v08_document_shows_one_clip_per_track() {
        let d = base();
        let all = clip_rects(&d, 0..d.track_count(), 0, u64::MAX);
        assert_eq!(all.len(), d.track_count());
        for (i, r) in all.iter().enumerate() {
            assert_eq!(r.id, ClipId::Primary(d.track(i).unwrap().id()));
            assert_eq!(
                (r.start, r.end),
                (0, d.track(i).unwrap().content_only_end_tick())
            );
            assert!(!r.linked && !r.owned && !r.muted);
        }
    }

    /// The model equals the placements one by one (`placements_on` +
    /// `clip_span`), and every viewport query equals a filter over the row —
    /// on the fixture and on 3,000 clips with overlaps, windows, linked and
    /// owned contents spread over 60 tracks.
    #[test]
    fn the_model_equals_the_placements_and_viewport_queries_equal_a_filter() {
        fn check(d: &PlumDocument) {
            let m = TrackListModel::build(d);
            let mut total = 0;
            for track in 0..d.track_count() {
                let mut want: Vec<(ClipId, u64, u64, bool)> = d
                    .placements_on(track)
                    .iter()
                    .map(|p| {
                        let (a, b) = crate::editor::clips::clip_span(d, p);
                        (p.id, a, b.max(a + 1), p.muted)
                    })
                    .collect();
                want.sort_by_key(|w| w.0);
                let mut got: Vec<(ClipId, u64, u64, bool)> = m
                    .row(track)
                    .iter()
                    .map(|r| (r.id, r.start, r.end, r.muted))
                    .collect();
                // Draw order: by start, the primary first at equal starts.
                let row = m.row(track);
                assert!(row.windows(2).all(|w| w[0].start <= w[1].start));
                got.sort_by_key(|w| w.0);
                assert_eq!(got, want, "track {track}");
                assert!(row.iter().all(|r| r.track == track));
                total += row.len();
                let end = row.iter().map(|r| r.end).max().unwrap_or(0) + 50;
                let mut k = 0u64;
                while k < 40 {
                    let t0 = (k * 7919 + track as u64 * 131) % end;
                    let t1 = t0 + 1 + (k * 3571) % (end / 3 + 1);
                    let q: Vec<ClipId> = m.visible(track, t0, t1).map(|r| r.id).collect();
                    let f: Vec<ClipId> = row
                        .iter()
                        .filter(|r| r.start < t1 && r.end > t0)
                        .map(|r| r.id)
                        .collect();
                    assert_eq!(q, f, "track {track} [{t0}, {t1})");
                    k += 1;
                }
            }
            assert_eq!(total, m.clip_count());
            // The one-pass grouping equals the per-track query.
            let by = d.placed_contents_by_track();
            assert_eq!(by.len(), d.track_count());
            for (track, all) in by.iter().enumerate() {
                let one = d.placed_contents(track);
                assert_eq!(all.len(), one.len(), "track {track}");
                for (a, b) in all.iter().zip(&one) {
                    assert_eq!(a.placement, b.placement);
                    assert!(std::ptr::eq(a.notes, b.notes) && std::ptr::eq(a.events, b.events));
                }
            }
        }
        check(&base());
        check(&clipped());
        // Many clips: linked instances of every track's content, owned
        // contents placed several times, overlapping, windowed and muted.
        let mut d = PlumDocument::new_project("m".into(), crate::model::project::Bpm::DEFAULT, 480);
        for _ in 1..60 {
            d.add_track().unwrap();
        }
        for ti in 0..60usize {
            let recs: Vec<_> = (0..(ti as u64 % 7) * 9)
                .map(|k| crate::model::clip::test_support::note(k * 97, 50 + k % 300, 60, 0))
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let ids: Vec<_> = d.tracks().iter().map(|t| t.id()).collect();
        let c = d.clips_mut();
        let mut contents = Vec::new();
        for k in 0..40u64 {
            let cid = c.new_content();
            let recs: Vec<_> = (0..1 + k % 5)
                .map(|j| crate::model::clip::test_support::note(j * 50, 40 + k, 70, 1))
                .collect();
            c.content_mut(cid)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
            if k % 3 == 0 {
                c.content_mut(cid).unwrap().name = Some(format!("Content {k}"));
            }
            contents.push(cid);
        }
        for k in 0..3_000u64 {
            let id = c.new_clip_id();
            let host = ids[(k * 37 % 60) as usize];
            let content = if k % 4 == 0 {
                ContentRef::Track(ids[(k * 11 % 60) as usize])
            } else {
                ContentRef::Owned(contents[(k % 40) as usize])
            };
            c.set_placement(ClipPlacement {
                id,
                track: host,
                content,
                start: (k * 7_777) % 400_000,
                offset: k % 3 * 10,
                length: (k % 5 == 0).then_some(100 + k % 900),
                muted: k % 9 == 0,
                ..ClipPlacement::primary(host)
            });
        }
        c.remove_placement(ClipId::Primary(ids[5]));
        check(&d);
        // Names fall back to the owned content's; links count every show.
        let m = TrackListModel::build(&d);
        let named = m
            .row(0)
            .iter()
            .chain(m.row(1))
            .filter(|r| r.owned && r.name.is_some())
            .count();
        assert!(named > 0);
        assert!(m.row(5).iter().all(|r| r.id != ClipId::Primary(ids[5])));
    }

    /// The memoized song end follows every input: a placement change, a
    /// content edit, an explicit track end — always equal to a fresh
    /// computation (a clone computes its own).
    #[test]
    fn the_memoized_song_end_follows_every_change() {
        let mut d = clipped();
        let fresh = |d: &PlumDocument| d.clone().content_end_tick();
        let e0 = d.content_end_tick();
        assert_eq!(e0, fresh(&d));
        assert_eq!(d.content_end_tick(), e0, "memo hit");
        // Move a clip far right.
        let t0 = d.track(0).unwrap().id();
        let mut p = d.clips().placement(ClipId::Primary(t0)).unwrap();
        p.start = 500_000;
        d.clips_mut().set_placement(p);
        let e1 = d.content_end_tick();
        assert!(e1 > 500_000 && e1 != e0);
        assert_eq!(e1, fresh(&d));
        // A note added inside an owned content's window, past every end.
        let cid = d.clips().contents()[0].id;
        let late = crate::model::clip::test_support::note(1_100, 900_000, 60, 0);
        d.clips_mut()
            .content_mut(cid)
            .unwrap()
            .notes
            .insert_sorted_batch(&[late], false)
            .unwrap();
        let e2 = d.content_end_tick();
        assert!(e2 > e1);
        assert_eq!(e2, fresh(&d));
        // An explicit track end beyond everything.
        d.track_mut(2).unwrap().end_of_track_tick = Some(5_000_000);
        assert_eq!(d.content_end_tick(), 5_000_000);
        d.track_mut(2).unwrap().end_of_track_tick = None;
        assert_eq!(d.content_end_tick(), e2);
    }

    /// `max_end_of_starts_in` equals a walk over every note, across chunk
    /// edges (20,000 notes: several chunks).
    #[test]
    fn window_note_ends_equal_a_full_walk() {
        use crate::model::note_store::NoteStore;
        let recs: Vec<_> = (0..20_000u64)
            .map(|k| crate::model::clip::test_support::note(k * 7, 1 + (k * 31) % 900, 60, 0))
            .collect();
        let s = NoteStore::from_sorted_records(&recs, false).unwrap();
        for (lo, hi) in [
            (0, 1),
            (0, 140_000),
            (5, 6),
            (7, 8),
            (28_000, 28_673),
            (99_999, 140_007),
            (139_993, u64::MAX),
            (140_000, u64::MAX),
        ] {
            let want = s
                .iter()
                .filter(|n| n.start_tick >= lo && n.start_tick < hi)
                .map(|n| n.start_tick + n.duration_ticks)
                .max();
            assert_eq!(s.max_end_of_starts_in(lo, hi), want, "[{lo}, {hi})");
        }
    }

    /// The song length follows the placements: what plays (the flattened
    /// twin's end) and every clip's window.
    #[test]
    fn the_song_ends_where_the_clips_end() {
        let d = clipped();
        let f = flatten(&d);
        let rects = clip_rects(&d, 0..d.track_count(), 0, u64::MAX);
        let windows = rects
            .iter()
            .filter(|r| !r.muted)
            .map(|r| r.end)
            .max()
            .unwrap();
        assert_eq!(d.content_end_tick(), f.content_end_tick().max(windows));
        assert_eq!(base().content_end_tick(), {
            let b = base();
            b.tracks()
                .iter()
                .map(|t| t.content_end_tick())
                .max()
                .unwrap()
        });
    }
}
