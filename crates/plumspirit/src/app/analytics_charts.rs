//! The analytics chart panel (v1.0-Launch L-E03 … L-E08): the MIDI
//! Inspector's NPS / Polyphony / Tempo charts (B1-0108 / B1-0109 / B2-F30),
//! made a component so Selection Analytics shows the same exact charts over
//! the selected notes only — plus a cumulative note counter.
//!
//! Every NPS / Polyphony column is the exact maximum over its own span at any
//! zoom (`inspector::Chart::sync_in` on the scope's own peak profile); the
//! headline maxima and the Peak buttons come from the profile's exact peaks,
//! never from the drawing grid. One visible window for all charts
//! (Ctrl+wheel or X −/+ zoom at the pointer, wheel / Shift+wheel or a right /
//! middle drag pans, the scrollbar, Fit, Peak jumps), each chart its own
//! vertical zoom (Alt+wheel or Y −/+, Fit Y); a double-click fits both. The
//! Tempo chart draws the Conductor's tempo as it plays (steps and linear BPM
//! ramps, its points marked).

use crate::inspector::{self, Axis, Chart, ChartView, Window, YView};
use plumspirit_engine::analysis::{AnalysisIndex, Timing};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::view::grid::MeterMap;
use std::sync::Arc;

/// Height of each chart in points.
pub const CHART_HEIGHT: f32 = 72.0;

/// B2-F30: reuse the editor's page-follow policy on the chart's own axis.
/// Page turns keep exact indexed column queries off the per-playback-frame
/// path. A view that already shows everything contains every position.
pub fn followed_window(w: Window, timing: &Timing, end: u64, tick: u64, anchor: u8) -> Window {
    use crate::piano_roll::viewport::{FollowStyle, follow_origin};
    use plumspirit_engine::view::TimeAxis;
    if w.is_full() {
        return w;
    }
    let (extent, position) = match w.axis {
        Axis::Time => (timing.nanos_at(end).max(1), timing.nanos_at(tick)),
        Axis::Musical => (end.max(1), tick),
    };
    // Width is normalized to 1; these units can be nanoseconds, so the
    // editor's pixels-per-tick zoom limits do not apply here.
    let time = TimeAxis {
        origin_tick: (w.lo * extent as f64) as u64,
        pixels_per_tick: 1.0 / (w.span() * extent as f64),
    };
    match follow_origin(
        &time,
        1.0,
        position,
        extent,
        FollowStyle::Page,
        f64::from(anchor.min(100)) / 100.0,
    ) {
        Some(origin) => {
            let lo = (origin as f64 / extent as f64).clamp(0.0, 1.0 - w.span());
            Window {
                lo,
                hi: lo + w.span(),
                ..w
            }
        }
        None => w,
    }
}

/// What a chart's pointer did this frame.
#[derive(Default)]
pub struct ChartInput {
    /// Seek to this fraction of the visible width (click / drag, V09-0058).
    pub seek: Option<f32>,
    /// Zoom the window by this factor at this fraction of the width.
    pub zoom_x: Option<(f64, f64)>,
    /// Zoom this chart vertically by this factor at this height fraction.
    pub zoom_y: Option<(f64, f64)>,
    /// Pan by these visible widths / heights (right or middle drag, wheel).
    pub pan: (f64, f64),
    /// Double-click: fit.
    pub fit: bool,
    /// The pointer's fraction of the width while it hovers.
    pub hover: Option<f64>,
}

