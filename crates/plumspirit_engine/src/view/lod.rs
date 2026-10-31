//! Level-of-detail density raster for zoomed-out views.
//!
//! When more notes are visible than can sensibly be drawn as individual rectangles,
//! the renderer draws a coarse raster instead: one cell per pixel column × pitch
//! row, holding how many notes cover it (and how many of those are selected).
//!
//! This is **visual only**. It never merges, deletes or changes notes, selection,
//! copy/paste or export; zooming back in shows exact rectangles again.
//!
//! Building the raster uses a difference array, so a note spanning many columns
//! costs O(1) instead of O(columns). Cells are stored column-major: notes arrive in
//! time order, so consecutive notes touch neighbouring memory, which keeps the
//! per-note cost low even for millions of visible notes. Buffers are reused between
//! frames (no per-frame allocation once warmed up).

use super::TimeAxis;
use crate::model::note_store::{NoteStore, QueryStats};

/// Visible-note count above which the renderer switches from exact rectangles to
/// the density raster. Around this count rectangle tessellation starts to dominate
/// frame time, while the raster's cost is bounded by pixels and a few nanoseconds
/// per note.
pub const EXACT_RENDER_LIMIT: usize = 12_000;

const ROWS: usize = 128;

#[derive(Clone, Debug, Default)]
pub struct DensityRaster {
    columns: usize,
    /// Column-major: index = column * 128 + pitch; one extra column for the
    /// difference-array end markers.
    counts: Vec<i32>,
    selected: Vec<i32>,
    notes_rasterized: usize,
    any_selected: bool,
}

