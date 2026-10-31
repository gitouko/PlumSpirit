//! Developer diagnostics: bounded logging and lightweight metrics. Diagnostics only
//! observe; they never change document state or musical output.

pub mod logging;
pub mod metrics;

use std::sync::Mutex;
use std::time::Duration;

/// Current process memory (resident set / working set) in bytes, when available.
pub fn process_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        crate::platform::working_set_bytes()
    }
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb: u64 = s
            .lines()
            .find(|l| l.starts_with("VmRSS:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

pub fn format_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{b} bytes")
    }
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}

/// Per-frame piano-roll statistics shown in the developer overlay (F12).
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub lod: bool,
    pub visible_estimate: usize,
    pub notes_queried: usize,
    pub notes_scanned: usize,
    pub chunks_visited: usize,
    pub chunks_total: usize,
    pub primitives: usize,
    pub grid_lines: usize,
    pub query_time: Duration,
    pub frame_time: Duration,
    /// Onion Skin: estimated visible ghost notes and primitives drawn.
    pub onion_estimate: usize,
    /// L-G05: `onion_estimate` stopped at the dense threshold (a lower bound).
    pub onion_estimate_partial: bool,
    pub onion_primitives: usize,
    pub onion_time: Duration,
    /// L-S48: parts of the paint (the slowest frame's breakdown in the
    /// overlay): the Onion's visible-note estimate and content identity, the
    /// active track's notes and previews, the overlays (selection, cursor,
    /// playhead) and the ruler / markers / keys / scrollbars.
    pub onion_estimate_time: Duration,
    pub onion_family_time: Duration,
    pub notes_time: Duration,
    pub overlay_time: Duration,
    pub chrome_time: Duration,
    /// Velocity Lane paint time and whether it drew aggregated columns.
    pub lane_time: Duration,
    pub lane_aggregated: bool,
    /// v0.8 dense subsystem: the active track and Onion (§207).
    pub dense: crate::piano_roll::dense::DenseStats,
    pub onion_dense: crate::piano_roll::dense::DenseStats,
}