/// One chart's canvas: allocates it, reads wheel / drag / click (see the
/// module documentation) and returns the rectangle and what to do.
pub fn chart_canvas(ui: &mut egui::Ui, hint: &str) -> (egui::Rect, ChartInput) {
    let width = ui.available_width().max(80.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, CHART_HEIGHT),
        egui::Sense::click_and_drag(),
    );
    let resp = resp.on_hover_text(hint);
    let mut out = ChartInput::default();
    let at = |p: egui::Pos2| {
        (
            f64::from((p.x - rect.left()) / rect.width()),
            f64::from((rect.bottom() - p.y) / rect.height()),
        )
    };
    if resp.double_clicked() {
        out.fit = true;
    } else if resp.dragged_by(egui::PointerButton::Primary) || resp.clicked() {
        out.seek = resp
            .interact_pointer_pos()
            .map(|p| (p.x - rect.left()) / rect.width());
    }
    if resp.dragged_by(egui::PointerButton::Secondary)
        || resp.dragged_by(egui::PointerButton::Middle)
    {
        let d = resp.drag_delta();
        out.pan = (
            -f64::from(d.x / rect.width()),
            f64::from(d.y / rect.height()),
        );
    }
    if let Some(p) = resp.hover_pos() {
        let (fx, fy) = at(p);
        out.hover = Some(fx.clamp(0.0, 1.0));
        let events = ui.input(|i| i.events.clone());
        for e in &events {
            let egui::Event::MouseWheel {
                unit,
                delta,
                modifiers: m,
                ..
            } = e
            else {
                continue;
            };
            let scale = match unit {
                egui::MouseWheelUnit::Point => 1.0,
                egui::MouseWheelUnit::Line => 40.0,
                egui::MouseWheelUnit::Page => rect.width(),
            };
            let d = f64::from((delta.y + delta.x) * scale);
            if d == 0.0 {
                continue;
            }
            let factor = crate::piano_roll::viewport::wheel_factor(d);
            if m.alt {
                out.zoom_y = Some((factor, fy));
            } else if m.command {
                out.zoom_x = Some((factor, fx));
            } else {
                out.pan.0 -= d / f64::from(rect.width().max(1.0));
            }
        }
        if out.zoom_x.is_some() || out.zoom_y.is_some() || out.pan.0 != 0.0 {
            // A chart gesture belongs to the chart, rather than also
            // scrolling the enclosing window.
            ui.input_mut(|i| {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
        }
    }
    (rect, out)
}

/// A thin scrollbar: `[lo, hi]` of `[0, 1]` shown; returns the new `lo`
/// when the thumb is dragged or the track clicked (a page).
pub fn scrollbar(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    lo: f64,
    hi: f64,
    vertical: bool,
    id: &str,
) -> Option<f64> {
    let resp = ui.interact(rect, ui.id().with(id), egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let v = ui.visuals();
    p.rect_filled(rect, 0.0, v.extreme_bg_color);
    let span = (hi - lo).clamp(0.0, 1.0);
    let (a, b) = if vertical {
        (
            rect.bottom() - (hi as f32) * rect.height(),
            rect.bottom() - (lo as f32) * rect.height(),
        )
    } else {
        (
            rect.left() + (lo as f32) * rect.width(),
            rect.left() + (hi as f32) * rect.width(),
        )
    };
    let thumb = if vertical {
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), a),
            egui::pos2(rect.right(), b.max(a + 6.0)),
        )
    } else {
        egui::Rect::from_min_max(
            egui::pos2(a, rect.top()),
            egui::pos2(b.max(a + 6.0), rect.bottom()),
        )
    };
    let fill = if resp.hovered() || resp.dragged() {
        v.widgets.hovered.bg_fill
    } else {
        v.widgets.inactive.bg_fill
    };
    p.rect_filled(thumb, 0.0, fill);
    let len = if vertical {
        rect.height()
    } else {
        rect.width()
    }
    .max(1.0);
    if resp.dragged() {
        let d = resp.drag_delta();
        let delta = if vertical { -d.y } else { d.x } / len;
        return Some((lo + f64::from(delta)).clamp(0.0, 1.0 - span));
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = if vertical {
            (rect.bottom() - pos.y) / len
        } else {
            (pos.x - rect.left()) / len
        };
        let f = f64::from(f);
        let page = if f < lo {
            lo - span
        } else if f > hi {
            lo + span
        } else {
            lo
        };
        return Some(page.clamp(0.0, 1.0 - span));
    }
    None
}

