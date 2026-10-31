//! Note hit testing. Only notes near the pointer are queried (a few pixels of time
//! around the pointer, filtered to the pointer's pitch row), never the whole track.

use super::Geometry;
use egui::{Pos2, Rect};
use plumspirit_engine::model::{NoteLoc, NoteStore, NoteView};

/// Tolerance around the pointer, in points.
const TOLERANCE_PX: f32 = 3.0;
/// Width of the right-edge resize handle, in points.
const EDGE_PX: f32 = 5.0;
/// Notes narrower than this are too small to offer a separate resize handle.
const MIN_WIDTH_FOR_EDGE: f32 = 8.0;
/// Minimum drawn / clickable width of a note (zero-duration notes stay visible).
pub const MIN_NOTE_WIDTH: f32 = 2.0;

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub loc: NoteLoc,
    pub note: NoteView,
    pub rect: Rect,
    /// Pointer is on the right edge (resize handle).
    pub on_edge: bool,
}

/// Screen rectangle of a note (not clipped).
pub fn note_rect(g: &Geometry, start: u64, end: u64, pitch: u8) -> Rect {
    // v0.8 §123: edges on the physical pixel grid, each from its own unrounded
    // position, so touching notes and adjacent rows share an edge exactly (no
    // 1-pixel gaps or overlaps from float conversion, stable while zooming);
    // drawing and hit-testing use the same rectangle.
    let ppp = if g.ppp.is_finite() && g.ppp > 0.0 {
        g.ppp
    } else {
        1.0
    };
    let snap = |v: f32| (v * ppp).round() / ppp;
    let x0 = snap(g.notes.left() + g.view.time.tick_to_x(start) as f32);
    let x1 = snap(g.notes.left() + g.view.time.tick_to_x(end) as f32).max(x0 + MIN_NOTE_WIDTH);
    let (y0, y1) = row_span(g.notes.top(), &g.view.pitch, ppp, pitch);
    Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
}

/// Top and bottom of pitch row `pitch` on the physical pixel grid (v0.8
/// §123), below a note area whose top is `notes_top`. B2-D02: the dense
/// tiles' rows take the same edges, so both renderers cover the same pixels.
pub fn row_span(
    notes_top: f32,
    axis: &plumspirit_engine::view::PitchAxis,
    ppp: f32,
    pitch: u8,
) -> (f32, f32) {
    let ppp = if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    };
    let snap = |v: f32| (v * ppp).round() / ppp;
    let top = axis.pitch_top_y(pitch);
    let y0 = snap(notes_top + top as f32);
    let y1 = snap(notes_top + (top + axis.row_height) as f32).max(y0 + 1.0 / ppp);
    (y0, y1)
}

/// Topmost note under `pos`. Priority is deterministic: selected notes first, then
/// the note drawn last (latest in stored order).
pub fn hit_note(store: &NoteStore, g: &Geometry, pos: Pos2) -> Option<Hit> {
    if !g.notes.contains(pos) {
        return None;
    }
    let pitch = g.view.pitch.y_to_pitch(f64::from(pos.y - g.notes.top()))?;
    let x = f64::from(pos.x - g.notes.left());
    let t0 = g
        .view
        .time
        .x_to_tick(x - f64::from(TOLERANCE_PX + MIN_NOTE_WIDTH));
    let t1 = g.view.time.x_to_tick(x + f64::from(TOLERANCE_PX));
    let mut best: Option<Hit> = None;
    store.query(t0, t1, |loc, note| {
        if note.attrs.pitch() != pitch {
            return;
        }
        let rect = note_rect(g, note.start_tick, note.end_tick(), pitch);
        let hit_rect = rect.expand2(egui::vec2(TOLERANCE_PX.min(rect.width()), 0.0));
        if !hit_rect.contains(pos) {
            return;
        }
        let on_edge = rect.width() >= MIN_WIDTH_FOR_EDGE && pos.x >= rect.right() - EDGE_PX;
        let candidate = Hit {
            loc,
            note,
            rect,
            on_edge,
        };
        let better = match &best {
            None => true,
            Some(b) => note.selected || !b.note.selected,
        };
        if better {
            best = Some(candidate);
        }
    });
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piano_roll::viewport::RollView;
    use plumspirit_engine::view::{PitchAxis, TimeAxis};

    #[test]
    fn note_edges_are_on_the_pixel_grid_and_touching_notes_share_them() {
        for ppp in [1.0f32, 1.25, 1.5, 2.0] {
            let view = RollView {
                time: TimeAxis::new(7, 0.137),
                pitch: PitchAxis {
                    scroll_rows: 3.3,
                    row_height: 7.3,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            };
            let g = Geometry::new(
                Rect::from_min_size(Pos2::new(3.7, 11.1), egui::vec2(900.0, 600.0)),
                view,
                ppp,
            );
            let a = note_rect(&g, 1000, 1337, 60);
            let b = note_rect(&g, 1337, 1900, 60);
            assert_eq!(a.max.x, b.min.x, "touching notes share an edge ({ppp})");
            let up = note_rect(&g, 1000, 1337, 61);
            assert_eq!(up.max.y, a.min.y, "adjacent rows share an edge ({ppp})");
            for v in [a.min.x, a.max.x, a.min.y, a.max.y] {
                assert!(((v * ppp) - (v * ppp).round()).abs() < 1e-3, "{v} at {ppp}");
            }
        }
    }
}
