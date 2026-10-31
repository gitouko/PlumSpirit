//! The selected shapes' settings (Spiderweb's per-shape panels, laid out the
//! PlumSpirit way): velocity, colour, free transform, live shapes, fill /
//! spam, polygon, Hz bass, text, funnel, tumours, formulas, claw and strum.
//! Every change is cleaned like a loaded file (`clean_shape`), applies to
//! every selected shape it fits, and a run of changes to one control is one
//! board undo step.

use super::fonts::LoadedFont;
use super::{Spiderweb, Tool};
use egui::{RichText, Ui};
use plumspirit_engine::json::Value;
use plumspirit_engine::spiderweb::edit::{self, Xform};
use plumspirit_engine::spiderweb::paths::P;
use plumspirit_engine::spiderweb::text::{self as swtext, FontSource};
use plumspirit_engine::spiderweb::{self as sw, custom, funnel, hzbass, pattern};

const FILL_NAMES: [(&str, &str, &str); 4] = [
    ("empty", "Outline", "Only the outline makes notes."),
    ("fill", "Fill", "One long note per key inside the shape."),
    ("spam", "Spam", "Notes of the gate length fill the inside."),
    (
        "outline_spam",
        "Outline spam",
        "Notes of the gate length along the outline.",
    ),
];
const ALIGN_NAMES: [(&str, &str); 3] = [
    ("auto", "Automatic"),
    ("aligned", "Aligned to the box"),
    ("centred", "Centered per key"),
];
const ENDS_NAMES: [(&str, &str); 5] = [
    ("round", "Rounded"),
    ("keep", "Keep short ends"),
    ("drop", "Drop short ends"),
    ("min", "At least one gate"),
    ("stretch", "Stretch to fit"),
];
/// Gate choices (beats).
const GATES: [(&str, f64); 8] = [
    ("1/4", 1.0),
    ("1/8", 0.5),
    ("1/16", 0.25),
    ("1/32", 0.125),
    ("1/64", 0.0625),
    ("1/128", 0.03125),
    ("1/256", 0.015625),
    ("1/512", 0.0078125),
];
const TUMOUR_SHAPES: [&str; 4] = ["triangle", "square", "circle", "parabola"];
const TUMOUR_SIDES: [(&str, &str); 4] = [
    ("alt", "Alternating"),
    ("left", "Left"),
    ("right", "Right"),
    ("random", "Random"),
];
const CLAW_MODES: [(&str, &str); 6] = [
    ("", "Off"),
    ("time", "Time slices"),
    ("notes", "Every n-th note"),
    ("keys", "Every n-th key"),
    ("chords", "Every n-th chord"),
    ("random", "Random"),
];
const CLAW_PERIODS: [f64; 5] = [1.0, 2.0, 4.0, 8.0, 16.0];
const CLAW_TRASHES: [(f64, f64); 9] = [
    (1.0, 4.0),
    (2.0, 4.0),
    (3.0, 4.0),
    (4.0, 4.0),
    (1.0, 3.0),
    (2.0, 3.0),
    (3.0, 3.0),
    (1.0, 2.0),
    (2.0, 2.0),
];
const STRUM_NUMS: [(&str, &str, f64, f64); 6] = [
    ("time", "Start offset (beats)", -4.0, 4.0),
    ("tension", "Start tension", -100.0, 100.0),
    ("vel", "Velocity change", -127.0, 127.0),
    ("vel_tension", "Velocity tension", -100.0, 100.0),
    ("end_time", "End offset (beats)", -4.0, 4.0),
    ("end_tension", "End tension", -100.0, 100.0),
];
const STRUM_FLAGS: [(&str, &str); 6] = [
    ("start", "Strum starts"),
    ("preserve", "Keep lengths"),
    ("ahead", "Ahead of the beat"),
    ("end", "Strum ends"),
    ("chop", "Chop at the starts"),
    ("alternate", "Alternate up / down"),
];
const FUNNEL_FILL: [(&str, &str); 2] = [("spam", "Spam"), ("long", "Long notes")];
const HZ_MODES: [&str; 3] = ["Exact tone", "Whole ticks", "Automatic"];

/// One requested change: (section, field, value); section "" = the shape
/// itself.
type Edit = (&'static str, &'static str, Value);

fn num_of(v: &Value, key: &str, d: f64) -> f64 {
    sw::getf(v, key, d)
}

fn str_of<'a>(v: &'a Value, key: &str, d: &'a str) -> &'a str {
    sw::gets(v, key).unwrap_or(d)
}

fn flag_of(v: &Value, key: &str) -> bool {
    v.get(key) == Some(&Value::Bool(true))
}

/// `[beat, pitch]` → note name ("C4" = 60).
pub fn key_name(k: i64) -> String {
    const N: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", N[k.rem_euclid(12) as usize], k.div_euclid(12) - 1)
}

/// Em size in keys (Spiderweb's `em_keys`).
pub fn em_keys(tx: &Value) -> f64 {
    let size = num_of(tx, "size", 24.0);
    if str_of(tx, "unit", "font") == "rows" {
        size / num_of(tx, "cap", 0.7).max(1e-6)
    } else {
        size
    }
}

/// A text shape's layout axes (origin, em x axis, em y axis) from its box.
pub fn text_axes(sh: &Value) -> Option<(P, P, P)> {
    let tx = sh.get("text")?;
    let bb: Vec<f64> = tx
        .get("bbox")?
        .as_arr()?
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    let f = sw::shape_pts(sh);
    if bb.len() != 4 || f.len() != 3 {
        return None;
    }
    let (w, h) = (bb[2] - bb[0], bb[3] - bb[1]);
    if w.abs() < 1e-12 || h.abs() < 1e-12 {
        return None;
    }
    let xa = [(f[1][0] - f[0][0]) / w, (f[1][1] - f[0][1]) / w];
    let ya = [(f[2][0] - f[0][0]) / h, (f[2][1] - f[0][1]) / h];
    let o = [
        f[0][0] - bb[0] * xa[0] - bb[1] * ya[0],
        f[0][1] - bb[0] * xa[1] - bb[1] * ya[1],
    ];
    Some((o, xa, ya))
}

/// A new text shape at `[beat, pitch]` (the baseline's left end) with the
/// settings `tx`; `k` = beats per key on screen.
pub fn new_text_shape(
    tx: &Value,
    at: P,
    k: f64,
    font: &dyn FontSource,
    base: &Value,
) -> Option<Value> {
    let mut tx = tx.clone();
    tx.set("k", Value::Num(k));
    tx.set("cap", Value::Num(font.cap()));
    if !tx.has("bbox") {
        tx.set(
            "bbox",
            Value::Arr([0.0, 0.0, 1.0, 1.0].into_iter().map(Value::Num).collect()),
        );
    }
    let tx = swtext::clean_text(&tx)?;
    let em = em_keys(&tx);
    let axes = ([at[0], at[1] - 0.5], [em * k, 0.0], [0.0, em]);
    retype(base, &tx, axes, font)
}

/// `sh` (or a new custom shape with `sh`'s settings) laid out again.
pub fn retype(sh: &Value, tx: &Value, axes: (P, P, P), font: &dyn FontSource) -> Option<Value> {
    let mut out = if sw::kind(sh) == "custom" {
        sh.clone()
    } else {
        edit::custom_shape(
            edit::box_frame([0.0, 0.0], [1.0, 1.0]),
            vec![edit::ellipse_stroke()],
            "Text",
            sh,
        )
    };
    if let Value::Obj(o) = &mut out {
        o.retain(|(k, _)| k != "polygon" && k != "from" && k != "areas");
    }
    let mut tx = tx.clone();
    tx.set("cap", Value::Num(font.cap()));
    if !swtext::build(&mut out, &tx, axes, font) {
        return None;
    }
    let name: String = swtext::shown_text(&tx).chars().take(24).collect();
    out.set("name", Value::Str(name));
    sw::clean_shape(&out)
}

