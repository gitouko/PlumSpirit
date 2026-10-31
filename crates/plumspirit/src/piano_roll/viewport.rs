//! Piano-roll view state (scroll and zoom). Separate from the document: panning and
//! zooming never modify notes.
//!
//! # Zoom X / Zoom Y
//!
//! Zoom is shown and typed as a percentage of a fixed reference, so the same
//! number always means the same size:
//!
//! * **Zoom X 100 %** = [`REFERENCE_POINTS_PER_QUARTER`] (32) points per quarter
//!   note, which is about eight 4/4 bars on a 1,100-point wide roll (the v0.2
//!   opening view). SMPTE files: 100 points per second. Files without timing:
//!   32 points per 480 ticks.
//! * **Zoom Y 100 %** = [`DEFAULT_ROW_HEIGHT`] (12) points per semitone (the v0.2
//!   row height).
//!
//! Any positive value is accepted as long as it gives a usable view: Zoom X
//! between the engine's smallest and largest pixels-per-tick, Zoom Y between
//! [`MIN_ROW_HEIGHT`] and [`MAX_ROW_HEIGHT`] points per row.
//!
//! Typed values zoom around the playhead / edit position when it is on screen,
//! otherwise around the centre of the view (Y: around the centre row). The
//! mouse wheel zooms around the pointer.

use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::timing::{Division, TimingKind};
use plumspirit_engine::view::{
    MAX_PIXELS_PER_TICK, MAX_ROW_HEIGHT, MIN_PIXELS_PER_TICK, MIN_ROW_HEIGHT, PitchAxis, TimeAxis,
    clamp_zoom,
};

pub const DEFAULT_ROW_HEIGHT: f64 = 12.0;
/// Points per quarter note at Zoom X 100 %.
pub const REFERENCE_POINTS_PER_QUARTER: f64 = 32.0;
/// Points per second at Zoom X 100 % for SMPTE timing.
const REFERENCE_POINTS_PER_SECOND: f64 = 100.0;

/// Pixels per tick at Zoom X 100 % for `division`.
pub fn reference_ppt(division: Division) -> f64 {
    match division.kind() {
        TimingKind::Ppqn(0) => REFERENCE_POINTS_PER_QUARTER / 480.0,
        TimingKind::Ppqn(p) => REFERENCE_POINTS_PER_QUARTER / f64::from(p),
        TimingKind::Smpte {
            frames_per_second,
            ticks_per_frame,
        } => {
            let fps = match frames_per_second {
                29 => 29.97,
                0 => 25.0,
                f => f64::from(f),
            };
            REFERENCE_POINTS_PER_SECOND / (fps * f64::from(ticks_per_frame.max(1)))
        }
    }
}

/// Zoom X percent → pixels per tick. B1-0090: any finite positive request is
/// accepted; beyond the precision limits ([`MIN_PIXELS_PER_TICK`] /
/// [`MAX_PIXELS_PER_TICK`]) it is clamped to them (the readout then shows
/// the zoom in effect). Errors only for values that are not a positive number.
pub fn zoom_x_from_percent(percent: f64, division: Division) -> Result<f64, String> {
    if !percent.is_finite() || percent <= 0.0 {
        return Err("Zoom X must be a positive number of percent.".into());
    }
    let ppt = percent / 100.0 * reference_ppt(division);
    Ok(if ppt.is_finite() {
        ppt.clamp(MIN_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK)
    } else {
        MAX_PIXELS_PER_TICK
    })
}

/// Zoom Y percent → row height. B1-0090: like Zoom X, any finite positive
/// request is accepted and clamped to [`MIN_ROW_HEIGHT`] – [`MAX_ROW_HEIGHT`].
pub fn zoom_y_from_percent(percent: f64) -> Result<f64, String> {
    if !percent.is_finite() || percent <= 0.0 {
        return Err("Zoom Y must be a positive number of percent.".into());
    }
    let rh = percent / 100.0 * DEFAULT_ROW_HEIGHT;
    Ok(if rh.is_finite() {
        rh.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT)
    } else {
        MAX_ROW_HEIGHT
    })
}

