//! The Zoom tool (contract §64): View → Zoom Tool… (F8), a modeless window.
//! Plain zooming keeps working as before (wheel, toolbar, Fit Song); the tool
//! adds ten functions:
//!
//! 1. named zoom regions (saved with the project, `.lup` section `ZOOM`);
//! 2. fit the active track; 3. fit the selected notes' subchannels;
//! 4. fit a pitch range; 5. fit a time range (positions such as `5` or `5:2:0`);
//! 6. a synchronized overview (the whole active track with the visible window;
//!    clicking it moves the Piano Roll, zooming the roll moves the window);
//! 7. a temporary magnifier lens (hold Z over the Piano Roll while the window
//!    is open);
//! 8. back / forward through the views the tool jumped between (Ctrl+[ / Ctrl+]);
//! 9. an optional animated (180 ms) transition;
//! 10. density-aware auto-fit (the 2nd–98th percentile of the track's notes, so
//!     a few far-away notes do not shrink the art).
//!
//! Every jump costs O(notes of the active track) at most (once per click);
//! the overview is cached per track revision and size; nothing runs per frame
//! except the lens (bounded to the notes inside it).

use super::PlumSpiritApp;
use crate::piano_roll::viewport::RollView;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::model::project::{MAX_ZOOM_REGIONS, ZoomRegion};
use std::time::{Duration, Instant};

/// Length of an animated transition.
const ANIM: Duration = Duration::from_millis(180);
/// Overview size in cells (columns × pitch rows).
const OV_COLS: usize = 240;
const OV_ROWS: usize = 64;
/// Most notes the lens draws per frame.
const LENS_NOTES: usize = 4_000;

/// Overview cells, time span and pitch span of the active track.
type Overview = (Vec<u32>, (u64, u64), (u8, u8));

#[derive(Default)]
pub struct ZoomTool {
    pub open: bool,
    pub name: String,
    pub pitch_from: String,
    pub pitch_to: String,
    pub time_from: String,
    pub time_to: String,
    pub animate: bool,
    pub lens: bool,
    history: Vec<RollView>,
    pos: usize,
    anim: Option<(RollView, RollView, Instant)>,
    /// (track id, notes revision) → counts per cell, time span, pitch span.
    overview: Option<((u64, u64), Overview)>,
    pub message: Option<String>,
}

impl ZoomTool {
    pub fn new() -> ZoomTool {
        ZoomTool {
            pitch_from: "C2".into(),
            pitch_to: "C6".into(),
            time_from: "1".into(),
            time_to: "9".into(),
            lens: true,
            ..Default::default()
        }
    }
}

fn lerp_view(a: RollView, b: RollView, t: f64) -> RollView {
    let mut v = a;
    // Zoom interpolates geometrically, positions linearly.
    let ppt = (a.time.pixels_per_tick.ln() * (1.0 - t) + b.time.pixels_per_tick.ln() * t).exp();
    v.time.pixels_per_tick = ppt;
    v.time.origin_tick =
        (a.time.origin_tick as f64 * (1.0 - t) + b.time.origin_tick as f64 * t).round() as u64;
    v.pitch.row_height = (a.pitch.row_height.ln() * (1.0 - t) + b.pitch.row_height.ln() * t).exp();
    v.pitch.scroll_rows = a.pitch.scroll_rows * (1.0 - t) + b.pitch.scroll_rows * t;
    v
}

impl PlumSpiritApp {
    /// Jumps to `v`, remembering the current view for Back.
    fn zoom_jump(&mut self, v: RollView) {
        let cur = self.roll.view;
        let z = &mut self.zoom;
        z.history.truncate(z.pos);
        z.history.push(cur);
        if z.history.len() > 64 {
            z.history.remove(0);
        }
        z.pos = z.history.len();
        if z.animate {
            z.anim = Some((cur, v, Instant::now()));
        } else {
            self.roll.set_view(v);
        }
    }

    /// The view that fits a box (computed on a copy of the roll view).
    pub(super) fn zoom_fit(&mut self, t0: u64, t1: u64, lo: u8, hi: u8) {
        let keep = self.roll.view;
        self.roll.fit_box(t0, t1, lo, hi);
        let target = self.roll.view;
        self.roll.view = keep;
        self.zoom_jump(target);
    }

    pub(super) fn zoom_back(&mut self) {
        let z = &mut self.zoom;
        if z.pos == 0 {
            return;
        }
        if z.pos == z.history.len() {
            z.history.push(self.roll.view);
        }
        z.pos -= 1;
        let v = z.history[z.pos];
        self.roll.set_view(v);
    }

    pub(super) fn zoom_forward(&mut self) {
        let z = &mut self.zoom;
        if z.pos + 1 >= z.history.len() {
            return;
        }
        z.pos += 1;
        let v = z.history[z.pos];
        self.roll.set_view(v);
    }

