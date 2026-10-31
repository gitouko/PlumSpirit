//! Direct selection scale handles (v0.7, contract §135–137): eight handles on
//! the selection's box — left / right scale X, top / bottom scale Y, corners
//! both. Dragging shows a transient preview (the app evaluates the same exact
//! `BoxScale` kernel it commits); releasing commits one undo step; Escape
//! cancels. Holding Shift snaps the dragged edges to the current Snap.
//!
//! v0.9 direct gestures on the corners (§084, §085): Ctrl+drag skews (the
//! drag's first clear direction picks time or pitch; the dragged corner's edge
//! slides, the opposite edge's two corners stay) and Ctrl+Shift+drag moves the
//! one corner freely (Perspective; the other three stay) — both through the
//! engine's exact [`CornerWarp`] kernel, previewed and committed like scaling.

use super::Geometry;
use super::hit_test::note_rect;
use egui::{Pos2, Rect, Vec2};
use plumspirit_engine::model::SelectionBounds;
use plumspirit_engine::transform::geom::{Corner, CornerWarp, GeomBox, WarpKind};

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const BOTTOM: u8 = 4;
pub const TOP: u8 = 8;

/// Half size of a handle square (points).
const HALF: f32 = 4.0;
/// Handles are shown only when the box is at least this large on screen.
const MIN_BOX: f32 = 6.0;

pub fn geom_box(b: &SelectionBounds) -> GeomBox {
    GeomBox {
        t0: b.min_start,
        t1: b.max_end.max(b.min_start + 1),
        p0: b.min_pitch,
        p1: b.max_pitch,
    }
}

/// The box on screen (top of row `p1` to bottom of row `p0`).
pub fn screen_rect(g: &Geometry, b: &GeomBox) -> Rect {
    let top = note_rect(g, b.t0, b.t1, b.p1);
    let bottom = note_rect(g, b.t0, b.t1, b.p0);
    top.union(bottom)
}

/// The eight handles: (edge mask, centre).
pub fn handles(r: Rect) -> [(u8, Pos2); 8] {
    let (l, rr, t, b) = (r.left(), r.right(), r.top(), r.bottom());
    let (cx, cy) = (r.center().x, r.center().y);
    [
        (LEFT | TOP, Pos2::new(l, t)),
        (RIGHT | TOP, Pos2::new(rr, t)),
        (LEFT | BOTTOM, Pos2::new(l, b)),
        (RIGHT | BOTTOM, Pos2::new(rr, b)),
        (LEFT, Pos2::new(l, cy)),
        (RIGHT, Pos2::new(rr, cy)),
        (TOP, Pos2::new(cx, t)),
        (BOTTOM, Pos2::new(cx, b)),
    ]
}

pub fn visible(r: Rect, area: Rect) -> bool {
    r.width() >= MIN_BOX && r.height() >= MIN_BOX && r.expand(HALF + 2.0).intersects(area)
}

pub fn handle_rect(c: Pos2) -> Rect {
    Rect::from_center_size(c, Vec2::splat(HALF * 2.0))
}

/// The handle under `pos` (a little larger than drawn).
pub fn hit(r: Rect, pos: Pos2) -> Option<u8> {
    handles(r)
        .into_iter()
        .find(|(_, c)| handle_rect(*c).expand(2.0).contains(pos))
        .map(|(m, _)| m)
}

/// The box after dragging handle `mask` to `pos` (`snap`: Shift held, with the
/// snap unit in ticks). Edges never cross: at least one tick and one row.
pub fn dragged(g: &Geometry, from: &GeomBox, mask: u8, pos: Pos2, snap: Option<u64>) -> GeomBox {
    let mut to = *from;
    let tick = || {
        let t = g.tick_at(pos.x);
        match snap {
            Some(u) => plumspirit_engine::transform::geom::snap_edge(i128::from(t), u)
                .clamp(0, i128::from(u64::MAX)) as u64,
            None => t,
        }
    };
    if mask & LEFT != 0 {
        to.t0 = tick().min(from.t1 - 1);
    }
    if mask & RIGHT != 0 {
        to.t1 = tick().max(from.t0 + 1);
    }
    let pitch = g.pitch_at_clamped(pos.y);
    if mask & TOP != 0 {
        to.p1 = pitch.max(from.p0);
    }
    if mask & BOTTOM != 0 {
        to.p0 = pitch.min(from.p1);
    }
    to
}

