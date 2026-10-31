//! Event Graphs under the piano roll: two independent graph slots that share the
//! roll's horizontal axis.
//!
//! Each slot shows one target:
//!
//! * **Velocity** — the note-on velocities of the active track's notes (bars tied
//!   to the notes themselves, see `velocity.rs`);
//! * **Pitch Bend** and **Control Change 0–127** (named ones such as Modulation,
//!   Volume, Panpot, Hold, LPF Cutoff / Resonance, or any number as Generic CC) —
//!   the track's real channel events on the slot's MIDI channel;
//! * **Master Volume** and **BPM** — the Conductor's global Master Volume and
//!   tempo map.
//!
//! Point targets hold a value until the next point (BPM points can ramp). With
//! the Edit tool: click empty space to add a point, drag a point to move it
//! (dragging a selected point moves the whole selection), Ctrl+click toggles a
//! point in the selection, Shift+drag on empty space box-selects, double-click
//! or right-click opens a numeric editor. The curve tools (Free-Hand, Line,
//! Gentle, Fast, S Curve, Random, Custom) draw from the press to the release;
//! with Anchors on, the two ends stay adjustable until Enter applies the curve.
//! Every gesture is one undo step, applied by the app.
//!
//! Only the *active* graph (highlighted header) receives drawing, selection,
//! deletion and curve generation; clicking a graph activates it. Values come from
//! indexed, cached data (controller points are collected once per events
//! revision; Conductor points are read by binary search) and dense views are
//! aggregated per pixel column — never one widget per event.

use super::velocity::{LaneInput, VelocityLane};
use super::{KEYS_WIDTH, RollAction, SCROLLBAR};
use crate::ui::theme::{Palette, size};
use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use plumspirit_engine::editor::controllers::{
    self, ControllerKind, ControllerPoint, collect_points, graph_controller, graph_short_label,
};
use plumspirit_engine::editor::curves::Shape as CurveShape;
use plumspirit_engine::model::conductor::{
    Interp, MASTER_VOLUME_MAX, format_master_volume, format_tempo,
};
use plumspirit_engine::model::timing::snap_nearest;
use plumspirit_engine::model::{Bpm, Conductor, GraphTarget, Track};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::grid::{GridLine, LineKind, MeterMap};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Pick tolerance around a point, in points.
const PICK_PX: f32 = 6.0;
/// Visible points above which a graph draws a per-pixel outline without handles.
const EXACT_POINTS: usize = 4_000;
const CLICK_SLOP: f32 = 3.0;
/// Free-hand pointer samples kept per stroke (they are thinned to the interval
/// grid anyway).
const MAX_STROKE_SAMPLES: usize = 100_000;

// ------------------------------------------------------------------ tools

/// What a press-and-drag does in the active graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphTool {
    /// Add, move, select and delete single events (v0.3 behaviour).
    Edit,
    FreeHand,
    Curve(CurveShape),
}

impl GraphTool {
    pub const ALL: [GraphTool; 8] = [
        GraphTool::Edit,
        GraphTool::FreeHand,
        GraphTool::Curve(CurveShape::Line),
        GraphTool::Curve(CurveShape::Gentle),
        GraphTool::Curve(CurveShape::Fast),
        GraphTool::Curve(CurveShape::SCurve),
        GraphTool::Curve(CurveShape::Random),
        GraphTool::Curve(CurveShape::Custom),
    ];

    pub fn label(self) -> &'static str {
        match self {
            GraphTool::Edit => "Edit",
            GraphTool::FreeHand => "Free",
            GraphTool::Curve(CurveShape::Line) => "Line",
            GraphTool::Curve(CurveShape::Gentle) => "Gentle",
            GraphTool::Curve(CurveShape::Fast) => "Fast",
            GraphTool::Curve(CurveShape::SCurve) => "S",
            GraphTool::Curve(CurveShape::Random) => "Random",
            GraphTool::Curve(CurveShape::Custom) => "f(t)",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            GraphTool::Edit => {
                "Edit: click empty space to add an event, drag an event to move it (a selected one moves the whole selection), Ctrl+click to select several, Shift+drag to box-select, double-click to type a value."
            }
            GraphTool::FreeHand => {
                "Free-Hand: drag to draw; the stroke becomes one event per interval step (Curve menu: Interval), gaps between pointer samples are filled in."
            }
            GraphTool::Curve(CurveShape::Line) => {
                "Line: drag from the start value to the end value; events at every interval step change linearly."
            }
            GraphTool::Curve(CurveShape::Gentle) => {
                "Gentle Curve: quadratic ease, 1 − (1 − u)²; the Curve menu's Bend chooses which end moves quickly."
            }
            GraphTool::Curve(CurveShape::Fast) => {
                "Fast Curve: exponential, (1 − e^(−5u)) / (1 − e^(−5)); most of the change happens at one end (Curve menu: Bend)."
            }
            GraphTool::Curve(CurveShape::SCurve) => {
                "S Curve: smoothstep 3u² − 2u³ — slow start, fast middle, slow end."
            }
            GraphTool::Curve(CurveShape::Random) => {
                "Random: seeded values between the two ends of the drag (the lower and higher value), one per interval step. The Curve menu sets the seed."
            }
            GraphTool::Curve(CurveShape::Custom) => {
                "Custom Function: the formula chosen in the Curve menu (Custom Functions…), in t from 0 to 1, drawn between the two ends of the drag."
            }
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            GraphTool::Edit => "edit",
            GraphTool::FreeHand => "freehand",
            GraphTool::Curve(CurveShape::Line) => "line",
            GraphTool::Curve(CurveShape::Gentle) => "gentle",
            GraphTool::Curve(CurveShape::Fast) => "fast",
            GraphTool::Curve(CurveShape::SCurve) => "s",
            GraphTool::Curve(CurveShape::Random) => "random",
            GraphTool::Curve(CurveShape::Custom) => "custom",
        }
    }

    pub fn from_key(k: &str) -> Option<GraphTool> {
        GraphTool::ALL.into_iter().find(|t| t.key() == k)
    }

    pub fn shape(self) -> Option<CurveShape> {
        match self {
            GraphTool::Curve(s) => Some(s),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ value axis

/// The vertical axis of a graph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    Linear {
        lo: f64,
        hi: f64,
    },
    /// Logarithmic (BPM).
    Log {
        lo: f64,
        hi: f64,
    },
}

impl Scale {
    pub fn y(self, v: f64, plot: Rect) -> f32 {
        let t = match self {
            Scale::Linear { lo, hi } => (v - lo) / (hi - lo).max(f64::MIN_POSITIVE),
            Scale::Log { lo, hi } => {
                (v.max(f64::MIN_POSITIVE).ln() - lo.ln()) / (hi.ln() - lo.ln())
            }
        };
        plot.bottom() - t.clamp(-0.05, 1.05) as f32 * plot.height()
    }

    /// The (unrounded) value at `y`, inside the axis.
    pub fn value(self, y: f32, plot: Rect) -> f64 {
        let t = f64::from(((plot.bottom() - y) / plot.height()).clamp(0.0, 1.0));
        match self {
            Scale::Linear { lo, hi } => lo + t * (hi - lo),
            Scale::Log { lo, hi } => (lo.ln() + t * (hi.ln() - lo.ln())).exp(),
        }
    }
}

/// Integer value range of a target (Velocity 1–127, CC 0–127, Pitch Bend and
/// Master Volume 0–16383). BPM has no integer range (`None`).
pub fn target_limits(t: GraphTarget) -> Option<(i64, i64)> {
    match t {
        GraphTarget::Velocity => Some((1, 127)),
        GraphTarget::PitchBend | GraphTarget::MasterVolume => Some((0, 16_383)),
        GraphTarget::Cc(_) => Some((0, 127)),
        GraphTarget::Bpm => None,
    }
}

/// The scale of a target (`bpm` is the BPM graph's range).
pub fn scale_of(t: GraphTarget, bpm: (f64, f64)) -> Scale {
    match target_limits(t) {
        Some((lo, hi)) => Scale::Linear {
            lo: lo as f64,
            hi: hi as f64,
        },
        None => Scale::Log {
            lo: bpm.0,
            hi: bpm.1,
        },
    }
}

/// Value text for readouts.
pub fn value_text(t: GraphTarget, v: f64) -> String {
    match t {
        GraphTarget::Velocity => format!("Velocity {}", v.round()),
        GraphTarget::MasterVolume => format!(
            "Master Volume {}",
            format_master_volume(v.round().clamp(0.0, 16_383.0) as u16)
        ),
        GraphTarget::Bpm => format!("{} BPM", fmt4(v)),
        other => match graph_controller(other) {
            Some(k) => format!(
                "{} {}",
                k.label(),
                k.display(v.round().clamp(0.0, f64::from(k.max())) as u16)
            ),
            None => fmt4(v),
        },
    }
}

// ------------------------------------------------------------------ actions

/// Graph edits for the app to apply (through the undo history).
#[derive(Clone, Debug, PartialEq)]
pub enum GraphAction {
    /// Add (target `None`) or change / move a controller event.
    SetController {
        kind: ControllerKind,
        channel: u8,
        target: Option<usize>,
        tick: u64,
        value: u16,
    },
    /// Move several controller events together.
    MoveControllers {
        kind: ControllerKind,
        channel: u8,
        indices: Vec<usize>,
        dtick: i64,
        dvalue: i32,
    },
    DeleteControllers(Vec<usize>),
    /// Open the numeric editor for an event (`index`) or a new one at `tick`.
    EditController {
        kind: ControllerKind,
        channel: u8,
        index: Option<usize>,
        tick: u64,
    },
    SetTempo {
        target: Option<u32>,
        tick: u64,
        bpm: Bpm,
        interp: Interp,
    },
    SetInterp {
        id: u32,
        interp: Interp,
    },
    DeleteTempo(Vec<u32>),
    EditTempo {
        id: Option<u32>,
        tick: u64,
    },
    SetMaster {
        target: Option<u32>,
        tick: u64,
        value: u16,
    },
    DeleteMaster(Vec<u32>),
    EditMaster {
        id: Option<u32>,
        tick: u64,
    },
    /// A curve drawn from `from` to `to` (tick, value in target units).
    Curve {
        from: (u64, f64),
        to: (u64, f64),
    },
    /// A free-hand stroke (tick, value) in drawing order.
    FreeHand {
        samples: Vec<(u64, f64)>,
    },
}

// ------------------------------------------------------------------ points

/// A point of a point graph for this frame.
#[derive(Clone, Copy, Debug)]
struct GraphPoint {
    /// Event index (controllers) or point id (BPM, Master Volume).
    key: u64,
    tick: u64,
    y: f32,
    /// Cannot move in time (the tick-0 tempo).
    pinned: bool,
}