/// A BPM for labels: grouped digits, up to 3 decimals (7 under 10 BPM, for
/// extreme slow tempos), trailing zeros dropped.
pub fn bpm_label(bpm: f64) -> String {
    if !bpm.is_finite() {
        return "—".into();
    }
    let decimals = if bpm < 10.0 { 7 } else { 3 };
    let s = format!("{bpm:.decimals$}");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let frac = frac.trim_end_matches('0');
    let int = int
        .parse::<u64>()
        .map_or_else(|_| int.to_string(), group_digits);
    if frac.is_empty() {
        int
    } else {
        format!("{int}.{frac}")
    }
}

/// The value range a chart shows: `[lo, hi]` of the full range through its
/// vertical view.
fn shown(y: YView, lo: f64, hi: f64) -> (f64, f64) {
    y.range(lo, hi.max(lo + f64::MIN_POSITIVE))
}

/// The note counter chart (L-E06): one value per chart column — the notes
/// started by the end of that column, exact.
pub struct Counter {
    pub title: String,
    pub hint: &'static str,
    pub values: Vec<u64>,
}

/// What the panel shows (all of it read-only; the caller keeps the state).
pub struct ChartsData<'a> {
    /// Prefix of the panel's widget ids (two panels can be open at once).
    pub id: &'static str,
    pub timing: &'a Timing,
    /// The chart domain is `[0, end]` (the song, or up to the selection's
    /// last end); windows are fractions of it.
    pub end: u64,
    pub chart: &'a Chart,
    /// The analysis the chart should describe (older columns stay drawn,
    /// marked "…", until its columns are ready).
    pub index: &'a Arc<AnalysisIndex>,
    /// The exact maxima of the whole scope (`None`: still computed).
    pub maxima: (Option<u64>, Option<u64>),
    /// The headline's tooltip.
    pub max_hint: &'static str,
    /// Where the highest NPS / polyphony is first reached.
    pub peak_ticks: (Option<u64>, Option<u64>),
    pub playhead: u64,
    pub meter: Option<MeterMap>,
    pub tempo_points: &'a [u64],
    pub ppqn: Option<u64>,
    /// What Fit and a double-click show.
    pub fit: Window,
    pub fit_hint: &'static str,
    /// A click / drag on a chart moves the playhead.
    pub seek: bool,
    /// The Follow Playhead checkbox's state (`None`: no checkbox).
    pub follow: Option<bool>,
    pub follow_hint: &'static str,
    /// The cumulative note counter (Selection Analytics).
    pub counter: Option<Counter>,
    /// Exact values at a tick for the hover readout (`None`: the column's).
    pub exact_at: Option<&'a dyn Fn(u64) -> String>,
}

/// What the panel's controls asked for.
#[derive(Default)]
pub struct ChartsOut {
    /// Move the playhead here.
    pub seek: Option<u64>,
    /// The Follow Playhead checkbox was changed to this.
    pub follow: Option<bool>,
    /// Resume following was pressed.
    pub resume: bool,
    /// The columns shown describe an older window (an update is running).
    pub busy: bool,
}

