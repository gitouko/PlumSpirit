//! Viewport math shared by renderers. No GUI types.
//!
//! View state (scroll position, zoom) is separate from document state: panning and
//! zooming never modify notes.

pub mod grid;
pub mod lod;
pub mod mini;
pub mod tiles;
pub mod track_list;

/// Horizontal mapping between ticks and pixels.
///
/// Large timelines: absolute ticks are `u64`, but converting e.g. tick
/// 9,000,000,000,123 straight to `f32`/`f64` loses the low digits. Positions are
/// therefore always computed *relative to* `origin_tick` in integer arithmetic first
/// and only the (small) difference is converted to floating point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeAxis {
    /// Tick shown at x = 0 of the note area.
    pub origin_tick: u64,
    /// Horizontal zoom.
    pub pixels_per_tick: f64,
}

/// Smallest and largest horizontal zoom (pixels per tick). B1-0090: these are
/// precision limits, not musical ones. At the smallest, the largest tick
/// (2^64) spans about 18 million points. At the largest, one tick — the
/// smallest position a note can have, and the step the view's origin moves
/// by — is 64 points wide, so scrolling stays smooth enough to place notes
/// tick by tick; at 30,720 PPQN that is about two million points per quarter
/// note. Typed zooms beyond either end are clamped, not refused.
pub const MIN_PIXELS_PER_TICK: f64 = 1e-12;
pub const MAX_PIXELS_PER_TICK: f64 = 64.0;

impl TimeAxis {
    pub fn new(origin_tick: u64, pixels_per_tick: f64) -> Self {
        TimeAxis {
            origin_tick,
            pixels_per_tick: clamp_zoom(pixels_per_tick),
        }
    }

    /// X coordinate (relative to the note area's left edge) of `tick`. Ticks before
    /// the origin give negative values; precision is that of the relative offset.
    #[inline]
    pub fn tick_to_x(&self, tick: u64) -> f64 {
        let rel = i128::from(tick) - i128::from(self.origin_tick);
        rel as f64 * self.pixels_per_tick
    }

    /// Tick under x (floor), clamped to `0..=u64::MAX`.
    pub fn x_to_tick(&self, x: f64) -> u64 {
        if !x.is_finite() {
            return self.origin_tick;
        }
        let rel = (x / self.pixels_per_tick).floor();
        // f64 -> i128 casts saturate, so absurd values cannot wrap.
        let t = i128::from(self.origin_tick) + rel as i128;
        t.clamp(0, i128::from(u64::MAX)) as u64
    }

    /// Inclusive tick range covered by `width_px` pixels.
    pub fn visible_range(&self, width_px: f64) -> (u64, u64) {
        let end = self.x_to_tick(width_px.max(0.0));
        (self.origin_tick, end.max(self.origin_tick))
    }

    /// Zooms to `pixels_per_tick`, keeping the tick under `anchor_x` fixed.
    pub fn zoom_around(&mut self, anchor_x: f64, pixels_per_tick: f64) {
        let new_ppt = clamp_zoom(pixels_per_tick);
        // The anchor is `anchor_x / ppt` ticks after the origin before and after
        // the zoom; only that (small) difference is computed in floating point, so
        // huge absolute ticks keep full precision.
        let delta = anchor_x / self.pixels_per_tick - anchor_x / new_ppt;
        let delta = if delta.is_finite() {
            delta.round()
        } else {
            0.0
        };
        let origin = i128::from(self.origin_tick) + delta as i128;
        self.origin_tick = origin.clamp(0, i128::from(u64::MAX)) as u64;
        self.pixels_per_tick = new_ppt;
    }

    /// Scrolls by `dx` pixels (positive = later in time), saturating at tick 0.
    pub fn scroll_pixels(&mut self, dx: f64) {
        let dt = (dx / self.pixels_per_tick).round();
        let t = i128::from(self.origin_tick) + dt as i128;
        self.origin_tick = t.clamp(0, i128::from(u64::MAX)) as u64;
    }
}

pub fn clamp_zoom(ppt: f64) -> f64 {
    if !ppt.is_finite() || ppt <= 0.0 {
        return MIN_PIXELS_PER_TICK;
    }
    ppt.clamp(MIN_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK)
}

/// Vertical mapping between pitches and pixels. The project's top key (127,
/// or 255 in 256-Key Mode) is at the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchAxis {
    /// Rows scrolled from the top (row 0 = the top key). Fractional values
    /// allowed.
    pub scroll_rows: f64,
    /// Height of one pitch row in pixels.
    pub row_height: f64,
    /// The top key: [`MAX_PITCH`](crate::model::note::MAX_PITCH) in a standard project, 255 in 256-Key Mode
    /// ([`crate::model::project::KeyRange::max_pitch`]).
    pub top: u8,
}

/// Smallest and largest pitch-row height in points. At the smallest, all 128
/// pitches take 64 points (256 keys: 128 points) — half a point is the
/// smallest row a screen can still show. B1-0090: the largest is a precision
/// limit, not a musical one: 16,384 points per semitone (one key is many
/// screens tall). Row positions are computed in `f64` and only visible rows
/// are drawn, so every row stays exact to the pixel.
pub const MIN_ROW_HEIGHT: f64 = 0.5;
pub const MAX_ROW_HEIGHT: f64 = 16_384.0;