    /// Advances an animated transition (called once per frame).
    pub(super) fn zoom_frame(&mut self, ctx: &egui::Context) {
        if let Some((a, b, t0)) = self.zoom.anim {
            let t = t0.elapsed().as_secs_f64() / ANIM.as_secs_f64();
            if t >= 1.0 {
                self.roll.set_view(b);
                self.zoom.anim = None;
            } else {
                let s = t * t * (3.0 - 2.0 * t);
                self.roll.set_view(lerp_view(a, b, s));
                ctx.request_repaint();
            }
        }
    }

    /// Bounding box (t0, t1, lo, hi) of the active track's notes that pass `f`.
    fn track_box(
        &self,
        f: impl Fn(&plumspirit_engine::model::NoteView) -> bool,
    ) -> Option<(u64, u64, u8, u8)> {
        let t = plumspirit_engine::editor::note_view(self.doc()?, self.edit_target()).ok()?;
        let mut b: Option<(u64, u64, u8, u8)> = None;
        for n in t.notes.iter() {
            if !f(&n) {
                continue;
            }
            let (s, e, p) = (n.start_tick, n.end_tick(), n.attrs.pitch());
            b = Some(match b {
                None => (s, e, p, p),
                Some((a, z, lo, hi)) => (a.min(s), z.max(e), lo.min(p), hi.max(p)),
            });
        }
        b
    }

    fn zoom_fit_track(&mut self) {
        match self.track_box(|_| true) {
            Some((a, z, lo, hi)) => self.zoom_fit(a, z, lo, hi),
            None => self.zoom.message = Some("The active track has no notes.".into()),
        }
    }

    fn zoom_fit_subchannels(&mut self) {
        let Some(t) = self
            .doc()
            .and_then(|d| plumspirit_engine::editor::note_view(d, self.edit_target()).ok())
        else {
            return;
        };
        let mut subs: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        t.notes.for_each_selected(|n| {
            subs.insert(n.sub.0);
        });
        if subs.is_empty()
            && let Some(s) = self.active_sub_of(self.active_track())
        {
            subs.insert(s.id.0);
        }
        match self.track_box(|n| subs.contains(&n.sub.0)) {
            Some((a, z, lo, hi)) => self.zoom_fit(a, z, lo, hi),
            None => self.zoom.message = Some("No notes in those subchannels.".into()),
        }
    }

    /// Density-aware fit: the 2nd–98th percentile of starts and pitches.
    fn zoom_auto_fit(&mut self) {
        let Some(t) = self
            .doc()
            .and_then(|d| plumspirit_engine::editor::note_view(d, self.edit_target()).ok())
        else {
            return;
        };
        let n = t.notes.len();
        if n == 0 {
            self.zoom.message = Some("The active track has no notes.".into());
            return;
        }
        let mut starts: Vec<u64> = Vec::with_capacity(n);
        // Every key a note word can hold (0–255, v0.9 256-Key Mode).
        let mut ph = [0u64; 256];
        for v in t.notes.iter() {
            starts.push(v.start_tick);
            ph[usize::from(v.attrs.pitch())] += 1;
        }
        let q = |v: &mut Vec<u64>, f: f64| -> u64 {
            let k = ((v.len() - 1) as f64 * f).round() as usize;
            *v.select_nth_unstable(k).1
        };
        let (a, z) = (q(&mut starts, 0.02), q(&mut starts, 0.98));
        let total = n as u64;
        let pq = |f: f64| -> u8 {
            let want = (total as f64 * f).ceil() as u64;
            let mut acc = 0;
            for (p, &c) in ph.iter().enumerate() {
                acc += c;
                if acc >= want.max(1) {
                    return p as u8;
                }
            }
            u8::MAX
        };
        let beat = self
            .doc()
            .and_then(|d| d.division().musical_ppqn())
            .unwrap_or(960);
        self.zoom_fit(a, z.max(a + beat), pq(0.02), pq(0.98));
    }

    /// Clicking the overview: centre the Piano Roll there.
    fn zoom_center_on(&mut self, tick: u64, pitch: f64) {
        let mut v = self.roll.view;
        let w = self.roll.view_width();
        let half = (w / 2.0 / v.time.pixels_per_tick) as u64;
        v.time.origin_tick = tick.saturating_sub(half);
        let h = self.roll.view_height();
        v.center_pitch(pitch, h);
        self.roll.set_view(v);
    }