/// Recent frame timings for the developer overlay: the last value and the maximum
/// over a short window, so spikes while panning are visible. Fixed-size ring buffer.
#[derive(Clone, Debug)]
pub struct FramePerf {
    /// (UI update CPU time, piano-roll paint time) per frame.
    samples: [(f32, f32); Self::LEN],
    /// L-S48: each frame's paint parts (ms, [`PAINT_PARTS`] order), so the
    /// overlay can say where the slowest frame of the window went.
    parts: [[f32; PAINT_PARTS.len()]; Self::LEN],
    /// L-S48: each update's wall time and its largest parts ([`PhaseClock`]).
    updates: Vec<(f32, Vec<(&'static str, f32)>)>,
    /// L-S49: each frame's CPU time split by [`FrameClock`] (when it runs).
    splits: [Option<FrameSplit>; Self::LEN],
    next_update: usize,
    next: usize,
    filled: usize,
    /// The frame being built waits in a native (modal) dialog; its time is the
    /// user's, not rendering work, so it is not recorded.
    skip: bool,
}

/// L-S48: wall time of the parts of one app update (`lap` after each part),
/// so the overlay can say where the slowest update of the window went — work
/// outside the Piano Roll's paint (applying an edit, a dialog, a panel).
#[derive(Debug)]
pub struct PhaseClock {
    started: std::time::Instant,
    last: std::time::Instant,
    parts: Vec<(&'static str, f32)>,
}

impl PhaseClock {
    pub fn start() -> PhaseClock {
        let now = std::time::Instant::now();
        PhaseClock {
            started: now,
            last: now,
            parts: Vec::with_capacity(12),
        }
    }

    /// Ends the part `name` (the time since the previous lap).
    pub fn lap(&mut self, name: &'static str) {
        let now = std::time::Instant::now();
        self.parts
            .push((name, now.duration_since(self.last).as_secs_f32() * 1000.0));
        self.last = now;
    }

    /// The update's total (ms) and its parts.
    pub fn finish(self) -> (f32, Vec<(&'static str, f32)>) {
        (self.started.elapsed().as_secs_f32() * 1000.0, self.parts)
    }
}

thread_local! {
    /// L-S49: a native (modal) dialog was shown in the current frame: its
    /// wait is the user's time, not rendering work ([`FramePerf`] leaves the
    /// frame out; every file dialog of `app::files` marks it). Dialogs open
    /// on the UI thread, where the frames are recorded.
    static NATIVE_DIALOG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current frame as one that waits in a native dialog (the
/// platforms without native file dialogs never do).
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn native_dialog_shown() {
    NATIVE_DIALOG.with(|d| d.set(true));
}

/// L-S49: what one frame's egui passes cost and produced, taken by
/// [`FrameClock`]: the passes up to the end of the app's update (ms), egui's
/// end of the passes (ms), and the output eframe tessellates, uploads and
/// paints after them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PassMarks {
    pub ui_ms: f32,
    /// Parts of `ui_ms`: before the app's update starts (egui's begin of
    /// pass) and after it returns (its locals dropped), when the app marks
    /// them ([`FrameClock::app_started`] / [`FrameClock::app_finished`]).
    pub begin_ms: f32,
    pub exit_ms: f32,
    pub end_ms: f32,
    /// Passes of the frame (egui runs another one when a pass asks to
    /// discard its output, e.g. a new window's sizing pass).
    pub passes: u8,
    /// Shapes of the frame's last pass (the ones painted).
    pub shapes: usize,
    /// Vertices of the meshes among the shapes (the tiles, the ghosts).
    pub mesh_vertices: usize,
    pub textures_set: usize,
    pub texture_bytes: u64,
    pub textures_freed: usize,
    /// OpenGL: from the end of the last pass to the first paint probe
    /// (platform output, tessellation, texture uploads) and from it to the
    /// last probe (the draw calls), ms; `None` without probes (Vulkan).
    pub tessellate_ms: Option<f32>,
    pub draw_ms: Option<f32>,
}

/// L-S49: when the frame's paint probes ran (OpenGL paint callbacks: the
/// first shape of the background layer, the last of the debug layer).
#[derive(Default)]
struct PaintProbes {
    first: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
}

/// L-S49: an egui plugin that times each frame's passes — each pass's start
/// (the input hook), the end of the app's update (the end-of-pass hook) and
/// the end of egui's own end of pass (the output hook) — and counts their
/// output, so the F12 overlay can split a frame's CPU time (eframe's
/// `cpu_usage`) into the update, egui's end of pass and eframe's work after
/// the passes (tessellation, texture uploads, painting). E0094 left ~0.3-0.4
/// s of a 562M-note Import Into's Apply frame outside the update unexplained.
#[derive(Default)]
pub struct FrameClock {
    started: Option<std::time::Instant>,
    app_started: Option<std::time::Instant>,
    app_finished: Option<std::time::Instant>,
    ui_done: Option<std::time::Instant>,
    /// The frame being timed (egui's frame number) and its marks so far.
    frame: Option<(u64, PassMarks)>,
    /// When the frame's last pass ended (its output hook).
    output_at: Option<std::time::Instant>,
    probes: std::sync::Arc<Mutex<PaintProbes>>,
}

impl FrameClock {
    /// The marks of the frame before the current one (taken once; `None`
    /// when that frame was not timed).
    pub fn take_last(ctx: &egui::Context) -> Option<PassMarks> {
        let previous = ctx.cumulative_frame_nr().checked_sub(1)?;
        let h = ctx.plugin_opt::<FrameClock>()?;
        let mut clock = h.lock();
        let probes = clock
            .probes
            .lock()
            .map(|mut p| (p.first.take(), p.last.take()))
            .unwrap_or_default();
        match clock.frame {
            Some((nr, mut marks)) if nr == previous => {
                clock.frame = None;
                let ms = |d: Duration| d.as_secs_f32() * 1000.0;
                if let (Some(out), (Some(first), Some(last))) = (clock.output_at.take(), probes) {
                    marks.tessellate_ms = Some(ms(first.saturating_duration_since(out)));
                    marks.draw_ms = Some(ms(last.saturating_duration_since(first)));
                }
                Some(marks)
            }
            _ => None,
        }
    }

    /// The app's update starts (its first statement).
    pub fn app_started(ctx: &egui::Context) {
        if let Some(h) = ctx.plugin_opt::<FrameClock>() {
            h.lock().app_started = Some(std::time::Instant::now());
        }
    }

    /// The app's update is about to return (its last statement).
    pub fn app_finished(ctx: &egui::Context) {
        if let Some(h) = ctx.plugin_opt::<FrameClock>() {
            h.lock().app_finished = Some(std::time::Instant::now());
        }
    }

    /// OpenGL only (`gl`): paint probes around this pass's shapes — the
    /// first shape of the background layer (call before anything is drawn)
    /// and the last of the debug layer — so the next frame knows when
    /// eframe started and finished drawing.
    pub fn add_paint_probes(ctx: &egui::Context, gl: bool) {
        if !gl {
            return;
        }
        let Some(h) = ctx.plugin_opt::<FrameClock>() else {
            return;
        };
        let probes = std::sync::Arc::clone(&h.lock().probes);
        let rect = ctx.content_rect();
        let probe = |first: bool| {
            let probes = std::sync::Arc::clone(&probes);
            egui::Shape::Callback(egui::PaintCallback {
                rect,
                callback: std::sync::Arc::new(eframe::egui_glow::CallbackFn::new(move |_, _| {
                    if let Ok(mut p) = probes.lock() {
                        let now = Some(std::time::Instant::now());
                        if first {
                            p.first = now;
                        } else {
                            p.last = now;
                        }
                    }
                })),
            })
        };
        ctx.layer_painter(egui::LayerId::background())
            .add(probe(true));
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Debug,
            egui::Id::new("plumspirit-frame-clock"),
        ))
        .add(probe(false));
    }
}

impl egui::Plugin for FrameClock {
    fn debug_name(&self) -> &'static str {
        "PlumSpirit frame clock"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, _input: &mut egui::RawInput) {
        self.started = Some(std::time::Instant::now());
        self.app_started = None;
        self.app_finished = None;
        self.ui_done = None;
    }

    fn on_end_pass(&mut self, _ui: &mut egui::Ui) {
        self.ui_done = Some(std::time::Instant::now());
    }

    fn output_hook(&mut self, ctx: &egui::Context, out: &mut egui::FullOutput) {
        let now = std::time::Instant::now();
        let Some(started) = self.started.take() else {
            return;
        };
        let ui_done = self.ui_done.take().unwrap_or(now);
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
        let nr = ctx.cumulative_frame_nr();
        let mut m = match self.frame {
            Some((n, m)) if n == nr => m,
            _ => PassMarks::default(),
        };
        m.ui_ms += ms(ui_done.saturating_duration_since(started));
        if let (Some(a), Some(b)) = (self.app_started.take(), self.app_finished.take()) {
            m.begin_ms += ms(a.saturating_duration_since(started));
            m.exit_ms += ms(ui_done.saturating_duration_since(b));
        }
        m.end_ms += ms(now.saturating_duration_since(ui_done));
        m.passes = m.passes.saturating_add(1);
        m.shapes = out.shapes.len();
        m.mesh_vertices = out
            .shapes
            .iter()
            .map(|c| match &c.shape {
                egui::Shape::Mesh(mesh) => mesh.vertices.len(),
                _ => 0,
            })
            .sum();
        for d in out.textures_delta.set.values().flatten() {
            let [w, h] = d.image.size();
            m.textures_set += 1;
            m.texture_bytes += (w * h * 4) as u64;
        }
        m.textures_freed += out.textures_delta.free.len();
        self.frame = Some((nr, m));
        self.output_at = Some(std::time::Instant::now());
    }
}

/// L-S49: one frame's CPU time split by [`FrameClock`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSplit {
    /// eframe's CPU time of the frame (ms; vsync and context switches not
    /// counted).
    pub cpu_ms: f32,
    pub pass: PassMarks,
}

impl FrameSplit {
    /// eframe's work after the pass: tessellation, texture uploads and
    /// painting (the frame's CPU time less the pass).
    pub fn after_pass_ms(&self) -> f32 {
        (self.cpu_ms - self.pass.ui_ms - self.pass.end_ms).max(0.0)
    }

