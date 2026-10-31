//! Clip mini notes (v0.9 V09-S09, contract §044, §045, §217;
//! `docs/TRACK_LIST.md` "Mini notes"): what a clip in the Track List shows of
//! its notes — the level of detail for its window and pixel size, the pitch
//! axis (the content's used keys folded into the body's pixel rows) and the
//! exact notes of a sparse window. Everything here is bounded by the clip's
//! pixels or by a note limit; the raster levels use the shared dense raster
//! (`view::tiles` with a [`KeyFold`] and a start window).

use crate::model::note_store::NoteStore;
use crate::model::project::KeyRange;
use crate::view::tiles::KeyFold;

/// Most notes per physical pixel column drawn as exact rectangles.
pub const EXACT_PER_COLUMN: usize = 4;
/// Most notes of one clip drawn as exact rectangles.
pub const MINI_EXACT: usize = 2_000;
/// Most exact rectangles over all clips of one frame.
pub const FRAME_EXACT: usize = 20_000;
/// Clips narrower than this (physical pixels) show the stand-in only.
pub const MIN_RASTER_WIDTH: f64 = 4.0;
/// Texel rows of mini tiles: a clip body uses the largest bucket at or below
/// its physical height, so a small row-height change reuses its tiles.
pub const ROW_BUCKETS: [usize; 9] = [8, 12, 16, 24, 32, 48, 64, 96, 128];
/// Keys added below and above the used keys.
pub const KEY_PAD: u8 = 2;
/// The fewest keys a pitch axis spans.
pub const MIN_KEYS: usize = 12;

/// How a clip draws its notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiniLod {
    /// One rectangle per note (a sparse window).
    Exact,
    /// Mini tiles (the dense raster with a key fold).
    Raster,
    /// The neutral chunk-span band only (a sliver of a clip; also what any
    /// clip shows while its tiles are built).
    StandIn,
}

/// The level of detail of a clip whose window holds about `estimate` notes
/// (`NoteStore::estimate_in_range`: an upper bound, no note visited) over
/// `width` physical pixels, with `exact_left` exact rectangles still
/// available this frame ([`FRAME_EXACT`] minus those drawn).
pub fn mini_lod(estimate: usize, width: f64, exact_left: usize) -> MiniLod {
    // NaN counts as a sliver.
    if width.is_nan() || width < MIN_RASTER_WIDTH {
        return MiniLod::StandIn;
    }
    let per_column = (width.ceil() as usize).saturating_mul(EXACT_PER_COLUMN);
    if estimate <= MINI_EXACT && estimate <= exact_left && estimate <= per_column {
        MiniLod::Exact
    } else {
        MiniLod::Raster
    }
}

/// The texel rows of a clip body `rows` physical pixels tall: the largest
/// [`ROW_BUCKETS`] entry at or below it (the smallest for a lower body).
pub fn row_bucket(rows: f64) -> usize {
    ROW_BUCKETS
        .iter()
        .rev()
        .copied()
        .find(|&b| b as f64 <= rows)
        .unwrap_or(ROW_BUCKETS[0])
}

/// Lowest and highest key of `store`'s notes (`None`: no notes). O(notes):
/// computed on the worker once per content revision.
pub fn used_keys(store: &NoteStore) -> Option<(u8, u8)> {
    let mut k: Option<(u8, u8)> = None;
    for v in store.iter() {
        let p = v.attrs.pitch();
        k = Some(k.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
    }
    k
}

/// The pitch axis of a content using keys `used` (`None`: not known yet —
/// the whole key range): padded by [`KEY_PAD`] keys, at least [`MIN_KEYS`]
/// tall (grown on both sides, kept inside `keys`), folded onto `rows` texel
/// rows.
pub fn fold_for(used: Option<(u8, u8)>, keys: KeyRange, rows: usize) -> KeyFold {
    let top = (keys.keys() - 1) as i32;
    let (lo, hi) = match used {
        Some((lo, hi)) => (
            (i32::from(lo) - i32::from(KEY_PAD)).max(0),
            (i32::from(hi) + i32::from(KEY_PAD)).min(top),
        ),
        None => (0, top),
    };
    let (mut lo, mut hi) = (lo.min(hi), lo.max(hi));
    let min = (MIN_KEYS as i32).min(top + 1);
    while hi - lo + 1 < min {
        if lo > 0 {
            lo -= 1;
        }
        if hi - lo + 1 < min && hi < top {
            hi += 1;
        }
    }
    KeyFold::new(lo as u8, hi as u8, rows)
}

/// One note of a clip, drawn exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiniNote {
    /// Song ticks `[start, end)` (the note's full length: the caller clips
    /// it to the clip rectangle).
    pub start: u64,
    pub end: u64,
    pub pitch: u8,
    pub sub: u32,
    pub velocity: u8,
}