impl DensityRaster {
    pub fn new() -> Self {
        DensityRaster::default()
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn notes_rasterized(&self) -> usize {
        self.notes_rasterized
    }

    /// Number of notes covering (column, pitch).
    #[inline]
    pub fn count(&self, column: usize, pitch: u8) -> u32 {
        self.counts[column * ROWS + usize::from(pitch)] as u32
    }

    /// Number of selected notes covering (column, pitch).
    #[inline]
    pub fn selected(&self, column: usize, pitch: u8) -> u32 {
        if !self.any_selected {
            return 0;
        }
        self.selected[column * ROWS + usize::from(pitch)] as u32
    }

    /// Rebuilds the raster for `columns` pixel columns starting at the axis origin,
    /// for pitches `pitch_lo..=pitch_hi`. Returns the query statistics.
    pub fn rebuild(
        &mut self,
        notes: &NoteStore,
        axis: &TimeAxis,
        columns: usize,
        pitch_lo: u8,
        pitch_hi: u8,
    ) -> QueryStats {
        self.columns = columns;
        let len = ROWS * (columns + 1);
        self.counts.clear();
        self.counts.resize(len, 0);
        let any_selected = notes.selected_count() > 0;
        self.any_selected = any_selected;
        self.selected.clear();
        if any_selected {
            self.selected.resize(len, 0);
        }
        self.notes_rasterized = 0;
        if columns == 0 {
            return QueryStats::default();
        }
        let (t0, t1) = axis.visible_range(columns as f64);
        let origin = axis.origin_tick;
        let ppt = axis.pixels_per_tick;
        let max_col = columns - 1;
        let counts = &mut self.counts;
        let selected = &mut self.selected;
        let mut n = 0usize;
        // Column of a tick at or after the origin (earlier ticks map to column 0).
        // `as usize` on a non-negative float truncates, i.e. floors.
        let col = |tick: u64| -> usize { ((tick.saturating_sub(origin)) as f64 * ppt) as usize };
        let stats = notes.query(t0, t1, |_, v| {
            let p = v.attrs.pitch();
            // The raster covers the 128 standard keys (256-Key Mode views
            // use the tiles, `view::tiles`, which size themselves).
            if p < pitch_lo || p > pitch_hi || usize::from(p) >= ROWS {
                return;
            }
            n += 1;
            let x0 = col(v.start_tick).min(max_col);
            let x1 = col(v.start_tick + v.duration_ticks).min(max_col);
            let pi = usize::from(p);
            counts[x0 * ROWS + pi] += 1;
            counts[(x1 + 1) * ROWS + pi] -= 1;
            if v.selected {
                selected[x0 * ROWS + pi] += 1;
                selected[(x1 + 1) * ROWS + pi] -= 1;
            }
        });
        self.notes_rasterized = n;
        // Prefix sums along time, column by column (sequential memory access).
        for c in 1..columns {
            let (prev, cur) = self.counts.split_at_mut(c * ROWS);
            let prev = &prev[(c - 1) * ROWS..];
            for (x, p) in cur[..ROWS].iter_mut().zip(prev) {
                *x += *p;
            }
            if any_selected {
                let (prev, cur) = self.selected.split_at_mut(c * ROWS);
                let prev = &prev[(c - 1) * ROWS..];
                for (x, p) in cur[..ROWS].iter_mut().zip(prev) {
                    *x += *p;
                }
            }
        }
        stats
    }

    pub fn heap_bytes(&self) -> usize {
        (self.counts.capacity() + self.selected.capacity()) * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};

    #[test]
    fn raster_counts_cover_spans() {
        let a = |p| NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs = vec![
            NoteRecord {
                start_tick: 0,
                duration_ticks: 99,
                attrs: a(60),
                sub: Default::default(),
                group: 0,
            },
            NoteRecord {
                start_tick: 10,
                duration_ticks: 5,
                attrs: a(60),
                sub: Default::default(),
                group: 0,
            },
            NoteRecord {
                start_tick: 50,
                duration_ticks: 1000,
                attrs: a(61),
                sub: Default::default(),
                group: 0,
            },
        ];
        let mut store = NoteStore::from_sorted_records(&recs, false).unwrap();
        store.select_in_region(50, 50, 61, 61);
        let axis = TimeAxis::new(0, 0.1); // 10 ticks per column
        let mut r = DensityRaster::new();
        r.rebuild(&store, &axis, 20, 0, 127);
        assert_eq!(r.count(0, 60), 1);
        assert_eq!(r.count(1, 60), 2);
        assert_eq!(r.count(9, 60), 1);
        assert_eq!(r.count(10, 60), 0);
        assert_eq!(r.count(4, 61), 0);
        assert_eq!(r.count(5, 61), 1);
        assert_eq!(r.count(19, 61), 1); // clipped at the right edge
        assert_eq!(r.selected(19, 61), 1);
        assert_eq!(r.selected(1, 60), 0);
        assert_eq!(r.notes_rasterized(), 3);
        // Rebuilding reuses buffers and leaves notes untouched.
        let before: Vec<_> = store.iter().collect();
        r.rebuild(&store, &axis, 20, 0, 127);
        assert_eq!(before, store.iter().collect::<Vec<_>>());
    }

    #[test]
    fn raster_matches_brute_force() {
        let a = |p| NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let mut seed = 3u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut recs: Vec<NoteRecord> = (0..2000)
            .map(|_| NoteRecord {
                start_tick: next() % 50_000,
                duration_ticks: next() % 3000,
                attrs: a((next() % 128) as u8),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        recs.sort_by_key(|r| r.start_tick);
        let store = NoteStore::from_sorted_records(&recs, false).unwrap();
        let axis = TimeAxis::new(10_000, 0.01); // 100 ticks per column
        let cols = 200;
        let mut r = DensityRaster::new();
        r.rebuild(&store, &axis, cols, 30, 90);
        for c in 0..cols {
            for p in 30..=90u8 {
                let expected = recs
                    .iter()
                    .filter(|n| n.attrs.pitch() == p)
                    .filter(|n| {
                        let end = n.start_tick + n.duration_ticks;
                        if end < 10_000 || n.start_tick > axis.visible_range(cols as f64).1 {
                            return false;
                        }
                        let x0 = (n.start_tick.saturating_sub(10_000) / 100).min(cols as u64 - 1);
                        let x1 = (end.saturating_sub(10_000) / 100).min(cols as u64 - 1);
                        (x0..=x1).contains(&(c as u64))
                    })
                    .count() as u32;
                assert_eq!(r.count(c, p), expected, "col {c} pitch {p}");
            }
        }
    }
}