/// Parses a typed zoom: "150", "150%", "1.5e2 %". Only plain positive numbers.
pub fn parse_percent(input: &str) -> Result<f64, String> {
    let t = input.trim();
    let t = t.strip_suffix('%').unwrap_or(t).trim();
    let t = t.replace(',', "");
    if t.is_empty() {
        return Err("Enter a zoom in percent, for example 100.".into());
    }
    // Only digits, one dot and an exponent: no "inf", "nan" or hex.
    let ok = t
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    match t.parse::<f64>() {
        Ok(v) if ok && v.is_finite() => Ok(v),
        _ => Err(format!(
            "\"{}\" is not a number of percent.",
            input.trim().chars().take(40).collect::<String>()
        )),
    }
}

/// A zoom readout: whole percent normally, more digits when small, so a real
/// zoom never reads "0%".
pub fn format_percent(p: f64) -> String {
    if !p.is_finite() || p <= 0.0 {
        return "—".to_owned();
    }
    if p >= 1e7 {
        format!("{p:.3e}%")
    } else if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 1.0 {
        format!("{p:.1}%")
    } else if p >= 0.01 {
        format!("{p:.2}%")
    } else {
        format!("{p:.2e}%")
    }
}

/// How the view follows the playhead during playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowStyle {
    /// When the playhead passes the follow position, the view turns a page so
    /// the playhead is at 10 % (or the follow position, if that is smaller).
    Page,
    /// Once the playhead reaches the follow position it stays there and the
    /// view scrolls continuously.
    Smooth,
}

/// Default follow position (v0.5): the centre of the piano roll. The user sets
/// 0–100 % in Follow settings; it is a fraction of the current width, so zoom
/// and window size changes simply recompute where it is.
pub const FOLLOW_THRESHOLD: f64 = 0.50;
/// Where the playhead lands after a page turn or a reveal (context before it).
pub const FOLLOW_LEAD_IN: f64 = 0.10;
/// The song end is never scrolled further left than this fraction of the width.
pub const END_PADDING: f64 = 0.90;
/// L-F07: at a follow position of 100 % the playhead is kept this far (points)
/// inside the right edge, so its line stays drawn.
pub const RIGHT_EDGE_MARGIN: f64 = 2.0;