/// The notes a clip plays — those starting in content ticks `[offset,
/// offset + length)` — moved to song ticks (the clip starts at song tick
/// `start`), in stored order; `None` when there are more than `limit` (the
/// caller draws the raster instead). Visits at most the notes overlapping
/// the window plus one.
pub fn exact_notes(
    store: &NoteStore,
    offset: u64,
    length: u64,
    start: u64,
    limit: usize,
) -> Option<Vec<MiniNote>> {
    let end = offset.saturating_add(length);
    if end <= offset {
        return Some(Vec::new());
    }
    let mut out = Vec::with_capacity(store.estimate_in_range(offset, end - 1).min(limit));
    let mut over = false;
    store.query(offset, end - 1, |_, v| {
        if over || v.start_tick < offset || v.start_tick >= end {
            return;
        }
        if out.len() == limit {
            over = true;
            return;
        }
        let s = start.saturating_add(v.start_tick - offset);
        out.push(MiniNote {
            start: s,
            end: s.saturating_add(v.duration_ticks),
            pitch: v.attrs.pitch(),
            sub: v.sub.0,
            velocity: v.attrs.velocity(),
        });
    });
    (!over).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};

    fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(u32::from(pitch % 3)),
            group: 0,
        }
    }

    fn store(mut v: Vec<NoteRecord>) -> NoteStore {
        v.sort_by_key(|r| r.start_tick);
        NoteStore::from_sorted_records(&v, false).unwrap()
    }

    #[test]
    fn the_level_of_detail_follows_notes_per_column_and_the_budgets() {
        // 300 pixels: up to 1,200 notes exact (4 per column), then the raster.
        assert_eq!(mini_lod(0, 300.0, FRAME_EXACT), MiniLod::Exact);
        assert_eq!(mini_lod(1_200, 300.0, FRAME_EXACT), MiniLod::Exact);
        assert_eq!(mini_lod(1_201, 300.0, FRAME_EXACT), MiniLod::Raster);
        // The per-clip cap (2,000) holds on a wide clip …
        assert_eq!(mini_lod(2_000, 4_000.0, FRAME_EXACT), MiniLod::Exact);
        assert_eq!(mini_lod(2_001, 4_000.0, FRAME_EXACT), MiniLod::Raster);
        // … and the frame budget too.
        assert_eq!(mini_lod(500, 300.0, 499), MiniLod::Raster);
        // §045: 20 M notes in a 300-pixel clip are never rectangles.
        assert_eq!(mini_lod(20_000_000, 300.0, FRAME_EXACT), MiniLod::Raster);
        // A sliver shows the stand-in; NaN widths too.
        assert_eq!(mini_lod(3, 3.9, FRAME_EXACT), MiniLod::StandIn);
        assert_eq!(mini_lod(3, f64::NAN, FRAME_EXACT), MiniLod::StandIn);
        assert_eq!(mini_lod(3, 4.0, FRAME_EXACT), MiniLod::Exact);
    }

    #[test]
    fn row_buckets_round_down_and_never_vanish() {
        assert_eq!(row_bucket(0.0), 8);
        assert_eq!(row_bucket(7.9), 8);
        assert_eq!(row_bucket(23.9), 16);
        assert_eq!(row_bucket(24.0), 24);
        assert_eq!(row_bucket(50.0), 48);
        assert_eq!(row_bucket(1_000.0), 128);
    }

    #[test]
    fn the_pitch_axis_is_the_used_keys_padded_and_at_least_an_octave() {
        let s = store(vec![rec(0, 10, 60), rec(5, 10, 72), rec(9, 1, 64)]);
        assert_eq!(used_keys(&s), Some((60, 72)));
        assert_eq!(used_keys(&NoteStore::new()), None);
        let f = fold_for(Some((60, 72)), KeyRange::Standard, 16);
        assert_eq!((f.lo(), f.hi(), f.rows()), (58, 74, 16));
        // One key: grown to 12 around it.
        let f = fold_for(Some((60, 60)), KeyRange::Standard, 32);
        assert_eq!(f.keys(), 12);
        assert!(f.lo() <= 58 && f.hi() >= 62);
        assert_eq!(f.rows(), 12, "never more rows than keys");
        // At the ends of the range: kept inside it.
        let f = fold_for(Some((0, 1)), KeyRange::Standard, 8);
        assert_eq!((f.lo(), f.hi()), (0, 11));
        let f = fold_for(Some((127, 127)), KeyRange::Standard, 8);
        assert_eq!((f.lo(), f.hi()), (116, 127));
        let f = fold_for(Some((200, 255)), KeyRange::Extended256, 24);
        assert_eq!((f.lo(), f.hi()), (198, 255));
        // Unknown yet: the whole range.
        let f = fold_for(None, KeyRange::Extended256, 24);
        assert_eq!((f.lo(), f.hi(), f.rows()), (0, 255, 24));
    }

    #[test]
    fn exact_notes_are_the_notes_starting_in_the_window_in_song_ticks() {
        // A long note from before the window, notes on both edges.
        let s = store(vec![
            rec(0, 5_000, 50),
            rec(960, 100, 60),
            rec(1_919, 10, 61),
            rec(1_920, 10, 62),
            rec(2_000, 10, 63),
        ]);
        // Window 960 .. 1,920 placed at song tick 10,000.
        let n = exact_notes(&s, 960, 960, 10_000, 100).unwrap();
        assert_eq!(
            n.iter()
                .map(|m| (m.start, m.end, m.pitch))
                .collect::<Vec<_>>(),
            vec![(10_000, 10_100, 60), (10_959, 10_969, 61)]
        );
        // The limit: more notes than allowed → the raster.
        assert_eq!(exact_notes(&s, 0, 3_000, 0, 4), None);
        assert_eq!(exact_notes(&s, 0, 3_000, 0, 5).unwrap().len(), 5);
        // An empty window.
        assert_eq!(exact_notes(&s, 960, 0, 0, 5), Some(Vec::new()));
    }
}