    /// What neither the passes nor the paint probes account for (input
    /// gathering and the clear before the passes, `post_rendering` and the
    /// viewport output after the draw calls); `None` without probes.
    pub fn rest_ms(&self) -> Option<f32> {
        Some((self.after_pass_ms() - self.pass.tessellate_ms? - self.pass.draw_ms?).max(0.0))
    }
}

/// L-S48: the parts of a piano-roll paint the overlay breaks the slowest
/// frame into (labels; values from [`paint_parts`]).
pub const PAINT_PARTS: [&str; 9] = [
    "onion est.",
    "onion id",
    "onion src",
    "onion upl.",
    "onion tiles",
    "onion queue",
    "notes",
    "overlays",
    "ruler/keys",
];

/// The [`PAINT_PARTS`] of one frame's statistics, in ms.
pub fn paint_parts(s: &RenderStats) -> [f32; PAINT_PARTS.len()] {
    let ms = |d: Duration| d.as_secs_f32() * 1000.0;
    let us = |u: u64| u as f32 / 1000.0;
    let o = &s.onion_dense;
    [
        ms(s.onion_estimate_time),
        ms(s.onion_family_time),
        us(o.make_micros),
        us(o.receive_micros),
        us(o.draw_micros),
        us(o.request_micros),
        ms(s.notes_time),
        ms(s.overlay_time),
        ms(s.chrome_time),
    ]
}

impl Default for FramePerf {
    fn default() -> Self {
        FramePerf {
            samples: [(0.0, 0.0); Self::LEN],
            parts: [[0.0; PAINT_PARTS.len()]; Self::LEN],
            updates: Vec::new(),
            splits: [None; Self::LEN],
            next_update: 0,
            next: 0,
            filled: 0,
            skip: false,
        }
    }
}

impl FramePerf {
    const LEN: usize = 120;