/// The origin that keeps `playhead` in view (see [`FollowStyle`]), or `None` to
/// leave the view as it is. `end` is the last tick of the song: the view does not
/// scroll so far that the end sits left of [`END_PADDING`] of the width.
pub fn follow_origin(
    time: &TimeAxis,
    width: f64,
    playhead: u64,
    end: u64,
    style: FollowStyle,
    anchor: f64,
) -> Option<u64> {
    if width.is_nan() || width <= 0.0 || !time.pixels_per_tick.is_finite() {
        return None;
    }
    let anchor = if anchor.is_finite() {
        anchor.clamp(0.0, 1.0)
    } else {
        FOLLOW_THRESHOLD
    }
    // L-F07: 100 % is the right edge, just inside it.
    .min((1.0 - RIGHT_EDGE_MARGIN / width).max(0.0));
    let visible = width / time.pixels_per_tick;
    let back = |frac: f64| -> u64 {
        let d = (visible * frac).min(u64::MAX as f64) as u64;
        playhead.saturating_sub(d)
    };
    let max_origin = end
        .max(playhead)
        .saturating_sub((visible * END_PADDING).min(u64::MAX as f64) as u64);
    let x = time.tick_to_x(playhead);
    // Behind the view (a zoom, a resize, a loop back): reveal it near the
    // left edge. Past the right edge: a page turns there; Smooth keeps the
    // playhead at its position (L-F07: at 100 % the playhead reaches the
    // edge every frame — that is following, not a jump; seeks use `reveal`).
    if x < 0.0 || (x > width && style == FollowStyle::Page) {
        let o = back(FOLLOW_LEAD_IN).min(max_origin);
        return (o != time.origin_tick).then_some(o);
    }
    if x <= width * anchor {
        return None;
    }
    let target = match style {
        FollowStyle::Page => back(FOLLOW_LEAD_IN.min(anchor)),
        FollowStyle::Smooth => back(anchor),
    }
    .min(max_origin);
    // Near the end: never move backwards, never past the end padding.
    (target > time.origin_tick).then_some(target)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RollView {
    pub time: TimeAxis,
    pub pitch: PitchAxis,
}

impl Default for RollView {
    fn default() -> Self {
        RollView {
            time: TimeAxis::new(0, 0.1),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: DEFAULT_ROW_HEIGHT,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        }
    }
}

impl RollView {
    /// Initial view for a document: Zoom X and Y at `zoom` percent (100 % /
    /// 100 % unless Settings say otherwise, B1-0059) from the start of the
    /// song, vertically centered on the notes of the active track (or middle C
    /// for an empty track). Out-of-range zooms are clamped to the roll's limits.
    pub fn initial(
        doc: &PlumDocument,
        track: usize,
        zoom: (f64, f64),
        _width: f64,
        height: f64,
    ) -> Self {
        let pct = |p: f64| if p.is_finite() && p > 0.0 { p } else { 100.0 } / 100.0;
        let ppt = clamp_zoom(reference_ppt(doc.division()) * pct(zoom.0));
        let row_height = (DEFAULT_ROW_HEIGHT * pct(zoom.1)).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        let mut v = RollView {
            time: TimeAxis::new(0, ppt),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height,
                top: doc.project().keys.max_pitch(),
            },
        };
        v.center_pitch(Self::focus_pitch(doc, track), height);
        v
    }

    /// Pitch to center on: the middle of the first notes of the track, or C4.
    fn focus_pitch(doc: &PlumDocument, track: usize) -> f64 {
        let Some(t) = doc.track(track) else {
            return 60.0;
        };
        let Some(first) = t.notes.min_start() else {
            return 60.0;
        };
        let mut lo = u8::MAX;
        let mut hi = 0u8;
        let mut n = 0;
        let span = doc.division().musical_ppqn().unwrap_or(480) * 16;
        t.notes.query(first, first.saturating_add(span), |_, v| {
            if n < 5000 {
                lo = lo.min(v.attrs.pitch());
                hi = hi.max(v.attrs.pitch());
                n += 1;
            }
        });
        if n == 0 {
            60.0
        } else {
            (f64::from(lo) + f64::from(hi)) / 2.0
        }
    }

    pub fn center_pitch(&mut self, pitch: f64, height: f64) {
        let rows_visible = height / self.pitch.row_height;
        self.pitch.scroll_rows = (f64::from(self.pitch.top) - pitch) - rows_visible / 2.0 + 0.5;
        self.pitch.clamp_scroll(height);
    }

    /// Follows the project's key range (v0.9 256-Key Mode on / off): the
    /// rows become `top + 1` and the pitch at the top of the view stays
    /// there (clamped into the new range when it no longer exists).
    pub fn set_top(&mut self, top: u8, height: f64) {
        if self.pitch.top == top {
            return;
        }
        self.pitch.scroll_rows += f64::from(top) - f64::from(self.pitch.top);
        self.pitch.top = top;
        self.pitch.clamp_scroll(height);
    }

    /// Smallest zoom: `span` ticks (the whole song, see [`zoom_span`]) fill 90% of
    /// the width. Zooming out further would only show empty timeline.
    pub fn min_zoom(span: u64, width: f64) -> f64 {
        (width * 0.9 / span.max(1) as f64).max(MIN_PIXELS_PER_TICK)
    }

    /// Zooms time around `anchor_x`. Zooming in is limited by the engine's maximum
    /// zoom; zooming out stops once `span` ticks fit the width, unless the view is
    /// already further out (then it never jumps inwards).
    pub fn zoom_time(&mut self, anchor_x: f64, factor: f64, span: u64, width: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let current = self.time.pixels_per_tick;
        let floor = Self::min_zoom(span, width).min(current);
        let target = clamp_zoom((current * factor).max(floor));
        self.time.zoom_around(anchor_x, target);
        if factor < 1.0 {
            // Zooming out should reveal the song, not empty timeline after its end:
            // keep the end of `span` at or beyond 90% of the width.
            let visible = width / self.time.pixels_per_tick;
            let max_origin = (span as f64 - visible * 0.9).max(0.0);
            if self.time.origin_tick as f64 > max_origin {
                self.time.origin_tick = max_origin as u64;
            }
        }
    }

    /// The one Y-zoom kernel (v0.8 §155): the typed percentage
    /// ([`Self::set_zoom_y`]), the Y-Zoom − / + buttons and Ctrl+Shift+wheel
    /// ([`wheel_factor`]) all scale the row height through here.
    pub fn zoom_pitch(&mut self, anchor_y: f64, factor: f64, height: f64) {
        self.zoom_pitch_floor(anchor_y, factor, height, MIN_ROW_HEIGHT);
    }

    /// [`Self::zoom_pitch`] that stops zooming out at `floor` points per row
    /// (B1-0087: the row height at which every key fills the height, unless
    /// Y Zoom Beyond the Keys is on).
    pub fn zoom_pitch_floor(&mut self, anchor_y: f64, factor: f64, height: f64, floor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old = self.pitch.row_height;
        let floor = floor.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        let new = (old * factor).clamp(floor.min(old), MAX_ROW_HEIGHT);
        // Keep the row under the pointer fixed.
        let row_at_anchor = anchor_y / old + self.pitch.scroll_rows;
        self.pitch.row_height = new;
        self.pitch.scroll_rows = row_at_anchor - anchor_y / new;
        self.pitch.clamp_scroll(height);
    }

    pub fn scroll_rows(&mut self, rows: f64, height: f64) {
        self.pitch.scroll_rows += rows;
        self.pitch.clamp_scroll(height);
    }

    /// End of the scrollable horizontal range: content plus half a screen.
    pub fn scroll_extent(&self, content_end: u64, width: f64) -> u64 {
        let visible = (width / self.time.pixels_per_tick).max(1.0);
        let end = content_end as f64 + visible * 0.5;
        let origin_end = self.time.origin_tick as f64 + visible;
        end.max(origin_end).min(u64::MAX as f64) as u64
    }
}