impl PitchAxis {
    /// Y of the top edge of `pitch`'s row relative to the note area's top.
    #[inline]
    pub fn pitch_top_y(&self, pitch: u8) -> f64 {
        (f64::from(self.top) - f64::from(pitch) - self.scroll_rows) * self.row_height
    }

    /// Number of pitch rows (128, or 256 in 256-Key Mode).
    #[inline]
    pub fn rows(&self) -> f64 {
        f64::from(self.top) + 1.0
    }

    /// Pitch whose row contains `y`, or `None` above the top key / below 0.
    pub fn y_to_pitch(&self, y: f64) -> Option<u8> {
        if !y.is_finite() {
            return None;
        }
        let row = (y / self.row_height + self.scroll_rows).floor();
        if !(0.0..=f64::from(self.top)).contains(&row) {
            return None;
        }
        Some(self.top - row as u8)
    }

    /// Pitch for `y`, clamped into 0..=top (for drags that leave the area).
    pub fn y_to_pitch_clamped(&self, y: f64) -> u8 {
        let row = (y / self.row_height + self.scroll_rows).floor();
        let row = if row.is_finite() {
            row.clamp(0.0, f64::from(self.top))
        } else {
            0.0
        };
        self.top - row as u8
    }

    /// Inclusive pitch range with any part visible in `height_px`.
    pub fn visible_pitches(&self, height_px: f64) -> (u8, u8) {
        let last = f64::from(self.top);
        let top_row = self.scroll_rows.floor().clamp(0.0, last) as u8;
        let bottom_row = ((height_px / self.row_height) + self.scroll_rows)
            .ceil()
            .clamp(0.0, last) as u8;
        (self.top - bottom_row, self.top - top_row)
    }

    /// Largest valid scroll for a viewport of `height_px`.
    pub fn max_scroll(&self, height_px: f64) -> f64 {
        (self.rows() - height_px / self.row_height).max(0.0)
    }

    pub fn clamp_scroll(&mut self, height_px: f64) {
        let max = self.max_scroll(height_px);
        self.scroll_rows = if self.scroll_rows.is_finite() {
            self.scroll_rows.clamp(0.0, max)
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::MAX_PITCH;

    #[test]
    fn huge_ticks_keep_local_precision() {
        let origin = 9_000_000_000_000_000_000u64;
        let axis = TimeAxis::new(origin, 0.5);
        // One tick after a huge origin is exactly half a pixel away.
        assert_eq!(axis.tick_to_x(origin + 1), 0.5);
        assert_eq!(axis.tick_to_x(origin + 1000), 500.0);
        assert_eq!(axis.tick_to_x(origin - 2), -1.0);
        assert_eq!(axis.x_to_tick(500.0), origin + 1000);
        assert_eq!(axis.x_to_tick(-1e30), 0);
        let near_max = TimeAxis::new(u64::MAX - 10, 1.0);
        assert_eq!(near_max.x_to_tick(1e30), u64::MAX);
        assert_eq!(near_max.tick_to_x(u64::MAX), 10.0);
    }

    #[test]
    fn zoom_keeps_anchor() {
        let mut axis = TimeAxis::new(1_000_000, 0.1);
        let before = axis.x_to_tick(300.0);
        axis.zoom_around(300.0, 0.4);
        let after = axis.x_to_tick(300.0);
        assert!(before.abs_diff(after) <= 3, "{before} vs {after}");
        let mut a = TimeAxis::new(0, 1.0);
        a.scroll_pixels(-50.0);
        assert_eq!(a.origin_tick, 0);
    }

    #[test]
    fn pitch_axis() {
        let a = PitchAxis {
            scroll_rows: 0.0,
            row_height: 10.0,
            top: MAX_PITCH,
        };
        assert_eq!(a.pitch_top_y(127), 0.0);
        assert_eq!(a.pitch_top_y(126), 10.0);
        assert_eq!(a.y_to_pitch(5.0), Some(127));
        assert_eq!(a.y_to_pitch(15.0), Some(126));
        assert_eq!(a.y_to_pitch(1280.0), None);
        assert_eq!(a.y_to_pitch(-1.0), None);
        assert_eq!(a.y_to_pitch_clamped(99999.0), 0);
        assert_eq!(a.visible_pitches(100.0), (117, 127));
        assert_eq!(a.max_scroll(640.0), 64.0);
    }

    /// 256-Key Mode: 256 rows, pitch 255 on top, every row maps both ways.
    #[test]
    fn pitch_axis_with_256_keys() {
        let a = PitchAxis {
            scroll_rows: 0.0,
            row_height: 10.0,
            top: crate::model::note::MAX_PITCH_256,
        };
        assert_eq!(a.rows(), 256.0);
        assert_eq!(a.pitch_top_y(255), 0.0);
        assert_eq!(a.pitch_top_y(0), 2550.0);
        for p in 0..=255u8 {
            assert_eq!(a.y_to_pitch(a.pitch_top_y(p) + 5.0), Some(p));
        }
        assert_eq!(a.y_to_pitch(2560.0), None);
        assert_eq!(a.y_to_pitch_clamped(99999.0), 0);
        assert_eq!(a.y_to_pitch_clamped(-5.0), 255);
        assert_eq!(a.visible_pitches(100.0), (245, 255));
        assert_eq!(a.max_scroll(640.0), 192.0);
    }
}