    /// Records one frame. `ui_cpu_seconds` is eframe's CPU time for the previous
    /// frame's update (None on the first frame); `roll` the Piano Roll's
    /// statistics of its last paint; `pass` the previous frame's egui pass
    /// ([`FrameClock::take_last`]).
    pub fn record(
        &mut self,
        ui_cpu_seconds: Option<f32>,
        roll: &RenderStats,
        pass: Option<PassMarks>,
    ) {
        let dialog = NATIVE_DIALOG.with(|d| d.replace(false));
        if std::mem::take(&mut self.skip) || dialog {
            return;
        }
        let Some(cpu) = ui_cpu_seconds else {
            return;
        };
        self.samples[self.next] = (cpu * 1000.0, roll.frame_time.as_secs_f32() * 1000.0);
        self.parts[self.next] = paint_parts(roll);
        self.splits[self.next] = pass.map(|pass| FrameSplit {
            cpu_ms: cpu * 1000.0,
            pass,
        });
        self.next = (self.next + 1) % Self::LEN;
        self.filled = (self.filled + 1).min(Self::LEN);
    }

    /// L-S48: records one app update's wall time and parts (the largest
    /// four kept).
    pub fn record_update(&mut self, (total, mut parts): (f32, Vec<(&'static str, f32)>)) {
        // L-S49: an update that waited in a native dialog is the user's time
        // (the flag stays for the next frame's `record`).
        if self.skip || NATIVE_DIALOG.with(|d| d.get()) {
            return;
        }
        parts.sort_by(|a, b| b.1.total_cmp(&a.1));
        parts.truncate(4);
        if self.updates.len() < Self::LEN {
            self.updates.push((total, parts));
        } else {
            self.updates[self.next_update] = (total, parts);
        }
        self.next_update = (self.next_update + 1) % Self::LEN;
    }

    /// L-S48: the slowest app update of the last [`FramePerf::LEN`] and its
    /// largest parts.
    pub fn slowest_update(&self) -> Option<&(f32, Vec<(&'static str, f32)>)> {
        self.updates.iter().max_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// L-S48: the slowest piano-roll paint of the window and its parts
    /// ([`PAINT_PARTS`] order).
    pub fn slowest_paint(&self) -> Option<(f32, [f32; PAINT_PARTS.len()])> {
        (0..self.filled)
            .max_by(|&a, &b| self.samples[a].1.total_cmp(&self.samples[b].1))
            .map(|i| (self.samples[i].1, self.parts[i]))
    }

    /// L-S49: the frame of the window that took the most CPU time, split
    /// by [`FrameClock`] (frames it did not time are not considered).
    pub fn slowest_split(&self) -> Option<FrameSplit> {
        self.splits[..self.filled]
            .iter()
            .flatten()
            .copied()
            .max_by(|a, b| a.cpu_ms.total_cmp(&b.cpu_ms))
    }

    /// Excludes the current frame (it blocks in a native file dialog).
    pub fn skip_current_frame(&mut self) {
        self.skip = true;
    }

    /// (last UI ms, max UI ms, max roll-paint ms) over the recorded window.
    pub fn summary(&self) -> Option<(f32, f32, f32)> {
        if self.filled == 0 {
            return None;
        }
        let last = self.samples[(self.next + Self::LEN - 1) % Self::LEN].0;
        let window = &self.samples[..self.filled];
        let max_ui = window.iter().map(|s| s.0).fold(0.0, f32::max);
        let max_paint = window.iter().map(|s| s.1).fold(0.0, f32::max);
        Some((last, max_ui, max_paint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_perf_keeps_a_bounded_window() {
        let mut p = FramePerf::default();
        assert!(p.summary().is_none());
        let paint = |ms: u64| RenderStats {
            frame_time: Duration::from_millis(ms),
            ..Default::default()
        };
        p.record(None, &paint(0), None);
        assert!(p.summary().is_none());
        p.record(Some(0.050), &paint(9), None);
        for _ in 0..FramePerf::LEN {
            p.record(Some(0.002), &paint(1), None);
        }
        // A frame spent in a native dialog is not recorded.
        p.skip_current_frame();
        p.record(Some(9.0), &paint(0), None);
        // The 50 ms spike has left the window.
        let (last, max_ui, max_paint) = p.summary().unwrap();
        assert!((last - 2.0).abs() < 1e-3);
        assert!((max_ui - 2.0).abs() < 1e-3);
        assert!((max_paint - 1.0).abs() < 1e-3);
    }

    /// L-S49: the frame clock times the update and egui's end of pass and
    /// counts the output; the overlay's slowest frame is the one with the
    /// most CPU time, and what the pass does not explain is eframe's work
    /// after it.
    #[test]
    fn the_frame_clock_splits_a_frame_into_its_pass_and_what_follows() {
        let ctx = egui::Context::default();
        ctx.add_plugin(FrameClock::default());
        assert!(FrameClock::take_last(&ctx).is_none());
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            std::thread::sleep(Duration::from_millis(20));
            let mut mesh = egui::Mesh::default();
            mesh.add_colored_rect(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
                egui::Color32::RED,
            );
            ui.painter().add(egui::Shape::mesh(mesh));
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(20.0, 0.0), egui::vec2(5.0, 5.0)),
                0.0,
                egui::Color32::BLUE,
            );
        });
        out.textures_delta.clear();
        let marks = FrameClock::take_last(&ctx).expect("timed");
        assert!(marks.ui_ms >= 20.0, "{marks:?}");
        // Not marked by the app: no begin / exit parts; no paint probes
        // without OpenGL.
        assert_eq!((marks.begin_ms, marks.exit_ms), (0.0, 0.0));
        assert_eq!((marks.tessellate_ms, marks.draw_ms), (None, None));
        assert!(marks.ui_ms < 2_000.0 && marks.end_ms >= 0.0);
        assert!(marks.shapes >= 2, "{marks:?}");
        assert_eq!(marks.mesh_vertices, 4);
        // The first pass uploads the font atlas.
        assert!(
            marks.textures_set >= 1 && marks.texture_bytes > 0,
            "{marks:?}"
        );
        assert_eq!(marks.passes, 1);
        // Taken once.
        assert!(FrameClock::take_last(&ctx).is_none());
        // A frame egui runs twice (a pass asked to discard its output): one
        // frame's marks — both passes' time, the last pass's shapes.
        let mut first = true;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            std::thread::sleep(Duration::from_millis(5));
            if std::mem::take(&mut first) {
                ui.ctx().request_discard("test");
            } else {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(5.0, 5.0)),
                    0.0,
                    egui::Color32::BLUE,
                );
            }
        });
        out.textures_delta.clear();
        let two = FrameClock::take_last(&ctx).expect("timed");
        assert_eq!(two.passes, 2, "{two:?}");
        assert!(two.ui_ms >= 10.0, "{two:?}");
        assert!(two.shapes >= 1);
        // The app marks its update: the time before it and after it.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            FrameClock::add_paint_probes(ui.ctx(), false);
            std::thread::sleep(Duration::from_millis(5));
            FrameClock::app_started(ui.ctx());
            std::thread::sleep(Duration::from_millis(5));
            FrameClock::app_finished(ui.ctx());
            std::thread::sleep(Duration::from_millis(10));
        });
        out.textures_delta.clear();
        let parts = FrameClock::take_last(&ctx).expect("timed");
        assert!(parts.begin_ms >= 5.0 && parts.exit_ms >= 10.0, "{parts:?}");
        assert!(parts.ui_ms >= parts.begin_ms + parts.exit_ms + 5.0);
        assert!(out.shapes.is_empty(), "no probes without OpenGL");
        // A later frame's take does not return an older frame's marks.
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let late = FrameClock::take_last(&ctx).expect("the last frame");
        assert_eq!(late.passes, 1);

        let split = FrameSplit {
            cpu_ms: 500.0,
            pass: PassMarks {
                ui_ms: 60.0,
                end_ms: 2.0,
                ..Default::default()
            },
        };
        assert!((split.after_pass_ms() - 438.0).abs() < 1e-3);
        let mut p = FramePerf::default();
        let roll = RenderStats::default();
        p.record(Some(0.010), &roll, Some(marks));
        p.record(Some(0.500), &roll, Some(split.pass));
        p.record(Some(0.900), &roll, None);
        p.record(Some(0.020), &roll, Some(marks));
        // The 900 ms frame was not timed: the slowest timed one is shown.
        let s = p.slowest_split().unwrap();
        assert_eq!(s.cpu_ms, 500.0);
        assert_eq!(s.pass, split.pass);
    }

    /// L-S49: a frame that waited in a native file dialog (Import Into's
    /// file choice: 2-7 s on the owner's PC) is the user's time — neither
    /// its update nor its frame time enters the overlay's maxima.
    #[test]
    fn a_frame_that_waits_in_a_native_dialog_is_left_out() {
        let mut p = FramePerf::default();
        let roll = RenderStats::default();
        p.record(Some(0.010), &roll, None);
        p.record_update((10.0, vec![("central view", 9.0)]));
        // The dialog frame: its update ends after the dialog closed.
        native_dialog_shown();
        p.record_update((7_000.0, vec![("edits / commands", 6_990.0)]));
        // The next frame records the dialog frame's time: left out.
        p.record(Some(7.0), &roll, None);
        p.record_update((12.0, vec![("central view", 11.0)]));
        p.record(Some(0.012), &roll, None);
        assert_eq!(p.slowest_update().unwrap().0, 12.0);
        let (_, max_ui, _) = p.summary().unwrap();
        assert!((max_ui - 12.0).abs() < 1e-3, "{max_ui}");
    }

    #[test]
    fn the_slowest_update_keeps_its_largest_parts() {
        let mut p = FramePerf::default();
        assert!(p.slowest_update().is_none());
        p.record_update((5.0, vec![("a", 1.0), ("b", 4.0)]));
        p.record_update((
            300.0,
            vec![
                ("x", 1.0),
                ("dialogs", 280.0),
                ("y", 2.0),
                ("z", 3.0),
                ("w", 0.5),
            ],
        ));
        let (ms, parts) = p.slowest_update().unwrap();
        assert_eq!(*ms, 300.0);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], ("dialogs", 280.0));
        for _ in 0..FramePerf::LEN {
            p.record_update((1.0, Vec::new()));
        }
        assert_eq!(p.slowest_update().unwrap().0, 1.0);
        let mut c = PhaseClock::start();
        c.lap("one");
        c.lap("two");
        let (total, parts) = c.finish();
        assert!(total >= 0.0);
        assert_eq!(
            parts.iter().map(|p| p.0).collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn byte_formatting_units() {
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1591), "1.6 KB");
        assert_eq!(format_bytes(20 * 1024 * 1024), "20.0 MB");
    }
}