    /// Counts per overview cell for the active track (cached).
    fn zoom_overview(&mut self) -> Option<Overview> {
        let t = plumspirit_engine::editor::note_view(self.doc()?, self.edit_target()).ok()?;
        let key = (
            crate::piano_roll::render::host_key(t.host()),
            t.notes.revision(),
        );
        if self.zoom.overview.as_ref().map(|o| o.0) != Some(key) {
            let mut b: Option<(u64, u64, u8, u8)> = None;
            for n in t.notes.iter() {
                let (s, e, p) = (n.start_tick, n.end_tick(), n.attrs.pitch());
                b = Some(match b {
                    None => (s, e, p, p),
                    Some((a, z, lo, hi)) => (a.min(s), z.max(e), lo.min(p), hi.max(p)),
                });
            }
            let (a, z, lo, hi) = b.unwrap_or((0, 1, 60, 60));
            let span = z.saturating_sub(a).max(1);
            let rows = f64::from(hi - lo) + 1.0;
            let mut cells = vec![0u32; OV_COLS * OV_ROWS];
            for n in t.notes.iter() {
                let c0 = ((n.start_tick - a) as f64 / span as f64 * OV_COLS as f64) as usize;
                let c1 =
                    (((n.end_tick() - a) as f64 / span as f64 * OV_COLS as f64) as usize).max(c0);
                let r = ((f64::from(n.attrs.pitch() - lo) / rows) * OV_ROWS as f64) as usize;
                let r = OV_ROWS - 1 - r.min(OV_ROWS - 1);
                for c in c0.min(OV_COLS - 1)..=c1.min(OV_COLS - 1) {
                    cells[r * OV_COLS + c] += 1;
                }
            }
            self.zoom.overview = Some((key, (cells, (a, z), (lo, hi))));
        }
        self.zoom.overview.as_ref().map(|o| o.1.clone())
    }