/// The toolbar, the NPS / Polyphony / (counter) / Tempo charts and the
/// scrollbar. `view` is the visible window and vertical zooms; manual
/// navigation sets `follow_suspended`; `rects` receives where the charts
/// were painted (tests click them).
pub fn charts_ui(
    ui: &mut egui::Ui,
    view_state: &mut ChartView,
    follow_suspended: &mut bool,
    rects_out: &mut Vec<egui::Rect>,
    d: &ChartsData<'_>,
) -> ChartsOut {
    let mut out = ChartsOut::default();
    let timing = d.timing;
    let end = d.end.max(1);
    let mut view = *view_state;
    let w = view.window;
    let id = d.id;
    // ------------------------------------------------------- toolbar
    if let Some(mut follow) = d.follow {
        ui.horizontal_wrapped(|ui| {
            if ui
                .checkbox(&mut follow, "Follow Playhead")
                .on_hover_text(d.follow_hint)
                .changed()
            {
                out.follow = Some(follow);
            }
            if follow && *follow_suspended {
                ui.label(egui::RichText::new("Following paused").weak());
                if ui
                    .button("Resume")
                    .on_hover_text("Bring the playhead into view and resume following.")
                    .clicked()
                {
                    out.resume = true;
                }
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Axis");
        let mut axis = w.axis;
        ui.selectable_value(&mut axis, Axis::Time, "Time")
            .on_hover_text("Seconds through the tempo map.");
        ui.selectable_value(&mut axis, Axis::Musical, "Bars")
            .on_hover_text("Musical time: ticks, bars evenly spaced.");
        if axis != w.axis {
            // Keep the same part in view.
            let (t0, t1) = (
                inspector::tick_of(timing, w.axis, end, w.lo),
                inspector::tick_of(timing, w.axis, end, w.hi),
            );
            let lo = inspector::fraction_of(timing, axis, end, t0);
            let hi = inspector::fraction_of(timing, axis, end, t1);
            view.window = Window {
                axis,
                lo,
                hi: hi.max(lo + inspector::MIN_SPAN),
            };
        }
        ui.separator();
        ui.label("X");
        if ui
            .small_button("\u{2212}")
            .on_hover_text("Zoom out (Ctrl+wheel over a chart)")
            .clicked()
        {
            view.window = view.window.zoomed(0.5, 0.5);
        }
        if ui
            .small_button("+")
            .on_hover_text("Zoom in (Ctrl+wheel over a chart)")
            .clicked()
        {
            view.window = view.window.zoomed(2.0, 0.5);
        }
        if ui.small_button("Fit").on_hover_text(d.fit_hint).clicked() {
            view.window = fit_on(timing, end, d.fit, view.window.axis);
        }
        let fit = fit_on(timing, end, d.fit, view.window.axis);
        ui.label(format!("{:.0} %", 100.0 * fit.span() / view.window.span()))
            .on_hover_text("Horizontal zoom: 100 % is what Fit shows.");
        ui.separator();
        for (k, (label, tick)) in [("NPS Peak", d.peak_ticks.0), ("Poly Peak", d.peak_ticks.1)]
            .into_iter()
            .enumerate()
        {
            let resp = ui.add_enabled(tick.is_some(), egui::Button::new(label).small());
            let resp = resp.on_hover_text(if k == 0 {
                "Zoom to where the highest NPS is first reached."
            } else {
                "Zoom to where the highest polyphony is first reached."
            });
            if resp.clicked()
                && let Some(t) = tick
            {
                let f = inspector::fraction_of(timing, view.window.axis, end, t);
                let span = if view.window.span() >= fit.span() * 0.999 {
                    fit.span() / 64.0
                } else {
                    view.window.span()
                };
                view.window = view.window.around(f, span.max(inspector::MIN_SPAN));
                if let Some(c) = view.y.get_mut(k) {
                    *c = YView::default();
                }
            }
        }
    });
    let w = view.window;
    // The shown window as text (time and bar), and the mapping tick → x.
    let place = |tick: u64| -> String {
        let clock = super::format_clock(timing.nanos_at(tick) as f64 / 1e9);
        match &d.meter {
            Some(m) => format!("{clock} (bar {})", m.format_position(tick)),
            None => clock,
        }
    };
    let (t_lo, t_hi) = (
        inspector::tick_of(timing, w.axis, end, w.lo),
        inspector::tick_of(timing, w.axis, end, w.hi),
    );
    ui.label(
        egui::RichText::new(format!("{}  –  {}", place(t_lo), place(t_hi)))
            .small()
            .weak(),
    );
    let x_of = |tick: u64, rect: egui::Rect| -> f32 {
        let f = inspector::fraction_of(timing, w.axis, end, tick);
        rect.left() + ((f - w.lo) / w.span()) as f32 * rect.width()
    };
    // ---------------------------------------------- NPS / Polyphony
    let cols = &d.chart.columns;
    let bounds = d.chart.bounds();
    let current = d.chart.window() == Some(w) && d.chart.describes(d.index);
    let fill = ui.visuals().selection.bg_fill;
    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(11.0);
    let playhead = d.playhead;
    let mut seek = None;
    let mut rects = Vec::with_capacity(4);
    let mut hover_text = None;
    let seek_hint = if d.seek {
        "Click or drag: move the playhead there. "
    } else {
        ""
    };
    let hover_line = |tick: u64, col_line: String| -> String {
        match d.exact_at {
            Some(f) => format!("{}\n{}\n{col_line}", place(tick), f(tick)),
            None => format!("{}\n{col_line}", place(tick)),
        }
    };
    let mut bar_charts: Vec<(usize, &str, Option<u64>)> =
        vec![(0usize, "NPS", d.maxima.0), (1, "Polyphony", d.maxima.1)];
    if d.counter.is_some() {
        bar_charts.push((3, "", None));
    }
    for (k, title, scope_max) in bar_charts {
        let counter = (k == 3).then_some(d.counter.as_ref()).flatten();
        let title: &str = counter.map_or(title, |c| c.title.as_str());
        let value = |i: usize, c: &(u64, u64)| match (k, counter) {
            (0, _) => c.0,
            (3, Some(cn)) => cn.values.get(i).copied().unwrap_or(0),
            _ => c.1,
        };
        // The exact maximum (minimum for the counter) in view: the columns
        // of this window.
        let visible = || {
            cols.iter()
                .enumerate()
                .zip(bounds.windows(2))
                .filter(|(_, b)| b[1] >= t_lo && b[0] <= t_hi)
                .map(|((i, c), _)| value(i, c))
        };
        let in_view = visible().max().unwrap_or(0);
        let floor = if counter.is_some() {
            visible().min().unwrap_or(0).saturating_sub(1)
        } else {
            0
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            match (counter, scope_max) {
                (Some(c), _) => {
                    ui.label(egui::RichText::new(title).strong())
                        .on_hover_text(c.hint);
                }
                (None, m) => {
                    let head = match m {
                        Some(m) => format!("{title} — max {}", group_digits(m)),
                        None => format!("{title} — max …"),
                    };
                    ui.label(egui::RichText::new(head).strong())
                        .on_hover_text(d.max_hint);
                }
            }
            let view_txt = match (current, counter) {
                (false, _) => "in view …".into(),
                (true, Some(_)) => format!(
                    "in view {} – {}",
                    group_digits(floor + u64::from(floor > 0 || in_view > 0)),
                    group_digits(in_view)
                ),
                (true, None) => format!("in view {}", group_digits(in_view)),
            };
            ui.label(egui::RichText::new(view_txt).small().color(weak))
                .on_hover_text(if counter.is_some() {
                    "The counts at the visible columns' ends."
                } else {
                    "The exact maximum of the visible part (each bar is the exact maximum of its own span)."
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Fit Y")
                    .on_hover_text("The highest visible bar fills the height.")
                    .clicked()
                {
                    view.y[k] = YView::default();
                }
                if ui
                    .small_button("Y+")
                    .on_hover_text("Zoom in vertically (Alt+wheel over the chart).")
                    .clicked()
                {
                    view.y[k] = view.y[k].zoomed(2.0, 0.0);
                }
                if ui
                    .small_button("Y\u{2212}")
                    .on_hover_text("Zoom out vertically.")
                    .clicked()
                {
                    view.y[k] = view.y[k].zoomed(0.5, 0.0);
                }
            });
        });
        let (rect, input) = chart_canvas(
            ui,
            &format!(
                "{seek_hint}Ctrl+wheel: zoom in time, wheel or right-drag: scroll, Alt+wheel: zoom the height, double-click: fit."
            ),
        );
        rects.push(rect);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let (y0, y1) = shown(view.y[k], floor as f64, in_view.max(floor + 1) as f64);
        let mut mesh = egui::epaint::Mesh::default();
        for (i, (c, b)) in cols.iter().zip(bounds.windows(2)).enumerate() {
            let val = value(i, c) as f64;
            if val <= y0 || b[1] < t_lo || b[0] > t_hi {
                continue;
            }
            let (xa, xb) = (x_of(b[0], rect), x_of(b[1], rect));
            let h = (((val - y0) / (y1 - y0)).min(1.0) as f32) * rect.height();
            mesh.add_colored_rect(
                egui::Rect::from_min_max(
                    egui::pos2(xa.max(rect.left()), rect.bottom() - h),
                    egui::pos2(xb.max(xa + 1.0).min(rect.right()), rect.bottom()),
                ),
                fill,
            );
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.text(
            rect.left_top() + egui::vec2(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            group_digits(y1.round() as u64),
            font.clone(),
            weak,
        );
        painter.text(
            rect.left_bottom() + egui::vec2(3.0, -1.0),
            egui::Align2::LEFT_BOTTOM,
            group_digits(y0.round() as u64),
            font.clone(),
            weak,
        );
        let px = x_of(playhead, rect);
        if rect.x_range().contains(px) {
            painter.vline(px, rect.y_range(), egui::Stroke::new(1.0, text));
        }
        if view.y[k].zoom > 1.0 {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.right() - 6.0, rect.top()),
                rect.right_bottom(),
            );
            let span = 1.0 / view.y[k].zoom;
            let lo = view.y[k].pos * (1.0 - span);
            if let Some(nlo) = scrollbar(ui, bar, lo, lo + span, true, &format!("{id}-y{k}")) {
                view.y[k].pos = if span >= 1.0 { 0.0 } else { nlo / (1.0 - span) };
            }
        }
        if let Some(f) = input.hover {
            let tick = inspector::tick_of(timing, w.axis, end, w.lo + f * w.span());
            let col = bounds
                .windows(2)
                .position(|b| tick >= b[0] && tick < b[1].max(b[0] + 1));
            let val = col.and_then(|i| cols.get(i).map(|c| value(i, c)));
            let col_line = if counter.is_some() {
                format!(
                    "{title}: {} by this bar's end{}",
                    val.map_or("—".into(), group_digits),
                    if current { "" } else { ", updating" }
                )
            } else {
                format!(
                    "{title}: {} (highest in this bar's span{})",
                    val.map_or("—".into(), group_digits),
                    if current { "" } else { ", updating" }
                )
            };
            hover_text = Some(hover_line(tick, col_line));
            let x = x_of(tick, rect);
            painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, weak));
        }
        apply_chart_input(&mut view, k, &input, follow_suspended, timing, end, d.fit);
        if d.seek {
            seek = seek.or(input.seek);
        }
    }
    // --------------------------------------------------------- Tempo
    ui.add_space(4.0);
    let ppqn = d.ppqn;
    let points = d.tempo_points;
    let bpm_at = |tick: u64| ppqn.map(|p| timing.map().bpm_at_tick(tick, p));
    // Exact extremes in view: steps and ramps change monotonically
    // between points, so the ends of every visible segment hold them.
    let mut lo_bpm = f64::INFINITY;
    let mut hi_bpm = f64::NEG_INFINITY;
    let mut probe = vec![t_lo, t_hi];
    for &t in points.iter().filter(|&&t| t > t_lo && t <= t_hi) {
        probe.push(t);
        probe.push(t - 1);
    }
    for t in probe {
        if let Some(b) = bpm_at(t) {
            lo_bpm = lo_bpm.min(b);
            hi_bpm = hi_bpm.max(b);
        }
    }
    ui.horizontal(|ui| {
        let head = match (ppqn, bpm_at(t_lo)) {
            (Some(_), Some(_)) => format!(
                "Tempo — {} BPM at start",
                bpm_label(bpm_at(0).unwrap_or(0.0))
            ),
            _ => "Tempo — SMPTE timing (one fixed rate)".into(),
        };
        ui.label(egui::RichText::new(head).strong()).on_hover_text(
            "The Conductor's tempo as it plays: steps, and ramps that change the BPM linearly in ticks (PlumSpirit's only ramp shape; exported as steps). Its points are marked. Times on every chart come from this same tempo map.",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(&mut view.tempo_log, "Log")
                .on_hover_text("A logarithmic BPM scale, for extreme tempo ranges.");
            if ui.small_button("Fit Y").clicked() {
                view.y[2] = YView::default();
            }
            if ui.small_button("Y+").clicked() {
                view.y[2] = view.y[2].zoomed(2.0, 0.5);
            }
            if ui.small_button("Y\u{2212}").clicked() {
                view.y[2] = view.y[2].zoomed(0.5, 0.5);
            }
        });
    });
    let (rect, input) = chart_canvas(
        ui,
        &format!(
            "The Conductor's tempo. {seek_hint}Ctrl+wheel: zoom in time, Alt+wheel: zoom the height, double-click: fit."
        ),
    );
    rects.push(rect);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    if lo_bpm.is_finite() {
        let tr = |b: f64| if view.tempo_log { b.max(1e-9).ln() } else { b };
        let pad =
            ((tr(hi_bpm) - tr(lo_bpm)).abs() * 0.08).max(if view.tempo_log { 0.05 } else { 1.0 });
        let (y0, y1) = shown(view.y[2], tr(lo_bpm) - pad, tr(hi_bpm) + pad);
        let y_of = |b: f64| {
            rect.bottom() - (((tr(b) - y0) / (y1 - y0)) as f32).clamp(-0.05, 1.05) * rect.height()
        };
        let n = (rect.width().round() as usize).max(2);
        let pts: Vec<egui::Pos2> = (0..=n)
            .filter_map(|i| {
                let f = w.lo + w.span() * i as f64 / n as f64;
                let tick = inspector::tick_of(timing, w.axis, end, f);
                bpm_at(tick)
                    .map(|b| egui::pos2(rect.left() + rect.width() * i as f32 / n as f32, y_of(b)))
            })
            .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, fill)));
        for &t in points.iter().filter(|&&t| t >= t_lo && t <= t_hi) {
            let x = x_of(t, rect);
            painter.line_segment(
                [
                    egui::pos2(x, rect.bottom() - 5.0),
                    egui::pos2(x, rect.bottom()),
                ],
                egui::Stroke::new(1.0, text),
            );
        }
        painter.text(
            rect.right_top() + egui::vec2(-9.0, 1.0),
            egui::Align2::RIGHT_TOP,
            format!("in view {} – {} BPM", bpm_label(lo_bpm), bpm_label(hi_bpm)),
            font.clone(),
            weak,
        );
        let back = |y: f64| if view.tempo_log { y.exp() } else { y };
        painter.text(
            rect.left_top() + egui::vec2(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            format!("{} BPM", bpm_label(back(y1))),
            font.clone(),
            weak,
        );
        painter.text(
            rect.left_bottom() + egui::vec2(3.0, -1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{} BPM", bpm_label(back(y0))),
            font.clone(),
            weak,
        );
        if let Some(f) = input.hover {
            let tick = inspector::tick_of(timing, w.axis, end, w.lo + f * w.span());
            hover_text = Some(hover_line(
                tick,
                format!("Tempo: {} BPM", bpm_label(bpm_at(tick).unwrap_or(0.0))),
            ));
            painter.vline(
                x_of(tick, rect),
                rect.y_range(),
                egui::Stroke::new(1.0, weak),
            );
        }
    }
    let px = x_of(playhead, rect);
    if rect.x_range().contains(px) {
        painter.vline(px, rect.y_range(), egui::Stroke::new(1.0, text));
    }
    if view.y[2].zoom > 1.0 {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 6.0, rect.top()),
            rect.right_bottom(),
        );
        let span = 1.0 / view.y[2].zoom;
        let lo = view.y[2].pos * (1.0 - span);
        if let Some(nlo) = scrollbar(ui, bar, lo, lo + span, true, &format!("{id}-y2")) {
            view.y[2].pos = if span >= 1.0 { 0.0 } else { nlo / (1.0 - span) };
        }
    }
    apply_chart_input(&mut view, 2, &input, follow_suspended, timing, end, d.fit);
    if d.seek {
        seek = seek.or(input.seek);
    }
    // ----------------------------------------------- the scrollbar
    let (bar, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(80.0), 9.0),
        egui::Sense::hover(),
    );
    if let Some(lo) = scrollbar(
        ui,
        bar,
        view.window.lo,
        view.window.hi,
        false,
        &format!("{id}-x"),
    ) {
        let span = view.window.span();
        view.window = Window {
            lo,
            hi: lo + span,
            ..view.window
        };
    }
    if let Some(t) = hover_text {
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            ui.id().with((id, "hover")),
            egui::PopupAnchor::Pointer,
        )
        .show(|ui| {
            ui.label(t);
        });
    }
    *rects_out = rects;
    if view != *view_state {
        *follow_suspended = true;
    }
    *view_state = view;
    // V09-0058: a click or drag on a chart seeks to the start of the
    // column under the pointer (the Tempo chart: the tick there).
    if let Some(f) = seek {
        let tick = if (f - 0.5).abs() <= 0.5 && current && !bounds.is_empty() {
            inspector::seek_tick(bounds, f)
        } else {
            None
        }
        .unwrap_or_else(|| {
            inspector::tick_of(
                timing,
                w.axis,
                end,
                w.lo + f64::from(f.clamp(0.0, 1.0)) * w.span(),
            )
        });
        out.seek = Some(tick);
    }
    out.busy = !current;
    out
}