/// The box corner of a corner handle (`None` for an edge handle).
pub fn corner_of(mask: u8) -> Option<Corner> {
    match mask {
        m if m == LEFT | BOTTOM => Some(Corner::BottomLeft),
        m if m == RIGHT | BOTTOM => Some(Corner::BottomRight),
        m if m == RIGHT | TOP => Some(Corner::TopRight),
        m if m == LEFT | TOP => Some(Corner::TopLeft),
        _ => None,
    }
}

/// A Skew drag picks its axis once the pointer has moved this far (points)
/// from where it was pressed: along time when the move was mostly
/// horizontal, along pitch otherwise.
pub const AXIS_SLOP: f32 = 6.0;

/// The skew axis of a drag from `origin` to `pos` (`None` until it is clear).
pub fn skew_axis(origin: Pos2, pos: Pos2) -> Option<WarpKind> {
    let d = pos - origin;
    (d.length() >= AXIS_SLOP).then_some(if d.x.abs() >= d.y.abs() {
        WarpKind::SkewX
    } else {
        WarpKind::SkewY
    })
}

/// Corner `corner` dragged from `origin` (where the handle was pressed) to
/// `pos`: whole ticks, and whole rows (the corner snaps to row lines — half a
/// row moves it one row). A click without movement changes nothing.
pub fn corner_warp(
    g: &Geometry,
    corner: Corner,
    kind: WarpKind,
    origin: Pos2,
    pos: Pos2,
) -> CornerWarp {
    let dt = i128::from(g.tick_at(pos.x)) - i128::from(g.tick_at(origin.x));
    let row = |y: f32| g.pitch_f_at(y).round() as i64;
    CornerWarp {
        kind,
        corner,
        dt: dt.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
        dr: row(pos.y) - row(origin.y),
    }
}

/// B1-0091: the rotation an Alt + corner drag from `origin` to `pos` makes —
/// the angle the pointer turned about the pivot on screen (counter-clockwise
/// positive), in 15° steps with `snap` (Shift); the view's aspect when the
/// drag is evaluated keeps it round on screen. `None` while the pointer is
/// too close to the pivot to give an angle.
pub fn rotation_warp(
    g: &Geometry,
    from: &GeomBox,
    corner: Corner,
    about_center: bool,
    origin: Pos2,
    pos: Pos2,
    snap: bool,
) -> Option<CornerWarp> {
    let ppt = g.view.time.pixels_per_tick.max(1e-12);
    let k = g.view.pitch.row_height / ppt;
    let w0 = CornerWarp::rotation(corner, about_center, 0.0, k);
    let (pu, pv) = w0.pivot_unit();
    let (x, y) = from.from_unit(pu, pv);
    let pc = g.point_of(x.max(0.0).round() as u64, y);
    if pos.distance(pc) < 4.0 || origin.distance(pc) < 1.0 {
        return None;
    }
    let ang = |p: Pos2| f64::from(-(p.y - pc.y)).atan2(f64::from(p.x - pc.x));
    let mut deg = (ang(pos) - ang(origin)).to_degrees();
    if snap {
        deg = (deg / 15.0).round() * 15.0;
    }
    Some(CornerWarp::rotation(corner, about_center, deg, k))
}

/// The warped box's outline on screen (bottom left, bottom right, top right,
/// top left).
pub fn warp_outline(g: &Geometry, from: &GeomBox, w: &CornerWarp) -> [Pos2; 4] {
    w.corners(from).map(|(x, y)| {
        let p = g.point_of(x.max(0.0).round() as u64, y);
        // Ticks left of zero stay visible as their offset from tick 0.
        if x < 0.0 {
            Pos2::new(p.x + (x * g.view.time.pixels_per_tick) as f32, p.y)
        } else {
            p
        }
    })
}

pub fn cursor(mask: u8) -> egui::CursorIcon {
    use egui::CursorIcon as C;
    match mask {
        m if m == LEFT | TOP || m == RIGHT | BOTTOM => C::ResizeNwSe,
        m if m == RIGHT | TOP || m == LEFT | BOTTOM => C::ResizeNeSw,
        LEFT | RIGHT => C::ResizeHorizontal,
        _ => C::ResizeVertical,
    }
}
