//! The Spiderweb board: beats × keys (the project's time: beat b = tick
//! b · PPQ), the shapes, the notes they make (rectangles when few are in
//! view, the density picture made with the result when many), and the
//! drawing tools. Coordinates follow Spiderweb: a shape point is
//! `[beat, pitch]`, pitch `p` is the middle of key `p`'s row.

use super::{Board, Rendered, Spiderweb, Tool};
use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use plumspirit_engine::json::Value;
use plumspirit_engine::spiderweb::paths::P;
use plumspirit_engine::spiderweb::{self as sw, edit, output::SLOT_COLORS};
use std::collections::BTreeSet;

/// Width of the key names at the left.
pub const KEYS_W: f32 = 34.0;
/// Height of the beat ruler at the top.
pub const RULER_H: f32 = 18.0;
/// The highest key of a standard board (v0.9: [`View::top_key`] follows the
/// project's keyboard).
pub const TOP_KEY: f64 = 127.0;
/// Most note rectangles drawn one by one (more: the density picture).
const MAX_RECTS: usize = 40_000;
/// How near (pixels) a click must be to a line to pick it.
const PICK: f32 = 6.0;
/// Pixels the mouse must move before a press becomes a drag.
const STILL: f32 = 4.0;

/// What part of the board is on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The beat at the left edge.
    pub left: f64,
    /// The pitch at the top edge.
    pub top: f64,
    pub px_beat: f64,
    pub px_key: f64,
    /// The board's highest key: 127, or 255 in a 256-Key Mode project.
    pub top_key: f64,
}

impl Default for View {
    fn default() -> Self {
        View {
            left: -0.25,
            top: 96.5,
            px_beat: 64.0,
            px_key: 9.0,
            top_key: TOP_KEY,
        }
    }
}

impl View {
    pub fn b2x(&self, r: Rect, b: f64) -> f32 {
        r.left() + ((b - self.left) * self.px_beat) as f32
    }
    pub fn x2b(&self, r: Rect, x: f32) -> f64 {
        self.left + f64::from(x - r.left()) / self.px_beat
    }
    pub fn p2y(&self, r: Rect, p: f64) -> f32 {
        r.top() + ((self.top - p) * self.px_key) as f32
    }
    pub fn y2p(&self, r: Rect, y: f32) -> f64 {
        self.top - f64::from(y - r.top()) / self.px_key
    }
    pub fn pos(&self, r: Rect, q: P) -> Pos2 {
        pos2(self.b2x(r, q[0]), self.p2y(r, q[1]))
    }
    /// Zoom around a screen point (factors > 1 zoom in).
    pub fn zoom(&mut self, r: Rect, at: Pos2, fx: f64, fy: f64) {
        let (b, p) = (self.x2b(r, at.x), self.y2p(r, at.y));
        self.px_beat = (self.px_beat * fx).clamp(0.05, 4000.0);
        self.px_key = (self.px_key * fy).clamp(2.0, 60.0);
        self.left = b - f64::from(at.x - r.left()) / self.px_beat;
        self.top = p + f64::from(at.y - r.top()) / self.px_key;
        self.clamp(r);
    }
    pub fn clamp(&mut self, r: Rect) {
        let rows = f64::from(r.height()) / self.px_key;
        self.top = self
            .top
            .clamp((rows - 0.5).min(self.top_key + 0.5), self.top_key + 0.5);
        self.left = self.left.max(-1.0);
    }
}

/// A shape being drawn.
#[derive(Clone, Debug)]
pub enum Draft {
    /// Line or curve from `a` to `b`; `follow`: started with a click, the end
    /// follows the mouse until the next click.
    Two {
        kind: &'static str,
        a: P,
        b: P,
        follow: bool,
    },
    /// Polyline: the last point follows the mouse.
    Poly(Vec<P>),
    /// Freehand: the points passed (unsnapped) and the last screen spot.
    Free(Vec<P>, Pos2),
    /// Arc: start + end following the mouse, or start, through, end; `bend`:
    /// dragged start → end, the middle point follows the mouse.
    Arc { pts: Vec<P>, bend: bool },
    /// A box being dragged for a circle, polygon or Hz bass.
    Box { tool: Tool, a: P, b: P },
}

/// What the mouse is doing.
#[derive(Clone, Debug)]
pub enum Drag {
    Pan(Pos2),
    Box {
        from: Pos2,
        to: Pos2,
        before: BTreeSet<usize>,
    },
    Move {
        start: P,
        orig: Vec<(usize, Vec<P>)>,
        moved: bool,
        /// Clicking one of several selected shapes without moving selects it.
        click: Option<usize>,
        press: Pos2,
    },
    Point {
        shape: usize,
        idx: usize,
        orig: Vec<P>,
    },
    /// Drawing: the press position (a press without moving is a click).
    Draw(Pos2),
}

/// Spiderweb's snap steps: (label, beats; 0 = off).
pub const SNAPS: [(&str, f64); 12] = [
    ("Off", 0.0),
    ("Bar", 4.0),
    ("1/1", 4.0),
    ("1/2", 2.0),
    ("1/4", 1.0),
    ("1/8", 0.5),
    ("1/16", 0.25),
    ("1/32", 0.125),
    ("1/3", 4.0 / 3.0),
    ("1/6", 2.0 / 3.0),
    ("1/12", 1.0 / 3.0),
    ("1/24", 1.0 / 6.0),
];