/// `fit` (a window on `fit.axis`) on `axis`: the same ticks.
pub fn fit_on(timing: &Timing, end: u64, fit: Window, axis: Axis) -> Window {
    if fit.axis == axis {
        return Window { axis, ..fit };
    }
    let (t0, t1) = (
        inspector::tick_of(timing, fit.axis, end, fit.lo),
        inspector::tick_of(timing, fit.axis, end, fit.hi),
    );
    let lo = inspector::fraction_of(timing, axis, end, t0);
    let hi = inspector::fraction_of(timing, axis, end, t1);
    Window {
        axis,
        lo,
        hi: hi.max(lo + inspector::MIN_SPAN),
    }
}

/// What a chart's pointer asked for, applied to the view.
fn apply_chart_input(
    view: &mut ChartView,
    k: usize,
    input: &ChartInput,
    follow_suspended: &mut bool,
    timing: &Timing,
    end: u64,
    fit: Window,
) {
    if input.fit || input.zoom_x.is_some() || input.zoom_y.is_some() || input.pan != (0.0, 0.0) {
        *follow_suspended = true;
    }
    if input.fit {
        view.window = fit_on(timing, end, fit, view.window.axis);
        view.y[k] = YView::default();
        return;
    }
    if let Some((factor, at)) = input.zoom_x {
        view.window = view.window.zoomed(factor, at);
    }
    if let Some((factor, at)) = input.zoom_y {
        view.y[k] = view.y[k].zoomed(factor, at);
    }
    if input.pan.0 != 0.0 {
        view.window = view.window.panned(input.pan.0);
    }
    if input.pan.1 != 0.0 && view.y[k].zoom > 1.0 {
        let span = 1.0 / view.y[k].zoom;
        let lo = (view.y[k].pos * (1.0 - span) + input.pan.1 * span).clamp(0.0, 1.0 - span);
        view.y[k].pos = lo / (1.0 - span);
    }
}