#[derive(Clone, Copy, Debug)]
struct PointGesture {
    /// `None`: adding a new point.
    key: Option<u64>,
    press: Pos2,
    grab_tick: u64,
    orig_tick: u64,
    orig_y: f32,
    tick: u64,
    y: f32,
    moved: bool,
    pinned: bool,
    /// Moving the whole selection.
    group: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Context {
    Point(u64),
    Empty(u64),
}

enum PointEvent {
    Add {
        tick: u64,
        y: f32,
    },
    Move {
        key: u64,
        tick: u64,
        y: f32,
    },
    MoveGroup {
        dtick: i64,
        from_y: f32,
        to_y: f32,
    },
    Edit(u64),
    /// Box selection in tick / y coordinates.
    Select {
        t0: u64,
        t1: u64,
        y0: f32,
        y1: f32,
        add: bool,
    },
}

/// Shared interaction of point graphs.
#[derive(Default)]
struct PointLane {
    gesture: Option<PointGesture>,
    boxing: Option<(Pos2, Pos2)>,
    selected: BTreeSet<u64>,
    hover: Option<u64>,
    context: Option<Context>,
    /// What `selected` / `context` keys refer to; a different one clears them.
    revision: Option<(u64, u64)>,
}

impl PointLane {
    fn reset_if_stale(&mut self, revision: (u64, u64)) {
        if self.revision != Some(revision) {
            if self.gesture.is_none() {
                self.selected.clear();
                self.context = None;
            }
            self.revision = Some(revision);
        }
    }

    fn pick(points: &[GraphPoint], time: &TimeAxis, plot: Rect, pos: Pos2) -> Option<GraphPoint> {
        let mut best: Option<(f32, GraphPoint)> = None;
        for p in points {
            let x = plot.left() + time.tick_to_x(p.tick) as f32;
            let d = (x - pos.x).abs().max((p.y - pos.y).abs() * 0.5);
            if (x - pos.x).abs() <= PICK_PX
                && (p.y - pos.y).abs() <= PICK_PX * 2.0
                && best.is_none_or(|b| d < b.0)
            {
                best = Some((d, *p));
            }
        }
        best.map(|b| b.1)
    }

    #[allow(clippy::too_many_arguments)] // one call site per graph kind
    fn interact(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        plot: Rect,
        time: &TimeAxis,
        points: &[GraphPoint],
        snap: Option<u64>,
        editable: bool,
        can_group: bool,
    ) -> Vec<PointEvent> {
        let mut out = Vec::new();
        let (pointer, pressed, released, down, escape, secondary, mods) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.key_pressed(egui::Key::Escape),
                i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.modifiers,
            )
        });
        let tick_at = |x: f32| time.x_to_tick(f64::from(x - plot.left()));
        let snapped = |t: u64| snap.map_or(t, |u| snap_nearest(t, u));
        self.hover = None;
        if escape || secondary {
            self.gesture = None;
            self.boxing = None;
        }
        if secondary
            && resp.contains_pointer()
            && let Some(pos) = pointer
        {
            self.context = Some(match Self::pick(points, time, plot, pos) {
                Some(p) => {
                    if !self.selected.contains(&p.key) {
                        self.selected.clear();
                        self.selected.insert(p.key);
                    }
                    Context::Point(p.key)
                }
                None => Context::Empty(snapped(tick_at(pos.x))),
            });
        }
        if resp.double_clicked()
            && editable
            && let Some(pos) = pointer
            && let Some(p) = Self::pick(points, time, plot, pos)
        {
            self.gesture = None;
            out.push(PointEvent::Edit(p.key));
            return out;
        }
        if pressed
            && editable
            && resp.contains_pointer()
            && let Some(pos) = pointer
        {
            let hit = Self::pick(points, time, plot, pos);
            match hit {
                Some(p) if mods.command => {
                    // Toggle in the selection; no drag.
                    if !self.selected.remove(&p.key) {
                        self.selected.insert(p.key);
                    }
                }
                None if mods.shift => self.boxing = Some((pos, pos)),
                _ => {
                    let group = hit.is_some_and(|p| {
                        can_group && self.selected.contains(&p.key) && self.selected.len() > 1
                    });
                    if !group {
                        self.selected.clear();
                        if let Some(p) = hit {
                            self.selected.insert(p.key);
                        }
                    }
                    let t = hit.map_or_else(|| snapped(tick_at(pos.x)), |p| p.tick);
                    let y = hit.map_or(pos.y, |p| p.y);
                    self.gesture = Some(PointGesture {
                        key: hit.map(|p| p.key),
                        press: pos,
                        grab_tick: tick_at(pos.x),
                        orig_tick: t,
                        orig_y: y,
                        tick: t,
                        y,
                        moved: false,
                        pinned: hit.is_some_and(|p| p.pinned),
                        group,
                    });
                }
            }
        }
        if let (Some(b), Some(pos)) = (self.boxing.as_mut(), pointer) {
            b.1 = pos;
        }
        if let Some((a, b)) = self.boxing
            && (released || !down)
        {
            self.boxing = None;
            let r = Rect::from_two_pos(a, b);
            out.push(PointEvent::Select {
                t0: tick_at(r.left()),
                t1: tick_at(r.right()),
                y0: r.top(),
                y1: r.bottom(),
                add: mods.shift,
            });
        }
        if let (Some(g), Some(pos)) = (self.gesture.as_mut(), pointer) {
            if (pos - g.press).length() >= CLICK_SLOP {
                g.moved = true;
            }
            if g.moved || g.key.is_none() {
                g.y = pos.y.clamp(plot.top(), plot.bottom());
                if !g.pinned && g.key.is_some() {
                    let d = i128::from(tick_at(pos.x)) - i128::from(g.grab_tick);
                    let t = (i128::from(g.orig_tick) + d).clamp(0, i128::from(u64::MAX)) as u64;
                    g.tick = snapped(t);
                }
            }
            ui.ctx().set_cursor_icon(if g.key.is_some() && !g.pinned {
                CursorIcon::Grabbing
            } else {
                CursorIcon::ResizeVertical
            });
        }
        if let Some(g) = self.gesture
            && (released || !down)
        {
            self.gesture = None;
            match g.key {
                None => out.push(PointEvent::Add {
                    tick: g.tick,
                    y: g.y,
                }),
                Some(_) if g.moved && g.group => out.push(PointEvent::MoveGroup {
                    dtick: (i128::from(g.tick) - i128::from(g.orig_tick))
                        .clamp(i128::from(i64::MIN), i128::from(i64::MAX))
                        as i64,
                    from_y: g.orig_y,
                    to_y: g.y,
                }),
                Some(key) if g.moved => out.push(PointEvent::Move {
                    key,
                    tick: g.tick,
                    y: g.y,
                }),
                Some(_) => {}
            }
        }
        if self.gesture.is_none()
            && resp.contains_pointer()
            && let Some(pos) = pointer
        {
            self.hover = Self::pick(points, time, plot, pos).map(|p| p.key);
            if editable {
                ui.ctx().set_cursor_icon(if self.hover.is_some() {
                    CursorIcon::Grab
                } else {
                    CursorIcon::Crosshair
                });
            }
        }
        out
    }

    /// Where to draw point `p` this frame (dragged points at their preview).
    fn shown(&self, p: &GraphPoint) -> (u64, f32) {
        match self.gesture {
            Some(g) if g.moved && g.group && self.selected.contains(&p.key) => {
                let dt = i128::from(g.tick) - i128::from(g.orig_tick);
                let t = (i128::from(p.tick) + dt).clamp(0, i128::from(u64::MAX)) as u64;
                (t, p.y + (g.y - g.orig_y))
            }
            Some(g) if g.key == Some(p.key) && g.moved => (g.tick, g.y),
            _ => (p.tick, p.y),
        }
    }

    fn draw_box(&self, p: &egui::Painter, pal: &Palette) {
        if let Some((a, b)) = self.boxing {
            let r = Rect::from_two_pos(a, b);
            p.rect_filled(r, 0.0, pal.accent.gamma_multiply(0.12));
            p.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, pal.accent),
                egui::StrokeKind::Inside,
            );
        }
    }
}