fn dist_seg(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    if l2 <= 1e-6 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

impl Spiderweb {
    /// The mouse as `[beat, pitch]`, snapped unless off or Shift.
    pub(super) fn board_pt(&self, r: Rect, at: Pos2, snap: bool) -> P {
        let x = at.x.clamp(r.left(), r.right());
        let y = at.y.clamp(r.top(), r.bottom());
        let (mut b, mut p) = (self.view.x2b(r, x), self.view.y2p(r, y));
        let sb = SNAPS[self.snap.min(SNAPS.len() - 1)].1;
        if snap && sb > 0.0 {
            b = (b / sb).round() * sb;
            p = p.round();
        }
        [b.max(0.0), p.clamp(0.0, self.view.top_key)]
    }

    /// The topmost shape under the mouse (selected ones first).
    pub(super) fn shape_at(&mut self, r: Rect, at: Pos2) -> Option<usize> {
        let mut best: Option<(bool, usize)> = None;
        for i in (0..self.board.len()).rev() {
            if self.hits(i, r, at) {
                let sel = self.sel.contains(&i);
                if sel {
                    return Some(i);
                }
                if best.is_none() {
                    best = Some((sel, i));
                }
            }
        }
        best.map(|b| b.1)
    }

    fn hits(&mut self, i: usize, r: Rect, at: Pos2) -> bool {
        let strokes = self.board.strokes(i);
        for s in strokes.iter() {
            if s.len() == 1 && (self.view.pos(r, s[0]) - at).length() < PICK {
                return true;
            }
            for w in s.windows(2) {
                if dist_seg(at, self.view.pos(r, w[0]), self.view.pos(r, w[1])) < PICK {
                    return true;
                }
            }
        }
        // inside a custom shape's frame
        let sh = self.board.get(i);
        if sw::kind(sh) == "custom" {
            let f = sw::shape_pts(sh);
            if f.len() == 3 {
                let (o, u, v) = (
                    self.view.pos(r, f[0]),
                    self.view.pos(r, f[1]),
                    self.view.pos(r, f[2]),
                );
                let (eu, ev) = (u - o, v - o);
                let det = eu.x * ev.y - eu.y * ev.x;
                if det.abs() > 1e-3 {
                    let d = at - o;
                    let a = (d.x * ev.y - d.y * ev.x) / det;
                    let b = (eu.x * d.y - eu.y * d.x) / det;
                    return (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b);
                }
            }
        }
        false
    }

    /// A point handle of a selected shape under the mouse.
    fn handle_at(&self, r: Rect, at: Pos2) -> Option<(usize, usize)> {
        for &i in self.sel.iter().rev() {
            let sh = self.board.get(i);
            if !point_kinds(sh) {
                continue;
            }
            let pts = sw::shape_pts(sh);
            for (k, q) in pts.iter().enumerate().rev() {
                if (self.view.pos(r, *q) - at).length() <= PICK {
                    return Some((i, k));
                }
            }
        }
        None
    }

    /// The board (allocates the rest of `size`): input, then painting.
    pub(super) fn canvas_ui(&mut self, ui: &mut Ui, size: egui::Vec2, ppq: u32) {
        let (full, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let r = Rect::from_min_max(pos2(full.left() + KEYS_W, full.top() + RULER_H), full.max);
        self.canvas_rect = Some(full);
        self.view.clamp(r);
        self.input(ui, r, &resp);
        if self.playhead.is_some()
            && self.scrub.is_none()
            && resp
                .hover_pos()
                .is_some_and(|p| Self::ruler_rect(r).contains(p))
        {
            resp.on_hover_text(
                "Click or drag to move the song's playhead (the main transport). Hold Shift to snap it to the Snap step (whole beats when Snap is off). Moving it never makes an undo step.",
            );
        }
        let painter = ui.painter_at(full);
        self.paint_grid(&painter, full, r, ui.visuals().dark_mode);
        let rendered = self.result.clone();
        if let Some(res) = &rendered {
            self.paint_notes(ui.ctx(), &painter, r, res, ppq);
        }
        self.paint_shapes(&painter, r, ui.visuals().dark_mode);
        self.paint_playhead(ui, &painter, full, r);
        if let Some(Drag::Box { from, to, .. }) = &self.drag {
            let b = Rect::from_two_pos(*from, *to);
            painter.rect_stroke(
                b,
                0.0,
                Stroke::new(1.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn input(&mut self, ui: &mut Ui, r: Rect, resp: &egui::Response) {
        let ctx = ui.ctx().clone();
        let hover = resp.hover_pos();
        let (shift, ctrl, alt) =
            ctx.input(|i| (i.modifiers.shift, i.modifiers.command, i.modifiers.alt));
        // wheel: keys (Shift: beats), Ctrl: zoom beats, Alt: zoom keys
        if let Some(at) = hover {
            let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if zoom != 1.0 {
                self.view.zoom(r, at, f64::from(zoom), 1.0);
            } else if scroll != egui::Vec2::ZERO {
                if ctrl {
                    let f = (f64::from(scroll.y) / 200.0).exp();
                    self.view.zoom(r, at, f, 1.0);
                } else if alt {
                    let f = (f64::from(scroll.y + scroll.x) / 200.0).exp();
                    self.view.zoom(r, at, 1.0, f);
                } else if shift {
                    self.view.left -= f64::from(scroll.y + scroll.x) / self.view.px_beat;
                } else {
                    self.view.left -= f64::from(scroll.x) / self.view.px_beat;
                    self.view.top += f64::from(scroll.y) / self.view.px_key;
                }
                self.view.clamp(r);
            }
        }
        let pointer = ctx.input(|i| i.pointer.clone());
        let pos = pointer.interact_pos().or(hover);
        // middle drag pans
        if resp.drag_started_by(egui::PointerButton::Middle)
            && let Some(p) = pos
        {
            self.drag = Some(Drag::Pan(p));
        }
        if let (Some(Drag::Pan(last)), Some(p)) = (&self.drag, pos) {
            let d = p - *last;
            self.view.left -= f64::from(d.x) / self.view.px_beat;
            self.view.top += f64::from(d.y) / self.view.px_key;
            self.view.clamp(r);
            self.drag = Some(Drag::Pan(p));
            if !pointer.middle_down() {
                self.drag = None;
            }
            return;
        }
        // B2-F26: the ruler seeks the shared transport (click or drag; the
        // playhead is free, Shift snaps it to the Snap step or whole beats).
        if self.seek_input(r, hover, pos, &pointer, shift) {
            return;
        }
        // follow-the-mouse drafts
        if let Some(p) = hover {
            let q = self.board_pt(r, p, !shift);
            match &mut self.draft {
                Some(Draft::Two {
                    b, follow: true, ..
                }) => *b = q,
                Some(Draft::Poly(pts)) => {
                    if let Some(l) = pts.last_mut() {
                        *l = q;
                    }
                }
                Some(Draft::Arc { pts, bend }) => {
                    let k = if *bend { 1 } else { pts.len() - 1 };
                    pts[k] = q;
                }
                _ => {}
            }
        }
        if resp.secondary_clicked()
            && self.tool == Tool::Paint
            && let Some(p) = hover
        {
            self.clear_areas(r, p);
            return;
        }
        if resp.secondary_clicked() {
            // right-click: finish a polyline / arc being drawn, else deselect
            match self.draft {
                Some(Draft::Poly(_)) => self.finish_poly(),
                Some(_) => self.draft = None,
                None => self.sel.clear(),
            }
            return;
        }
        if resp.double_clicked() && matches!(self.draft, Some(Draft::Poly(_))) {
            self.finish_poly();
            return;
        }
        if resp.double_clicked()
            && self.tool == Tool::Select
            && let Some(p) = hover
            && self.curve_add_anchor(r, p)
        {
            return;
        }
        let primary_pressed = pointer.button_pressed(egui::PointerButton::Primary);
        if primary_pressed && let Some(p) = hover.filter(|p| r.contains(*p)) {
            self.press(r, p, shift, ctrl, alt);
        }
        if pointer.primary_down()
            && let Some(p) = pos
        {
            self.drag_to(r, p, shift);
        }
        if pointer.button_released(egui::PointerButton::Primary)
            && let Some(p) = pos
        {
            self.release(r, p, shift, ctrl);
        }
    }

    /// The ruler band above the board.
    pub(super) fn ruler_rect(r: Rect) -> Rect {
        Rect::from_min_max(pos2(r.left(), r.top() - RULER_H), pos2(r.right(), r.top()))
    }

    /// The board beat a ruler click at `x` seeks to.
    pub(super) fn seek_beat(&self, r: Rect, x: f32, snap: bool) -> f64 {
        let b = self.view.x2b(r, x.clamp(r.left(), r.right())).max(0.0);
        if !snap {
            return b;
        }
        let step = SNAPS[self.snap.min(SNAPS.len() - 1)].1;
        let step = if step > 0.0 { step } else { 1.0 };
        (b / step).round() * step
    }

    /// Ruler press / drag: queues seeks (one per new position); true while
    /// the ruler has the pointer, so the board's tools get nothing.
    fn seek_input(
        &mut self,
        r: Rect,
        hover: Option<Pos2>,
        pos: Option<Pos2>,
        pointer: &egui::PointerState,
        shift: bool,
    ) -> bool {
        if self.playhead.is_none() {
            self.scrub = None;
            return false;
        }
        if self.scrub.is_none()
            && pointer.button_pressed(egui::PointerButton::Primary)
            && hover.is_some_and(|p| Self::ruler_rect(r).contains(p))
        {
            self.scrub = Some(f64::NAN);
        }
        let Some(last) = self.scrub else {
            return false;
        };
        if let Some(p) = pos {
            let b = self.seek_beat(r, p.x, shift);
            if b != last {
                self.transport.push(super::TransportAction::Seek(b));
                self.scrub = Some(b);
            }
        }
        if !pointer.primary_down() {
            self.scrub = None;
        }
        true
    }

    fn press(&mut self, r: Rect, at: Pos2, shift: bool, ctrl: bool, alt: bool) {
        let q = self.board_pt(r, at, !shift);
        // a shape started with a click: this click finishes it
        match &mut self.draft {
            Some(Draft::Two {
                follow: true, b, ..
            }) => {
                *b = q;
                self.commit_draft();
                return;
            }
            Some(Draft::Poly(pts)) => {
                if let Some(l) = pts.last_mut() {
                    *l = q;
                }
                pts.push(q);
                self.drag = Some(Drag::Draw(at));
                return;
            }
            Some(Draft::Arc { pts, bend }) => {
                if *bend {
                    pts[1] = q;
                    *bend = false;
                    self.finish_arc();
                } else if pts.len() == 2 {
                    pts[1] = q;
                    pts.push(q);
                } else {
                    pts[2] = q;
                    self.finish_arc();
                }
                return;
            }
            _ => {}
        }
        match self.tool {
            Tool::Select => {
                if let Some((i, k)) = self.handle_at(r, at) {
                    if alt {
                        self.toggle_sharp(i, k);
                        return;
                    }
                    self.checkpoint();
                    self.drag = Some(Drag::Point {
                        shape: i,
                        idx: k,
                        orig: sw::shape_pts(self.board.get(i)),
                    });
                    return;
                }
                match self.shape_at(r, at) {
                    None => {
                        let before = if ctrl {
                            self.sel.clone()
                        } else {
                            BTreeSet::new()
                        };
                        if !ctrl {
                            self.sel.clear();
                        }
                        self.drag = Some(Drag::Box {
                            from: at,
                            to: at,
                            before,
                        });
                    }
                    Some(i) => {
                        let mut click = None;
                        if ctrl {
                            if !self.sel.remove(&i) {
                                self.sel.insert(i);
                            }
                            if !self.sel.contains(&i) {
                                return;
                            }
                        } else if !self.sel.contains(&i) {
                            self.sel.clear();
                            self.sel.insert(i);
                        } else {
                            click = Some(i);
                        }
                        self.checkpoint();
                        let orig = self
                            .sel
                            .iter()
                            .map(|&j| (j, sw::shape_pts(self.board.get(j))))
                            .collect();
                        self.drag = Some(Drag::Move {
                            start: self.board_pt(r, at, !shift),
                            orig,
                            moved: false,
                            click,
                            press: at,
                        });
                    }
                }
            }
            Tool::Line | Tool::Curve => {
                let kind = if self.tool == Tool::Line {
                    "line"
                } else {
                    "curve"
                };
                self.draft = Some(Draft::Two {
                    kind,
                    a: q,
                    b: q,
                    follow: false,
                });
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Poly => {
                self.draft = Some(Draft::Poly(vec![q, q]));
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Free => {
                let q = self.board_pt(r, at, false);
                self.draft = Some(Draft::Free(vec![q], at));
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Arc => {
                self.draft = Some(Draft::Arc {
                    pts: vec![q, q],
                    bend: false,
                });
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Circle | Tool::Polygon | Tool::Hz => {
                self.draft = Some(Draft::Box {
                    tool: self.tool,
                    a: q,
                    b: q,
                });
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Funnel => {
                self.draft = Some(Draft::Two {
                    kind: "funnel",
                    a: q,
                    b: q,
                    follow: false,
                });
                self.drag = Some(Drag::Draw(at));
            }
            Tool::Text => self.place_text(q),
            Tool::Paint => self.paint_area(r, at),
        }
    }

    /// Area colour: the custom shape under the mouse gets a coloured spot
    /// here (Spiderweb's area painting; the area around the spot takes it).
    pub(super) fn paint_area(&mut self, r: Rect, at: Pos2) {
        let Some(i) = self.shape_at(r, at) else {
            return;
        };
        let sh = self.board.get(i);
        if sw::kind(sh) != "custom" || sh.has("notes") || sh.has("text") {
            self.share_msg = Some((
                false,
                "Areas can be colored in custom shapes (circles, polygons, live shapes).".into(),
            ));
            return;
        }
        let f = sw::shape_pts(sh);
        let q = self.board_pt(r, at, false);
        let (o, u, v) = (f[0], f[1], f[2]);
        let (ub, up, vb, vp) = (u[0] - o[0], u[1] - o[1], v[0] - o[0], v[1] - o[1]);
        let det = ub * vp - up * vb;
        if det.abs() < 1e-12 {
            return;
        }
        let (db, dp) = (q[0] - o[0], q[1] - o[1]);
        let (uu, vv) = ((db * vp - dp * vb) / det, (ub * dp - up * db) / det);
        let mut areas = sw::areas::clean_areas(sh.get("areas"));
        areas.push((uu, vv, self.paint_colour.clamp(0, sw::areas::COLOURS)));
        let mut s = sh.clone();
        s.set("areas", sw::areas::areas_value(&areas));
        if sw::gets(&s, "fill") == Some("empty") {
            s.set("fill", Value::Str("fill".into()));
        }
        if let Some(c) = sw::clean_shape(&s) {
            self.checkpoint();
            self.board.set(i, c);
            self.sel.clear();
            self.sel.insert(i);
        }
    }

    /// Area colour: right-click clears a shape's coloured areas.
    fn clear_areas(&mut self, r: Rect, at: Pos2) {
        if let Some(i) = self.shape_at(r, at)
            && self.board.get(i).has("areas")
        {
            let mut s = self.board.get(i).clone();
            if let Value::Obj(o) = &mut s {
                o.retain(|(k, _)| k != "areas");
            }
            if let Some(c) = sw::clean_shape(&s) {
                self.checkpoint();
                self.board.set(i, c);
            }
        }
    }

    /// Select: double-click on a selected curve adds an anchor there.
    pub(super) fn curve_add_anchor(&mut self, r: Rect, at: Pos2) -> bool {
        for &i in self.sel.iter().rev() {
            let sh = self.board.get(i);
            if sw::kind(sh) != "curve" || sw::joined::is_joined(sh) {
                continue;
            }
            let pts = sw::shape_pts(sh);
            let mut best: Option<(f32, usize, f64)> = None;
            for seg in 0..(pts.len() - 1) / 3 {
                let c = &pts[seg * 3..seg * 3 + 4];
                for k in 0..=64 {
                    let t = f64::from(k) / 64.0;
                    let q = sw::curves::seg_point(c[0], c[1], c[2], c[3], t);
                    let d = (self.view.pos(r, q) - at).length();
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, seg, t));
                    }
                }
            }
            let Some((d, seg, t)) = best else { continue };
            if d > PICK || t <= 0.0 || t >= 1.0 {
                continue;
            }
            let lerp = |a: P, b: P| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            let c = &pts[seg * 3..seg * 3 + 4];
            let (q0, q1, q2) = (lerp(c[0], c[1]), lerp(c[1], c[2]), lerp(c[2], c[3]));
            let (r0, r1) = (lerp(q0, q1), lerp(q1, q2));
            let m = lerp(r0, r1);
            let mut out: Vec<P> = pts[..=seg * 3].to_vec();
            out.extend([q0, r0, m, r1, q2]);
            out.extend_from_slice(&pts[seg * 3 + 3..]);
            let mut s = sh.clone();
            s.set("pts", sw::pts_value(&out));
            // anchors after the new one move up by one
            if let Some(sharp) = s.get("sharp").and_then(Value::as_arr) {
                let moved: Vec<Value> = sharp
                    .iter()
                    .filter_map(Value::as_f64)
                    .map(|a| Value::Num(if a as usize > seg { a + 1.0 } else { a }))
                    .collect();
                s.set("sharp", Value::Arr(moved));
            }
            if let Some(c) = sw::clean_shape(&s) {
                self.checkpoint();
                self.board.set(i, c);
                return true;
            }
        }
        false
    }

    /// Select + Alt-click on a curve's anchor: a sharp corner (or smooth again).
    pub(super) fn toggle_sharp(&mut self, shape: usize, idx: usize) {
        let sh = self.board.get(shape);
        if sw::kind(sh) != "curve" || !idx.is_multiple_of(3) {
            return;
        }
        let a = (idx / 3) as f64;
        let mut sharp: Vec<f64> = sh
            .get("sharp")
            .and_then(Value::as_arr)
            .map(|v| v.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if let Some(p) = sharp.iter().position(|x| *x == a) {
            sharp.remove(p);
        } else {
            sharp.push(a);
        }
        let mut s = sh.clone();
        s.set(
            "sharp",
            Value::Arr(sharp.into_iter().map(Value::Num).collect()),
        );
        if let Some(c) = sw::clean_shape(&s)
            && c != *self.board.get(shape)
        {
            self.checkpoint();
            self.board.set(shape, c);
        }
    }

    fn drag_to(&mut self, r: Rect, at: Pos2, shift: bool) {
        let q = self.board_pt(r, at, !shift);
        let free_q = self.board_pt(r, at, false);
        match &mut self.drag {
            Some(Drag::Box { to, .. }) => {
                *to = at;
                self.box_select(r);
            }
            Some(Drag::Move {
                start,
                orig,
                moved,
                press,
                ..
            }) => {
                if !*moved && (at - *press).length() < STILL {
                    return;
                }
                *moved = true;
                let (db, dp) = (q[0] - start[0], q[1] - start[1]);
                let orig = orig.clone();
                for (j, pts) in orig {
                    let moved: Vec<P> = pts.iter().map(|p| [p[0] + db, p[1] + dp]).collect();
                    self.board.set_pts(j, &moved);
                }
            }
            Some(Drag::Point { shape, idx, orig }) => {
                let (i, k) = (*shape, *idx);
                let mut pts = orig.clone();
                let d = [q[0] - pts[k][0], q[1] - pts[k][1]];
                pts[k] = q;
                // a curve's anchor takes its handles along
                if sw::kind(self.board.get(i)) == "curve" && k % 3 == 0 {
                    for h in [k.wrapping_sub(1), k + 1] {
                        if h < pts.len() && h != k {
                            pts[h] = [orig[h][0] + d[0], orig[h][1] + d[1]];
                        }
                    }
                }
                self.board.set_pts(i, &pts);
            }
            Some(Drag::Draw(_)) => match &mut self.draft {
                Some(Draft::Two { b, .. }) => *b = q,
                Some(Draft::Poly(pts)) => {
                    if let Some(l) = pts.last_mut() {
                        *l = q;
                    }
                }
                Some(Draft::Free(pts, last)) if (at - *last).length() >= 3.0 => {
                    pts.push(free_q);
                    *last = at;
                }
                Some(Draft::Arc { pts, .. }) => {
                    let k = pts.len() - 1;
                    pts[k] = q;
                }
                Some(Draft::Box { b, .. }) => *b = q,
                Some(Draft::Free(..)) | None => {}
            },
            _ => {}
        }
    }

    fn release(&mut self, r: Rect, at: Pos2, shift: bool, ctrl: bool) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Box { from, to, .. } => {
                if (to - from).length() < STILL && !ctrl {
                    self.sel.clear();
                }
            }
            Drag::Move { moved, click, .. } => {
                if !moved {
                    self.drop_checkpoint();
                    if let Some(i) = click
                        && self.sel.len() > 1
                    {
                        self.sel.clear();
                        self.sel.insert(i);
                    }
                }
            }
            Drag::Point { .. } => {}
            Drag::Draw(from) => {
                let still = (at - from).length() < STILL;
                let q = self.board_pt(r, at, !shift);
                match &mut self.draft {
                    Some(Draft::Two { follow, b, .. }) => {
                        if still {
                            *follow = true; // the end follows the mouse until the next click
                        } else {
                            *b = q;
                            self.commit_draft();
                        }
                    }
                    Some(Draft::Poly(pts)) => {
                        if !still && let Some(l) = pts.last_mut() {
                            // dragged: the point lands where it was let go, a new one follows
                            *l = q;
                            pts.push(q);
                        }
                    }
                    Some(Draft::Free(..)) => self.commit_draft(),
                    Some(Draft::Arc { pts, bend }) if !still && pts.len() == 2 => {
                        // dragged start → end: now it bends with the mouse until a click
                        let (a, b) = (pts[0], pts[1]);
                        *pts = vec![a, [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0], b];
                        *bend = true;
                    }
                    Some(Draft::Arc { .. }) => {}
                    Some(Draft::Box { b, .. }) => {
                        if still {
                            self.draft = None;
                        } else {
                            *b = q;
                            self.commit_draft();
                        }
                    }
                    None => {}
                }
            }
            Drag::Pan(_) => {}
        }
    }

    fn box_select(&mut self, r: Rect) {
        let Some(Drag::Box { from, to, before }) = &self.drag else {
            return;
        };
        let b = Rect::from_two_pos(*from, *to);
        let before = before.clone();
        let mut sel = before;
        if b.width() >= STILL || b.height() >= STILL {
            for i in 0..self.board.len() {
                let strokes = self.board.strokes(i);
                let touches = strokes.iter().any(|s| {
                    s.iter().any(|q| b.contains(self.view.pos(r, *q)))
                        || s.windows(2).any(|w| {
                            segment_hits_rect(self.view.pos(r, w[0]), self.view.pos(r, w[1]), b)
                        })
                });
                if touches {
                    sel.insert(i);
                }
            }
        }
        self.sel = sel;
    }

    /// The finished draft becomes a shape (cleaned like a file's).
    pub(super) fn commit_draft(&mut self) {
        let Some(d) = self.draft.take() else {
            return;
        };
        let k = self.view.px_key / self.view.px_beat; // beats per key on screen
        if let Draft::Box { tool, a, b } = d {
            if (a[0] - b[0]).abs() < 1e-9 || (a[1] - b[1]).abs() < 1e-9 {
                return;
            }
            let base = self.new_shape_defaults();
            let made = match tool {
                Tool::Circle => edit::circle_shape(a, b, &base),
                Tool::Polygon => edit::polygon_shape(a, b, &self.new_polygon, &base),
                _ => edit::hz_shape(
                    a,
                    b,
                    self.new_hz.0,
                    self.new_hz.1,
                    self.project_bpm.unwrap_or(120.0),
                    &base,
                ),
            };
            if let Some(sh) = made {
                self.checkpoint();
                let i = self.board.push(sh);
                self.sel.clear();
                self.sel.insert(i);
            }
            return;
        }
        let (kind, pts): (&str, Vec<P>) = match d {
            Draft::Two { kind, a, b, .. } => {
                if a == b {
                    return;
                }
                if kind == "curve" {
                    let mid = (a[0] + b[0]) / 2.0;
                    ("curve", vec![a, [mid, a[1]], [mid, b[1]], b])
                } else if kind == "funnel" {
                    ("funnel", vec![a, b])
                } else {
                    ("line", vec![a, b])
                }
            }
            Draft::Poly(pts) => ("poly", pts),
            Draft::Free(pts, _) => {
                if pts.len() < 2 {
                    return;
                }
                ("free", pts)
            }
            Draft::Arc { pts, .. } => ("arc", pts),
            Draft::Box { .. } => return,
        };
        let mut sh = self.new_shape_defaults();
        sh.set("kind", Value::Str(kind.into()));
        sh.set("pts", sw::pts_value(&pts));
        if kind == "arc" {
            sh.set("k", Value::Num(k));
        }
        if kind == "free" {
            sh.set("smooth", Value::Num(f64::from(self.free_smooth)));
            sh.set("k", Value::Num(k));
        }
        if let Some(clean) = sw::clean_shape(&sh) {
            self.checkpoint();
            let i = self.board.push(clean);
            self.sel.clear();
            self.sel.insert(i);
        }
    }

    pub(super) fn finish_poly(&mut self) {
        let Some(Draft::Poly(pts)) = self.draft.take() else {
            return;
        };
        let mut out: Vec<P> = Vec::new();
        for p in &pts[..pts.len().saturating_sub(1)] {
            if out.last() != Some(p) {
                out.push(*p);
            }
        }
        if out.len() >= 2 {
            self.draft = Some(Draft::Poly(out));
            self.commit_draft();
        }
    }

    fn finish_arc(&mut self) {
        if let Some(Draft::Arc { pts, .. }) = &mut self.draft {
            if pts[0] == pts[1] && pts[1] == pts[2] {
                pts.pop(); // still waiting for the end
                return;
            }
            self.commit_draft();
        }
    }

    // ------------------------------------------------------------ painting

    fn paint_grid(&self, painter: &egui::Painter, full: Rect, r: Rect, dark: bool) {
        let bg = if dark {
            Color32::from_gray(24)
        } else {
            Color32::from_gray(246)
        };
        let black_row = if dark {
            Color32::from_gray(18)
        } else {
            Color32::from_gray(232)
        };
        let line = if dark {
            Color32::from_gray(48)
        } else {
            Color32::from_gray(214)
        };
        let bar = if dark {
            Color32::from_gray(78)
        } else {
            Color32::from_gray(170)
        };
        let text = if dark {
            Color32::from_gray(170)
        } else {
            Color32::from_gray(70)
        };
        painter.rect_filled(full, 0.0, bg);
        // key rows
        let p_hi = self.view.y2p(r, r.top()).ceil().min(self.view.top_key) as i64;
        let p_lo = self.view.y2p(r, r.bottom()).floor().max(0.0) as i64;
        for p in p_lo..=p_hi {
            let y0 = self.view.p2y(r, p as f64 + 0.5).max(r.top());
            let y1 = self.view.p2y(r, p as f64 - 0.5).min(r.bottom());
            if y1 <= y0 {
                continue;
            }
            let black = matches!(p % 12, 1 | 3 | 6 | 8 | 10);
            if black {
                painter.rect_filled(
                    Rect::from_x_y_ranges(r.left()..=r.right(), y0..=y1),
                    0.0,
                    black_row,
                );
            }
            if p % 12 == 0 {
                painter.hline(r.left()..=r.right(), y1, Stroke::new(1.0, line));
                if self.view.px_key >= 7.0 || p % 24 == 0 {
                    painter.text(
                        pos2(full.left() + 2.0, (y0 + y1) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        format!("C{}", p / 12 - 1),
                        egui::FontId::proportional(10.0),
                        text,
                    );
                }
            }
        }
        // beat lines: every beat, or bars / groups of bars when zoomed out
        let mut step = 1.0;
        while step * self.view.px_beat < 10.0 {
            step *= 4.0;
        }
        let b0 = (self.view.x2b(r, r.left()) / step).floor() * step;
        let b1 = self.view.x2b(r, r.right());
        let mut b = b0.max(0.0);
        while b <= b1 {
            let x = self.view.b2x(r, b);
            let is_bar = (b / 4.0).fract().abs() < 1e-9;
            painter.vline(
                x,
                r.top()..=r.bottom(),
                Stroke::new(1.0, if is_bar { bar } else { line }),
            );
            if is_bar && (b / (4.0 * step.max(4.0) / 4.0)).fract().abs() < 1e-9 {
                painter.text(
                    pos2(x + 2.0, full.top() + RULER_H / 2.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{}", (b / 4.0) as i64 + 1),
                    egui::FontId::proportional(10.0),
                    text,
                );
            }
            b += step;
        }
        painter.hline(full.left()..=full.right(), r.top(), Stroke::new(1.0, bar));
        painter.vline(r.left(), full.top()..=full.bottom(), Stroke::new(1.0, bar));
    }

    /// B2-F26: the shared transport's playhead (a thin line, the roll's
    /// playhead colour, a marker on the ruler; the position while seeking).
    fn paint_playhead(&self, ui: &Ui, painter: &egui::Painter, full: Rect, r: Rect) {
        let Some(b) = self.playhead else {
            return;
        };
        let x = self.view.b2x(r, b);
        if !(r.left()..=r.right()).contains(&x) {
            return;
        }
        let col = crate::ui::theme::palette(ui.ctx().theme()).playhead;
        painter.vline(x, full.top()..=r.bottom(), Stroke::new(1.0, col));
        let t = r.top();
        painter.add(egui::Shape::convex_polygon(
            vec![pos2(x - 5.0, t - 7.0), pos2(x + 5.0, t - 7.0), pos2(x, t)],
            col,
            Stroke::NONE,
        ));
        let hovering = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| Self::ruler_rect(r).contains(p));
        if (self.scrub.is_some() || hovering) && !self.playhead_label.is_empty() {
            let font = egui::FontId::proportional(11.0);
            let text_col = ui.visuals().strong_text_color();
            let galley = painter.layout_no_wrap(self.playhead_label.clone(), font, text_col);
            let mut at = pos2(x + 6.0, full.top() + 1.0);
            if at.x + galley.size().x > r.right() {
                at.x = x - 6.0 - galley.size().x;
            }
            let bg = Rect::from_min_size(at, galley.size()).expand(2.0);
            painter.rect_filled(bg, 0.0, ui.visuals().extreme_bg_color);
            painter.rect_stroke(bg, 0.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
            painter.galley(at, galley, text_col);
        }
    }

    fn paint_notes(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        r: Rect,
        res: &Rendered,
        ppq: u32,
    ) {
        let ppq = f64::from(ppq.max(1));
        let t_lo = (self.view.x2b(r, r.left()) * ppq).floor() as i64;
        let t_hi = (self.view.x2b(r, r.right()) * ppq).ceil() as i64;
        let first = res.notes.partition_point(|n| n[0] < t_lo - res.max_dur);
        let last = res.notes.partition_point(|n| n[0] <= t_hi);
        let px_tick = self.view.px_beat / ppq;
        let dense = last.saturating_sub(first) > MAX_RECTS
            || res
                .density
                .as_ref()
                .is_some_and(|d| px_tick * d.res as f64 <= 1.0);
        if dense && let Some(d) = &res.density {
            if self.density_tex.as_ref().map(|t| t.0) != Some(res.id) {
                let tex = ctx.load_texture(
                    "plumspirit-spiderweb-density",
                    d.image.clone(),
                    egui::TextureOptions::NEAREST,
                );
                self.density_tex = Some((res.id, tex));
            }
            if let Some((_, tex)) = &self.density_tex {
                let span = (d.w as i64 * d.res) as f64;
                let x0 = self.view.b2x(r, d.t0 as f64 / ppq);
                let x1 = self.view.b2x(r, (d.t0 as f64 + span) / ppq);
                let y0 = self.view.p2y(r, self.view.top_key + 0.5);
                let y1 = self.view.p2y(r, -0.5);
                painter.image(
                    tex.id(),
                    Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::from_white_alpha(170),
                );
            }
            return;
        }
        for n in &res.notes[first..last] {
            if n[1] < t_lo {
                continue;
            }
            let x0 = self.view.b2x(r, n[0] as f64 / ppq);
            let x1 = self.view.b2x(r, n[1] as f64 / ppq).max(x0 + 1.0);
            let y0 = self.view.p2y(r, n[2] as f64 + 0.5);
            let y1 = self.view.p2y(r, n[2] as f64 - 0.5);
            let c = SLOT_COLORS[(n[4].max(0) as usize) % SLOT_COLORS.len()];
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)),
                0.0,
                Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 150),
            );
        }
    }

    fn paint_shapes(&mut self, painter: &egui::Painter, r: Rect, dark: bool) {
        let normal = if dark {
            Color32::from_rgb(230, 230, 240)
        } else {
            Color32::from_rgb(40, 40, 60)
        };
        let selected = Color32::from_rgb(255, 150, 40);
        for i in 0..self.board.len() {
            let sel = self.sel.contains(&i);
            let strokes = self.board.strokes(i);
            let col = if sel { selected } else { normal };
            for s in strokes.iter() {
                let pts: Vec<Pos2> = s.iter().map(|q| self.view.pos(r, *q)).collect();
                if pts.len() == 1 {
                    painter.circle_filled(pts[0], 2.0, col);
                } else {
                    painter.add(egui::Shape::line(pts, Stroke::new(1.5, col)));
                }
            }
            if sel {
                self.paint_handles(painter, r, i, selected);
            }
        }
        // the draft
        let green = Color32::from_rgb(10, 170, 10);
        if let Some(Draft::Box { tool, a, b }) = &self.draft {
            let (lo, hi) = (
                pos2(
                    self.view.b2x(r, a[0].min(b[0])),
                    self.view.p2y(r, a[1].max(b[1])),
                ),
                pos2(
                    self.view.b2x(r, a[0].max(b[0])),
                    self.view.p2y(r, a[1].min(b[1])),
                ),
            );
            let rect = Rect::from_min_max(lo, hi);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, green), egui::StrokeKind::Inside);
            if *tool == Tool::Circle {
                painter.add(egui::Shape::ellipse_stroke(
                    rect.center(),
                    rect.size() / 2.0,
                    Stroke::new(2.0, green),
                ));
            }
        }
        let funnel_draft = match &self.draft {
            Some(Draft::Two {
                kind: "funnel",
                a,
                b,
                ..
            }) if a != b => {
                let mut f = Value::obj();
                f.set("kind", Value::Str("funnel".into()));
                f.set("pts", sw::pts_value(&[*a, *b]));
                sw::clean_shape(&f).map(|f| sw::shape_strokes(&f))
            }
            _ => None,
        };
        if let Some(strokes) = funnel_draft {
            for s in strokes {
                let pts: Vec<Pos2> = s.iter().map(|q| self.view.pos(r, *q)).collect();
                painter.add(egui::Shape::line(pts, Stroke::new(2.0, green)));
            }
            return;
        }
        let draft_pts: Option<Vec<P>> = match &self.draft {
            Some(Draft::Two { kind, a, b, .. }) => Some(if *kind == "curve" {
                let mid = (a[0] + b[0]) / 2.0;
                sw::curves::sample(&[*a, [mid, a[1]], [mid, b[1]], *b], 48)
            } else {
                vec![*a, *b]
            }),
            Some(Draft::Poly(p)) | Some(Draft::Free(p, _)) => Some(p.clone()),
            Some(Draft::Arc { pts, .. }) => Some(if pts.len() == 3 {
                sw::curves::arc_points(pts, self.view.px_key / self.view.px_beat)
            } else {
                pts.clone()
            }),
            Some(Draft::Box { .. }) | None => None,
        };
        if let Some(p) = draft_pts {
            let pts: Vec<Pos2> = p.iter().map(|q| self.view.pos(r, *q)).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(2.0, green)));
        }
    }

    fn paint_handles(&self, painter: &egui::Painter, r: Rect, i: usize, col: Color32) {
        let sh = self.board.get(i);
        let pts = sw::shape_pts(sh);
        if sw::kind(sh) == "custom" {
            if pts.len() == 3 {
                let (o, u, v) = (pts[0], pts[1], pts[2]);
                let w = [u[0] + v[0] - o[0], u[1] + v[1] - o[1]];
                let poly: Vec<Pos2> = [o, u, w, v, o]
                    .iter()
                    .map(|q| self.view.pos(r, *q))
                    .collect();
                painter.add(egui::Shape::dashed_line(
                    &poly,
                    Stroke::new(1.0, col),
                    4.0,
                    3.0,
                ));
                // its frame points: drag to resize, turn or skew the box
                for q in [o, u, v] {
                    painter.rect_filled(
                        Rect::from_center_size(self.view.pos(r, q), vec2(7.0, 7.0)),
                        0.0,
                        col,
                    );
                }
            }
            return;
        }
        if !point_kinds(sh) {
            return;
        }
        let curve = sw::kind(sh) == "curve";
        if curve {
            for (k, q) in pts.iter().enumerate() {
                if k % 3 != 0 {
                    let a = if k % 3 == 1 { pts[k - 1] } else { pts[k + 1] };
                    painter.line_segment(
                        [self.view.pos(r, a), self.view.pos(r, *q)],
                        Stroke::new(1.0, Color32::from_rgb(70, 130, 255)),
                    );
                }
            }
        }
        for (k, q) in pts.iter().enumerate() {
            let p = self.view.pos(r, *q);
            if curve && k % 3 != 0 {
                painter.circle_filled(p, 3.5, Color32::from_rgb(70, 130, 255));
            } else {
                painter.rect_filled(Rect::from_center_size(p, vec2(7.0, 7.0)), 0.0, col);
            }
        }
    }
}

/// Shapes whose points are dragged one by one.
fn point_kinds(sh: &Value) -> bool {
    matches!(
        sw::kind(sh),
        "line" | "poly" | "curve" | "arc" | "funnel" | "custom"
    )
}

fn segment_hits_rect(a: Pos2, b: Pos2, r: Rect) -> bool {
    if r.contains(a) || r.contains(b) {
        return true;
    }
    let corners = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
    ];
    (0..4).any(|i| segments_cross(a, b, corners[i], corners[(i + 1) % 4]))
}

fn segments_cross(a: Pos2, b: Pos2, c: Pos2, d: Pos2) -> bool {
    let cross = |o: Pos2, p: Pos2, q: Pos2| (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
    let (d1, d2) = (cross(c, d, a), cross(c, d, b));
    let (d3, d4) = (cross(a, b, c), cross(a, b, d));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

impl Board {
    /// Moves a shape's points (formulas, tumours and settings stay).
    pub fn set_pts(&mut self, i: usize, pts: &[P]) {
        let mut sh = self.get(i).clone();
        sh.set("pts", sw::pts_value(pts));
        self.set(i, sh);
    }
}