    /// View → Zoom Tool… window.
    pub(super) fn zoom_window_ui(&mut self, ctx: &egui::Context) {
        if !self.zoom.open || self.open.is_none() {
            return;
        }
        enum A {
            Save,
            Go(usize),
            Delete(usize),
            FitTrack,
            FitSubs,
            FitPitch,
            FitTime,
            Auto,
            Back,
            Forward,
            Center(u64, f64),
        }
        let mut act: Option<A> = None;
        let mut open = true;
        let regions = self
            .doc()
            .map(|d| d.project().zoom_regions.clone())
            .unwrap_or_default();
        let overview = self.zoom_overview();
        let view = self.roll.view;
        let (vw, vh) = (self.roll.view_width(), self.roll.view_height());
        let pal = &crate::ui::theme::ui_palette(ctx);
        let (can_back, can_fwd) = (
            self.zoom.pos > 0,
            self.zoom.pos + 1 < self.zoom.history.len(),
        );
        let mut z = std::mem::take(&mut self.zoom);
        egui::Window::new("Zoom")
            .id(egui::Id::new("plumspirit-zoom"))
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-340.0, 120.0))
            .open(&mut open)
            .default_width(300.0)
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                // 6. Overview (synchronized with the Piano Roll).
                if let Some((cells, (a, zt), (lo, hi))) = &overview {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 72.0),
                        egui::Sense::click_and_drag(),
                    );
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, 0.0, pal.roll_black_row);
                    let max = cells.iter().copied().max().unwrap_or(1).max(1);
                    let cw = rect.width() / OV_COLS as f32;
                    let ch = rect.height() / OV_ROWS as f32;
                    let mut mesh = egui::Mesh::default();
                    for r in 0..OV_ROWS {
                        for c in 0..OV_COLS {
                            let n = cells[r * OV_COLS + c];
                            if n == 0 {
                                continue;
                            }
                            let f = (f64::from(n).ln_1p() / f64::from(max).ln_1p()) as f32;
                            let col = pal.accent.gamma_multiply(0.35 + 0.65 * f);
                            mesh.add_colored_rect(
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(c as f32 * cw, r as f32 * ch),
                                    egui::vec2(cw.max(1.0), ch.max(1.0)),
                                ),
                                col,
                            );
                        }
                    }
                    p.add(egui::Shape::mesh(mesh));
                    // The Piano Roll's window.
                    let span = zt.saturating_sub(*a).max(1) as f64;
                    let rows = f64::from(hi - lo) + 1.0;
                    let x_of = |t: f64| rect.left() + ((t - *a as f64) / span) as f32 * rect.width();
                    let t0 = view.time.origin_tick as f64;
                    let t1 = t0 + vw / view.time.pixels_per_tick;
                    let top_pitch = view.pitch.rows() - view.pitch.scroll_rows;
                    let bottom_pitch = top_pitch - vh / view.pitch.row_height;
                    let y_of = |pp: f64| {
                        rect.bottom() - (((pp - f64::from(*lo)) / rows) as f32) * rect.height()
                    };
                    let win = egui::Rect::from_min_max(
                        egui::pos2(x_of(t0), y_of(top_pitch)),
                        egui::pos2(x_of(t1), y_of(bottom_pitch)),
                    )
                    .intersect(rect);
                    p.rect_stroke(win, 0.0, egui::Stroke::new(1.5, pal.text), egui::StrokeKind::Inside);
                    if (resp.clicked() || resp.dragged())
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let t = *a as f64 + f64::from((pos.x - rect.left()) / rect.width()) * span;
                        let pp = f64::from(*lo)
                            + f64::from((rect.bottom() - pos.y) / rect.height()) * rows;
                        act = Some(A::Center(t.max(0.0) as u64, pp));
                    }
                    resp.on_hover_text("The active track; the frame is the Piano Roll's view. Click or drag to move the view.");
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(can_back, egui::Button::new("◀ Back")).on_hover_text(super::commands::Command::ZoomBack.shortcut()).clicked() {
                        act = Some(A::Back);
                    }
                    if ui.add_enabled(can_fwd, egui::Button::new("Forward ▶")).on_hover_text(super::commands::Command::ZoomForward.shortcut()).clicked() {
                        act = Some(A::Forward);
                    }
                    ui.checkbox(&mut z.animate, "Animate");
                    ui.checkbox(&mut z.lens, "Lens (hold Z)");
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Fit Track").clicked() {
                        act = Some(A::FitTrack);
                    }
                    if ui.button("Fit Subchannels").on_hover_text("The subchannels of the selected notes (or the active subchannel)").clicked() {
                        act = Some(A::FitSubs);
                    }
                    if ui.button("Auto-Fit").on_hover_text("The dense part of the track: 2nd–98th percentile of its notes").clicked() {
                        act = Some(A::Auto);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Pitches");
                    ui.add(egui::TextEdit::singleline(&mut z.pitch_from).desired_width(44.0));
                    ui.label("to");
                    ui.add(egui::TextEdit::singleline(&mut z.pitch_to).desired_width(44.0));
                    if ui.button("Fit").clicked() {
                        act = Some(A::FitPitch);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Bars");
                    ui.add(egui::TextEdit::singleline(&mut z.time_from).desired_width(44.0));
                    ui.label("to");
                    ui.add(egui::TextEdit::singleline(&mut z.time_to).desired_width(44.0));
                    if ui.button("Fit").clicked() {
                        act = Some(A::FitTime);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut z.name)
                            .hint_text("Region name")
                            .desired_width(150.0),
                    );
                    if ui
                        .add_enabled(regions.len() < MAX_ZOOM_REGIONS, egui::Button::new("Save View"))
                        .clicked()
                    {
                        act = Some(A::Save);
                    }
                });
                for (i, r) in regions.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button(&r.name).on_hover_text("Go to this view").clicked() {
                            act = Some(A::Go(i));
                        }
                        if ui.small_button("×").on_hover_text("Delete this region").clicked() {
                            act = Some(A::Delete(i));
                        }
                    });
                }
                if let Some(m) = &z.message {
                    ui.label(egui::RichText::new(m).weak());
                }
            });
        self.zoom = z;
        if !open {
            self.zoom.open = false;
        }
        let Some(a) = act else {
            return;
        };
        self.zoom.message = None;
        match a {
            A::Save => {
                let name = self.zoom.name.trim().to_owned();
                let name = if name.is_empty() {
                    format!("View {}", regions.len() + 1)
                } else {
                    name
                };
                let v = self.roll.view;
                let r = ZoomRegion::new(
                    &name,
                    v.time.origin_tick,
                    v.time.pixels_per_tick,
                    // Rows below pitch 127's row (the v0.8 meaning, so a saved
                    // view stays on its keys when 256-Key Mode changes).
                    v.pitch.scroll_rows - (f64::from(v.pitch.top) - 127.0),
                    v.pitch.row_height,
                );
                if let Some(d) = self.doc_mut() {
                    d.project_mut().zoom_regions.push(r);
                    self.zoom.name.clear();
                    self.set_status(
                        format!("Saved the view “{name}” (with the project)."),
                        StatusKind::Info,
                    );
                }
            }
            A::Go(i) => {
                if let Some(r) = regions.get(i) {
                    let mut v = self.roll.view;
                    v.time.origin_tick = r.origin_tick;
                    v.time.pixels_per_tick = r.pixels_per_tick();
                    v.pitch.scroll_rows = r.scroll_rows() + (f64::from(v.pitch.top) - 127.0);
                    v.pitch.row_height = r.row_height();
                    self.zoom_jump(v);
                }
            }
            A::Delete(i) => {
                if let Some(d) = self.doc_mut()
                    && i < d.project().zoom_regions.len()
                {
                    d.project_mut().zoom_regions.remove(i);
                }
            }
            A::FitTrack => self.zoom_fit_track(),
            A::FitSubs => self.zoom_fit_subchannels(),
            A::Auto => self.zoom_auto_fit(),
            A::FitPitch => {
                let keys = self.doc().map(|d| d.project().keys).unwrap_or_default();
                let p = |s: &str| plumspirit_engine::model::note::parse_pitch_in(s, keys);
                match (p(&self.zoom.pitch_from), p(&self.zoom.pitch_to)) {
                    (Ok(a), Ok(b)) => {
                        let v = self.roll.view;
                        let w = self.roll.view_width();
                        let t0 = v.time.origin_tick;
                        let t1 = t0 + (w / v.time.pixels_per_tick) as u64;
                        let keep = self.roll.view;
                        self.roll.fit_box(t0, t1, a.min(b), a.max(b));
                        let mut target = self.roll.view;
                        // Pitch only: keep the time axis.
                        target.time = keep.time;
                        self.roll.view = keep;
                        self.zoom_jump(target);
                    }
                    (Err(e), _) | (_, Err(e)) => self.zoom.message = Some(e),
                }
            }
            A::FitTime => {
                let meter = self.meter.as_ref().map(|m| m.1.clone());
                let r = meter.map(|m| {
                    (
                        m.parse_position(&self.zoom.time_from),
                        m.parse_position(&self.zoom.time_to),
                    )
                });
                match r {
                    Some((Ok(a), Ok(b))) if a != b => {
                        let keep = self.roll.view;
                        let top = self.roll.view.pitch.top;
                        self.roll.fit_box(a.min(b), a.max(b), 0, top);
                        let mut target = self.roll.view;
                        // Time only: keep the pitch axis.
                        target.pitch = keep.pitch;
                        self.roll.view = keep;
                        self.zoom_jump(target);
                    }
                    Some((Err(e), _)) | Some((_, Err(e))) => self.zoom.message = Some(e),
                    _ => self.zoom.message = Some("Enter two different positions.".into()),
                }
            }
            A::Back => self.zoom_back(),
            A::Forward => self.zoom_forward(),
            A::Center(t, p) => self.zoom_center_on(t, p),
        }
    }

    /// The magnifier lens (function 7): while Z is held over the Piano Roll,
    /// the notes around the pointer are drawn 3× larger in a box next to it.
    pub(super) fn draw_zoom_lens(&self, ctx: &egui::Context) {
        if !self.zoom.open || !self.zoom.lens || ctx.egui_wants_keyboard_input() {
            return;
        }
        let held = ctx.input(|i| i.key_down(egui::Key::Z) && !i.modifiers.command);
        let Some(area) = self.roll.notes_rect() else {
            return;
        };
        let Some(pos) = ctx.pointer_hover_pos().filter(|p| area.contains(*p)) else {
            return;
        };
        if !held {
            return;
        }
        let Some(t) = self
            .doc()
            .and_then(|d| plumspirit_engine::editor::note_view(d, self.edit_target()).ok())
        else {
            return;
        };
        let v = self.roll.view;
        let mag = 3.0;
        let size = egui::vec2(260.0, 170.0);
        let mut lens = egui::Rect::from_min_size(pos + egui::vec2(18.0, 18.0), size);
        if lens.right() > area.right() {
            lens = lens.translate(egui::vec2(-size.x - 36.0, 0.0));
        }
        if lens.bottom() > area.bottom() {
            lens = lens.translate(egui::vec2(0.0, -size.y - 36.0));
        }
        let ppt = v.time.pixels_per_tick * mag;
        let rh = v.pitch.row_height * mag;
        let tick_at = |x: f32| {
            v.time.origin_tick as f64 + f64::from(x - area.left()) / v.time.pixels_per_tick
        };
        let pitch_at = |y: f32| {
            v.pitch.rows() - (f64::from(y - area.top()) / v.pitch.row_height + v.pitch.scroll_rows)
        };
        let (ct, cp) = (tick_at(pos.x), pitch_at(pos.y));
        let half_t = f64::from(size.x) / 2.0 / ppt;
        let half_p = f64::from(size.y) / 2.0 / rh;
        let t0 = (ct - half_t).max(0.0) as u64;
        let t1 = (ct + half_t).max(0.0) as u64 + 1;
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("zoom-lens"),
        ));
        let pal = &crate::ui::theme::ui_palette(ctx);
        let palette = crate::ui::theme::note_palette(ctx);
        let dark = ctx.theme() == egui::Theme::Dark;
        painter.rect_filled(lens, 0.0, pal.panel_bg);
        let p = painter.with_clip_rect(lens);
        let x_of = |tick: f64| lens.center().x + ((tick - ct) * ppt) as f32;
        let y_of = |pitch: f64| lens.center().y - ((pitch - cp) * rh) as f32;
        let mut mesh = egui::Mesh::default();
        let mut drawn = 0usize;
        t.notes.query(t0, t1, |_, n| {
            if drawn >= LENS_NOTES {
                return;
            }
            let pf = f64::from(n.attrs.pitch());
            if (pf - cp).abs() > half_p + 1.0 {
                return;
            }
            let r = egui::Rect::from_min_max(
                egui::pos2(x_of(n.start_tick as f64), y_of(pf + 1.0)),
                egui::pos2(
                    x_of(n.end_tick() as f64).max(x_of(n.start_tick as f64) + 2.0),
                    y_of(pf),
                ),
            );
            let c = crate::ui::theme::subchannel_color(&palette, n.sub.0, dark);
            mesh.add_colored_rect(r.shrink(0.5), c);
            drawn += 1;
        });
        p.add(egui::Shape::mesh(mesh));
        painter.rect_stroke(
            lens,
            0.0,
            egui::Stroke::new(1.0, pal.text_weak),
            egui::StrokeKind::Inside,
        );
        painter.text(
            lens.left_bottom() + egui::vec2(6.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            "×3",
            egui::FontId::proportional(11.0),
            pal.text_weak,
        );
    }
}