/// A text shape split into one shape per line or per letter, each with
/// its own fixed colour (`first`, `first + 1`, …), placed exactly where they
/// were.
pub fn split_text(sh: &Value, per_letter: bool, first: i64, font: &dyn FontSource) -> Vec<Value> {
    let (Some(tx), Some((o, xa, ya))) = (sh.get("text"), text_axes(sh)) else {
        return Vec::new();
    };
    let shown = swtext::shown_text(tx);
    let step = font.line_height() * num_of(tx, "leading", 100.0) / 100.0;
    let track = num_of(tx, "tracking", 0.0) / 1000.0;
    let align = str_of(tx, "align", "left");
    let at = |x: f64, y: f64| [o[0] + x * xa[0] + y * ya[0], o[1] + x * xa[1] + y * ya[1]];
    let mut out = Vec::new();
    let mut colour = first;
    for (n, line) in shown.split('\n').enumerate() {
        let y = -(n as f64) * step;
        if !per_letter {
            let mut t = tx.clone();
            t.set("text", Value::Str(line.to_owned()));
            t.set("caps", Value::Str("none".into()));
            let o2 = at(0.0, y);
            if let Some(mut s) = retype(sh, &t, (o2, xa, ya), font) {
                edit::set_fixed_colour(&mut s, Some(colour));
                if let Some(s) = sw::clean_shape(&s) {
                    out.push(s);
                }
            }
            colour += 1;
            continue;
        }
        let mut xs = Vec::new();
        let mut x = 0.0;
        let mut prev: Option<char> = None;
        for ch in line.chars() {
            if let Some(p) = prev {
                x += font.kerning(p, ch) + track;
            }
            xs.push(x);
            x += font.glyph(ch).0;
            prev = Some(ch);
        }
        let shift = match align {
            "center" => -x / 2.0,
            "right" => -x,
            _ => 0.0,
        };
        for (ch, cx) in line.chars().zip(xs) {
            if font.glyph(ch).1.is_empty() {
                continue;
            }
            let mut t = tx.clone();
            t.set("text", Value::Str(ch.to_string()));
            t.set("caps", Value::Str("none".into()));
            t.set("align", Value::Str("left".into()));
            t.set("underline", Value::Bool(false));
            t.set("strike", Value::Bool(false));
            if let Some(mut s) = retype(sh, &t, (at(cx + shift, y), xa, ya), font) {
                edit::set_fixed_colour(&mut s, Some(colour));
                if let Some(s) = sw::clean_shape(&s) {
                    out.push(s);
                }
            }
            colour += 1;
        }
    }
    out
}

fn combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    label: &str,
    cur: T,
    items: &[(T, &str)],
) -> Option<T> {
    let mut v = cur;
    ui.horizontal(|ui| {
        ui.label(label);
        let shown = items
            .iter()
            .find(|(t, _)| *t == cur)
            .map_or("—", |(_, n)| *n);
        egui::ComboBox::from_id_salt(id)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for (t, n) in items {
                    ui.selectable_value(&mut v, *t, *n);
                }
            });
    });
    (v != cur).then_some(v)
}

fn drag(
    ui: &mut Ui,
    label: &str,
    v: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            crate::ui::stepper::Stepper::new(v)
                .range(range)
                .speed(speed),
        )
        .changed()
    })
    .inner
}

impl Spiderweb {
    /// Applies edits to every selected shape `fits` accepts (one undo step
    /// per run of changes to the same control).
    pub(super) fn apply_edits(
        &mut self,
        what: &'static str,
        edits: Vec<Edit>,
        fits: impl Fn(&Value) -> bool,
    ) {
        if edits.is_empty() {
            return;
        }
        let mut out: Vec<(usize, Value)> = Vec::new();
        for &i in &self.sel {
            let sh = self.board.get(i);
            if !fits(sh) {
                continue;
            }
            let mut s = sh.clone();
            for (sec, key, v) in &edits {
                if sec.is_empty() {
                    if matches!(v, Value::Null) {
                        if let Value::Obj(o) = &mut s {
                            o.retain(|(k, _)| k != key);
                        }
                    } else {
                        s.set(key, v.clone());
                    }
                } else {
                    let mut o = s.get(sec).cloned().unwrap_or_else(Value::obj);
                    o.set(key, v.clone());
                    s.set(sec, o);
                }
            }
            if let Some(c) = sw::clean_shape(&s)
                && c != *sh
            {
                out.push((i, c));
            }
        }
        if out.is_empty() {
            return;
        }
        self.checkpoint_once(what);
        for (i, c) in out {
            self.board.set(i, c);
        }
    }

    fn first_selected(&self) -> Option<Value> {
        self.sel.iter().next().map(|&i| self.board.get(i).clone())
    }

    fn any_selected(&self, f: impl Fn(&Value) -> bool) -> Option<Value> {
        self.sel
            .iter()
            .map(|&i| self.board.get(i))
            .find(|s| f(s))
            .cloned()
    }

    /// The panel for the selection (nothing selected: nothing shown).
    pub(super) fn props_ui(&mut self, ui: &mut Ui) {
        let Some(first) = self.first_selected() else {
            return;
        };
        self.common_ui(ui, &first);
        self.transform_ui(ui);
        self.live_ui(ui);
        if let Some(c) = self.any_selected(|s| sw::kind(s) == "custom" && !s.has("notes")) {
            self.fill_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| s.has("polygon")) {
            self.polygon_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| s.has("hz")) {
            self.hz_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| s.has("text")) {
            self.text_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| sw::kind(s) == "funnel") {
            self.funnel_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| sw::LINE_KINDS.contains(&sw::kind(s))) {
            self.tumour_ui(ui, &c);
        }
        if let Some(c) = self.any_selected(|s| sw::FORMULA_KINDS.contains(&sw::kind(s))) {
            self.formula_ui(ui, &c);
        }
        self.claw_ui(ui, &first);
        self.strum_ui(ui, &first);
    }