impl RollView {
    /// Sets Zoom X to `ppt` pixels per tick, keeping the tick at `anchor_x` fixed.
    pub fn set_zoom_x(&mut self, anchor_x: f64, ppt: f64) {
        self.time.zoom_around(anchor_x, ppt);
    }

    /// Sets Zoom Y to `row_height`, keeping the row at `anchor_y` fixed.
    pub fn set_zoom_y(&mut self, anchor_y: f64, row_height: f64, height: f64) {
        let old = self.pitch.row_height;
        if old > 0.0 {
            self.zoom_pitch(anchor_y, row_height / old, height);
        }
    }

    /// Zoom X as a percentage of the reference (see module docs).
    pub fn zoom_x_percent(&self, division: Division) -> f64 {
        self.time.pixels_per_tick / reference_ppt(division) * 100.0
    }

    /// Zoom Y as a percentage of the reference row height.
    pub fn zoom_y_percent(&self) -> f64 {
        self.pitch.row_height / DEFAULT_ROW_HEIGHT * 100.0
    }

    /// Shows ticks `0..=span` across 95 % of the width (Fit Song).
    pub fn fit(&mut self, span: u64, width: f64) {
        let ppt = clamp_zoom(width * 0.95 / span.max(1) as f64);
        self.time = TimeAxis::new(0, ppt);
    }
}

/// Tick span the zoom-out limit is based on: the song (or the edit cursor, if it is
/// further right), but at least 16 beats so empty documents zoom sensibly.
pub fn zoom_span(doc: &PlumDocument, cursor_tick: u64) -> u64 {
    let beat = doc.division().musical_ppqn().map_or(480, u64::from);
    doc.content_end_tick()
        .max(cursor_tick)
        .max(beat.saturating_mul(16))
}

/// Zoom factor of a wheel movement of `delta` points (Ctrl+wheel = X,
/// Ctrl+Shift+wheel = Y).
pub fn wheel_factor(delta: f64) -> f64 {
    1.004f64.powf(delta)
}