/// Draws a point handle: squares hold their value, diamonds ramp to the next.
fn handle(mesh: &mut Mesh, c: Pos2, color: Color32, ring: Option<Color32>, diamond: bool) {
    let r = 3.5;
    let add = |mesh: &mut Mesh, r: f32, col: Color32| {
        if diamond {
            let i = mesh.vertices.len() as u32;
            for p in [
                c + Vec2::new(0.0, -r),
                c + Vec2::new(r, 0.0),
                c + Vec2::new(0.0, r),
                c + Vec2::new(-r, 0.0),
            ] {
                mesh.colored_vertex(p, col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        } else {
            mesh.add_colored_rect(Rect::from_center_size(c, Vec2::splat(r * 2.0)), col);
        }
    };
    if let Some(ring) = ring {
        add(mesh, r + 1.5, ring);
    }
    add(mesh, r, color);
}

fn readout(p: &egui::Painter, plot: Rect, x: f32, text: String, pal: &Palette) {
    let small = FontId::proportional(size::SMALL);
    let galley = p.layout_no_wrap(text, small, pal.text);
    let pos = Pos2::new(
        (x + 8.0)
            .min(plot.right() - galley.size().x - 6.0)
            .max(plot.left() + 2.0),
        plot.top() + 2.0,
    );
    let bg = Rect::from_min_size(pos, galley.size()).expand(3.0);
    p.rect_filled(bg, 0.0, pal.overlay_bg);
    p.galley(pos, galley, pal.text);
}

fn scale_label(p: &egui::Painter, plot: Rect, y: f32, text: &str, pal: &Palette) {
    p.text(
        Pos2::new(plot.left() + 3.0, y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(size::SMALL),
        pal.text_weak.gamma_multiply(0.8),
    );
}

/// A step line through `pts` (x, y), holding each value until the next point.
fn step_path(pts: &[(f32, f32)], right: f32) -> Vec<Pos2> {
    let mut path: Vec<Pos2> = Vec::with_capacity(pts.len() * 2 + 1);
    for (i, &(x, y)) in pts.iter().enumerate() {
        if i > 0 {
            path.push(Pos2::new(x, pts[i - 1].1));
        }
        path.push(Pos2::new(x, y));
    }
    if let Some(&(_, y)) = pts.last() {
        path.push(Pos2::new(right + 2.0, y));
    }
    path
}

// ------------------------------------------------------------------ controllers

/// (note host key — `render::host_key` —, events revision, kind, channel).
type ControllerKey = (u64, u64, ControllerKind, u8);

/// Controller points per (track, events revision, kind, channel), shared
/// cheaply between frames.
#[derive(Default)]
struct PointCache {
    entries: Vec<(ControllerKey, Arc<[ControllerPoint]>)>,
}

impl PointCache {
    fn points(
        &mut self,
        track: plumspirit_engine::editor::HostView<'_>,
        kind: ControllerKind,
        channel: u8,
    ) -> Arc<[ControllerPoint]> {
        let key = (
            super::render::host_key(track.host()),
            track.events_revision(),
            kind,
            channel,
        );
        if let Some((_, p)) = self.entries.iter().find(|(k, _)| *k == key) {
            return Arc::clone(p);
        }
        // Drop stale revisions of this track / kind / channel.
        self.entries
            .retain(|(k, _)| !(k.0 == key.0 && k.2 == key.2 && k.3 == key.3));
        let p: Arc<[ControllerPoint]> = collect_points(track.events(), kind, channel).into();
        self.entries.push((key, Arc::clone(&p)));
        if self.entries.len() > 24 {
            self.entries.remove(0);
        }
        p
    }
}

// ------------------------------------------------------------------ BPM

fn bpm_range(c: &Conductor) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = 0.0f64;
    for p in c.tempo_points() {
        let b = p.bpm_f64();
        if b.is_finite() && b > 0.0 {
            lo = lo.min(b);
            hi = hi.max(b);
        }
    }
    if !(lo.is_finite() && hi > 0.0) {
        return (60.0, 240.0);
    }
    if hi / lo < 1.5 {
        let mid = (hi * lo).sqrt();
        (mid / 2.0, mid * 2.0)
    } else {
        (lo / 1.25, hi * 1.25)
    }
}

/// BPM rounded to four significant digits (clean values when dragging).
pub fn clean_bpm(b: f64) -> Option<Bpm> {
    if !b.is_finite() || b <= 0.0 {
        return None;
    }
    // B1-0101: within what a MIDI file can store (3.58 keeps four digits
    // above the slowest, 3.5762788 BPM).
    let b = b.clamp(3.58, 60_000_000.0);
    let digits = 4 - 1 - b.log10().floor() as i32;
    let text = if digits > 0 {
        format!("{:.*}", digits.min(12) as usize, b)
    } else {
        let scale = 10f64.powi(-digits);
        format!("{}", (b / scale).round() * scale)
    };
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    };
    Bpm::parse(&text).ok()
}

// ------------------------------------------------------------------ curves

/// A curve being drawn (or its anchors being adjusted).
#[derive(Clone, Debug, PartialEq)]
struct CurveDrag {
    from: (u64, f64),
    to: (u64, f64),
    samples: Vec<(u64, f64)>,
    last_px: Pos2,
    /// Adjusting an anchor (0 = start, 1 = end) in Anchor mode.
    moving: Option<usize>,
    /// Released in Anchor mode: waiting for Enter.
    placed: bool,
}

/// What the app previews for a curve (the events it would generate).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CurvePreview {
    /// (tick, value) of the events, when there are few enough to draw.
    pub points: Vec<(u64, f64)>,
    /// The shape as (u in 0…1, value) when the events are too many to list.
    pub shape: Vec<(f64, f64)>,
    /// A short explanation (event count, an error).
    pub note: Option<String>,
    /// The curve cannot be applied (the note explains why).
    pub error: bool,
}

/// Computes the preview for a curve on `target` from `from` to `to`.
pub type Previewer<'a> = &'a dyn Fn(GraphTarget, (u64, f64), (u64, f64)) -> CurvePreview;

// ------------------------------------------------------------------ host

/// Inputs for one frame of one graph slot.
pub struct HostInput<'a> {
    pub slot: usize,
    pub target: GraphTarget,
    /// MIDI channel (0–15) of channel targets.
    pub channel: u8,
    /// This graph receives drawing, selection, deletion and curves.
    pub active: bool,
    pub tool: GraphTool,
    pub anchors: bool,
    /// The notes and events of the active regular track — an opened
    /// clip's owned content on it (V09-S11) — `None` while the Conductor is
    /// selected.
    pub track: Option<plumspirit_engine::editor::HostView<'a>>,
    pub conductor: &'a Conductor,
    pub time: TimeAxis,
    pub meter: &'a MeterMap,
    pub palette: &'a Palette,
    pub dark: bool,
    pub editable: bool,
    pub playhead: Option<u64>,
    pub snap: Option<u64>,
    /// Onion Skin sources: (track, its color) drawn as ghosts.
    pub ghosts: &'a [(&'a Track, Color32)],
    pub preview: Previewer<'a>,
    /// A note shown instead of the graph (e.g. why it is empty).
    pub note: Option<&'a str>,
}

#[derive(Default)]
pub struct HostOutput {
    pub actions: Vec<RollAction>,
    pub graph_actions: Vec<GraphAction>,
    /// The graph was clicked (make it active).
    pub activate: bool,
    pub target: Option<GraphTarget>,
    pub channel: Option<u8>,
    /// Hide this graph.
    pub hide: bool,
    pub aggregated: bool,
}

/// One graph slot's interaction state.
#[derive(Default)]
pub struct GraphView {
    pub velocity: VelocityLane,
    controller: PointLane,
    bpm: PointLane,
    master: PointLane,
    /// Tempo revision → BPM axis range (log scale).
    bpm_range: Option<(u64, (f64, f64))>,
    visible: Vec<GraphPoint>,
    curve: Option<CurveDrag>,
    grid: Vec<GridLine>,
}

impl GraphView {
    pub fn cancel(&mut self) {
        self.velocity.cancel();
        self.controller.gesture = None;
        self.controller.boxing = None;
        self.bpm.gesture = None;
        self.bpm.boxing = None;
        self.master.gesture = None;
        self.master.boxing = None;
        self.curve = None;
    }

    pub fn is_dragging(&self) -> bool {
        self.velocity.is_dragging()
            || self.controller.gesture.is_some()
            || self.bpm.gesture.is_some()
            || self.master.gesture.is_some()
            || self.curve.as_ref().is_some_and(|c| !c.placed)
    }

    /// Anchors placed in Anchor mode, waiting for Apply.
    pub fn pending_anchors(&self) -> Option<((u64, f64), (u64, f64))> {
        self.curve
            .as_ref()
            .filter(|c| c.placed)
            .map(|c| (c.from, c.to))
    }

    pub fn clear_anchors(&mut self) {
        self.curve = None;
    }

    pub fn selected_controllers(&self) -> Vec<usize> {
        self.controller
            .selected
            .iter()
            .map(|&k| k as usize)
            .collect()
    }

    pub fn selected_tempo(&self) -> Vec<u32> {
        self.bpm.selected.iter().map(|&k| k as u32).collect()
    }

    pub fn selected_master(&self) -> Vec<u32> {
        self.master.selected.iter().map(|&k| k as u32).collect()
    }

    pub fn clear_point_selection(&mut self) {
        self.controller.selected.clear();
        self.bpm.selected.clear();
        self.master.selected.clear();
    }

    /// Selects controller events / points by key (Select Range, event list).
    pub fn select_controllers(&mut self, keys: impl IntoIterator<Item = usize>, add: bool) {
        if !add {
            self.controller.selected.clear();
        }
        self.controller
            .selected
            .extend(keys.into_iter().map(|k| k as u64));
    }

    pub fn select_tempo(&mut self, ids: impl IntoIterator<Item = u32>, add: bool) {
        if !add {
            self.bpm.selected.clear();
        }
        self.bpm.selected.extend(ids.into_iter().map(u64::from));
    }

    pub fn select_master(&mut self, ids: impl IntoIterator<Item = u32>, add: bool) {
        if !add {
            self.master.selected.clear();
        }
        self.master.selected.extend(ids.into_iter().map(u64::from));
    }

    /// Makes the selection refer to the current events revision (after the app
    /// selected by key for a revision it just read).
    /// `host`: the note host's cache key (`render::host_key`).
    pub fn bind_controller_revision(&mut self, host: u64, events_revision: u64, channel: u8) {
        self.controller.revision = Some((events_revision, (host << 8) | u64::from(channel)));
    }