    fn common_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let mut v0 = num_of(sh, "vel0", 127.0).clamp(1.0, 127.0);
        let mut v1 = num_of(sh, "vel1", 127.0).clamp(1.0, 127.0);
        let has_env = sh.get("vel_env").is_some_and(Value::truthy);
        let mut edits = Vec::new();
        ui.horizontal(|ui| {
            ui.label("Velocity from");
            if ui
                .add(
                    crate::ui::stepper::Stepper::new(&mut v0)
                        .range(1.0..=127.0)
                        .speed(0.5),
                )
                .changed()
            {
                edits.push(("", "vel0", Value::Num(v0.round())));
                edits.push(("", "vel_env", Value::Null));
            }
            ui.label("to");
            if ui
                .add(
                    crate::ui::stepper::Stepper::new(&mut v1)
                        .range(1.0..=127.0)
                        .speed(0.5),
                )
                .changed()
            {
                edits.push(("", "vel1", Value::Num(v1.round())));
                edits.push(("", "vel_env", Value::Null));
            }
        });
        let mut curve = has_env;
        if ui
            .checkbox(&mut curve, "Velocity curve")
            .on_hover_text(
                "Velocity along the shape's time follows a curve you draw (else a straight ramp).",
            )
            .changed()
        {
            if curve {
                let env = Value::Arr(vec![
                    Value::Arr(vec![Value::Num(0.0), Value::Num(v0)]),
                    Value::Arr(vec![
                        Value::Num(0.5),
                        Value::Num(((v0 + v1) / 2.0).max(1.0)),
                    ]),
                    Value::Arr(vec![Value::Num(1.0), Value::Num(v1)]),
                ]);
                edits.push(("", "vel_env", env));
            } else {
                edits.push(("", "vel_env", Value::Null));
            }
        }
        if has_env && let Some(env) = sh.get("vel_env").and_then(sw::pts_of) {
            let c = plumspirit_engine::transform::Curve::new(
                env.iter()
                    .map(|p| (p[0].clamp(0.0, 1.0), ((p[1] - 1.0) / 126.0).clamp(0.0, 1.0)))
                    .collect(),
            );
            let id = ui.make_persistent_id("sw-velenv");
            let full = plumspirit_engine::transform::Curve::flat(1.0);
            if let Some(c) = crate::ui::param_editors::curve_editor_with(ui, id, &c, &full, false) {
                let env = Value::Arr(
                    c.points
                        .iter()
                        .map(|(u, y)| {
                            Value::Arr(vec![Value::Num(*u), Value::Num((1.0 + y * 126.0).round())])
                        })
                        .collect(),
                );
                edits.push(("", "vel_env", env));
            }
        }
        if sw::LINE_KINDS.contains(&sw::kind(sh)) {
            let mut dot = flag_of(sh, "end_dot");
            if ui
                .checkbox(&mut dot, "End dot")
                .on_hover_text("A note at the very end of the line too.")
                .changed()
            {
                edits.push(("", "end_dot", Value::Bool(dot)));
            }
        }
        self.apply_edits("velocity", edits, |_| true);
        // colour
        let fixed = edit::fixed_colour(sh);
        let mut on = fixed.is_some();
        let mut c = fixed.unwrap_or(0) + 1;
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut on, "Fixed color")
                .on_hover_text("All of the shape's notes get this color (subchannel / subtrack when pasted), whatever the channel mode.")
                .changed();
            if on {
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut c).range(1..=1024).prefix("Color "))
                    .changed();
                let rgb = plumspirit_engine::spiderweb::output::SLOT_COLORS
                    [((c - 1).max(0) as usize) % 15];
                super::super::paste_ui::swatch(ui, rgb);
            }
        });
        if changed {
            self.checkpoint_once("colour");
            let sel: Vec<usize> = self.sel.iter().copied().collect();
            for i in sel {
                let mut s = self.board.get(i).clone();
                edit::set_fixed_colour(&mut s, on.then_some(c - 1));
                if let Some(s) = sw::clean_shape(&s) {
                    self.board.set(i, s);
                }
            }
        }
    }

    fn transform_ui(&mut self, ui: &mut Ui) {
        egui::CollapsingHeader::new("Transform")
            .id_salt("sw-xform")
            .default_open(false)
            .show(ui, |ui| {
                let mut go: Option<Xform> = None;
                ui.horizontal(|ui| {
                    ui.label("Rotate");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut self.xform.rotate)
                            .range(-360.0..=360.0)
                            .speed(0.5)
                            .suffix("°"),
                    );
                    if ui.button("Rotate").clicked() {
                        go = Some(Xform::rotation(self.xform.rotate));
                    }
                    for (d, t) in [(90.0, "↺ 90°"), (-90.0, "↻ 90°"), (180.0, "180°")] {
                        if ui.small_button(t).clicked() {
                            go = Some(Xform::rotation(d));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Scale X");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut self.xform.sx)
                            .range(-100.0..=100.0)
                            .speed(0.01)
                            .suffix("×"),
                    );
                    ui.label("Y");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut self.xform.sy)
                            .range(-100.0..=100.0)
                            .speed(0.01)
                            .suffix("×"),
                    );
                    let ok = self.xform.sx.abs() >= 1e-6 && self.xform.sy.abs() >= 1e-6;
                    if ui
                        .add_enabled(ok, egui::Button::new("Scale"))
                        .on_hover_text("Negative values mirror the shapes.")
                        .clicked()
                    {
                        go = Some(Xform {
                            rotate: 0.0,
                            sx: self.xform.sx,
                            sy: self.xform.sy,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button("Flip X")
                        .on_hover_text("Mirror in time (around the selection's middle).")
                        .clicked()
                    {
                        go = Some(Xform::flip_x());
                    }
                    if ui
                        .button("Flip Y")
                        .on_hover_text("Mirror the keys (around the selection's middle).")
                        .clicked()
                    {
                        go = Some(Xform::flip_y());
                    }
                });
                ui.label(
                    RichText::new(
                        "Around the selection's middle, in the board's proportions on screen.",
                    )
                    .weak()
                    .small(),
                );
                if let Some(xf) = go {
                    self.transform_selected(&xf);
                }
            });
    }

    /// Transforms the selected shapes as one group (one undo step).
    pub(super) fn transform_selected(&mut self, xf: &Xform) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        if sel.is_empty() || xf.is_identity() {
            return;
        }
        let shapes: Vec<Value> = sel.iter().map(|&i| self.board.get(i).clone()).collect();
        match edit::transform_shapes(&shapes, xf, None, self.view_k()) {
            Some(out) => {
                self.checkpoint();
                for (i, s) in sel.into_iter().zip(out) {
                    self.board.set(i, s);
                }
                self.last_control = None;
            }
            None => {
                self.share_msg = Some((false, "That transform would break a shape.".into()));
            }
        }
    }

    fn live_ui(&mut self, ui: &mut Ui) {
        let lines = self
            .sel
            .iter()
            .any(|&i| sw::kind(self.board.get(i)) != "custom");
        let customs = self.sel.iter().any(|&i| {
            let s = self.board.get(i);
            sw::kind(s) == "custom" && !s.has("text") && !s.has("notes")
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(lines && !self.sel.is_empty(), egui::Button::new("Make a live shape"))
                .on_hover_text("The selected shapes become one shape whose inside can be filled (Ctrl+L).")
                .clicked()
            {
                self.make_live();
            }
            if ui
                .add_enabled(customs, egui::Button::new("Split"))
                .on_hover_text("A live / custom shape back into its shapes, or one shape per stroke (Ctrl+Shift+L).")
                .clicked()
            {
                self.split_selected();
            }
        });
    }

    pub(super) fn make_live(&mut self) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        if sel.is_empty() {
            return;
        }
        let shapes: Vec<Value> = sel.iter().map(|&i| self.board.get(i).clone()).collect();
        let Some(live) = edit::to_live(&shapes) else {
            self.share_msg = Some((false, "These shapes cannot become a live shape.".into()));
            return;
        };
        self.checkpoint();
        let set: std::collections::BTreeSet<usize> = sel.into_iter().collect();
        self.board.remove(&set);
        let i = self.board.push(live);
        self.sel.clear();
        self.sel.insert(i);
    }

    pub(super) fn split_selected(&mut self) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let mut parts: Vec<(usize, Vec<Value>)> = Vec::new();
        for i in sel {
            if let Some(p) = edit::split(self.board.get(i)) {
                parts.push((i, p));
            }
        }
        if parts.is_empty() {
            return;
        }
        self.checkpoint();
        let set: std::collections::BTreeSet<usize> = parts.iter().map(|p| p.0).collect();
        self.board.remove(&set);
        self.sel.clear();
        for (_, p) in parts {
            for s in p {
                let i = self.board.push(s);
                self.sel.insert(i);
            }
        }
    }

    fn fill_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let is_custom = |s: &Value| sw::kind(s) == "custom" && !s.has("notes");
        egui::CollapsingHeader::new("Fill")
            .id_salt("sw-fill")
            .default_open(true)
            .show(ui, |ui| {
                let mut edits = Vec::new();
                let fill = str_of(sh, "fill", "empty");
                ui.horizontal_wrapped(|ui| {
                    for (k, n, h) in FILL_NAMES {
                        if ui.selectable_label(fill == k, n).on_hover_text(h).clicked() && fill != k {
                            edits.push(("", "fill", Value::Str(k.into())));
                        }
                    }
                });
                let spam = custom::SPAM_FILLS.contains(&fill);
                if spam && !sh.has("hz") {
                    let gate = num_of(sh, "gate", 0.0625);
                    ui.horizontal(|ui| {
                        ui.label("Gate");
                        let mut g = gate;
                        egui::ComboBox::from_id_salt("sw-gate")
                            .width(64.0)
                            .selected_text(
                                GATES
                                    .iter()
                                    .find(|(_, b)| (b - gate).abs() < 1e-12)
                                    .map_or_else(|| format!("{gate:.4}"), |(n, _)| (*n).to_owned()),
                            )
                            .show_ui(ui, |ui| {
                                for (n, b) in GATES {
                                    ui.selectable_value(&mut g, b, n);
                                }
                            });
                        ui.add(
                            crate::ui::stepper::Stepper::new(&mut g)
                                .range(1e-6..=64.0)
                                .speed(0.001)
                                .suffix(" beats"),
                        );
                        if (g - gate).abs() > 1e-15 {
                            edits.push(("", "gate", Value::Num(g)));
                        }
                    });
                }
                if fill != "empty" {
                    let al = str_of(sh, "align", "auto");
                    if let Some(a) = combo(ui, "sw-align", "Align", al, &ALIGN_NAMES) {
                        edits.push(("", "align", Value::Str(a.into())));
                    }
                    if spam {
                        let en = str_of(sh, "ends", "drop");
                        if let Some(e) = combo(ui, "sw-ends", "Ends", en, &ENDS_NAMES) {
                            edits.push(("", "ends", Value::Str(e.into())));
                        }
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    for (k, n, h) in [
                        ("union", "Union", "Overlapping strokes make one inside."),
                        ("apart", "Outline apart", "The outline gets a color of its own."),
                        ("borders", "Borders", "Area borders make notes too."),
                    ] {
                        let mut b = flag_of(sh, k);
                        if ui.checkbox(&mut b, n).on_hover_text(h).changed() {
                            edits.push(("", k, Value::Bool(b)));
                        }
                    }
                });
                if fill != "empty" {
                    let mut edge = num_of(sh, "edge", 0.0);
                    ui.horizontal(|ui| {
                        ui.label("Outline gate");
                        if ui
                            .add(
                                crate::ui::stepper::Stepper::new(&mut edge)
                                    .range(0.0..=64.0)
                                    .speed(0.01)
                                    .suffix(" beats"),
                            )
                            .on_hover_text("Only a band this wide inside the outline is filled (0 = all of it).")
                            .changed()
                        {
                            edits.push(("", "edge", Value::Num(edge)));
                        }
                        let mut side = str_of(sh, "edge_mode", "band") == "sideways";
                        if edge > 0.0 && ui.checkbox(&mut side, "Sideways").changed() {
                            edits.push((
                                "",
                                "edge_mode",
                                Value::Str(if side { "sideways" } else { "band" }.into()),
                            ));
                        }
                    });
                }
                // Colours (cycle)
                let cyc = sh.get("cycle");
                let by = cyc.and_then(|c| sw::gets(c, "by")).unwrap_or("");
                if let Some(b) = combo(
                    ui,
                    "sw-cycle",
                    "Colors",
                    by,
                    &[("", "One"), ("step", "By step"), ("key", "By key"), ("time", "By time")],
                ) {
                    if b.is_empty() {
                        edits.push(("", "cycle", Value::Null));
                    } else {
                        let mut c = Value::obj();
                        c.set("by", Value::Str(b.into()));
                        c.set("n", Value::Num(cyc.map_or(2.0, |c| num_of(c, "n", 2.0))));
                        edits.push(("", "cycle", c));
                    }
                }
                if let Some(c) = cyc.filter(|_| !by.is_empty()) {
                    let mut n = num_of(c, "n", 2.0);
                    let mut cy = c.clone();
                    let mut ch = drag(ui, "Colors used", &mut n, 2.0..=15.0, 0.05);
                    if by == "time" {
                        let ev: Vec<f64> = c
                            .get("every")
                            .and_then(Value::as_arr)
                            .map(|a| a.iter().filter_map(Value::as_f64).collect())
                            .unwrap_or_else(|| vec![1.0, 4.0]);
                        let (mut a, mut b) = (ev.first().copied().unwrap_or(1.0), ev.get(1).copied().unwrap_or(4.0));
                        ui.horizontal(|ui| {
                            ui.label("Change every");
                            ch |= ui.add(crate::ui::stepper::Stepper::new(&mut a).range(1.0..=10000.0)).changed();
                            ui.label("/");
                            ch |= ui.add(crate::ui::stepper::Stepper::new(&mut b).range(1.0..=10000.0)).changed();
                            ui.label("bars");
                        });
                        cy.set("every", Value::Arr(vec![Value::Num(a), Value::Num(b)]));
                    } else {
                        let mut e = num_of(c, "every", 1.0);
                        ch |= drag(ui, "Change every", &mut e, 1.0..=10000.0, 0.05);
                        cy.set("every", Value::Num(e.round()));
                    }
                    if ch {
                        cy.set("n", Value::Num(n.round()));
                        edits.push(("", "cycle", cy));
                    }
                }
                self.apply_edits("fill", edits, is_custom);
            });
    }

    fn polygon_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let Some(pg) = sh.get("polygon").cloned() else {
            return;
        };
        egui::CollapsingHeader::new("Polygon")
            .id_salt("sw-poly")
            .default_open(true)
            .show(ui, |ui| {
                let mut edits = Vec::new();
                let mut p = pg.clone();
                if poly_settings_ui(ui, "sw-pg", &mut p) {
                    edits.push(("", "polygon", p));
                }
                self.apply_edits("polygon", edits, |s| s.has("polygon"));
            });
    }

    fn hz_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let Some(hz) = sh.get("hz").cloned() else {
            return;
        };
        egui::CollapsingHeader::new("Hz bass")
            .id_salt("sw-hz")
            .default_open(true)
            .show(ui, |ui| {
                let mut h = hz.clone();
                let mut changed = false;
                let mut key = num_of(&hz, "key", 33.0);
                let mut cents = num_of(&hz, "cents", 0.0);
                let mut bpm = num_of(&hz, "bpm", 120.0);
                // v0.9: the board's keys (0–255 in 256-Key Mode).
                let top = self.view.top_key;
                ui.horizontal(|ui| {
                    ui.label("Tone");
                    changed |= ui
                        .add(
                            crate::ui::stepper::Stepper::new(&mut key)
                                .range(0.0..=top)
                                .speed(0.1)
                                .custom_formatter(|v, _| key_name(v.round() as i64)),
                        )
                        .changed();
                    ui.label("cents");
                    changed |= ui
                        .add(crate::ui::stepper::Stepper::new(&mut cents).range(-1200.0..=1200.0).speed(0.5))
                        .changed();
                });
                ui.label(
                    RichText::new(format!(
                        "{:.2} Hz — one wave = {:.5} beats",
                        hzbass::hz_of(key.round(), cents),
                        hzbass::hz_gate(key.round(), cents, bpm)
                    ))
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    ui.label("Tempo");
                    changed |= ui
                        .add(crate::ui::stepper::Stepper::new(&mut bpm).range(1.0..=10000.0).speed(0.1).step(1.0).suffix(" BPM"))
                        .on_hover_text("The tempo the tone is tuned for (the project's tempo where the box starts when placed).")
                        .changed();
                    if let Some(pb) = self.project_bpm
                        && (pb - bpm).abs() > 1e-9
                        && ui.small_button("Project tempo").clicked()
                    {
                        bpm = pb;
                        changed = true;
                    }
                });
                let mode = if flag_of(&hz, "fixed") {
                    1
                } else if hz.has("auto") {
                    2
                } else {
                    0
                };
                let mut m = mode;
                ui.horizontal(|ui| {
                    for (i, n) in HZ_MODES.iter().enumerate() {
                        ui.radio_value(&mut m, i, *n).on_hover_text(match i {
                            0 => "Every gate as exact as the tone needs (lengths vary by a tick).",
                            1 => "Every gate a whole number of ticks (the tone is a little off).",
                            _ => "Whole ticks while the tone stays within the threshold.",
                        });
                    }
                });
                if m != mode {
                    changed = true;
                    if let Value::Obj(o) = &mut h {
                        o.retain(|(k, _)| k != "fixed" && k != "auto");
                    }
                    match m {
                        1 => h.set("fixed", Value::Bool(true)),
                        2 => h.set("auto", Value::Num(hzbass::AUTO)),
                        _ => {}
                    }
                }
                if m == 2 {
                    let mut a = num_of(&h, "auto", hzbass::AUTO);
                    if drag(ui, "Threshold (cents)", &mut a, 0.0..=hzbass::AUTO_MOST, 0.05) {
                        h.set("auto", Value::Num(a));
                        changed = true;
                    }
                }
                // effects: one amount each over the whole box
                egui::CollapsingHeader::new("Effects")
                    .id_salt("sw-hzfx")
                    .default_open(false)
                    .show(ui, |ui| {
                        let mut fx = h.get("fx").cloned().unwrap_or_else(Value::obj);
                        let mut fx_changed = false;
                        for name in hzbass::FX {
                            let neutral = match name {
                                "volume" => 1.0,
                                "pitch" => 0.5,
                                _ => 0.0,
                            };
                            let cur = fx
                                .get(name)
                                .and_then(sw::pts_of)
                                .and_then(|p| p.first().map(|q| q[1]))
                                .unwrap_or(neutral);
                            let mut v = cur;
                            ui.horizontal(|ui| {
                                ui.label(format!("{:>9}", name));
                                if crate::ui::stepper::slider(ui, &mut v, 0.0..=1.0, "").changed() {
                                    fx_changed = true;
                                }
                            });
                            if v != cur {
                                if v == neutral {
                                    if let Value::Obj(o) = &mut fx {
                                        o.retain(|(k, _)| k != name);
                                    }
                                } else {
                                    fx.set(
                                        name,
                                        Value::Arr(vec![Value::Arr(vec![Value::Num(0.0), Value::Num(v)])]),
                                    );
                                }
                            }
                        }
                        if fx_changed {
                            h.set("fx", fx);
                            changed = true;
                        }
                        if h.get("tones").is_some_and(Value::truthy) {
                            ui.label(
                                RichText::new("This box has placed tones (from Spiderweb); they are kept and play as made.")
                                    .weak()
                                    .small(),
                            );
                        }
                    });
                if changed {
                    h.set("key", Value::Num(key.round()));
                    h.set("cents", Value::Num(cents));
                    h.set("bpm", Value::Num(bpm));
                    let gate = hzbass::hz_gate(key.round(), cents, bpm);
                    self.apply_edits(
                        "hz",
                        vec![("", "hz", h), ("", "gate", Value::Num(gate))],
                        |s| s.has("hz"),
                    );
                }
            });
    }

    fn text_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let Some(tx) = sh.get("text").cloned() else {
            return;
        };
        egui::CollapsingHeader::new("Text")
            .id_salt("sw-text")
            .default_open(true)
            .show(ui, |ui| {
                let mut t = tx.clone();
                let mut changed = text_settings_ui(
                    ui,
                    "sw-seltext",
                    &mut t,
                    self.fonts.library.as_deref(),
                    &mut self.font_filter,
                );
                let mut split: Option<bool> = None;
                ui.horizontal(|ui| {
                    ui.label("Colors");
                    if ui
                        .button("One per line")
                        .on_hover_text("Each line becomes a shape of its own color.")
                        .clicked()
                    {
                        split = Some(false);
                    }
                    if ui
                        .button("One per letter")
                        .on_hover_text("Each letter becomes a shape of its own color.")
                        .clicked()
                    {
                        split = Some(true);
                    }
                });
                if self.fonts.busy() {
                    ui.label(RichText::new("Reading the installed fonts…").weak().small());
                }
                if changed || split.is_some() {
                    let Some(font) = self.font_for(&t) else {
                        self.share_msg = Some((false, "No font is available to type with.".into()));
                        return;
                    };
                    if let Some(per_letter) = split {
                        changed = false;
                        self.split_text_selected(per_letter, &font);
                    }
                    if changed {
                        self.retype_selected(&t, &tx, &font);
                    }
                }
            });
    }

    /// Lays out every selected text shape again with `t` (`old` = the
    /// settings shown, to keep each shape's other settings).
    fn retype_selected(&mut self, t: &Value, old: &Value, font: &LoadedFont) {
        self.checkpoint_once("text");
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        for i in sel {
            let sh = self.board.get(i).clone();
            let (Some(cur), Some((o, xa, ya))) = (sh.get("text").cloned(), text_axes(&sh)) else {
                continue;
            };
            // only the settings that changed go to every selected text
            let mut n = cur.clone();
            if let (Value::Obj(new), Value::Obj(prev)) = (t, old) {
                for (k, v) in new {
                    if prev.iter().find(|(pk, _)| pk == k).map(|(_, pv)| pv) != Some(v) {
                        n.set(k, v.clone());
                    }
                }
            }
            let s = em_keys(&n) / em_keys(&cur).max(1e-12);
            let axes = (o, [xa[0] * s, xa[1] * s], [ya[0] * s, ya[1] * s]);
            if let Some(out) = retype(&sh, &n, axes, font) {
                self.board.set(i, out);
            }
        }
    }

    fn split_text_selected(&mut self, per_letter: bool, font: &LoadedFont) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let mut parts = Vec::new();
        let mut next = 0;
        for i in sel {
            let sh = self.board.get(i);
            if !sh.has("text") {
                continue;
            }
            let first = edit::fixed_colour(sh).unwrap_or(next);
            let p = split_text(sh, per_letter, first, font);
            next = first + p.len() as i64;
            if !p.is_empty() {
                parts.push((i, p));
            }
        }
        if parts.is_empty() {
            return;
        }
        self.checkpoint();
        let set: std::collections::BTreeSet<usize> = parts.iter().map(|p| p.0).collect();
        self.board.remove(&set);
        self.sel.clear();
        for (_, p) in parts {
            for s in p {
                let i = self.board.push(s);
                self.sel.insert(i);
            }
        }
        self.last_control = None;
    }

    fn funnel_ui(&mut self, ui: &mut Ui, sh: &Value) {
        egui::CollapsingHeader::new("Funnel")
            .id_salt("sw-funnel")
            .default_open(true)
            .show(ui, |ui| {
                let mut edits = Vec::new();
                let fill = str_of(sh, "fill", "spam");
                if let Some(f) = combo(ui, "sw-ffill", "Fill", fill, &FUNNEL_FILL) {
                    edits.push(("", "fill", Value::Str(f.into())));
                }
                let mut g0 = num_of(sh, "gate0", 0.0625);
                let mut g1 = num_of(sh, "gate1", g0);
                let mut vary = flag_of(sh, "vary");
                if drag(ui, "Gate at the start (beats)", &mut g0, 1e-6..=64.0, 0.001) {
                    edits.push(("", "gate0", Value::Num(g0)));
                    if !vary {
                        edits.push(("", "gate1", Value::Num(g0)));
                    }
                }
                if ui
                    .checkbox(&mut vary, "Gate changes towards the wall")
                    .changed()
                {
                    edits.push(("", "vary", Value::Bool(vary)));
                }
                if vary {
                    if drag(ui, "Gate at the wall (beats)", &mut g1, 1e-6..=64.0, 0.001) {
                        edits.push(("", "gate1", Value::Num(g1)));
                    }
                    let ch = str_of(sh, "change", "steps");
                    if let Some(c) = combo(
                        ui,
                        "sw-fchange",
                        "Change",
                        ch,
                        &[("steps", "In steps"), ("smooth", "Smoothly")],
                    ) {
                        edits.push(("", "change", Value::Str(c.into())));
                    }
                    let fo = str_of(sh, "follow", "time");
                    if let Some(c) = combo(
                        ui,
                        "sw-ffollow",
                        "Follows",
                        fo,
                        &[("time", "Time"), ("curve", "The curve")],
                    ) {
                        edits.push(("", "follow", Value::Str(c.into())));
                    }
                }
                let wall = str_of(sh, "wall", "in");
                if let Some(c) = combo(
                    ui,
                    "sw-fwall",
                    "Wall",
                    wall,
                    &[
                        ("in", "Notes stop at the wall"),
                        ("past", "Notes go past the wall"),
                    ],
                ) {
                    edits.push(("", "wall", Value::Str(c.into())));
                }
                let _ = funnel::FUNNEL_FILLS;
                self.apply_edits("funnel", edits, |s| sw::kind(s) == "funnel");
            });
    }

    fn tumour_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let fits = |s: &Value| sw::LINE_KINDS.contains(&sw::kind(s));
        egui::CollapsingHeader::new("Tumours")
            .id_salt("sw-tumour")
            .default_open(false)
            .show(ui, |ui| {
                let tm = sh.get("tumour").cloned();
                let mut on = tm
                    .as_ref()
                    .is_some_and(|t| t.get("on") != Some(&Value::Bool(false)));
                let mut edits: Vec<Edit> = Vec::new();
                if ui.checkbox(&mut on, "Bumps along the line").changed() {
                    let mut t = tm.clone().unwrap_or_else(Value::obj);
                    t.set("on", Value::Bool(on));
                    if !t.has("k") {
                        t.set("k", Value::Num(self.view_k()));
                    }
                    edits.push(("", "tumour", t));
                }
                if let Some(t) = tm.filter(|_| on) {
                    let shp = str_of(&t, "shape", "triangle");
                    let items: Vec<(&str, &str)> = TUMOUR_SHAPES.iter().map(|s| (*s, *s)).collect();
                    if let Some(s) = combo(ui, "sw-tshape", "Shape", shp, &items) {
                        edits.push(("tumour", "shape", Value::Str(s.into())));
                    }
                    let side = str_of(&t, "side", "alt");
                    if let Some(s) = combo(ui, "sw-tside", "Side", side, &TUMOUR_SIDES) {
                        edits.push(("tumour", "side", Value::Str(s.into())));
                    }
                    for (key, label, d, lo, hi, speed) in [
                        ("size", "Size (keys)", 3.0, -1000.0, 1000.0, 0.05),
                        ("length", "Length (beats)", 0.125, 0.0, 1e6, 0.005),
                        ("dist", "Spacing (beats)", 0.125, 1e-9, 1e6, 0.005),
                        ("start", "From (0–1)", 0.0, 0.0, 1.0, 0.005),
                        ("end", "To (0–1)", 1.0, 0.0, 1.0, 0.005),
                        ("ease", "Ease", 0.0, 0.0, 1e6, 0.01),
                        ("rot", "Rotation (°)", 0.0, -180.0, 180.0, 0.5),
                        ("slant", "Slant", 0.0, -1.0, 1.0, 0.01),
                        ("seed", "Seed", 1.0, -1e9, 1e9, 0.2),
                    ] {
                        let mut v = num_of(&t, key, d);
                        if drag(ui, label, &mut v, lo..=hi, speed) {
                            let v = if key == "seed" { v.round() } else { v };
                            edits.push(("tumour", key, Value::Num(v)));
                        }
                    }
                    ui.horizontal(|ui| {
                        for (key, label) in [("fit", "Fit to the line"), ("mirror", "Mirror")] {
                            let mut b = flag_of(&t, key);
                            if ui.checkbox(&mut b, label).changed() {
                                edits.push(("tumour", key, Value::Bool(b)));
                            }
                        }
                        let mut wrap = str_of(&t, "wrap", "simple") == "wrap";
                        if ui.checkbox(&mut wrap, "Wrap").changed() {
                            edits.push((
                                "tumour",
                                "wrap",
                                Value::Str(if wrap { "wrap" } else { "simple" }.into()),
                            ));
                        }
                    });
                }
                self.apply_edits("tumour", edits, fits);
            });
    }

    fn formula_ui(&mut self, ui: &mut Ui, sh: &Value) {
        let fits = |s: &Value| sw::FORMULA_KINDS.contains(&sw::kind(s));
        egui::CollapsingHeader::new("Formulas")
            .id_salt("sw-formula")
            .default_open(false)
            .show(ui, |ui| {
                let mut edits: Vec<Edit> = Vec::new();
                let k = self.view_k();
                // pattern along the line
                let pat = sh.get("pattern").cloned();
                let cur = pat.as_ref().map_or("", |p| str_of(p, "preset", ""));
                let mut items: Vec<(&str, &str)> = vec![("", "No pattern")];
                items.extend(pattern::PATTERN_PRESETS.iter().map(|(n, _, _)| (*n, *n)));
                if let Some(p) = combo(ui, "sw-pat", "Pattern", cur, &items) {
                    if p.is_empty() {
                        edits.push(("", "pattern", Value::Null));
                    } else if let Some((n, f, nums)) =
                        pattern::PATTERN_PRESETS.iter().find(|(n, _, _)| *n == p)
                    {
                        let mut v = Value::obj();
                        v.set("preset", Value::Str((*n).into()));
                        v.set("formula", Value::Str((*f).into()));
                        let mut vars = Value::obj();
                        for (vn, vv) in nums.iter() {
                            vars.set(vn, Value::Num(*vv));
                        }
                        v.set("vars", vars);
                        if let Some((_, a)) = pattern::PRESET_ALONG.iter().find(|(n2, _)| n2 == n) {
                            v.set("along", Value::Str((*a).into()));
                        }
                        v.set("loops", Value::Num(pattern::LOOPS_DEFAULT));
                        v.set("k", Value::Num(k));
                        edits.push(("", "pattern", v));
                    }
                }
                if let Some(p) =
                    pat.filter(|p| !str_of(p, "formula", "").is_empty() || p.has("loop"))
                {
                    let mut loops = num_of(&p, "loops", 4.0);
                    if drag(ui, "Loops", &mut loops, 0.01..=10000.0, 0.05) {
                        edits.push(("pattern", "loops", Value::Num(loops)));
                    }
                    let mut scale = num_of(&p, "scale", 1.0);
                    if drag(ui, "Scale", &mut scale, -1000.0..=1000.0, 0.01) {
                        edits.push(("pattern", "scale", Value::Num(scale)));
                    }
                    if let Some(Value::Obj(vars)) = p.get("vars") {
                        let mut nv = Value::Obj(vars.clone());
                        let mut ch = false;
                        for (name, val) in vars {
                            let mut x = val.as_f64().unwrap_or(0.0);
                            if drag(ui, name, &mut x, -1e6..=1e6, 0.01) {
                                nv.set(name, Value::Num(x));
                                ch = true;
                            }
                        }
                        if ch {
                            edits.push(("pattern", "vars", nv));
                        }
                    }
                    ui.horizontal(|ui| {
                        for (key, label) in [("mirror", "Mirror"), ("each", "Each piece")] {
                            let mut b = flag_of(&p, key);
                            if ui.checkbox(&mut b, label).changed() {
                                edits.push(("pattern", key, Value::Bool(b)));
                            }
                        }
                    });
                }
                // shape formula
                let shp = sh.get("shape").cloned();
                let cur = shp.as_ref().map_or("", |p| str_of(p, "preset", ""));
                let mut items: Vec<(&str, &str)> = vec![("", "As drawn")];
                items.extend(pattern::SHAPE_PRESETS.iter().map(|(n, _, _, _)| (*n, *n)));
                if let Some(p) = combo(ui, "sw-shp", "Shape", cur, &items) {
                    if p.is_empty() {
                        edits.push(("", "shape", Value::Null));
                    } else if let Some((n, x, y, nums)) =
                        pattern::SHAPE_PRESETS.iter().find(|(n, _, _, _)| *n == p)
                    {
                        let mut v = Value::obj();
                        v.set("preset", Value::Str((*n).into()));
                        v.set("x", Value::Str((*x).into()));
                        v.set("y", Value::Str((*y).into()));
                        let mut vars = Value::obj();
                        for (vn, vv) in nums.iter() {
                            vars.set(vn, Value::Num(*vv));
                        }
                        v.set("vars", vars);
                        v.set("k", Value::Num(k));
                        edits.push(("", "shape", v));
                    }
                }
                if let Some(p) = shp
                    && let Some(Value::Obj(vars)) = p.get("vars")
                {
                    let mut nv = Value::Obj(vars.clone());
                    let mut ch = false;
                    for (name, val) in vars {
                        let mut x = val.as_f64().unwrap_or(0.0);
                        if drag(ui, name, &mut x, -1e6..=1e6, 0.01) {
                            nv.set(name, Value::Num(x));
                            ch = true;
                        }
                    }
                    if ch {
                        edits.push(("shape", "vars", nv));
                    }
                    let mut b = flag_of(&p, "mirror");
                    if ui.checkbox(&mut b, "Mirror the shape").changed() {
                        edits.push(("shape", "mirror", Value::Bool(b)));
                    }
                }
                self.apply_edits("formula", edits, fits);
            });
    }

    fn claw_ui(&mut self, ui: &mut Ui, sh: &Value) {
        egui::CollapsingHeader::new("Claw machine")
            .id_salt("sw-claw")
            .default_open(false)
            .show(ui, |ui| {
                let claw = sh.get("claw").cloned();
                let mode: String = claw
                    .as_ref()
                    .map_or(String::new(), |c| str_of(c, "mode", "time").to_owned());
                let mode = mode.as_str();
                let mut edits: Vec<Edit> = Vec::new();
                if let Some(m) = combo(ui, "sw-claw-mode", "Mode", mode, &CLAW_MODES) {
                    if m.is_empty() {
                        edits.push(("", "claw", Value::Null));
                    } else {
                        let mut c = Value::obj();
                        c.set("mode", Value::Str(m.into()));
                        match m {
                            "time" => {
                                c.set("trash", Value::Arr(vec![Value::Num(2.0), Value::Num(4.0)]))
                            }
                            "random" => c.set("pct", Value::Num(50.0)),
                            _ => c.set("skip", Value::Num(1.0)),
                        }
                        edits.push(("", "claw", c));
                    }
                }
                if let Some(c) = claw {
                    match mode {
                        "time" => {
                            let per = num_of(&c, "period", 1.0);
                            let items: Vec<(u32, String)> = CLAW_PERIODS
                                .iter()
                                .map(|p| {
                                    (
                                        *p as u32,
                                        format!("{p} beat{}", if *p == 1.0 { "" } else { "s" }),
                                    )
                                })
                                .collect();
                            let items2: Vec<(u32, &str)> =
                                items.iter().map(|(a, b)| (*a, b.as_str())).collect();
                            if let Some(p) = combo(ui, "sw-claw-per", "Period", per as u32, &items2)
                            {
                                edits.push(("claw", "period", Value::Num(f64::from(p))));
                            }
                            let tr = c.get("trash").and_then(Value::as_arr).map(|a| {
                                (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0))
                            });
                            let names: Vec<(usize, String)> =
                                std::iter::once((usize::MAX, "Keep all".to_owned()))
                                    .chain(
                                        CLAW_TRASHES.iter().enumerate().map(|(i, (a, b))| {
                                            (i, format!("Trash part {a} of {b}"))
                                        }),
                                    )
                                    .collect();
                            let cur = tr
                                .and_then(|t| CLAW_TRASHES.iter().position(|x| *x == t))
                                .unwrap_or(usize::MAX);
                            let names2: Vec<(usize, &str)> =
                                names.iter().map(|(a, b)| (*a, b.as_str())).collect();
                            if let Some(i) = combo(ui, "sw-claw-trash", "Cut out", cur, &names2) {
                                edits.push((
                                    "claw",
                                    "trash",
                                    match CLAW_TRASHES.get(i) {
                                        Some((a, b)) => {
                                            Value::Arr(vec![Value::Num(*a), Value::Num(*b)])
                                        }
                                        None => Value::Null,
                                    },
                                ));
                            }
                            let mut d = num_of(&c, "dist", 0.0);
                            if drag(ui, "Bend time", &mut d, -100.0..=100.0, 0.5) {
                                edits.push(("claw", "dist", Value::Num(d)));
                            }
                            ui.horizontal(|ui| {
                                for (k, n) in [
                                    ("stretch", "Stretch to the old length"),
                                    ("short", "Drop tiny notes"),
                                ] {
                                    let mut b = flag_of(&c, k);
                                    if ui.checkbox(&mut b, n).changed() {
                                        edits.push(("claw", k, Value::Bool(b)));
                                    }
                                }
                            });
                        }
                        "random" | "notes" | "keys" | "chords" => {
                            if mode == "random" {
                                let mut p = num_of(&c, "pct", 50.0);
                                if drag(ui, "Keep (%)", &mut p, 0.0..=100.0, 0.5) {
                                    edits.push(("claw", "pct", Value::Num(p)));
                                }
                                let mut s = num_of(&c, "seed", 1.0);
                                if drag(ui, "Seed", &mut s, 0.0..=1e9, 0.2) {
                                    edits.push(("claw", "seed", Value::Num(s.round())));
                                }
                            } else {
                                let mut k = num_of(&c, "keep", 1.0);
                                let mut s = num_of(&c, "skip", 1.0);
                                if drag(ui, "Keep", &mut k, 1.0..=1000.0, 0.05) {
                                    edits.push(("claw", "keep", Value::Num(k.round())));
                                }
                                if drag(ui, "then skip", &mut s, 0.0..=1000.0, 0.05) {
                                    edits.push(("claw", "skip", Value::Num(s.round())));
                                }
                            }
                            let mut b = flag_of(&c, "shorten");
                            if ui.checkbox(&mut b, "Shorten instead of removing").changed() {
                                edits.push(("claw", "shorten", Value::Bool(b)));
                            }
                            if b {
                                let mut cut = num_of(&c, "cut", 25.0);
                                if drag(ui, "Shorten to (%)", &mut cut, 1.0..=99.0, 0.5) {
                                    edits.push(("claw", "cut", Value::Num(cut)));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                self.apply_edits("claw", edits, |_| true);
            });
    }

    fn strum_ui(&mut self, ui: &mut Ui, sh: &Value) {
        egui::CollapsingHeader::new("Strum")
            .id_salt("sw-strum")
            .default_open(false)
            .show(ui, |ui| {
                let st = sh.get("strum").cloned();
                let mut on = st.is_some();
                let mut edits: Vec<Edit> = Vec::new();
                if ui
                    .checkbox(&mut on, "Chord notes one after another")
                    .changed()
                {
                    if on {
                        let mut s = Value::obj();
                        s.set("time", Value::Num(1.0 / 3.0));
                        s.set("start", Value::Bool(true));
                        s.set("end", Value::Bool(true));
                        edits.push(("", "strum", s));
                    } else {
                        edits.push(("", "strum", Value::Null));
                    }
                }
                if let Some(s) = st {
                    for (k, n, lo, hi) in STRUM_NUMS {
                        let mut v = num_of(&s, k, 0.0);
                        if drag(ui, n, &mut v, lo..=hi, 0.01) {
                            edits.push(("strum", k, Value::Num(v)));
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (k, n) in STRUM_FLAGS {
                            let mut b = flag_of(&s, k);
                            if ui.checkbox(&mut b, n).changed() {
                                edits.push(("strum", k, Value::Bool(b)));
                            }
                        }
                    });
                }
                self.apply_edits("strum", edits, |_| true);
            });
    }

    /// The loaded font for text settings (cached).
    pub(super) fn font_for(&mut self, tx: &Value) -> Option<std::rc::Rc<LoadedFont>> {
        let fam = str_of(tx, "font", "Arial").to_owned();
        let weight = num_of(tx, "weight", 400.0).clamp(1.0, 1000.0) as u16;
        let italic = flag_of(tx, "italic");
        let key = (fam.to_lowercase(), weight, italic);
        if let Some((k, f)) = &self.font_cache
            && *k == key
        {
            return Some(f.clone());
        }
        let lib = self.fonts.library.clone();
        let f = lib
            .as_deref()
            .and_then(|l| LoadedFont::from_library(l, &fam, weight, italic))
            .or_else(|| {
                // the family is not installed here: the default family, else
                // egui's own font
                lib.as_deref()
                    .and_then(|l| l.default_family(&self.ui_family))
                    .and_then(|d| LoadedFont::from_library(lib.as_deref()?, &d, weight, italic))
            })
            .or_else(LoadedFont::built_in)?;
        let f = std::rc::Rc::new(f);
        self.font_cache = Some((key, f.clone()));
        Some(f)
    }

    /// Beats per key as the board is shown now.
    pub(super) fn view_k(&self) -> f64 {
        (self.view.px_key / self.view.px_beat).clamp(1e-9, 1e9)
    }

    /// Settings of the tool about to place a shape (polygon, Hz bass, text).
    pub(super) fn tool_settings_ui(&mut self, ui: &mut Ui) {
        match self.tool {
            Tool::Polygon => {
                let mut p = self.new_polygon.clone();
                if poly_settings_ui(ui, "sw-newpg", &mut p) {
                    self.new_polygon = p;
                }
            }
            Tool::Hz => {
                ui.label("Tone");
                let top = self.keys - 1;
                ui.add(
                    crate::ui::stepper::Stepper::new(&mut self.new_hz.0)
                        .range(0..=top)
                        .custom_formatter(|v, _| key_name(v.round() as i64)),
                );
                ui.label("cents");
                ui.add(
                    crate::ui::stepper::Stepper::new(&mut self.new_hz.1).range(-1200.0..=1200.0),
                );
            }
            Tool::Text if ui.button("Text settings…").clicked() => {
                self.text_settings_open = !self.text_settings_open;
            }
            Tool::Paint => {
                ui.label("Color");
                ui.add(crate::ui::stepper::Stepper::new(&mut self.paint_colour).range(0..=15));
                if self.paint_colour > 0 {
                    super::super::paste_ui::swatch(
                        ui,
                        plumspirit_engine::spiderweb::output::SLOT_COLORS
                            [(self.paint_colour - 1) as usize % 15],
                    );
                }
            }
            _ => {}
        }
    }
}

/// Polygon / star / crossing star settings; true when changed.
pub fn poly_settings_ui(ui: &mut Ui, id: &str, p: &mut Value) -> bool {
    let mut changed = false;
    let style = str_of(p, "style", "polygon").to_owned();
    if let Some(s) = combo(
        ui,
        &format!("{id}-style"),
        "Style",
        style.as_str(),
        &[
            ("polygon", "Polygon"),
            ("star", "Star"),
            ("cross", "Crossing star"),
        ],
    ) {
        p.set("style", Value::Str(s.into()));
        changed = true;
    }
    let mut n = num_of(p, "points", 5.0);
    if drag(ui, "Points", &mut n, 3.0..=200.0, 0.05) {
        p.set("points", Value::Num(n.round()));
        changed = true;
    }
    match style.as_str() {
        "star" => {
            let mut inner = num_of(p, "inner", 50.0);
            if drag(ui, "Inner size (%)", &mut inner, 0.0..=1000.0, 0.5) {
                p.set("inner", Value::Num(inner));
                changed = true;
            }
        }
        "cross" => {
            let mut skip = num_of(p, "skip", 2.0);
            if drag(ui, "Skip", &mut skip, 1.0..=199.0, 0.05) {
                p.set("skip", Value::Num(skip.round()));
                changed = true;
            }
        }
        _ => {}
    }
    changed
}

/// Text settings (text, font, size, styles, spacing, alignment, threshold,
/// grow); true when changed.
pub fn text_settings_ui(
    ui: &mut Ui,
    id: &str,
    t: &mut Value,
    lib: Option<&super::fonts::Library>,
    filter: &mut String,
) -> bool {
    let mut changed = false;
    let mut text = str_of(t, "text", "").to_owned();
    if ui
        .add(
            egui::TextEdit::multiline(&mut text)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("Text"),
        )
        .changed()
    {
        t.set("text", Value::Str(text));
        changed = true;
    }
    let fam = str_of(t, "font", "Arial").to_owned();
    ui.horizontal(|ui| {
        ui.label("Font");
        egui::ComboBox::from_id_salt(format!("{id}-font"))
            .width(170.0)
            .selected_text(&fam)
            .height(320.0)
            .show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(filter).hint_text("Find a font"));
                match lib {
                    Some(l) if !l.families.is_empty() => {
                        let f = filter.to_lowercase();
                        for name in l
                            .families
                            .keys()
                            .filter(|n| f.is_empty() || n.to_lowercase().contains(&f))
                        {
                            if ui.selectable_label(*name == fam, name).clicked() && *name != fam {
                                t.set("font", Value::Str(name.clone()));
                                changed = true;
                            }
                        }
                    }
                    Some(_) => {
                        ui.label("No installed fonts were found; PlumSpirit's own font is used.");
                    }
                    None => {
                        ui.label("Reading the installed fonts…");
                    }
                }
            });
    });
    if let Some(l) = lib
        && l.find(&fam).is_none()
        && !l.families.is_empty()
    {
        ui.label(
            RichText::new(format!(
                "{fam} is not installed here; another font is used for new typing."
            ))
            .weak()
            .small(),
        );
    }
    let mut size = num_of(t, "size", 24.0);
    ui.horizontal(|ui| {
        ui.label("Size");
        if ui
            .add(
                crate::ui::stepper::Stepper::new(&mut size)
                    .range(0.1..=10000.0)
                    .speed(0.1),
            )
            .changed()
        {
            t.set("size", Value::Num(size));
            changed = true;
        }
        let unit = str_of(t, "unit", "font").to_owned();
        for (u, n, h) in [
            (
                "font",
                "keys per em",
                "The font size in keys (like a font size in points).",
            ),
            (
                "rows",
                "keys per capital",
                "A capital letter is this many keys tall.",
            ),
        ] {
            if ui.radio(unit == u, n).on_hover_text(h).clicked() && unit != u {
                t.set("unit", Value::Str(u.into()));
                changed = true;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let w = num_of(t, "weight", 400.0);
        let mut bold = w >= 600.0;
        if ui.checkbox(&mut bold, "Bold").changed() {
            t.set("weight", Value::Num(if bold { 700.0 } else { 400.0 }));
            changed = true;
        }
        for (k, n) in [
            ("italic", "Italic"),
            ("underline", "Underline"),
            ("strike", "Strikeout"),
        ] {
            let mut b = flag_of(t, k);
            if ui.checkbox(&mut b, n).changed() {
                t.set(k, Value::Bool(b));
                changed = true;
            }
        }
    });
    let caps = str_of(t, "caps", "none").to_owned();
    if let Some(c) = combo(
        ui,
        &format!("{id}-caps"),
        "Letters",
        caps.as_str(),
        &[
            ("none", "As typed"),
            ("upper", "UPPER CASE"),
            ("lower", "lower case"),
            ("title", "Title Case"),
        ],
    ) {
        t.set("caps", Value::Str(c.into()));
        changed = true;
    }
    let al = str_of(t, "align", "left").to_owned();
    if let Some(a) = combo(
        ui,
        &format!("{id}-talign"),
        "Align",
        al.as_str(),
        &[("left", "Left"), ("center", "Center"), ("right", "Right")],
    ) {
        t.set("align", Value::Str(a.into()));
        changed = true;
    }
    for (k, n, d, lo, hi, speed, help) in [
        (
            "tracking",
            "Letter spacing",
            0.0,
            -1000.0,
            10000.0,
            1.0,
            "In thousandths of an em.",
        ),
        (
            "leading",
            "Line spacing (%)",
            100.0,
            1.0,
            1000.0,
            0.5,
            "Of the font's line height.",
        ),
        (
            "threshold",
            "Threshold (%)",
            50.0,
            0.0,
            100.0,
            0.5,
            "How much of a key's height a letter must cover to make a note there.",
        ),
        (
            "grow",
            "Grow (keys)",
            0.0,
            -100.0,
            100.0,
            0.01,
            "Letters made thicker (or thinner when negative).",
        ),
    ] {
        let mut v = num_of(t, k, d);
        let r = ui
            .horizontal(|ui| {
                ui.label(n);
                ui.add(
                    crate::ui::stepper::Stepper::new(&mut v)
                        .range(lo..=hi)
                        .speed(speed),
                )
                .on_hover_text(help)
                .changed()
            })
            .inner;
        if r {
            t.set(k, Value::Num(v));
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(text: &str) -> Value {
        let mut t = Value::obj();
        t.set("text", Value::Str(text.into()));
        t.set("size", Value::Num(10.0));
        t
    }

    #[test]
    fn a_text_keeps_its_place_and_size_when_retyped() {
        let font = LoadedFont::built_in().unwrap();
        let base = Value::obj();
        let sh = new_text_shape(&tx("Hi"), [2.0, 60.0], 0.25, &font, &base).unwrap();
        let (o, xa, ya) = text_axes(&sh).unwrap();
        assert!(
            (o[0] - 2.0).abs() < 1e-6 && (o[1] - 59.5).abs() < 1e-6,
            "{o:?}"
        );
        assert!(
            (xa[0] - 2.5).abs() < 1e-6 && (ya[1] - 10.0).abs() < 1e-6,
            "{xa:?} {ya:?}"
        );
        let mut t2 = sh.get("text").unwrap().clone();
        t2.set("text", Value::Str("Hi there".into()));
        let again = retype(&sh, &t2, (o, xa, ya), &font).unwrap();
        let (o2, xa2, _) = text_axes(&again).unwrap();
        assert!((o2[0] - o[0]).abs() < 1e-6 && (xa2[0] - xa[0]).abs() < 1e-6);
        assert!(!sw::shape_notes(&again, 96, 128).is_empty());
    }

    #[test]
    fn text_split_per_letter_gives_one_coloured_shape_per_visible_letter() {
        let font = LoadedFont::built_in().unwrap();
        let sh = new_text_shape(&tx("ab c\nd"), [0.0, 60.0], 0.25, &font, &Value::obj()).unwrap();
        let parts = split_text(&sh, true, 3, &font);
        assert_eq!(parts.len(), 4);
        let colours: Vec<i64> = parts.iter().filter_map(edit::fixed_colour).collect();
        assert_eq!(colours, vec![3, 4, 5, 6]);
        let lines = split_text(&sh, false, 0, &font);
        assert_eq!(lines.len(), 2);
        // the letters stay where they were: the first letter's box starts
        // where the whole text's does
        let (lo_all, _) = edit::bounds(&[sh]).unwrap();
        let (lo_a, _) = edit::bounds(&parts[..1]).unwrap();
        assert!((lo_all[0] - lo_a[0]).abs() < 1e-6, "{lo_all:?} {lo_a:?}");
    }

    #[test]
    fn key_names_follow_middle_c_as_c4() {
        assert_eq!(key_name(60), "C4");
        assert_eq!(key_name(33), "A1");
        assert_eq!(key_name(0), "C-1");
    }
}