/// Factor of one Y-Zoom − / + button press (and of View → Zoom In / Out Y).
pub const Y_ZOOM_STEP: f64 = 1.25;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_zoom_paths_share_one_kernel() {
        // v0.8 §155: typed value, button and wheel give the same view.
        let h = 600.0;
        let mut typed = RollView::default();
        typed.set_zoom_y(h / 2.0, zoom_y_from_percent(125.0).unwrap(), h);
        let mut button = RollView::default();
        button.zoom_pitch(h / 2.0, Y_ZOOM_STEP, h);
        let mut wheel = RollView::default();
        let d = Y_ZOOM_STEP.ln() / 1.004f64.ln();
        wheel.zoom_pitch(h / 2.0, wheel_factor(d), h);
        for v in [&button, &wheel] {
            assert!((v.pitch.row_height - typed.pitch.row_height).abs() < 1e-9);
            assert!((v.pitch.scroll_rows - typed.pitch.scroll_rows).abs() < 1e-9);
        }
        assert!((typed.zoom_y_percent() - 125.0).abs() < 1e-9);
        // In then out returns to the start (around the same row).
        let start = RollView::default();
        button.zoom_pitch(h / 2.0, 1.0 / Y_ZOOM_STEP, h);
        assert!((button.pitch.row_height - start.pitch.row_height).abs() < 1e-9);
        assert!((button.pitch.scroll_rows - start.pitch.scroll_rows).abs() < 1e-9);
        // Limits hold for every path.
        for _ in 0..200 {
            button.zoom_pitch(h / 2.0, Y_ZOOM_STEP, h);
        }
        assert_eq!(button.pitch.row_height, MAX_ROW_HEIGHT);
    }

    #[test]
    fn zoom_percent_maps_to_the_documented_reference() {
        let d = Division::ppqn(960);
        let ppt = zoom_x_from_percent(100.0, d).unwrap();
        assert!((ppt * 960.0 - REFERENCE_POINTS_PER_QUARTER).abs() < 1e-12);
        assert!((zoom_x_from_percent(250.0, d).unwrap() / ppt - 2.5).abs() < 1e-12);
        assert_eq!(zoom_y_from_percent(100.0).unwrap(), DEFAULT_ROW_HEIGHT);
        assert_eq!(zoom_y_from_percent(200.0).unwrap(), 24.0);
        // SMPTE: 100 points per second (25 fps × 40 ticks per frame).
        let s = Division::from_raw(0xE728);
        assert!((zoom_x_from_percent(100.0, s).unwrap() * 1000.0 - 100.0).abs() < 1e-9);
        // Round trip through the view.
        let mut v = RollView::default();
        v.set_zoom_x(0.0, zoom_x_from_percent(333.0, d).unwrap());
        assert!((v.zoom_x_percent(d) - 333.0).abs() < 1e-9);
        v.set_zoom_y(0.0, zoom_y_from_percent(50.0).unwrap(), 500.0);
        assert!((v.zoom_y_percent() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn typed_zoom_values_are_checked() {
        assert_eq!(parse_percent(" 150 % "), Ok(150.0));
        assert_eq!(parse_percent("1.5e2"), Ok(150.0));
        assert_eq!(parse_percent("1,000"), Ok(1000.0));
        for bad in ["", "abc", "inf", "NaN", "0x10", "%"] {
            assert!(parse_percent(bad).is_err(), "{bad}");
        }
        let d = Division::ppqn(960);
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(zoom_x_from_percent(bad, d).is_err(), "{bad}");
        }
        // Far beyond a slider's range, but still usable: accepted.
        assert!(zoom_x_from_percent(50_000.0, d).is_ok());
        assert!(zoom_x_from_percent(0.000_1, d).is_ok());
        // B1-0090: beyond the precision limits a request is clamped to them.
        assert_eq!(zoom_x_from_percent(1e30, d), Ok(MAX_PIXELS_PER_TICK));
        assert_eq!(zoom_x_from_percent(1e-30, d), Ok(MIN_PIXELS_PER_TICK));
        assert_eq!(zoom_x_from_percent(f64::MAX, d), Ok(MAX_PIXELS_PER_TICK));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(zoom_y_from_percent(bad).is_err(), "{bad}");
        }
        assert_eq!(zoom_y_from_percent(1.0), Ok(MIN_ROW_HEIGHT));
        assert_eq!(zoom_y_from_percent(5_000.0), Ok(600.0));
        assert_eq!(zoom_y_from_percent(1e300), Ok(MAX_ROW_HEIGHT));
        assert!(zoom_y_from_percent(5.0).is_ok());
        assert!(zoom_y_from_percent(4_000.0).is_ok());
        assert_eq!(format_percent(100.0), "100%");
        assert_eq!(format_percent(0.37), "0.37%");
        assert_eq!(format_percent(0.000_12), "1.20e-4%");
    }

    #[test]
    fn zooming_keeps_huge_positions_exact() {
        let d = Division::ppqn(960);
        let far = u64::MAX / 2 + 12_345;
        let mut v = RollView {
            time: TimeAxis::new(far, reference_ppt(d)),
            ..RollView::default()
        };
        let anchor = v.time.tick_to_x(far + 9_600);
        v.set_zoom_x(anchor, zoom_x_from_percent(400.0, d).unwrap());
        assert_eq!(v.time.x_to_tick(v.time.tick_to_x(far + 9_600)), far + 9_600);
        assert!((v.time.tick_to_x(far + 9_600) - anchor).abs() < 1.0);
    }

    #[test]
    fn follow_position_is_a_percentage_of_the_width() {
        // 1000 px wide, 1 px per tick, song far away.
        let t = TimeAxis::new(0, 1.0);
        let end = 1_000_000;
        for (pct, at) in [(0.0, 0u64), (0.25, 250), (0.50, 500), (0.75, 750)] {
            // At the position: nothing moves; just past it: the view follows so
            // the playhead stays at the position (smooth).
            assert_eq!(
                follow_origin(&t, 1000.0, at, end, FollowStyle::Smooth, pct),
                None
            );
            assert_eq!(
                follow_origin(&t, 1000.0, at + 10, end, FollowStyle::Smooth, pct),
                Some(10),
                "{pct}"
            );
        }
        // 100 %: the playhead reaches the right edge (2 points inside it)
        // and stays there — L-F07: it used to be revealed near the left edge,
        // a page turn every time.
        assert_eq!(
            follow_origin(&t, 1000.0, 998, end, FollowStyle::Smooth, 1.0),
            None
        );
        assert_eq!(
            follow_origin(&t, 1000.0, 1_001, end, FollowStyle::Smooth, 1.0),
            Some(3)
        );
        // The default is the centre.
        assert_eq!(FOLLOW_THRESHOLD, 0.5);
        // Zoom / resize: the same percentage of a new width.
        let t = TimeAxis::new(0, 0.5); // 2000 ticks visible in 1000 px
        assert_eq!(
            follow_origin(&t, 1000.0, 1_010, end, FollowStyle::Smooth, 0.5),
            Some(10)
        );
        assert_eq!(
            follow_origin(&t, 500.0, 510, end, FollowStyle::Smooth, 0.5),
            Some(10)
        );
        // Smooth following never jitters backwards around the threshold.
        let t = TimeAxis::new(10, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, 509, end, FollowStyle::Smooth, 0.5),
            None
        );
        // Page turns land at 10 % (or at the position when it is smaller).
        let t = TimeAxis::new(0, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, 510, end, FollowStyle::Page, 0.5),
            Some(410)
        );
        assert_eq!(
            follow_origin(&t, 1000.0, 60, end, FollowStyle::Page, 0.05),
            Some(10)
        );
        // Invalid input falls back to the default instead of misbehaving.
        assert_eq!(
            follow_origin(&t, 1000.0, 510, end, FollowStyle::Smooth, f64::NAN),
            Some(10)
        );
    }

    /// L-F07 (Flopp: "smooth follow is broken at follow position 100%"):
    /// frame by frame playback at 0 / 25 / 50 / 75 / 100 % across widths and
    /// zooms (as the UI scale changes them): once the playhead reaches its
    /// position it stays there (within one tick's width) — never a jump, never
    /// backwards — until the song's end stops the view; at 100 % it stays
    /// just inside the right edge, visible.
    #[test]
    fn smooth_follow_keeps_the_playhead_at_its_position_at_every_percentage() {
        for width in [300.0f64, 731.0, 1000.0, 1920.0] {
            for ppt in [0.05f64, 0.5, 1.0, 3.0] {
                for pct in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    let end = 2_000_000u64;
                    let mut t = TimeAxis::new(0, ppt);
                    let want = (pct * width).min(width - RIGHT_EDGE_MARGIN);
                    let step = (37.0 / ppt).max(1.0) as u64; // ~37 px a frame
                    let mut reached = false;
                    let mut last_origin = 0;
                    let mut playhead = 0u64;
                    while playhead < end / 2 {
                        playhead += step;
                        if let Some(o) =
                            follow_origin(&t, width, playhead, end, FollowStyle::Smooth, pct)
                        {
                            t.origin_tick = o;
                        }
                        assert!(
                            t.origin_tick >= last_origin,
                            "{width} {ppt} {pct}: backwards"
                        );
                        last_origin = t.origin_tick;
                        let x = t.tick_to_x(playhead);
                        assert!(
                            (0.0..=width).contains(&x),
                            "{width} {ppt} {pct}: the playhead left the view (x {x})"
                        );
                        // A tick is the finest step the view can take.
                        if x >= want - ppt - 1e-9 {
                            reached = true;
                        }
                        if reached {
                            assert!(
                                (x - want).abs() <= ppt.max(1.0) + 1e-6,
                                "{width} {ppt} {pct}: x {x}, want {want}"
                            );
                        }
                    }
                    assert!(reached, "{width} {ppt} {pct}");
                }
            }
        }
    }

    #[test]
    fn page_follow_turns_pages_at_the_threshold_and_stops_at_the_end() {
        // 1000 px wide, 1 px per tick: 1000 ticks visible; song ends at 10,000.
        let t = TimeAxis::new(0, 1.0);
        let end = 10_000;
        // Before 75 %: nothing moves (no jump every frame).
        assert_eq!(
            follow_origin(&t, 1000.0, 740, end, FollowStyle::Page, 0.75),
            None
        );
        // Past 75 %: a page turn puts the playhead at 10 %.
        assert_eq!(
            follow_origin(&t, 1000.0, 760, end, FollowStyle::Page, 0.75),
            Some(660)
        );
        // Smooth: the playhead stays at 75 %.
        assert_eq!(
            follow_origin(&t, 1000.0, 760, end, FollowStyle::Smooth, 0.75),
            Some(10)
        );
        // Near the end the song end stays at 90 % of the width at most.
        let t = TimeAxis::new(8_900, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, 9_700, end, FollowStyle::Page, 0.75),
            Some(9_100)
        );
        let t = TimeAxis::new(9_100, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, 9_900, end, FollowStyle::Page, 0.75),
            None
        );
        assert_eq!(
            follow_origin(&t, 1000.0, 10_000, end, FollowStyle::Smooth, 0.75),
            None
        );
        // Out of view (seek backwards or forwards): revealed at 10 %.
        let t = TimeAxis::new(5_000, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, 100, end, FollowStyle::Page, 0.75),
            Some(0)
        );
        assert_eq!(
            follow_origin(&t, 1000.0, 7_000, end, FollowStyle::Page, 0.75),
            Some(6_900)
        );
        // Any zoom: 0.001 px per tick (1,000,000 ticks visible).
        let t = TimeAxis::new(0, 0.001);
        assert_eq!(
            follow_origin(&t, 1000.0, 800_000, 100_000_000, FollowStyle::Page, 0.75),
            Some(700_000)
        );
        // Huge ticks.
        let base = u64::MAX - 10_000_000;
        let t = TimeAxis::new(base, 1.0);
        assert_eq!(
            follow_origin(&t, 1000.0, base + 800, u64::MAX, FollowStyle::Page, 0.75),
            Some(base + 700)
        );
        // A page turn repeated along a whole song always keeps the playhead visible.
        let mut t = TimeAxis::new(0, 0.37);
        let w = 777.0;
        for p in (0..200_000u64).step_by(97) {
            if let Some(o) = follow_origin(&t, w, p, 200_000, FollowStyle::Page, 0.75) {
                t.origin_tick = o;
            }
            let x = t.tick_to_x(p);
            assert!((0.0..=w).contains(&x), "playhead {p} at x {x}");
        }
    }

    #[test]
    fn zoom_out_stops_when_the_song_fills_the_width() {
        let mut v = RollView::default();
        v.time.pixels_per_tick = 0.1;
        let span = 100_000;
        for _ in 0..200 {
            v.zoom_time(0.0, 0.5, span, 1000.0);
        }
        let expected = 1000.0 * 0.9 / span as f64;
        assert!((v.time.pixels_per_tick - expected).abs() < 1e-12);
        // Zooming in again works.
        v.zoom_time(0.0, 2.0, span, 1000.0);
        assert!(v.time.pixels_per_tick > expected);
    }

    #[test]
    fn zooming_out_fully_shows_the_whole_song() {
        let mut v = RollView {
            time: TimeAxis::new(90_000, 0.1),
            ..RollView::default()
        };
        let span = 100_000;
        for _ in 0..60 {
            v.zoom_time(500.0, 0.8, span, 1000.0);
        }
        assert_eq!(v.time.origin_tick, 0);
    }

    #[test]
    fn zooming_never_jumps_inwards_from_a_wider_view() {
        let mut v = RollView::default();
        v.time.pixels_per_tick = 0.001;
        // A tiny span would put the limit far inside the current zoom.
        v.zoom_time(0.0, 0.5, 10, 1000.0);
        assert!(v.time.pixels_per_tick <= 0.001);
    }
}