    pub fn has_point_selection(&self, target: GraphTarget) -> bool {
        match target {
            GraphTarget::Velocity => false,
            GraphTarget::Bpm => !self.bpm.selected.is_empty(),
            GraphTarget::MasterVolume => !self.master.selected.is_empty(),
            _ => !self.controller.selected.is_empty(),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        full: Rect,
        input: HostInput<'_>,
        cache: &mut GraphCache,
    ) -> HostOutput {
        let mut out = HostOutput::default();
        let pal = input.palette;
        let header =
            Rect::from_min_max(full.min, Pos2::new(full.left() + KEYS_WIDTH, full.bottom()));
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, pal.roll_bg);
        painter.rect_filled(header, 0.0, pal.ruler_bg);
        painter.hline(
            full.x_range(),
            full.top() + 0.5,
            Stroke::new(1.0, pal.panel_border),
        );
        painter.vline(
            header.right() - 0.5,
            full.y_range(),
            Stroke::new(1.0, pal.panel_border),
        );
        if input.active {
            // Active graph: accent bar at the header's left edge.
            painter.rect_filled(
                Rect::from_min_size(
                    header.min + Vec2::new(0.0, 3.0),
                    Vec2::new(3.0, header.height() - 6.0),
                ),
                0.0,
                pal.accent,
            );
        }
        self.header(ui, header, &input, &mut out);
        let plot_outer = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, full.top()),
            Pos2::new(full.right() - SCROLLBAR, full.bottom()),
        );
        if ui.input(|i| i.pointer.any_pressed())
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| full.contains(p))
        {
            out.activate = true;
        }
        if let Some(note) = input.note {
            let p = ui.painter_at(plot_outer);
            p.text(
                plot_outer.center(),
                Align2::CENTER_CENTER,
                note,
                FontId::proportional(size::SMALL),
                pal.text_weak,
            );
            return out;
        }
        // Only the active graph edits; the other one is a view until clicked.
        let editable = input.editable && input.active;
        let curve_tool = input.tool != GraphTool::Edit;
        let drawable = editable && curve_tool && target_drawable(&input);
        match input.target {
            GraphTarget::Velocity => {
                if let Some(t) = input.track {
                    self.ghost_velocity(ui, plot_outer, &input);
                    let o = ui
                        .push_id(("graph-velocity", input.slot), |ui| {
                            self.velocity.show(
                                ui,
                                full,
                                LaneInput {
                                    store: t.notes,
                                    key: super::render::host_key(t.host()),
                                    time: input.time,
                                    meter: input.meter,
                                    palette: pal,
                                    dark: input.dark,
                                    editable: editable && !curve_tool,
                                    playhead: input.playhead,
                                },
                            )
                        })
                        .inner;
                    out.actions.extend(o.actions);
                    out.aggregated = o.aggregated;
                }
            }
            GraphTarget::Bpm => self.show_bpm(ui, full, &input, editable && !curve_tool, &mut out),
            GraphTarget::MasterVolume => {
                self.show_master(ui, full, &input, editable && !curve_tool, &mut out)
            }
            other => {
                if let (Some(t), Some(kind)) = (input.track, graph_controller(other)) {
                    self.show_controller(
                        ui,
                        full,
                        t,
                        kind,
                        &input,
                        editable && !curve_tool,
                        cache,
                        &mut out,
                    );
                }
            }
        }
        if curve_tool {
            self.curve_gesture(ui, plot_outer, &input, drawable, &mut out);
        } else {
            self.curve = None;
        }
        out
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        header: Rect,
        input: &HostInput<'_>,
        out: &mut HostOutput,
    ) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header.shrink2(Vec2::new(4.0, 3.0)))
                .layout(egui::Layout::top_down(egui::Align::Min))
                .id_salt(("graph-header", input.slot)),
        );
        let ui = &mut child;
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(3.0, 1.0);
        let small = |t: &str| egui::RichText::new(t).size(size::SMALL);
        let slot_name = format!("Graph {}", input.slot + 1);
        ui.menu_button(small(&graph_short_label(input.target)), |ui| {
            ui.label(small(&slot_name).strong());
            for t in controllers::NAMED_GRAPH_TARGETS {
                if ui
                    .radio(input.target == t, controllers::graph_label(t))
                    .clicked()
                {
                    out.target = Some(t);
                    ui.close();
                }
            }
            ui.menu_button("Generic CC", |ui| {
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for n in 0..=127u8 {
                            let t = GraphTarget::Cc(n);
                            let k = ControllerKind::Cc(n);
                            let mut text = format!("CC {n} · {}", k.label());
                            if !controllers::internal_synth_responds(k) {
                                text.push_str(" *");
                            }
                            if ui.radio(input.target == t, text).clicked() {
                                out.target = Some(t);
                                ui.close();
                            }
                        }
                    });
                ui.label(small("* not used by the built-in synthesizer (saved, exported and sent to external outputs)").weak());
            });
            if input.slot == 1 {
                ui.separator();
                if ui.button("Hide Graph 2").clicked() {
                    out.hide = true;
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(format!(
            "{slot_name}: {}. Click to choose what it shows.",
            controllers::graph_label(input.target)
        ));
        if graph_controller(input.target).is_some() {
            ui.menu_button(small(&format!("Ch {}", input.channel + 1)), |ui| {
                egui::Grid::new(("graph-channels", input.slot))
                    .num_columns(4)
                    .show(ui, |ui| {
                        for c in 0..16u8 {
                            if ui
                                .selectable_label(input.channel == c, format!("{}", c + 1))
                                .clicked()
                            {
                                out.channel = Some(c);
                                ui.close();
                            }
                            if c % 4 == 3 {
                                ui.end_row();
                            }
                        }
                    });
            })
            .response
            .on_hover_text("MIDI channel shown and edited (a track can use several channels)");
        }
        let badge = if input.active {
            egui::RichText::new(format!("{} ●", input.slot + 1))
                .size(size::SMALL)
                .color(input.palette.accent)
        } else {
            egui::RichText::new(format!("{}", input.slot + 1))
                .size(size::SMALL)
                .color(input.palette.text_weak)
        };
        ui.label(badge).on_hover_text(if input.active {
            "The active graph: drawing, selection, Delete and curves apply here."
        } else {
            "Click the graph to make it active."
        });
    }

    fn grid(
        &mut self,
        painter: &egui::Painter,
        plot: Rect,
        plot_outer: Rect,
        input: &HostInput<'_>,
    ) {
        let (t0, t1) = input.time.visible_range(f64::from(plot.width()));
        input.meter.lines(
            t0,
            t1,
            input.time.pixels_per_tick,
            7.0,
            None,
            &mut self.grid,
        );
        for l in self.grid.iter().filter(|l| l.kind == LineKind::Major) {
            let x = (plot.left() + input.time.tick_to_x(l.tick) as f32).round() + 0.5;
            painter.vline(
                x,
                plot_outer.y_range(),
                Stroke::new(1.0, input.palette.grid_minor),
            );
        }
    }

    /// Ghost velocity stems of the Onion Skin tracks (only when few are visible).
    fn ghost_velocity(&self, ui: &egui::Ui, plot_outer: Rect, input: &HostInput<'_>) {
        if input.ghosts.is_empty() {
            return;
        }
        let plot = plot_outer.shrink2(Vec2::new(0.0, 6.0));
        let (t0, t1) = input.time.visible_range(f64::from(plot.width()));
        // L-G05: counted only until the limit (15,000 ghosts are not all
        // asked when the first few already exceed it).
        let mut total = 0usize;
        for (t, _) in input.ghosts {
            total += t.notes.estimate_in_range(t0, t1);
            if total > super::velocity::EXACT_BARS {
                return;
            }
        }
        let p = ui.painter_at(plot_outer);
        let mut mesh = Mesh::default();
        let scale = Scale::Linear { lo: 1.0, hi: 127.0 };
        for (t, color) in input.ghosts {
            t.notes.query(t0, t1, |_, n| {
                if n.start_tick < t0 {
                    return;
                }
                let x = plot.left() + input.time.tick_to_x(n.start_tick) as f32;
                let y = scale.y(f64::from(n.attrs.velocity()), plot);
                mesh.add_colored_rect(
                    Rect::from_min_max(Pos2::new(x - 0.5, y), Pos2::new(x + 0.5, plot.bottom())),
                    *color,
                );
            });
        }
        p.add(Shape::mesh(mesh));
    }

    #[allow(clippy::too_many_arguments)] // one call site
    fn show_controller(
        &mut self,
        ui: &mut egui::Ui,
        full: Rect,
        track: plumspirit_engine::editor::HostView<'_>,
        kind: ControllerKind,
        input: &HostInput<'_>,
        editable: bool,
        cache: &mut GraphCache,
        out: &mut HostOutput,
    ) {
        let pal = input.palette;
        let plot_outer = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, full.top()),
            Pos2::new(full.right() - SCROLLBAR, full.bottom()),
        );
        let plot = plot_outer.shrink2(Vec2::new(0.0, 6.0));
        let resp = ui.interact(
            plot_outer,
            ui.id().with(("graph-controller", input.slot)),
            Sense::click_and_drag(),
        );
        let channel = input.channel;
        self.controller.reset_if_stale((
            track.events_revision(),
            (super::render::host_key(track.host()) << 8) | u64::from(channel),
        ));
        let scale = Scale::Linear {
            lo: 0.0,
            hi: f64::from(kind.max()),
        };
        let vy = |v: u16| scale.y(f64::from(v), plot);
        let yv = |y: f32| {
            scale
                .value(y, plot)
                .round()
                .clamp(0.0, f64::from(kind.max())) as u16
        };
        let time = input.time;
        let (t0, t1) = time.visible_range(f64::from(plot.width()));
        let all = cache.points.points(track, kind, channel);
        let first = all.partition_point(|p| p.tick < t0);
        let last = all.partition_point(|p| p.tick <= t1);
        let lo = first.saturating_sub(1);
        let visible_count = last - first;
        self.visible.clear();
        if visible_count <= EXACT_POINTS {
            self.visible
                .extend(all[lo..last.min(all.len())].iter().map(|p| GraphPoint {
                    key: p.index as u64,
                    tick: p.tick,
                    y: vy(p.value),
                    pinned: false,
                }));
        }
        let events = self.controller.interact(
            ui,
            &resp,
            plot,
            &time,
            &self.visible,
            input.snap,
            editable,
            true,
        );
        for e in events {
            match e {
                PointEvent::Add { tick, y } => out.graph_actions.push(GraphAction::SetController {
                    kind,
                    channel,
                    target: None,
                    tick,
                    value: if kind.is_channel_mode() {
                        kind.command_value()
                    } else {
                        yv(y)
                    },
                }),
                PointEvent::Move { key, tick, y } => {
                    out.graph_actions.push(GraphAction::SetController {
                        kind,
                        channel,
                        target: Some(key as usize),
                        tick,
                        value: yv(y),
                    })
                }
                PointEvent::MoveGroup {
                    dtick,
                    from_y,
                    to_y,
                } => out.graph_actions.push(GraphAction::MoveControllers {
                    kind,
                    channel,
                    indices: self.selected_controllers(),
                    dtick,
                    dvalue: i32::from(yv(to_y)) - i32::from(yv(from_y)),
                }),
                PointEvent::Edit(key) => {
                    let tick = all
                        .iter()
                        .find(|p| p.index as u64 == key)
                        .map_or(0, |p| p.tick);
                    out.graph_actions.push(GraphAction::EditController {
                        kind,
                        channel,
                        index: Some(key as usize),
                        tick,
                    });
                }
                PointEvent::Select {
                    t0,
                    t1,
                    y0,
                    y1,
                    add,
                } => {
                    let (vlo, vhi) = (yv(y1), yv(y0));
                    if !add {
                        self.controller.selected.clear();
                    }
                    let a = all.partition_point(|p| p.tick < t0);
                    let b = all.partition_point(|p| p.tick <= t1);
                    for p in &all[a..b] {
                        if p.value >= vlo && p.value <= vhi {
                            self.controller.selected.insert(p.index as u64);
                        }
                    }
                }
            }
        }

        // ------------------------------------------------------------- paint
        let p = ui.painter_at(plot_outer);
        self.grid(&p, plot, plot_outer, input);
        let center_y = vy(kind.center());
        p.hline(
            plot.x_range(),
            center_y.round() + 0.5,
            Stroke::new(1.0, pal.grid_major),
        );
        for v in [0, kind.max()] {
            p.hline(
                plot.x_range(),
                vy(v).round() + 0.5,
                Stroke::new(1.0, pal.grid_sub),
            );
        }
        let (top, mid, bottom) = match kind {
            ControllerKind::PitchBend => ("+8191", "0", "-8192"),
            k if k.is_bipolar() => ("127 R", "64", "0 L"),
            k if k.is_switch() => ("On", "", "Off"),
            _ => ("127", "", "0"),
        };
        scale_label(&p, plot, plot.top() + 5.0, top, pal);
        if !mid.is_empty() {
            scale_label(&p, plot, center_y - 6.0, mid, pal);
        }
        scale_label(&p, plot, plot.bottom() - 5.0, bottom, pal);
        // Onion Skin ghosts: the same controller on the same channel of the
        // source tracks (drawn under the active track's line).
        for (gt, color) in input.ghosts {
            let gp = cache.points.points((*gt).into(), kind, channel);
            let a = gp.partition_point(|q| q.tick < t0).saturating_sub(1);
            let b = gp.partition_point(|q| q.tick <= t1);
            if b.saturating_sub(a) > EXACT_POINTS || a >= b {
                continue;
            }
            let pts: Vec<(f32, f32)> = gp[a..b]
                .iter()
                .map(|q| {
                    (
                        (plot.left() + time.tick_to_x(q.tick) as f32).max(plot.left() - 2.0),
                        vy(q.value),
                    )
                })
                .collect();
            let path = step_path(&pts, plot.right());
            if path.len() >= 2 {
                p.add(Shape::Path(PathShape::line(
                    path,
                    PathStroke::new(1.0, *color),
                )));
            }
        }
        let line_color = crate::ui::theme::velocity_palette(
            &crate::ui::theme::note_palette(ui.ctx()),
            input.dark,
        )
        .fill[100];
        if visible_count > EXACT_POINTS {
            // Dense: one vertical span per pixel column (min–max of the values).
            let ppp = ui.ctx().pixels_per_point();
            let cols = ((plot.width() * ppp) as usize).clamp(1, 8192);
            let mut span: Vec<(u16, u16)> = vec![(u16::MAX, 0); cols];
            let ppt = time.pixels_per_tick * f64::from(ppp);
            for pt in &all[first..last] {
                let c = (((pt.tick - time.origin_tick) as f64 * ppt) as usize).min(cols - 1);
                span[c] = (span[c].0.min(pt.value), span[c].1.max(pt.value));
            }
            let mut mesh = Mesh::default();
            for (c, &(a, b)) in span.iter().enumerate() {
                if a <= b {
                    let x = plot.left() + c as f32 / ppp;
                    mesh.add_colored_rect(
                        Rect::from_min_max(
                            Pos2::new(x, vy(b) - 1.0),
                            Pos2::new(x + 1.0 / ppp, vy(a) + 1.0),
                        ),
                        line_color,
                    );
                }
            }
            p.add(Shape::mesh(mesh));
            out.aggregated = true;
        } else {
            // Before the first event the channel's default applies (dashed).
            let first_x = self.visible.first().map_or(plot.right(), |q| {
                plot.left() + time.tick_to_x(self.controller.shown(q).0) as f32
            });
            if all.first().is_none_or(|q| q.tick > t0) {
                p.add(Shape::dashed_line(
                    &[
                        Pos2::new(plot.left(), center_y),
                        Pos2::new(first_x.min(plot.right()), center_y),
                    ],
                    Stroke::new(1.0, pal.text_weak),
                    4.0,
                    3.0,
                ));
            }
            let pts: Vec<(f32, f32, u64)> = self
                .visible
                .iter()
                .map(|q| {
                    let (t, y) = self.controller.shown(q);
                    (plot.left() + time.tick_to_x(t) as f32, y, q.key)
                })
                .collect();
            let mut sorted: Vec<(f32, f32)> = pts
                .iter()
                .map(|&(x, y, _)| (x.max(plot.left() - 2.0), y))
                .collect();
            sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
            let path = step_path(&sorted, plot.right());
            if path.len() >= 2 {
                p.add(Shape::Path(PathShape::line(
                    path,
                    PathStroke::new(1.5, line_color),
                )));
            }
            let mut mesh = Mesh::default();
            for &(x, y, key) in &pts {
                if x < plot.left() - 4.0 {
                    continue;
                }
                let sel = self.controller.selected.contains(&key);
                let hot = self.controller.hover == Some(key);
                let color = if sel {
                    pal.note_selected_fill
                } else {
                    line_color
                };
                let ring = (sel || hot).then_some(if sel {
                    pal.note_selected_border
                } else {
                    pal.note_hover_border
                });
                handle(&mut mesh, Pos2::new(x, y), color, ring, false);
            }
            if let Some(g) = self.controller.gesture.filter(|g| g.key.is_none()) {
                let x = plot.left() + time.tick_to_x(g.tick) as f32;
                handle(
                    &mut mesh,
                    Pos2::new(x, g.y),
                    pal.note_selected_fill,
                    Some(pal.note_selected_border),
                    false,
                );
            }
            p.add(Shape::mesh(mesh));
        }
        self.controller.draw_box(&p, pal);
        if let Some(t) = input.playhead {
            super::render::draw_playhead(&p, plot_outer, plot.left(), &time, pal, t);
        }
        // Readout while dragging / hovering.
        let text_of = |v: u16| format!("{} {} · Ch {}", kind.label(), kind.display(v), channel + 1);
        if let Some(g) = self.controller.gesture {
            let x = plot.left() + time.tick_to_x(g.tick) as f32;
            readout(
                &p,
                plot,
                x,
                format!("{} · {}", text_of(yv(g.y)), input.meter.describe(g.tick)),
                pal,
            );
        } else if let Some(h) = self.controller.hover
            && let Some(pt) = all.iter().find(|q| q.index as u64 == h)
        {
            let x = plot.left() + time.tick_to_x(pt.tick) as f32;
            readout(
                &p,
                plot,
                x,
                format!("{} · {}", text_of(pt.value), input.meter.describe(pt.tick)),
                pal,
            );
        }
        // Context menu.
        let ctx = self.controller.context;
        let selected = self.selected_controllers();
        resp.context_menu(|ui| match ctx {
            Some(Context::Point(key)) => {
                if ui
                    .add_enabled(editable, egui::Button::new("Edit Value…"))
                    .clicked()
                {
                    let tick = all
                        .iter()
                        .find(|q| q.index as u64 == key)
                        .map_or(0, |q| q.tick);
                    out.graph_actions.push(GraphAction::EditController {
                        kind,
                        channel,
                        index: Some(key as usize),
                        tick,
                    });
                    ui.close();
                }
                let label = if selected.len() > 1 {
                    format!("Delete {} Selected Events", selected.len())
                } else {
                    format!("Delete {} Event", kind.label())
                };
                if ui.add_enabled(editable, egui::Button::new(label)).clicked() {
                    out.graph_actions.push(GraphAction::DeleteControllers(
                        if selected.is_empty() {
                            vec![key as usize]
                        } else {
                            selected.clone()
                        },
                    ));
                    ui.close();
                }
            }
            Some(Context::Empty(tick)) => {
                if ui
                    .add_enabled(
                        editable,
                        egui::Button::new(format!("Add {} Event…", kind.label())),
                    )
                    .clicked()
                {
                    out.graph_actions.push(GraphAction::EditController {
                        kind,
                        channel,
                        index: None,
                        tick,
                    });
                    ui.close();
                }
                if ui
                    .add_enabled(!all.is_empty(), egui::Button::new("Select All in Graph"))
                    .clicked()
                {
                    self.controller
                        .selected
                        .extend(all.iter().map(|q| q.index as u64));
                    ui.close();
                }
            }
            None => ui.close(),
        });
    }

    fn show_master(
        &mut self,
        ui: &mut egui::Ui,
        full: Rect,
        input: &HostInput<'_>,
        editable: bool,
        out: &mut HostOutput,
    ) {
        let pal = input.palette;
        let c = input.conductor;
        let plot_outer = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, full.top()),
            Pos2::new(full.right() - SCROLLBAR, full.bottom()),
        );
        let plot = plot_outer.shrink2(Vec2::new(0.0, 6.0));
        let resp = ui.interact(
            plot_outer,
            ui.id().with(("graph-master", input.slot)),
            Sense::click_and_drag(),
        );
        let editable = editable && c.master_editable();
        self.master.reset_if_stale((c.master_revision(), 1));
        let scale = Scale::Linear {
            lo: 0.0,
            hi: f64::from(MASTER_VOLUME_MAX),
        };
        let vy = |v: u16| scale.y(f64::from(v), plot);
        let yv = |y: f32| {
            scale
                .value(y, plot)
                .round()
                .clamp(0.0, f64::from(MASTER_VOLUME_MAX)) as u16
        };
        let time = input.time;
        let (t0, t1) = time.visible_range(f64::from(plot.width()));
        let pts = c.master_volume();
        let first = pts.partition_point(|p| p.tick < t0);
        let last = pts.partition_point(|p| p.tick <= t1);
        let lo = first.saturating_sub(1);
        let dense = last - first > EXACT_POINTS;
        self.visible.clear();
        if !dense {
            self.visible
                .extend(pts[lo..last.min(pts.len())].iter().map(|p| GraphPoint {
                    key: u64::from(p.id),
                    tick: p.tick,
                    y: vy(p.value),
                    pinned: false,
                }));
        }
        let events = self.master.interact(
            ui,
            &resp,
            plot,
            &time,
            &self.visible,
            input.snap,
            editable,
            false,
        );
        for e in events {
            match e {
                PointEvent::Add { tick, y } => out.graph_actions.push(GraphAction::SetMaster {
                    target: None,
                    tick,
                    value: yv(y),
                }),
                PointEvent::Move { key, tick, y } => {
                    out.graph_actions.push(GraphAction::SetMaster {
                        target: Some(key as u32),
                        tick,
                        value: yv(y),
                    })
                }
                PointEvent::MoveGroup { .. } => {}
                PointEvent::Edit(key) => {
                    let tick = c.master_by_id(key as u32).map_or(0, |(_, p)| p.tick);
                    out.graph_actions.push(GraphAction::EditMaster {
                        id: Some(key as u32),
                        tick,
                    });
                }
                PointEvent::Select {
                    t0,
                    t1,
                    y0,
                    y1,
                    add,
                } => {
                    let (vlo, vhi) = (yv(y1), yv(y0));
                    if !add {
                        self.master.selected.clear();
                    }
                    for p in pts.iter().filter(|p| p.tick >= t0 && p.tick <= t1) {
                        if p.value >= vlo && p.value <= vhi {
                            self.master.selected.insert(u64::from(p.id));
                        }
                    }
                }
            }
        }
        // ------------------------------------------------------------- paint
        let p = ui.painter_at(plot_outer);
        self.grid(&p, plot, plot_outer, input);
        for v in [0, MASTER_VOLUME_MAX / 2, MASTER_VOLUME_MAX] {
            p.hline(
                plot.x_range(),
                vy(v).round() + 0.5,
                Stroke::new(1.0, pal.grid_sub),
            );
        }
        scale_label(&p, plot, plot.top() + 5.0, "100%", pal);
        scale_label(&p, plot, vy(MASTER_VOLUME_MAX / 2) - 6.0, "50%", pal);
        scale_label(&p, plot, plot.bottom() - 5.0, "0%", pal);
        let line_color = pal.accent;
        let shown: Vec<(f32, f32, u64)> = self
            .visible
            .iter()
            .map(|q| {
                let (t, y) = self.master.shown(q);
                (plot.left() + time.tick_to_x(t) as f32, y, q.key)
            })
            .collect();
        // Full level before the first point (dashed).
        let full_y = vy(MASTER_VOLUME_MAX);
        let first_x = shown.iter().map(|s| s.0).fold(plot.right(), f32::min);
        if pts.first().is_none_or(|q| q.tick > t0) {
            p.add(Shape::dashed_line(
                &[
                    Pos2::new(plot.left(), full_y),
                    Pos2::new(first_x.min(plot.right()), full_y),
                ],
                Stroke::new(1.0, pal.text_weak),
                4.0,
                3.0,
            ));
        }
        if dense {
            let ppp = ui.ctx().pixels_per_point();
            let cols = ((plot.width() * ppp) as usize).clamp(1, 8192);
            let path: Vec<Pos2> = (0..cols)
                .map(|col| {
                    let x = col as f32 / ppp;
                    let t = time.x_to_tick(f64::from(x));
                    Pos2::new(plot.left() + x, vy(c.master_at(t)))
                })
                .collect();
            p.add(Shape::Path(PathShape::line(
                path,
                PathStroke::new(1.5, line_color),
            )));
            out.aggregated = true;
        } else {
            let mut sorted: Vec<(f32, f32)> = shown
                .iter()
                .map(|&(x, y, _)| (x.max(plot.left() - 2.0), y))
                .collect();
            sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
            let path = step_path(&sorted, plot.right());
            if path.len() >= 2 {
                p.add(Shape::Path(PathShape::line(
                    path,
                    PathStroke::new(1.5, line_color),
                )));
            }
            let mut mesh = Mesh::default();
            for &(x, y, key) in &shown {
                if x < plot.left() - 4.0 {
                    continue;
                }
                let sel = self.master.selected.contains(&key);
                let hot = self.master.hover == Some(key);
                let color = if sel {
                    pal.note_selected_fill
                } else {
                    line_color
                };
                let ring = (sel || hot).then_some(if sel {
                    pal.note_selected_border
                } else {
                    pal.note_hover_border
                });
                handle(&mut mesh, Pos2::new(x, y), color, ring, false);
            }
            if let Some(g) = self.master.gesture.filter(|g| g.key.is_none()) {
                let x = plot.left() + time.tick_to_x(g.tick) as f32;
                handle(
                    &mut mesh,
                    Pos2::new(x, g.y),
                    pal.note_selected_fill,
                    Some(pal.note_selected_border),
                    false,
                );
            }
            p.add(Shape::mesh(mesh));
        }
        self.master.draw_box(&p, pal);
        if let Some(t) = input.playhead {
            super::render::draw_playhead(&p, plot_outer, plot.left(), &time, pal, t);
        }
        if !c.master_editable() {
            p.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "Master Volume cannot be edited in a Format 2 file (its sequences are independent).",
                FontId::proportional(size::SMALL),
                pal.text_weak,
            );
        }
        let text_of = |v: u16| format!("Master Volume {}", format_master_volume(v));
        if let Some(g) = self.master.gesture {
            let x = plot.left() + time.tick_to_x(g.tick) as f32;
            readout(
                &p,
                plot,
                x,
                format!("{} · {}", text_of(yv(g.y)), input.meter.describe(g.tick)),
                pal,
            );
        } else if let Some(h) = self.master.hover
            && let Some((_, pt)) = c.master_by_id(h as u32)
        {
            let x = plot.left() + time.tick_to_x(pt.tick) as f32;
            readout(
                &p,
                plot,
                x,
                format!("{} · {}", text_of(pt.value), input.meter.describe(pt.tick)),
                pal,
            );
        }
        let ctx = self.master.context;
        let selected = self.selected_master();
        resp.context_menu(|ui| match ctx {
            Some(Context::Point(key)) => {
                if ui
                    .add_enabled(editable, egui::Button::new("Edit Master Volume…"))
                    .clicked()
                {
                    let tick = c.master_by_id(key as u32).map_or(0, |(_, p)| p.tick);
                    out.graph_actions.push(GraphAction::EditMaster {
                        id: Some(key as u32),
                        tick,
                    });
                    ui.close();
                }
                let n = selected.len().max(1);
                if ui
                    .add_enabled(
                        editable,
                        egui::Button::new(if n > 1 {
                            format!("Delete {n} Selected Points")
                        } else {
                            "Delete Master Volume Point".to_owned()
                        }),
                    )
                    .clicked()
                {
                    out.graph_actions
                        .push(GraphAction::DeleteMaster(if selected.is_empty() {
                            vec![key as u32]
                        } else {
                            selected.clone()
                        }));
                    ui.close();
                }
            }
            Some(Context::Empty(tick)) => {
                if ui
                    .add_enabled(editable, egui::Button::new("Add Master Volume Point…"))
                    .clicked()
                {
                    out.graph_actions
                        .push(GraphAction::EditMaster { id: None, tick });
                    ui.close();
                }
            }
            None => ui.close(),
        });
    }

    fn bpm_scale_range(&mut self, c: &Conductor) -> (f64, f64) {
        let rev = c.tempo_revision();
        // The scale follows the tempo map, but never changes under a drag.
        if self.bpm_range.is_none_or(|(r, _)| r != rev)
            && self.bpm.gesture.is_none()
            && self.curve.is_none()
        {
            self.bpm_range = Some((rev, bpm_range(c)));
        }
        self.bpm_range.map_or((60.0, 240.0), |r| r.1)
    }

    fn show_bpm(
        &mut self,
        ui: &mut egui::Ui,
        full: Rect,
        input: &HostInput<'_>,
        editable: bool,
        out: &mut HostOutput,
    ) {
        let pal = input.palette;
        let c = input.conductor;
        let plot_outer = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, full.top()),
            Pos2::new(full.right() - SCROLLBAR, full.bottom()),
        );
        let plot = plot_outer.shrink2(Vec2::new(0.0, 8.0));
        let resp = ui.interact(
            plot_outer,
            ui.id().with(("graph-bpm", input.slot)),
            Sense::click_and_drag(),
        );
        let editable = editable && c.timing_editable();
        self.bpm.reset_if_stale((c.tempo_revision(), 0));
        let range = self.bpm_scale_range(c);
        let scale = Scale::Log {
            lo: range.0,
            hi: range.1,
        };
        let by = |b: f64| scale.y(b, plot);
        let yb = |y: f32| clean_bpm(scale.value(y, plot));
        let time = input.time;
        let (t0, t1) = time.visible_range(f64::from(plot.width()));
        let pts = c.tempo_points();
        let first = pts.partition_point(|p| p.tick < t0);
        let last = pts.partition_point(|p| p.tick <= t1);
        let lo = first.saturating_sub(1);
        let hi = (last + 1).min(pts.len());
        let dense = last - first > EXACT_POINTS;
        self.visible.clear();
        if !dense {
            self.visible.extend(pts[lo..hi].iter().map(|p| GraphPoint {
                key: u64::from(p.id),
                tick: p.tick,
                y: by(p.bpm_f64()),
                pinned: p.tick == 0 && pts.first().map(|f| f.id) == Some(p.id),
            }));
        }
        let events = self.bpm.interact(
            ui,
            &resp,
            plot,
            &time,
            &self.visible,
            input.snap,
            editable,
            false,
        );
        for e in events {
            match e {
                PointEvent::Add { tick, y } => {
                    if let Some(bpm) = yb(y) {
                        // A new point continues the shape of the segment it splits.
                        let interp = pts[c.tempo_index_at(tick)].interp;
                        out.graph_actions.push(GraphAction::SetTempo {
                            target: None,
                            tick,
                            bpm,
                            interp,
                        });
                    }
                }
                PointEvent::Move { key, tick, y } => {
                    if let (Some(bpm), Some((_, p))) = (yb(y), c.tempo_by_id(key as u32)) {
                        out.graph_actions.push(GraphAction::SetTempo {
                            target: Some(key as u32),
                            tick,
                            bpm,
                            interp: p.interp,
                        });
                    }
                }
                PointEvent::MoveGroup { .. } => {}
                PointEvent::Edit(key) => {
                    let tick = c.tempo_by_id(key as u32).map_or(0, |(_, p)| p.tick);
                    out.graph_actions.push(GraphAction::EditTempo {
                        id: Some(key as u32),
                        tick,
                    });
                }
                PointEvent::Select {
                    t0,
                    t1,
                    y0,
                    y1,
                    add,
                } => {
                    let (blo, bhi) = (scale.value(y1, plot), scale.value(y0, plot));
                    if !add {
                        self.bpm.selected.clear();
                    }
                    for p in pts.iter().filter(|p| p.tick >= t0 && p.tick <= t1) {
                        let b = p.bpm_f64();
                        if b >= blo && b <= bhi {
                            self.bpm.selected.insert(u64::from(p.id));
                        }
                    }
                }
            }
        }

        // ------------------------------------------------------------- paint
        let p = ui.painter_at(plot_outer);
        self.grid(&p, plot, plot_outer, input);
        let (blo, bhi) = range;
        let mid = (blo * bhi).sqrt();
        for b in [blo, mid, bhi] {
            p.hline(
                plot.x_range(),
                by(b).round() + 0.5,
                Stroke::new(1.0, pal.grid_sub),
            );
        }
        let fmt = |b: f64| {
            if b >= 100.0 {
                format!("{b:.0}")
            } else if b >= 1.0 {
                format!("{b:.1}")
            } else {
                format!("{b:.3}")
            }
        };
        scale_label(&p, plot, by(bhi) + 5.0, &fmt(bhi), pal);
        scale_label(&p, plot, by(mid) - 6.0, &fmt(mid), pal);
        scale_label(&p, plot, by(blo) - 5.0, &fmt(blo), pal);
        let line_color = pal.accent;
        if dense {
            let ppp = ui.ctx().pixels_per_point();
            let cols = ((plot.width() * ppp) as usize).clamp(1, 8192);
            let path: Vec<Pos2> = (0..cols)
                .map(|col| {
                    let x = col as f32 / ppp;
                    let t = time.x_to_tick(f64::from(x));
                    Pos2::new(plot.left() + x, by(c.bpm_at(t)))
                })
                .collect();
            p.add(Shape::Path(PathShape::line(
                path,
                PathStroke::new(1.5, line_color),
            )));
            out.aggregated = true;
        } else {
            let shown: Vec<(u64, f64, Interp, u64)> = self
                .visible
                .iter()
                .map(|q| {
                    let (t, y) = self.bpm.shown(q);
                    let b = match self.bpm.gesture {
                        Some(g) if g.key == Some(q.key) && g.moved => {
                            yb(y).map_or(0.0, Bpm::to_f64)
                        }
                        _ => c
                            .tempo_by_id(q.key as u32)
                            .map_or(0.0, |(_, pt)| pt.bpm_f64()),
                    };
                    let interp = c
                        .tempo_by_id(q.key as u32)
                        .map_or(Interp::Step, |(_, pt)| pt.interp);
                    (t, b, interp, q.key)
                })
                .collect();
            let mut seq = shown.clone();
            seq.sort_by_key(|s| s.0);
            let x_of = |t: u64| plot.left() + time.tick_to_x(t) as f32;
            let mut path: Vec<Pos2> = Vec::new();
            for (i, &(t, b, interp, _)) in seq.iter().enumerate() {
                let x0 = x_of(t).max(plot.left() - 4.0);
                let y0 = by(b);
                path.push(Pos2::new(x0, y0));
                match seq.get(i + 1) {
                    Some(&(tn, bn, ..)) if interp == Interp::Linear && tn > t => {
                        let x1 = x_of(tn).min(plot.right() + 4.0);
                        let n = (((x1 - x0) / 4.0) as usize).clamp(2, 96);
                        for k in 1..=n {
                            let x = x0 + (x1 - x0) * k as f32 / n as f32;
                            let tt = time.x_to_tick(f64::from(x - plot.left()));
                            let f = (tt.saturating_sub(t)) as f64 / (tn - t) as f64;
                            let bb = b + (bn - b) * f.clamp(0.0, 1.0);
                            path.push(Pos2::new(x, by(bb)));
                        }
                    }
                    Some(&(tn, ..)) => path.push(Pos2::new(x_of(tn).min(plot.right() + 4.0), y0)),
                    None => path.push(Pos2::new(plot.right() + 4.0, y0)),
                }
            }
            if path.len() >= 2 {
                p.add(Shape::Path(PathShape::line(
                    path,
                    PathStroke::new(1.5, line_color),
                )));
            }
            let mut mesh = Mesh::default();
            for &(t, b, interp, key) in &shown {
                let x = x_of(t);
                if x < plot.left() - 4.0 {
                    continue;
                }
                let sel = self.bpm.selected.contains(&key);
                let hot = self.bpm.hover == Some(key);
                let color = if sel {
                    pal.note_selected_fill
                } else {
                    line_color
                };
                let ring = (sel || hot).then_some(if sel {
                    pal.note_selected_border
                } else {
                    pal.note_hover_border
                });
                handle(
                    &mut mesh,
                    Pos2::new(x, by(b)),
                    color,
                    ring,
                    interp == Interp::Linear,
                );
            }
            if let Some(g) = self.bpm.gesture.filter(|g| g.key.is_none()) {
                let x = x_of(g.tick);
                handle(
                    &mut mesh,
                    Pos2::new(x, g.y),
                    pal.note_selected_fill,
                    Some(pal.note_selected_border),
                    false,
                );
            }
            p.add(Shape::mesh(mesh));
        }
        self.bpm.draw_box(&p, pal);
        if let Some(t) = input.playhead {
            super::render::draw_playhead(&p, plot_outer, plot.left(), &time, pal, t);
        }
        if !c.timing_editable() {
            let note = c
                .scope()
                .timing_note()
                .unwrap_or("The tempo of this file cannot be edited here.");
            p.text(
                plot.center(),
                Align2::CENTER_CENTER,
                note,
                FontId::proportional(size::SMALL),
                pal.text_weak,
            );
        }
        // Readout.
        if let Some(g) = self.bpm.gesture {
            if let Some(b) = yb(g.y) {
                let x = plot.left() + time.tick_to_x(g.tick) as f32;
                readout(
                    &p,
                    plot,
                    x,
                    format!("{b} BPM · {}", input.meter.describe(g.tick)),
                    pal,
                );
            }
        } else if let Some(h) = self.bpm.hover
            && let Some((_, pt)) = c.tempo_by_id(h as u32)
        {
            let x = plot.left() + time.tick_to_x(pt.tick) as f32;
            let shape = match pt.interp {
                Interp::Step => "step",
                Interp::Linear => "linear ramp to the next point",
            };
            readout(
                &p,
                plot,
                x,
                format!(
                    "{} BPM ({shape}) · {}",
                    format_tempo(pt.value),
                    input.meter.describe(pt.tick)
                ),
                pal,
            );
        } else if resp.hovered()
            && self.curve.is_none()
            && let Some(pos) = resp.hover_pos()
        {
            let t = time.x_to_tick(f64::from(pos.x - plot.left()));
            let b = c.bpm_at(t);
            readout(
                &p,
                plot,
                pos.x,
                format!("{} BPM at {}", fmt4(b), input.meter.describe(t)),
                pal,
            );
        }
        // Context menu.
        let ctx = self.bpm.context;
        let selected = self.selected_tempo();
        resp.context_menu(|ui| match ctx {
            Some(Context::Point(key)) => {
                let id = key as u32;
                let Some((i, pt)) = c.tempo_by_id(id) else {
                    ui.close();
                    return;
                };
                if ui
                    .add_enabled(editable, egui::Button::new("Edit Tempo…"))
                    .clicked()
                {
                    out.graph_actions.push(GraphAction::EditTempo {
                        id: Some(id),
                        tick: pt.tick,
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        editable,
                        egui::RadioButton::new(
                            pt.interp == Interp::Step,
                            "Step (hold until the next point)",
                        ),
                    )
                    .clicked()
                {
                    out.graph_actions.push(GraphAction::SetInterp {
                        id,
                        interp: Interp::Step,
                    });
                    ui.close();
                }
                if ui
                    .add_enabled(
                        editable,
                        egui::RadioButton::new(
                            pt.interp == Interp::Linear,
                            "Linear ramp to the next point",
                        ),
                    )
                    .clicked()
                {
                    out.graph_actions.push(GraphAction::SetInterp {
                        id,
                        interp: Interp::Linear,
                    });
                    ui.close();
                }
                ui.separator();
                let first_id = pts.first().map(|p| p.id);
                let deletable: Vec<u32> = if selected.len() > 1 {
                    selected
                        .iter()
                        .copied()
                        .filter(|x| Some(*x) != first_id)
                        .collect()
                } else if i > 0 {
                    vec![id]
                } else {
                    Vec::new()
                };
                let label = if deletable.len() > 1 {
                    format!("Delete {} Selected Tempo Points", deletable.len())
                } else {
                    "Delete Tempo Point".to_owned()
                };
                if ui
                    .add_enabled(editable && !deletable.is_empty(), egui::Button::new(label))
                    .on_disabled_hover_text("The song needs a tempo at its start; edit it instead.")
                    .clicked()
                {
                    out.graph_actions.push(GraphAction::DeleteTempo(deletable));
                    ui.close();
                }
            }
            Some(Context::Empty(tick)) => {
                if ui
                    .add_enabled(editable, egui::Button::new("Add Tempo Change…"))
                    .clicked()
                {
                    out.graph_actions
                        .push(GraphAction::EditTempo { id: None, tick });
                    ui.close();
                }
            }
            None => ui.close(),
        });
    }

    /// Curve tools: press–drag–release draws from the press to the release (or
    /// places two anchors in Anchor mode); the preview shows the events the app
    /// would generate.
    fn curve_gesture(
        &mut self,
        ui: &mut egui::Ui,
        plot_outer: Rect,
        input: &HostInput<'_>,
        drawable: bool,
        out: &mut HostOutput,
    ) {
        let pal = input.palette;
        let plot = plot_outer.shrink2(Vec2::new(
            0.0,
            if input.target == GraphTarget::Bpm {
                8.0
            } else {
                6.0
            },
        ));
        let bpm = if input.target == GraphTarget::Bpm {
            self.bpm_scale_range(input.conductor)
        } else {
            (60.0, 240.0)
        };
        let scale = scale_of(input.target, bpm);
        let resp = ui.interact(
            plot_outer,
            ui.id().with(("graph-curve", input.slot)),
            Sense::click_and_drag(),
        );
        let time = input.time;
        let tick_at = |x: f32| time.x_to_tick(f64::from(x - plot.left()));
        let freehand = input.tool == GraphTool::FreeHand;
        let snapped = |t: u64| {
            if freehand {
                t
            } else {
                input.snap.map_or(t, |u| snap_nearest(t, u))
            }
        };
        let value_at = |y: f32| {
            let v = scale.value(y, plot);
            match target_limits(input.target) {
                Some((lo, hi)) => v.round().clamp(lo as f64, hi as f64),
                None => clean_bpm(v).map_or(v, Bpm::to_f64),
            }
        };
        let (pointer, pressed, released, down, escape, enter) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Enter),
            )
        });
        if escape {
            self.curve = None;
        }
        let x_of = |t: u64| plot.left() + time.tick_to_x(t) as f32;
        if drawable && resp.contains_pointer() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        if pressed
            && drawable
            && resp.contains_pointer()
            && let Some(pos) = pointer
        {
            // In Anchor mode, pressing near a placed anchor moves it.
            let grab = self.curve.as_ref().filter(|c| c.placed).and_then(|c| {
                [c.from, c.to].iter().position(|&(t, v)| {
                    (x_of(t) - pos.x).abs() <= PICK_PX * 1.5
                        && (scale.y(v, plot) - pos.y).abs() <= PICK_PX * 2.0
                })
            });
            match grab {
                Some(i) => {
                    if let Some(c) = self.curve.as_mut() {
                        c.moving = Some(i);
                    }
                }
                None => {
                    let start = (snapped(tick_at(pos.x)), value_at(pos.y));
                    self.curve = Some(CurveDrag {
                        from: start,
                        to: start,
                        samples: vec![(tick_at(pos.x), scale.value(pos.y, plot))],
                        last_px: pos,
                        moving: None,
                        placed: false,
                    });
                }
            }
        }
        if let (Some(c), Some(pos)) = (self.curve.as_mut(), pointer)
            && down
        {
            let at = (
                snapped(tick_at(pos.x)),
                value_at(pos.y.clamp(plot.top(), plot.bottom())),
            );
            match c.moving {
                Some(0) => c.from = at,
                Some(_) => c.to = at,
                None if !c.placed => {
                    c.to = at;
                    if freehand
                        && (pos - c.last_px).length() >= 1.0
                        && c.samples.len() < MAX_STROKE_SAMPLES
                    {
                        c.samples.push((
                            tick_at(pos.x),
                            scale.value(pos.y.clamp(plot.top(), plot.bottom()), plot),
                        ));
                        c.last_px = pos;
                    }
                }
                None => {}
            }
        }
        if released && let Some(c) = self.curve.as_mut() {
            if c.moving.is_some() {
                c.moving = None;
            } else if !c.placed {
                if freehand {
                    let samples = std::mem::take(&mut c.samples);
                    self.curve = None;
                    if !samples.is_empty() {
                        out.graph_actions.push(GraphAction::FreeHand { samples });
                    }
                } else if c.from.0 == c.to.0 {
                    // A click, not a drag: nothing to draw.
                    self.curve = None;
                } else if input.anchors {
                    c.placed = true;
                } else {
                    out.graph_actions.push(GraphAction::Curve {
                        from: c.from,
                        to: c.to,
                    });
                    self.curve = None;
                }
            }
        }
        if enter
            && input.active
            && let Some(c) = self.curve.as_ref().filter(|c| c.placed)
        {
            out.graph_actions.push(GraphAction::Curve {
                from: c.from,
                to: c.to,
            });
            self.curve = None;
        }
        // ------------------------------------------------------------- paint
        let Some(c) = &self.curve else {
            let over_point = self.controller.hover.is_some()
                || self.master.hover.is_some()
                || self.bpm.hover.is_some();
            if resp.hovered()
                && drawable
                && !over_point
                && let Some(pos) = pointer
            {
                let t = tick_at(pos.x);
                readout(
                    &ui.painter_at(plot_outer),
                    plot,
                    pos.x,
                    format!(
                        "{} · {}",
                        value_text(input.target, value_at(pos.y)),
                        input.meter.describe(t)
                    ),
                    pal,
                );
            }
            return;
        };
        let p = ui.painter_at(plot_outer);
        let color = pal.playhead;
        if freehand {
            let pts: Vec<Pos2> = c
                .samples
                .iter()
                .map(|&(t, v)| Pos2::new(x_of(t), scale.y(v, plot)))
                .collect();
            if pts.len() >= 2 {
                p.add(Shape::Path(PathShape::line(
                    pts,
                    PathStroke::new(1.5, color),
                )));
            }
            return;
        }
        let (a, b) = if c.from.0 <= c.to.0 {
            (c.from, c.to)
        } else {
            (c.to, c.from)
        };
        if a.0 != b.0 {
            let pv = (input.preview)(input.target, c.from, c.to);
            if !pv.points.is_empty() {
                let pts: Vec<(f32, f32)> = pv
                    .points
                    .iter()
                    .map(|&(t, v)| (x_of(t), scale.y(v, plot)))
                    .collect();
                let path: Vec<Pos2> = pts.iter().map(|&(x, y)| Pos2::new(x, y)).collect();
                if path.len() >= 2 {
                    p.add(Shape::Path(PathShape::line(
                        path,
                        PathStroke::new(1.0, color.gamma_multiply(0.7)),
                    )));
                }
                if pts.len() <= 600 {
                    let mut mesh = Mesh::default();
                    for &(x, y) in &pts {
                        mesh.add_colored_rect(
                            Rect::from_center_size(Pos2::new(x, y), Vec2::splat(3.0)),
                            color,
                        );
                    }
                    p.add(Shape::mesh(mesh));
                }
            } else if !pv.shape.is_empty() {
                let (x0, x1) = (x_of(c.from.0), x_of(c.to.0));
                let path: Vec<Pos2> = pv
                    .shape
                    .iter()
                    .map(|&(u, v)| Pos2::new(x0 + (x1 - x0) * u as f32, scale.y(v, plot)))
                    .collect();
                if path.len() >= 2 {
                    p.add(Shape::Path(PathShape::line(
                        path,
                        PathStroke::new(1.5, color),
                    )));
                }
            }
            if let Some(note) = &pv.note {
                let col = if pv.error { pal.error } else { pal.text };
                let galley = p.layout_no_wrap(note.clone(), FontId::proportional(size::SMALL), col);
                let pos = Pos2::new(plot.left() + 4.0, plot.bottom() - galley.size().y - 2.0);
                p.rect_filled(
                    Rect::from_min_size(pos, galley.size()).expand(3.0),
                    0.0,
                    pal.overlay_bg,
                );
                p.galley(pos, galley, col);
            }
        }
        // Anchors.
        let mut mesh = Mesh::default();
        for &(t, v) in [&c.from, &c.to] {
            handle(
                &mut mesh,
                Pos2::new(x_of(t), scale.y(v, plot)),
                color,
                Some(pal.text),
                true,
            );
        }
        p.add(Shape::mesh(mesh));
        let (t, v) = if c.moving == Some(0) { c.from } else { c.to };
        let hint = if c.placed {
            " · Enter applies, Esc cancels, drag an anchor to adjust"
        } else {
            ""
        };
        readout(
            &p,
            plot,
            x_of(t),
            format!(
                "{} → {} · {}{hint}",
                value_text(input.target, c.from.1),
                value_text(input.target, v),
                input.meter.describe(t)
            ),
            pal,
        );
    }
}