/// B1-S43 (B1-0087 – B1-0090): Fit 128 / 256 Keys, Lock Fit Range, the Y
/// zoom floor at the keys, and typed zooms beyond the limits clamped, through
/// whole application frames.
#[cfg(test)]
mod fit_tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use crate::app::track_list::MainView;
    use plumspirit_engine::history::Operation;
    use plumspirit_engine::model::project::KeyRange;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use plumspirit_engine::view::{MAX_PIXELS_PER_TICK, MAX_ROW_HEIGHT, MIN_ROW_HEIGHT};

    fn song(ppqn: u32) -> PlumDocument {
        let mut d = PlumDocument::new_project("fit".into(), Bpm::DEFAULT, ppqn);
        let q = u64::from(ppqn);
        let recs: Vec<NoteRecord> = (0..32u64)
            .map(|k| {
                NoteRecord::new(
                    k * q / 2,
                    q / 2,
                    NoteAttrs::new(48 + (k % 24) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    /// Frames of the Piano Roll on a `w` × `ht` point window at `native`
    /// pixels per point (the first frame of a document picks its own view).
    fn roll(h: &mut Harness, w: f32, ht: f32, native: f32) {
        let size = egui::vec2(w, ht) * native;
        if h.app.main_view != MainView::PianoRoll {
            h.app_frame(size, native);
            h.app.set_main_view(MainView::PianoRoll);
        }
        h.app_frame(size, native);
        h.app_frame(size, native);
    }

    fn fits(h: &Harness, rows: f64) -> bool {
        let r = &h.app.roll;
        (r.view.pitch.row_height * rows - r.view_height()).abs() < 1e-6
    }

    /// L-F11 (contract §008): in the Piano Roll, Ctrl+Shift+wheel (a mouse
    /// wheel or a two-finger trackpad scroll) zooms the pitch rows around the
    /// pointer — the key under it stays — and never the time axis; Ctrl alone
    /// zooms time and keeps the rows; a plain wheel scrolls without zooming;
    /// fractional deltas zoom fractionally.
    #[test]
    fn ctrl_shift_wheel_zooms_the_pitch_rows_around_the_pointer() {
        let mut h = Harness::new(song(960));
        roll(&mut h, 1400.0, 900.0, 1.0);
        let area = h.app.roll.notes_rect().expect("the note area");
        let p = egui::pos2(area.center().x, area.top() + area.height() * 0.3);
        let wheel = |h: &mut Harness, delta: egui::Vec2, m: egui::Modifiers| {
            let size = egui::vec2(1400.0, 900.0);
            h.app_frame_events(size, 1.0, vec![egui::Event::PointerMoved(p)]);
            h.app_frame_events(
                size,
                1.0,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta,
                        modifiers: m,
                        phase: egui::TouchPhase::Move,
                    },
                ],
            );
            h.app_frame(size, 1.0);
        };
        let key_at = |h: &Harness| {
            h.app
                .roll
                .view
                .pitch
                .y_to_pitch(f64::from(p.y - area.top()))
        };
        let cs = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        let (rh, key, ppt) = (
            h.app.roll.view.pitch.row_height,
            key_at(&h),
            h.app.roll.view.time.pixels_per_tick,
        );
        wheel(&mut h, egui::vec2(0.0, 80.0), cs);
        assert!(h.app.roll.view.pitch.row_height > rh * 1.2);
        assert_eq!(key_at(&h), key, "the key under the pointer stays");
        assert_eq!(h.app.roll.view.time.pixels_per_tick, ppt, "time untouched");
        // Fractional.
        let before = h.app.roll.view.pitch.row_height;
        wheel(&mut h, egui::vec2(0.0, 0.5), cs);
        let after = h.app.roll.view.pitch.row_height;
        assert!(
            after > before && after < before * 1.01,
            "{before} → {after}"
        );
        // Ctrl alone: time, the rows stay.
        wheel(&mut h, egui::vec2(0.0, 120.0), egui::Modifiers::COMMAND);
        assert!(h.app.roll.view.time.pixels_per_tick > ppt);
        assert_eq!(h.app.roll.view.pitch.row_height, after);
        // A plain wheel scrolls the rows, no zoom.
        let scroll = h.app.roll.view.pitch.scroll_rows;
        wheel(&mut h, egui::vec2(0.0, -60.0), egui::Modifiers::NONE);
        assert_eq!(h.app.roll.view.pitch.row_height, after);
        assert_ne!(h.app.roll.view.pitch.scroll_rows, scroll);
    }

    /// L-F02 (contract §010; Flopp: "particularly for vertical zoom which i
    /// personally prefer at 128"): with Default Y View = Fit All Keys every
    /// new view — a project created or opened, Reset View — shows all keys
    /// exactly (128, or 256 in 256-Key Mode), stays fitted on resize (Lock
    /// Fit Range), is kept across Track List / Piano Roll switches, and the
    /// choice survives a restart.
    #[test]
    fn a_remembered_fit_all_keys_view_opens_every_project_and_survives_switches() {
        let mut h = Harness::new(song(960));
        h.app.settings.default_fit_keys = true;
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 128.0), "{}", h.app.roll.view.pitch.row_height);
        assert_eq!(h.app.roll.view.pitch.scroll_rows, 0.0);
        // Resized: still fitted (Lock Fit Range, on by default).
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert!(fits(&h, 128.0));
        // A zoom of the user's own is kept across Track List ↔ Piano Roll.
        assert!(h.command(Command::ZoomInY));
        roll(&mut h, 1400.0, 700.0, 1.0);
        let zoomed = h.app.roll.view.pitch.row_height;
        h.app.set_main_view(MainView::TrackList);
        h.app_frame(egui::vec2(1400.0, 700.0), 1.0);
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert_eq!(h.app.roll.view.pitch.row_height, zoomed, "the view is kept");
        // Reset View: the overview again.
        assert!(h.command(Command::ResetView));
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert!(fits(&h, 128.0));
        // Close the project, create another: the overview.
        h.app.request_close_project();
        h.app.create_project("b", "120", "960", false).unwrap();
        // Every project opens in the Track List (its first frame).
        h.app_frame(egui::vec2(1400.0, 700.0), 1.0);
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert!(fits(&h, 128.0));
        // A 256-Key Mode project: all 256 keys.
        h.app.request_close_project();
        h.app.create_project("c", "120", "960", true).unwrap();
        // Every project opens in the Track List (its first frame).
        h.app_frame(egui::vec2(1400.0, 700.0), 1.0);
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 256.0), "{}", h.app.roll.view.pitch.row_height);
        assert!(
            h.app.roll.view.pitch.row_height >= MIN_ROW_HEIGHT,
            "no zero-pixel rows"
        );
        // A restart reads the choice back.
        let back = crate::settings::parse(&crate::settings::to_json(&h.app.settings));
        assert!(back.settings.default_fit_keys);
        // Off: a new view starts at Default Zoom Y again.
        h.app.settings.default_fit_keys = false;
        assert!(h.command(Command::ResetView));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(!fits(&h, 256.0));
    }

    #[test]
    fn fit_128_keys_fills_the_height_exactly_and_lock_keeps_it_when_the_height_changes() {
        let mut h = Harness::new(song(960));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(h.command(Command::FitKeys128));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 128.0), "{}", h.app.roll.view.pitch.row_height);
        assert_eq!(h.app.roll.view.pitch.scroll_rows, 0.0);
        let hv = h.app.roll.view_height();
        assert_eq!(h.app.roll.view.pitch.visible_pitches(hv - 1e-6), (0, 127));
        // The window (or the graph panels) change the roll's height: Lock
        // Fit Range keeps the 128 keys fitted.
        let before = h.app.roll.view_height();
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert!(h.app.roll.view_height() < before);
        assert!(fits(&h, 128.0));
        h.app.settings.velocity_lane_visible = !h.app.settings.velocity_lane_visible;
        roll(&mut h, 1400.0, 700.0, 1.0);
        assert!(fits(&h, 128.0), "a graph panel shown / hidden");
        // High DPI: the same points, finer pixels.
        roll(&mut h, 1400.0, 700.0, 2.0);
        assert!(fits(&h, 128.0));
        // Lock off: a height change leaves the row height alone (no gap: the
        // floor at the keys still fills a taller roll).
        assert!(h.command(Command::ToggleLockFitRange));
        assert!(!h.app.settings.lock_fit_range);
        let rh = h.app.roll.view.pitch.row_height;
        roll(&mut h, 1400.0, 600.0, 1.0);
        assert_eq!(h.app.roll.view.pitch.row_height, rh);
        assert!(h.command(Command::ToggleLockFitRange));
        // A Y zoom ends the fit.
        assert!(h.command(Command::FitKeys128));
        roll(&mut h, 1400.0, 600.0, 1.0);
        assert!(h.command(Command::ZoomInY));
        roll(&mut h, 1400.0, 600.0, 1.0);
        let rh = h.app.roll.view.pitch.row_height;
        roll(&mut h, 1400.0, 500.0, 1.0);
        assert_eq!(h.app.roll.fitted_rows(), None);
        assert_eq!(h.app.roll.view.pitch.row_height, rh, "no longer fitted");
    }

    #[test]
    fn zoom_out_y_stops_at_the_keys_unless_beyond_is_on() {
        let mut h = Harness::new(song(960));
        assert!(!h.app.settings.y_zoom_beyond_keys, "off by default");
        roll(&mut h, 1400.0, 900.0, 1.0);
        for _ in 0..40 {
            h.command(Command::ZoomOutY);
            roll(&mut h, 1400.0, 900.0, 1.0);
        }
        assert!(fits(&h, 128.0), "{}", h.app.roll.view.pitch.row_height);
        h.app.commit_zoom_text("1", false);
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 128.0), "a typed zoom stops there too");
        // A taller window does not change the zoom (B1-0062); the next Zoom
        // Out Y does not go further, Zoom In Y still works.
        let rh = h.app.roll.view.pitch.row_height;
        roll(&mut h, 1400.0, 1100.0, 1.0);
        assert_eq!(h.app.roll.view.pitch.row_height, rh);
        h.command(Command::ZoomOutY);
        roll(&mut h, 1400.0, 1100.0, 1.0);
        assert_eq!(h.app.roll.view.pitch.row_height, rh);
        h.command(Command::ZoomInY);
        roll(&mut h, 1400.0, 1100.0, 1.0);
        assert!(h.app.roll.view.pitch.row_height > rh);
        assert!(h.command(Command::ToggleYZoomBeyondKeys));
        for _ in 0..40 {
            h.command(Command::ZoomOutY);
            roll(&mut h, 1400.0, 1100.0, 1.0);
        }
        assert_eq!(h.app.roll.view.pitch.row_height, MIN_ROW_HEIGHT);
        assert!(h.app.roll.view.pitch.row_height * 128.0 < h.app.roll.view_height());
    }

    #[test]
    fn fit_all_256_keys_and_fit_128_in_256_key_mode() {
        let mut h = Harness::new(song(960));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(!h.app.enabled(Command::FitAllKeys), "standard keys");
        h.app.apply_op(Operation::SetKeyRange {
            keys: KeyRange::Extended256,
        });
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(h.command(Command::FitAllKeys));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 256.0));
        assert_eq!(h.app.roll.view.pitch.scroll_rows, 0.0);
        // Fit 128 Keys shows the standard MIDI keys 0 – 127 (rows 128 – 255).
        assert!(h.command(Command::FitKeys128));
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(fits(&h, 128.0));
        assert_eq!(h.app.roll.view.pitch.scroll_rows, 128.0);
        let hv = h.app.roll.view_height();
        assert_eq!(h.app.roll.view.pitch.visible_pitches(hv - 1e-6), (0, 127));
    }

    #[test]
    fn typed_zooms_beyond_the_limits_are_clamped_and_30720_ppqn_works() {
        let mut h = Harness::new(song(30_720));
        roll(&mut h, 1400.0, 900.0, 1.0);
        h.app.status = None;
        h.app.commit_zoom_text("1e12", true);
        h.app.commit_zoom_text("1e9", false);
        roll(&mut h, 1400.0, 900.0, 1.0);
        assert!(h.app.status.is_none(), "accepted, not refused");
        assert_eq!(h.app.roll.view.time.pixels_per_tick, MAX_PIXELS_PER_TICK);
        assert_eq!(h.app.roll.view.pitch.row_height, MAX_ROW_HEIGHT);
        h.app.commit_zoom_text("-3", true);
        assert!(h.app.status.is_some(), "not a positive number");
        // Reset Zoom and Fit Song at 30,720 PPQN.
        assert!(h.command(Command::ResetZoom));
        roll(&mut h, 1400.0, 900.0, 1.0);
        let ppt = h.app.roll.view.time.pixels_per_tick;
        assert!((ppt - 32.0 / 30_720.0).abs() < 1e-12, "{ppt}");
        assert!(h.command(Command::FitSong));
        roll(&mut h, 1400.0, 900.0, 1.0);
        let (t0, t1) = h.app.roll.view.time.visible_range(h.app.roll.view_width());
        assert!(t0 == 0 && t1 >= 16 * 30_720, "{t0}..{t1}");
        // The Track List: row height beyond its limit is clamped too.
        h.app.main_view = MainView::TrackList;
        h.app.commit_zoom_text("1e6", false);
        assert_eq!(
            h.app.track_list.row_percent(),
            f64::from(
                super::super::track_list::MAX_ROW_H / super::super::track_list::DEFAULT_ROW_H
            ) * 100.0
        );
    }
}