/// The target can take a curve here (a track for track targets; editable
/// Conductor timing / Master Volume for global ones).
fn target_drawable(input: &HostInput<'_>) -> bool {
    match input.target {
        GraphTarget::Bpm => input.conductor.timing_editable(),
        GraphTarget::MasterVolume => input.conductor.master_editable(),
        GraphTarget::Velocity => input.track.is_some(),
        t => {
            input.track.is_some() && graph_controller(t).is_some_and(ControllerKind::curve_drawable)
        }
    }
}

/// Caches shared by both graph slots.
#[derive(Default)]
pub struct GraphCache {
    points: PointCache,
}

/// Both graph slots.
#[derive(Default)]
pub struct GraphHost {
    pub views: [GraphView; 2],
    pub cache: GraphCache,
}

impl GraphHost {
    pub fn cancel(&mut self) {
        for v in &mut self.views {
            v.cancel();
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.views.iter().any(GraphView::is_dragging)
    }

    pub fn clear_point_selection(&mut self) {
        for v in &mut self.views {
            v.clear_point_selection();
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, full: Rect, input: HostInput<'_>) -> HostOutput {
        let slot = input.slot.min(1);
        self.views[slot].show(ui, full, input, &mut self.cache)
    }
}

/// Four significant digits (display).
pub fn fmt4(b: f64) -> String {
    if !b.is_finite() {
        return "?".into();
    }
    let digits = (3 - b.abs().max(1e-12).log10().floor() as i32).clamp(0, 9) as usize;
    let s = format!("{b:.digits$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_values_map_to_the_full_range_both_ways() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 10.0), Pos2::new(100.0, 110.0));
        for t in [
            GraphTarget::PitchBend,
            GraphTarget::Cc(10),
            GraphTarget::MasterVolume,
        ] {
            let (lo, hi) = target_limits(t).unwrap();
            let s = scale_of(t, (60.0, 240.0));
            assert_eq!(s.value(plot.bottom(), plot).round() as i64, lo);
            assert_eq!(s.value(plot.top(), plot).round() as i64, hi);
            assert_eq!(s.value(-500.0, plot).round() as i64, hi);
        }
        let s = scale_of(GraphTarget::Cc(7), (60.0, 240.0));
        for v in 0..=127i64 {
            assert_eq!(s.value(s.y(v as f64, plot), plot).round() as i64, v);
        }
        let vel = scale_of(GraphTarget::Velocity, (60.0, 240.0));
        assert_eq!(vel.value(plot.bottom(), plot), 1.0);
    }

    #[test]
    fn bpm_scale_is_logarithmic_and_values_are_clean() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0));
        let s = Scale::Log {
            lo: 50.0,
            hi: 200.0,
        };
        // The geometric middle sits in the middle.
        assert!((s.y(100.0, plot) - 50.0).abs() < 1e-3);
        let b = clean_bpm(s.value(50.0, plot)).unwrap();
        assert_eq!(b.to_string(), "100");
        // Rounded to four significant digits, any magnitude — kept within
        // what a MIDI file can store (B1-0101: 3.58 – 60,000,000 BPM).
        for (lo, hi) in [(1e-3, 1e-2), (0.5, 2.0), (1e6, 1e9)] {
            let s = Scale::Log { lo, hi };
            for y in [0.0f32, 13.0, 50.0, 99.0] {
                let b = clean_bpm(s.value(y, plot)).unwrap();
                let t = b.to_string();
                let digits = t.chars().filter(char::is_ascii_digit).collect::<String>();
                assert!(
                    digits.trim_start_matches('0').trim_end_matches('0').len() <= 4,
                    "{t}"
                );
                let (lo, hi) = (lo.clamp(3.58, 6e7), hi.clamp(3.58, 6e7));
                assert!(b.to_f64() >= lo * 0.99 && b.to_f64() <= hi * 1.01, "{t}");
                assert!(b.check_midi().is_ok(), "{t}");
            }
        }
        // A single tempo gets a sensible range around it.
        let c = Conductor::new_project(Bpm::parse("120").unwrap());
        let (lo, hi) = bpm_range(&c);
        assert!(lo < 120.0 && hi > 120.0);
        assert_eq!(fmt4(123.456), "123.5");
        assert_eq!(fmt4(0.001234), "0.001234");
    }

    #[test]
    fn tools_have_stable_setting_keys() {
        for t in GraphTool::ALL {
            assert_eq!(GraphTool::from_key(t.key()), Some(t));
            assert!(!t.tip().is_empty());
        }
    }

    /// Frame cost of the controller graphs (200,000 events), the BPM graph
    /// (10,000 tempo points, half of them ramps) and Master Volume at 4 bars and
    /// over the whole song.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn graph_frame_cost() {
        use plumspirit_engine::fixtures::{
            FixtureShape, add_dense_controllers, generate, set_dense_tempo,
        };
        use std::time::Instant;
        let mut doc = generate(FixtureShape::Dense, 1_000_000, 960, 42);
        let end = doc.content_end_tick();
        set_dense_tempo(&mut doc, 10_000, end / 10_000, 5);
        add_dense_controllers(&mut doc, 1, 200_000, end / 200_000, 3);
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::new(0.0, 600.0), Vec2::new(1280.0, 110.0));
        let preview = |_: GraphTarget, _: (u64, f64), _: (u64, f64)| CurvePreview::default();
        for target in [
            GraphTarget::Velocity,
            GraphTarget::PitchBend,
            GraphTarget::Cc(10),
            GraphTarget::Bpm,
        ] {
            let mut host = GraphHost::default();
            for (name, ppt) in [
                ("4 bars", 1200.0 / (960.0 * 16.0)),
                ("whole song", 1200.0 / end as f64),
            ] {
                let time = TimeAxis::new(0, ppt);
                let mut times = Vec::new();
                for _ in 0..5 {
                    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                        let t = Instant::now();
                        host.show(
                            ui,
                            full,
                            HostInput {
                                slot: 0,
                                target,
                                channel: 0,
                                active: true,
                                tool: GraphTool::Edit,
                                anchors: false,
                                track: doc.track(1).map(Into::into),
                                conductor: doc.conductor(),
                                time,
                                meter: &meter,
                                palette: &crate::ui::theme::DARK,
                                dark: true,
                                editable: true,
                                playhead: None,
                                snap: None,
                                ghosts: &[],
                                preview: &preview,
                                note: None,
                            },
                        );
                        times.push(t.elapsed());
                    });
                }
                println!(
                    "{target:?} graph, {name}: first frame {:.2} ms, steady {:.2} ms",
                    times[0].as_secs_f64() * 1e3,
                    times[4].as_secs_f64() * 1e3
                );
            }
        }
    }
}
