//! The Spiderweb window (v0.8 built-in plugin, contract §028–036; engine:
//! `plumspirit_engine::spiderweb`, a faithful port of Spiderweb by Kanade
//! Tachibana, MIT, expanded for PlumSpirit by ouko). Shapes are drawn on the
//! board (the project's time and keys); every change re-renders the board
//! on a worker after a preflight (estimated notes, temporary memory, undo;
//! huge boards wait for a click, impossible ones are refused before
//! anything is allocated, and a run can be stopped). The notes are previewed
//! on the active track and pasted with the shared paste options (colours =
//! channel slots → new Subtracks / Subchannels with the Subchannel Paste
//! Offset / chosen destinations) as one undo step. Shapes travel as
//! Spiderweb share codes both ways.

mod canvas;
pub(crate) mod fonts;
mod props;

use super::paste_ui::{self, PasteOptions};
use super::{PlumSpiritApp, StatusKind};
use egui::RichText;
use plumspirit_engine::json::Value;
use plumspirit_engine::spiderweb::edit::Xform;
use plumspirit_engine::spiderweb::engine::{ChannelMode, Note6, Split};
use plumspirit_engine::spiderweb::output::{self, Channels, SLOT_COLORS};
use plumspirit_engine::spiderweb::paths::P;
use plumspirit_engine::spiderweb::{self as sw, share};
use plumspirit_engine::transform::TNote;
use plumspirit_engine::transform::paste::{self, Generated, PastePlan};
use plumspirit_engine::transform::preflight::{self, Estimate, Limits, Verdict};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// Undo steps kept for the board.
const UNDO_STEPS: usize = 200;

/// v1.0-beta2 (B2-F26, contract §020): what the window asks of the shared
/// transport. Spiderweb has no player of its own: the app turns these into
/// the main transport's seek / play / stop / loop (one playhead, the outputs
/// and tempo map of the song); playhead travel never makes an undo step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransportAction {
    PlayPause,
    Stop,
    /// Go to the beginning of the song.
    Start,
    /// Go to the end of the song.
    End,
    ToggleLoop,
    /// Seek to this board beat (the roll's axis: beat b = content tick b · PPQ).
    Seek(f64),
}
/// Widest density picture (columns).
const DENSITY_COLUMNS: i64 = 4096;
/// Keys a standard board renders (v0.9: [`Spiderweb::keys`] follows the
/// project — 256 in 256-Key Mode).
const KEYS: i64 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Line,
    Poly,
    Free,
    Curve,
    Arc,
    Circle,
    Polygon,
    Funnel,
    Text,
    Hz,
    Paint,
}

/// (tool, name, key, what it does).
pub const TOOLS: [(Tool, &str, egui::Key, &str); 12] = [
    (
        Tool::Select,
        "Select",
        egui::Key::V,
        "Click a shape to select it (Ctrl: add / remove), drag to move; drag on empty space to select with a box; drag a selected shape's points. On a selected curve: double-click adds a point, Alt-click a point makes a sharp corner.",
    ),
    (
        Tool::Line,
        "Line",
        egui::Key::L,
        "Drag from start to end (or click start, then end).",
    ),
    (
        Tool::Poly,
        "Polyline",
        egui::Key::P,
        "Click each point (or drag each segment); double-click, Enter or right-click finishes.",
    ),
    (
        Tool::Free,
        "Freehand",
        egui::Key::F,
        "Draw with the mouse held down; \"Straighten\" makes new strokes perfect lines, curves and shapes.",
    ),
    (
        Tool::Curve,
        "Curve",
        egui::Key::C,
        "Drag from start to end; then drag its points and blue handles with Select.",
    ),
    (
        Tool::Arc,
        "Arc",
        egui::Key::A,
        "Click start, a point it passes through, end — or drag start → end, then click where it bends.",
    ),
    (
        Tool::Circle,
        "Circle",
        egui::Key::O,
        "Drag a box: a circle (ellipse) fills it. Fill it from the shape's settings.",
    ),
    (
        Tool::Polygon,
        "Polygon",
        egui::Key::Q,
        "Drag a box: a polygon, star or crossing star (settings next to the tools) fills it.",
    ),
    (
        Tool::Funnel,
        "Funnel",
        egui::Key::N,
        "Drag from where the funnel starts to the top of its wall; drag its points with Select.",
    ),
    (
        Tool::Text,
        "Text",
        egui::Key::X,
        "Click where the text starts (its baseline); type and style it in the panel.",
    ),
    (
        Tool::Hz,
        "Hz bass",
        egui::Key::H,
        "Drag a box: spam so fast it sounds like the tone (set next to the tools).",
    ),
    (
        Tool::Paint,
        "Area color",
        egui::Key::K,
        "Click inside a filled custom shape: that area's notes get the color (0 = none); right-click clears the shape's colors.",
    ),
];

const CHANNEL_MODES: [(&str, &str); 3] = [
    (
        "As drawn",
        "One channel; overlapping notes are kept as drawn.",
    ),
    (
        "Single channel",
        "One channel; overlapping notes are fixed (Spiderweb's \"Single\").",
    ),
    (
        "Multi channel",
        "Overlapping shapes go to channels of their own (one color per channel).",
    ),
];

/// The board's shapes (cleaned Spiderweb objects, shared with the undo
/// steps), each with an identity that changes with it (the geometry cache).
#[derive(Default)]
pub struct Board {
    shapes: Vec<Arc<Value>>,
    ids: Vec<u64>,
    next: u64,
    /// Changes with every edit (re-render trigger).
    pub rev: u64,
    cache: HashMap<u64, Arc<Vec<Vec<P>>>>,
}

impl Board {
    pub fn len(&self) -> usize {
        self.shapes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }
    pub fn get(&self, i: usize) -> &Value {
        &self.shapes[i]
    }
    fn new_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    pub fn push(&mut self, v: Value) -> usize {
        let id = self.new_id();
        self.shapes.push(Arc::new(v));
        self.ids.push(id);
        self.rev += 1;
        self.shapes.len() - 1
    }
    pub fn set(&mut self, i: usize, v: Value) {
        self.shapes[i] = Arc::new(v);
        self.ids[i] = self.new_id();
        self.rev += 1;
    }
    pub fn remove(&mut self, which: &BTreeSet<usize>) {
        let mut k = 0;
        self.shapes.retain(|_| {
            k += 1;
            !which.contains(&(k - 1))
        });
        let mut k = 0;
        self.ids.retain(|_| {
            k += 1;
            !which.contains(&(k - 1))
        });
        self.rev += 1;
    }
    fn snapshot(&self) -> Vec<Arc<Value>> {
        self.shapes.clone()
    }
    fn restore(&mut self, s: Vec<Arc<Value>>) {
        self.ids = (0..s.len()).map(|_| 0).collect();
        for k in 0..s.len() {
            self.ids[k] = self.new_id();
        }
        self.shapes = s;
        self.rev += 1;
    }
    pub fn values(&self) -> Vec<Value> {
        self.shapes.iter().map(|s| (**s).clone()).collect()
    }
    /// The shape's strokes (computed once per version).
    pub fn strokes(&mut self, i: usize) -> Arc<Vec<Vec<P>>> {
        let id = self.ids[i];
        if let Some(s) = self.cache.get(&id) {
            return s.clone();
        }
        if self.cache.len() > 2 * self.shapes.len() + 64 {
            let live: BTreeSet<u64> = self.ids.iter().copied().collect();
            self.cache.retain(|k, _| live.contains(k));
        }
        let s = Arc::new(sw::shape_strokes(&self.shapes[i]));
        self.cache.insert(id, s.clone());
        s
    }
}

/// A coarse picture of the notes (zoomed-out drawing): one column per
/// `res` ticks from `t0`, one row per key (row 0 = the top key).
pub struct Density {
    pub t0: i64,
    pub res: i64,
    pub w: usize,
    pub image: egui::ColorImage,
}

/// A rendered board.
pub struct Rendered {
    /// Identity (texture cache).
    pub id: u64,
    /// `(start, end, pitch, velocity, slot, owner)` in ticks, by start.
    pub notes: Vec<Note6>,
    pub max_dur: i64,
    pub slots: usize,
    /// The colour art to paste (first note at tick 0) and where it starts.
    pub art: Arc<Generated>,
    pub t0: i64,
    pub took: Duration,
    pub density: Option<Density>,
}

fn density(notes: &[Note6], keys: i64) -> Option<Density> {
    let t0 = notes.iter().map(|n| n[0]).min()?;
    let t1 = notes.iter().map(|n| n[1]).max()?;
    let span = (t1 - t0).max(1);
    let res = ((span + DENSITY_COLUMNS - 1) / DENSITY_COLUMNS).max(1);
    let w = ((span + res - 1) / res).max(1) as usize;
    let rows = keys.clamp(1, 256) as usize;
    let mut image = egui::ColorImage::new([w, rows], vec![egui::Color32::TRANSPARENT; w * rows]);
    // notes are by start: per key, each column is filled once
    let mut filled = vec![-1i64; rows];
    for n in notes {
        if !(0..rows as i64).contains(&n[2]) || n[1] <= n[0] {
            continue;
        }
        let row = (rows as i64 - 1 - n[2]) as usize;
        let c0 = ((n[0] - t0) / res).max(filled[row] + 1);
        let c1 = ((n[1] - 1 - t0) / res).min(w as i64 - 1);
        if c1 < c0 {
            continue;
        }
        let c = SLOT_COLORS[(n[4].max(0) as usize) % SLOT_COLORS.len()];
        let col = egui::Color32::from_rgb(c[0], c[1], c[2]);
        for x in c0..=c1 {
            image.pixels[row * w + x as usize] = col;
        }
        filled[row] = c1;
    }
    Some(Density { t0, res, w, image })
}

type JobResult = (u64, Result<Rendered, String>);

struct Job {
    generation: u64,
    rx: Receiver<JobResult>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

/// What a render is for: board revision, channels, PPQ.
type RenderKey = (u64, Channels, u32, i64);

/// The Spiderweb window state.
pub struct Spiderweb {
    pub open: bool,
    /// Keys the board renders: the project's keyboard (128, or 256 in
    /// 256-Key Mode; v0.9 §059).
    pub keys: i64,
    pub board: Board,
    pub sel: BTreeSet<usize>,
    pub tool: Tool,
    pub view: canvas::View,
    pub draft: Option<canvas::Draft>,
    drag: Option<canvas::Drag>,
    undo: Vec<Vec<Arc<Value>>>,
    redo: Vec<Vec<Arc<Value>>>,
    /// Snap step (index into `canvas::SNAPS`).
    pub snap: usize,
    /// "Straighten" for new freehand strokes (0 = as drawn … 100).
    pub free_smooth: u8,
    /// Velocities new shapes start with.
    pub vel: (u8, u8),
    pub channels: Channels,
    pub paste: PasteOptions,
    /// Paste where the notes are drawn (the board's time is the project's).
    where_drawn: bool,
    generation: u64,
    job: Option<Job>,
    wanted: Option<RenderKey>,
    done: Option<RenderKey>,
    pub result: Option<Arc<Rendered>>,
    estimate: Estimate,
    verdict: Verdict,
    /// Huge boards render without asking.
    allow_big: bool,
    plan_key: String,
    pub plan: Option<PastePlan>,
    pub preview: Vec<TNote>,
    pub show_preview: bool,
    error: Option<String>,
    share_text: String,
    share_msg: Option<(bool, String)>,
    clipboard: Vec<Value>,
    density_tex: Option<(u64, egui::TextureHandle)>,
    canvas_rect: Option<egui::Rect>,
    window_rect: Option<egui::Rect>,
    /// The pointer was over the window last frame: its keys are the board's.
    hovered: bool,
    /// The control edited last and when (one undo step per run of edits).
    last_control: Option<(&'static str, Instant)>,
    /// The board the estimate is for.
    est_key: Option<RenderKey>,
    /// Free transform values (Transform section).
    pub xform: Xform,
    /// Installed fonts (read on first use of text).
    fonts: fonts::Scan,
    font_cache: Option<((String, u16, bool), std::rc::Rc<fonts::LoadedFont>)>,
    font_filter: String,
    /// The UI font's family (a fallback for text).
    pub ui_family: String,
    /// Settings for the next polygon / Hz bass box / text.
    new_polygon: Value,
    new_hz: (i64, f64),
    /// Colour the Area colour tool paints with (0 = none).
    pub paint_colour: i64,
    new_text: Value,
    text_settings_open: bool,
    /// The project's tempo where the board starts (Hz bass tuning).
    project_bpm: Option<f64>,
    /// Requests the app handles (they need the project).
    take_notes: bool,
    listen: bool,
    /// B2-F26: transport requests of this frame (the app handles them).
    pub(super) transport: Vec<TransportAction>,
    /// Where the shared transport is, in board beats (`None`: no song).
    pub(super) playhead: Option<f64>,
    /// The transport's position as the transport bar shows it.
    pub(super) playhead_label: String,
    pub(super) playing: bool,
    pub(super) looping: bool,
    /// The ruler is being dragged: the last beat sought.
    pub(super) scrub: Option<f64>,
}

impl Default for Spiderweb {
    fn default() -> Self {
        Spiderweb {
            open: false,
            keys: KEYS,
            board: Board::default(),
            sel: BTreeSet::new(),
            tool: Tool::Line,
            view: canvas::View::default(),
            draft: None,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            snap: 6,
            free_smooth: 0,
            vel: (127, 127),
            channels: Channels::default(),
            paste: PasteOptions::default(),
            where_drawn: true,
            generation: 0,
            job: None,
            wanted: None,
            done: None,
            result: None,
            estimate: Estimate::default(),
            verdict: Verdict::Ok,
            allow_big: false,
            plan_key: String::new(),
            plan: None,
            preview: Vec::new(),
            show_preview: true,
            error: None,
            share_text: String::new(),
            share_msg: None,
            clipboard: Vec::new(),
            density_tex: None,
            canvas_rect: None,
            window_rect: None,
            hovered: false,
            last_control: None,
            est_key: None,
            xform: Xform::default(),
            fonts: fonts::Scan::default(),
            font_cache: None,
            font_filter: String::new(),
            ui_family: String::new(),
            new_polygon: {
                let mut p = Value::obj();
                p.set("points", Value::Num(5.0));
                p.set("style", Value::Str("star".into()));
                p.set("inner", Value::Num(50.0));
                p.set("skip", Value::Num(2.0));
                p
            },
            new_hz: (33, 0.0),
            paint_colour: 1,
            new_text: {
                let mut t = Value::obj();
                t.set("text", Value::Str("Text".into()));
                t.set("font", Value::Str("Arial".into()));
                t.set("size", Value::Num(24.0));
                t
            },
            text_settings_open: false,
            project_bpm: None,
            take_notes: false,
            listen: false,
            transport: Vec::new(),
            playhead: None,
            playhead_label: String::new(),
            playing: false,
            looping: false,
            scrub: None,
        }
    }
}

/// Renders the board over `keys` keys (128, or 256 in 256-Key Mode).
fn render_job(
    shapes: Vec<Arc<Value>>,
    ppq: u32,
    ch: Channels,
    cancel: &AtomicBool,
    id: u64,
    keys: i64,
) -> Result<Rendered, String> {
    let started = Instant::now();
    let shapes: Vec<Value> = shapes.iter().map(|s| (**s).clone()).collect();
    let (mut notes, slots) = output::render_board(&shapes, ppq, keys, ch, cancel)
        .ok_or_else(|| "Stopped.".to_string())?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Stopped.".into());
    }
    let (art, t0) = output::to_generated(&notes, slots, true, keys);
    notes.sort_unstable_by_key(|n| (n[0], n[2]));
    let max_dur = notes.iter().map(|n| n[1] - n[0]).max().unwrap_or(0);
    let density = density(&notes, keys);
    Ok(Rendered {
        id,
        notes,
        max_dur,
        slots,
        art: Arc::new(art),
        t0,
        took: started.elapsed(),
        density,
    })
}

impl Spiderweb {
    /// v0.8 §069: the window's settings for `settings.json` (not the board).
    pub fn memory(&self) -> serde_json::Value {
        serde_json::json!({
            "snap": self.snap,
            "free_smooth": self.free_smooth,
            "vel_start": self.vel.0,
            "vel_end": self.vel.1,
            "channel_mode": match self.channels.mode {
                ChannelMode::Raw => "raw",
                ChannelMode::Single => "single",
                ChannelMode::Auto => "auto",
            },
            "split": match self.channels.split {
                Split::Key => "key",
                Split::Time => "time",
            },
            "where_drawn": self.where_drawn,
            "paste": crate::app::plugin_memory::paste_json(&self.paste),
        })
    }

    /// Reads [`Spiderweb::memory`] back (invalid values keep their defaults).
    pub fn restore(&mut self, v: &serde_json::Value) {
        use crate::app::plugin_memory::{flag, int, paste_restore};
        let Some(o) = v.as_object() else {
            return;
        };
        if let Some(i) = int(o, "snap", 0, canvas::SNAPS.len() as i64 - 1) {
            self.snap = i as usize;
        }
        if let Some(i) = int(o, "free_smooth", 0, 100) {
            self.free_smooth = i as u8;
        }
        if let (Some(a), Some(b)) = (int(o, "vel_start", 1, 127), int(o, "vel_end", 1, 127)) {
            self.vel = (a as u8, b as u8);
        }
        match o.get("channel_mode").and_then(serde_json::Value::as_str) {
            Some("raw") => self.channels.mode = ChannelMode::Raw,
            Some("single") => self.channels.mode = ChannelMode::Single,
            Some("auto") => self.channels.mode = ChannelMode::Auto,
            _ => {}
        }
        match o.get("split").and_then(serde_json::Value::as_str) {
            Some("key") => self.channels.split = Split::Key,
            Some("time") => self.channels.split = Split::Time,
            _ => {}
        }
        if let Some(b) = flag(o, "where_drawn") {
            self.where_drawn = b;
        }
        paste_restore(&mut self.paste, o.get("paste"));
    }

    /// Before an edit: the board as it was (one undo step).
    pub(super) fn checkpoint(&mut self) {
        self.undo.push(self.board.snapshot());
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// The last checkpoint changed nothing.
    pub(super) fn drop_checkpoint(&mut self) {
        self.undo.pop();
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.board.snapshot());
            self.board.restore(s);
            self.sel.clear();
            self.draft = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.board.snapshot());
            self.board.restore(s);
            self.sel.clear();
            self.draft = None;
        }
    }

    pub fn delete_selected(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint();
        let sel = std::mem::take(&mut self.sel);
        self.board.remove(&sel);
    }

    /// A new shape's settings (velocities).
    pub(super) fn new_shape_defaults(&self) -> Value {
        let mut sh = Value::obj();
        sh.set("vel0", Value::Num(f64::from(self.vel.0)));
        sh.set("vel1", Value::Num(f64::from(self.vel.1)));
        sh.set("end_dot", Value::Bool(false));
        sh
    }

    /// The board's keys are the board's while the pointer is over the window.
    pub fn captures_keys(&self) -> bool {
        self.open && self.hovered
    }

    /// Adds shapes (from a share code or the clipboard), selected.
    fn add_shapes(&mut self, shapes: Vec<Value>) {
        if shapes.is_empty() {
            return;
        }
        self.checkpoint();
        self.sel.clear();
        for sh in shapes {
            let i = self.board.push(sh);
            self.sel.insert(i);
        }
    }

    fn selected_or_all(&self) -> Vec<Value> {
        if self.sel.is_empty() {
            self.board.values()
        } else {
            self.sel
                .iter()
                .map(|&i| self.board.get(i).clone())
                .collect()
        }
    }

    /// Reads a share code (the text field or a clipboard paste).
    fn paste_share(&mut self, text: &str) -> bool {
        match share::unpack(text).and_then(|v| share::read_shapes(&v)) {
            Ok(shapes) => {
                let n = shapes.len();
                self.add_shapes(shapes);
                self.share_msg = Some((
                    true,
                    format!("{n} shape{} added.", if n == 1 { "" } else { "s" }),
                ));
                true
            }
            Err(e) => {
                self.share_msg = Some((false, e.to_string()));
                false
            }
        }
    }

    /// The board's keyboard: `keys` keys (the render and the canvas follow;
    /// a change re-renders).
    pub fn set_keys(&mut self, keys: i64) {
        self.keys = keys;
        self.view.top_key = (keys - 1) as f64;
    }

    /// Polls the worker; starts a render when the board changed (after the
    /// preflight).
    fn update(&mut self, ctx: &egui::Context, ppq: u32) {
        if self.fonts.poll() {
            self.font_cache = None;
        } else if self.fonts.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let key: RenderKey = (self.board.rev, self.channels, ppq, self.keys);
        if let Some(job) = &self.job {
            match job.rx.try_recv() {
                Ok((g, r)) => {
                    if g == job.generation {
                        match r {
                            Ok(res) => {
                                if self.where_drawn {
                                    self.paste.at = res.t0.max(0) as u64;
                                }
                                self.result = Some(Arc::new(res));
                                self.error = None;
                            }
                            Err(e) => {
                                if self.wanted == self.done {
                                    self.error = Some(e);
                                }
                            }
                        }
                        self.plan_key.clear();
                    }
                    self.job = None;
                }
                Err(_) => {
                    if self.wanted != Some(key) {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                    ctx.request_repaint_after(Duration::from_millis(30));
                }
            }
        }
        self.wanted = Some(key);
        if self.job.is_some() || self.done == Some(key) {
            return;
        }
        if self.board.is_empty() {
            self.result = None;
            self.error = None;
            self.verdict = Verdict::Ok;
            self.estimate = Estimate::default();
            self.done = Some(key);
            self.plan_key.clear();
            return;
        }
        let shapes = self.board.snapshot();
        if self.est_key != Some(key) {
            let values: Vec<Value> = shapes.iter().map(|s| (**s).clone()).collect();
            self.estimate = output::estimate(&values, ppq);
            let limits = Limits::for_system_memory(crate::platform::total_memory_bytes());
            self.verdict = preflight::check(&self.estimate, &limits);
            self.est_key = Some(key);
        }
        match &self.verdict {
            Verdict::Refuse(_) => {
                self.done = Some(key);
                self.result = None;
                self.plan_key.clear();
                return;
            }
            Verdict::Warn(_) if !self.allow_big => return,
            _ => {}
        }
        self.done = Some(key);
        self.generation += 1;
        let g = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let (tx, rx) = channel();
        let c = ctx.clone();
        let ch = self.channels;
        let keys = self.keys;
        let spawned = std::thread::Builder::new()
            .name("plumspirit-spiderweb".into())
            .spawn(move || {
                let r = render_job(shapes, ppq, ch, &c2, g, keys);
                let _ = tx.send((g, r));
                c.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.job = Some(Job {
                    generation: g,
                    rx,
                    cancel,
                    started: Instant::now(),
                })
            }
            Err(e) => self.error = Some(format!("Spiderweb could not start: {e}.")),
        }
    }

    /// Keys while the pointer is over the window.
    fn keys(&mut self, ctx: &egui::Context) {
        if !self.hovered || ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut pasted: Option<String> = None;
        let mut copy = false;
        let mut cut = false;
        let (undo, redo, del, esc, enter, all, tool, live, split) = ctx.input_mut(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Paste(t) => pasted = Some(t.clone()),
                    egui::Event::Copy => copy = true,
                    egui::Event::Cut => cut = true,
                    _ => {}
                }
            }
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let redo = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let del = i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
            let esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            let enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            let all = i.consume_key(egui::Modifiers::COMMAND, egui::Key::A);
            let split = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::L,
            );
            let live = i.consume_key(egui::Modifiers::COMMAND, egui::Key::L);
            let mut tool = None;
            for (t, _, k, _) in TOOLS {
                if i.consume_key(egui::Modifiers::NONE, k) {
                    tool = Some(t);
                }
            }
            (undo, redo, del, esc, enter, all, tool, live, split)
        });
        // (Ctrl+Shift+Z consumed first would be read as Ctrl+Z by egui's loose
        // matching; both are consumed above, redo wins)
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if del {
            self.delete_selected();
        }
        if esc {
            if self.draft.is_some() {
                self.draft = None;
            } else {
                self.sel.clear();
            }
        }
        if enter {
            self.finish_poly();
        }
        if all {
            self.sel = (0..self.board.len()).collect();
        }
        if split {
            self.split_selected();
        } else if live {
            self.make_live();
        }
        if let Some(t) = tool {
            self.set_tool(t);
        }
        if copy || cut {
            self.clipboard = self
                .sel
                .iter()
                .map(|&i| self.board.get(i).clone())
                .collect();
            if !self.clipboard.is_empty() {
                ctx.copy_text(share::shapes_line(&self.clipboard));
            }
            if cut {
                self.delete_selected();
            }
        }
        if let Some(t) = pasted {
            if share::unpack(&t).is_ok() {
                self.paste_share(&t);
            } else {
                let c = self.clipboard.clone();
                self.add_shapes(c);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (t, name, key, hint) in TOOLS {
                let r = ui.selectable_label(self.tool == t, name).on_hover_text(format!(
                    "{hint} ({})",
                    key.name()
                ));
                if r.clicked() {
                    self.set_tool(t);
                }
            }
            self.tool_settings_ui(ui);
            ui.separator();
            ui.label("Snap");
            egui::ComboBox::from_id_salt("sw-snap")
                .width(56.0)
                .selected_text(canvas::SNAPS[self.snap].0)
                .show_ui(ui, |ui| {
                    for (i, (s, _)) in canvas::SNAPS.iter().enumerate() {
                        ui.selectable_value(&mut self.snap, i, *s);
                    }
                })
                .response
                .on_hover_text("Points snap to this step and to whole keys (hold Shift to place freely).");
            if self.tool == Tool::Free {
                ui.label("Straighten");
                ui.add(egui::Slider::new(&mut self.free_smooth, 0..=100))
                    .on_hover_text("0 = as drawn; higher values make new freehand strokes perfect lines, curves and shapes.");
            }
            ui.separator();
            self.transport_ui(ui);
            ui.separator();
            if ui
                .add_enabled(!self.undo.is_empty(), egui::Button::new("Undo"))
                .on_hover_text("Undo the last change to the board (Ctrl+Z).")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(!self.redo.is_empty(), egui::Button::new("Redo"))
                .on_hover_text("Ctrl+Shift+Z / Ctrl+Y")
                .clicked()
            {
                self.redo();
            }
            if ui
                .add_enabled(!self.sel.is_empty(), egui::Button::new("Delete"))
                .clicked()
            {
                self.delete_selected();
            }
        });
    }

    /// B2-F26: the shared transport's buttons (the main window's transport;
    /// its keys — Space, Home, End, … — work over the board as everywhere).
    fn transport_ui(&mut self, ui: &mut egui::Ui) {
        use super::commands::Command;
        let on = self.playhead.is_some();
        let buttons = [
            (
                "⏮",
                "Go to the beginning of the song",
                Command::GoToStart,
                TransportAction::Start,
            ),
            (
                if self.playing { "⏸" } else { "▶" },
                "Play / Pause the song: the main transport, its outputs and tempo map",
                Command::PlayPause,
                TransportAction::PlayPause,
            ),
            (
                "⏹",
                "Stop (also ends the notes Listen plays)",
                Command::Stop,
                TransportAction::Stop,
            ),
            (
                "⏭",
                "Go to the end of the song",
                Command::GoToEnd,
                TransportAction::End,
            ),
        ];
        for (label, hint, cmd, action) in buttons {
            if ui
                .add_enabled(on, egui::Button::new(label))
                .on_hover_ui(|ui| {
                    ui.label(format!("{hint}{}.", super::keybinds::hint(cmd)));
                })
                .clicked()
            {
                self.transport.push(action);
            }
        }
        if ui
            .add_enabled(on, egui::Button::selectable(self.looping, "Loop"))
            .on_hover_ui(|ui| {
                ui.label(format!(
                    "Loop the song's loop range (the main transport's Loop){}.",
                    super::keybinds::hint(Command::ToggleLoop)
                ));
            })
            .clicked()
        {
            self.transport.push(TransportAction::ToggleLoop);
        }
    }

    fn side_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Shapes")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(format!(
                    "{} shape{} on the board, {} selected.",
                    self.board.len(),
                    if self.board.len() == 1 { "" } else { "s" },
                    self.sel.len()
                ));
                ui.horizontal(|ui| {
                    ui.label("New shapes' velocity from");
                    ui.add(crate::ui::stepper::Stepper::new(&mut self.vel.0).range(1..=127));
                    ui.label("to");
                    ui.add(crate::ui::stepper::Stepper::new(&mut self.vel.1).range(1..=127));
                });
                if ui
                    .button("Take the selected notes")
                    .on_hover_text("The notes selected in the Piano Roll's active track become one shape on the board (their subchannels become its colors).")
                    .clicked()
                {
                    self.take_notes = true;
                }
            });
        if self.tool == Tool::Text || self.text_settings_open {
            egui::CollapsingHeader::new("New text")
                .id_salt("sw-newtext")
                .default_open(true)
                .show(ui, |ui| {
                    let mut t = self.new_text.clone();
                    if props::text_settings_ui(
                        ui,
                        "sw-newtext",
                        &mut t,
                        self.fonts.library.as_deref(),
                        &mut self.font_filter,
                    ) {
                        self.new_text = t;
                    }
                    ui.label(
                        RichText::new("Click on the board where the text starts.")
                            .weak()
                            .small(),
                    );
                });
        }
        if !self.sel.is_empty() {
            egui::CollapsingHeader::new(format!(
                "Selected shape{}",
                if self.sel.len() == 1 { "" } else { "s" }
            ))
            .id_salt("sw-props")
            .default_open(true)
            .show(ui, |ui| self.props_ui(ui));
        }
        egui::CollapsingHeader::new("Channels")
            .default_open(true)
            .show(ui, |ui| {
                let mut m = match self.channels.mode {
                    ChannelMode::Raw => 0,
                    ChannelMode::Single => 1,
                    ChannelMode::Auto => 2,
                };
                egui::ComboBox::from_id_salt("sw-ch")
                    .selected_text(CHANNEL_MODES[m].0)
                    .show_ui(ui, |ui| {
                        for (i, (n, h)) in CHANNEL_MODES.iter().enumerate() {
                            ui.selectable_value(&mut m, i, *n).on_hover_text(*h);
                        }
                    });
                self.channels.mode = [ChannelMode::Raw, ChannelMode::Single, ChannelMode::Auto][m];
                if m == 2 {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.channels.split, Split::Key, "Same key clashes");
                        ui.radio_value(
                            &mut self.channels.split,
                            Split::Time,
                            "Any overlap clashes",
                        );
                    });
                }
            });
        egui::CollapsingHeader::new("Share codes")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new("Spiderweb shares shapes as one line of text; PlumSpirit reads and writes the same codes.").weak());
                if ui
                    .add_enabled(!self.board.is_empty(), egui::Button::new(if self.sel.is_empty() {
                        "Copy the board as a share code"
                    } else {
                        "Copy the selected shapes as a share code"
                    }))
                    .clicked()
                {
                    let line = share::shapes_line(&self.selected_or_all());
                    ui.ctx().copy_text(line);
                    self.share_msg = Some((true, "Copied.".into()));
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.share_text)
                        .desired_rows(2)
                        .hint_text("Paste a share code here (SPIDERWEB1:…)"),
                );
                if ui
                    .add_enabled(!self.share_text.trim().is_empty(), egui::Button::new("Add to the board"))
                    .clicked()
                {
                    let t = self.share_text.clone();
                    if self.paste_share(&t) {
                        self.share_text.clear();
                    }
                }
                if let Some((ok, m)) = &self.share_msg {
                    if *ok {
                        ui.label(m);
                    } else {
                        ui.colored_label(ui.visuals().error_fg_color, m);
                    }
                }
            });
    }

    fn paste_section(
        &mut self,
        ui: &mut egui::Ui,
        doc: &plumspirit_engine::model::PlumDocument,
        ti: usize,
        cursor: u64,
    ) {
        egui::CollapsingHeader::new("Paste into the Piano Roll")
            .default_open(true)
            .show(ui, |ui| {
                let art = self.result.as_ref().map(|r| r.art.clone());
                let before = self.paste.at;
                paste_ui::options_ui(
                    ui,
                    "sw-paste",
                    &mut self.paste,
                    doc,
                    ti,
                    art.as_deref(),
                    cursor,
                );
                if self.paste.at != before {
                    self.where_drawn = false;
                }
                let mut wd = self.where_drawn;
                if ui
                    .checkbox(&mut wd, "Where drawn")
                    .on_hover_text("The board's time is the project's: the notes go exactly where they are on the board.")
                    .changed()
                {
                    self.where_drawn = wd;
                    if wd && let Some(r) = &self.result {
                        self.paste.at = r.t0.max(0) as u64;
                    }
                }
                if let (Some(plan), Some(a)) = (&self.plan, &art) {
                    paste_ui::mapping_ui(ui, "sw", plan, a);
                }
            });
    }

    fn status_ui(&mut self, ui: &mut egui::Ui, apply: &mut bool) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_preview, "Preview")
                .on_hover_text("Show the notes on the active track before Apply.");
            if let Some(j) = &self.job {
                ui.spinner();
                ui.label(
                    RichText::new(format!(
                        "making notes… {:.1} s",
                        j.started.elapsed().as_secs_f64()
                    ))
                    .weak(),
                );
                if ui.button("Stop").clicked() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            if !self.board.is_empty() {
                let e = &self.estimate;
                ui.label(
                    RichText::new(format!(
                        "Estimate: up to {} notes · {} while making · {} of undo",
                        plumspirit_engine::midi::group_digits(e.notes),
                        crate::diagnostics::format_bytes(e.temp_bytes),
                        crate::diagnostics::format_bytes(e.undo_bytes)
                    ))
                    .weak()
                    .small(),
                )
                .on_hover_text("Worked out before any note is made: boards too big for this PC's memory are refused, big ones wait for \"Make them\".");
            }
            match &self.verdict {
                Verdict::Refuse(m) => {
                    ui.colored_label(ui.visuals().error_fg_color, m);
                }
                Verdict::Warn(m) if !self.allow_big => {
                    ui.colored_label(ui.visuals().warn_fg_color, m);
                    if ui.button("Make them").clicked() {
                        self.allow_big = true;
                    }
                }
                _ => {}
            }
        });
        ui.horizontal(|ui| {
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if let (Some(r), Some(plan)) = (&self.result, &self.plan) {
                ui.label(format!(
                    "{} notes on {} channel{} in {}. {}",
                    plumspirit_engine::midi::group_digits(r.notes.len() as u64),
                    r.slots,
                    if r.slots == 1 { "" } else { "s" },
                    super::tasks::format_task_duration(r.took),
                    plan.summary()
                ));
                if let Some(e) = &plan.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            } else if self.board.is_empty() {
                ui.label(
                    RichText::new("Draw shapes on the board; their notes appear here.").weak(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.open = false;
                }
                if ui
                    .add_enabled(!self.preview.is_empty(), egui::Button::new("Listen"))
                    .on_hover_text("Play the notes through PlumSpirit's synthesizer (Esc or Stop in Scrub stops).")
                    .clicked()
                {
                    self.listen = true;
                }
                let can = self
                    .plan
                    .as_ref()
                    .is_some_and(|p| p.error.is_none() && p.notes > 0)
                    && self.job.is_none()
                    && self.done == self.wanted;
                if ui
                    .add_enabled(can, egui::Button::new("Apply"))
                    .on_hover_text("Paste the notes into the active track (one undo step).")
                    .clicked()
                {
                    *apply = true;
                }
            });
        });
    }

    /// One undo step for a run of edits from the same control (dragging a
    /// value): a new step only when another control (or a gesture) came
    /// between.
    fn checkpoint_once(&mut self, what: &'static str) {
        let now = Instant::now();
        let again = self
            .last_control
            .is_some_and(|(w, t)| w == what && now.duration_since(t) < Duration::from_millis(800));
        if !again {
            self.checkpoint();
        }
        self.last_control = Some((what, now));
    }

    fn set_tool(&mut self, t: Tool) {
        self.tool = t;
        self.draft = None;
        if t == Tool::Text {
            self.fonts.start();
        }
    }

    /// Places new text at `at` (`[beat, pitch]`), selected.
    pub(super) fn place_text(&mut self, at: sw::paths::P) {
        if self.fonts.busy() {
            self.share_msg = Some((
                false,
                "Reading the installed fonts… click again in a moment.".into(),
            ));
            return;
        }
        let t = self.new_text.clone();
        let Some(font) = self.font_for(&t) else {
            self.share_msg = Some((false, "No font is available to type with.".into()));
            return;
        };
        let base = self.new_shape_defaults();
        match props::new_text_shape(&t, at, self.view_k(), font.as_ref(), &base) {
            Some(sh) => {
                self.checkpoint();
                let i = self.board.push(sh);
                self.sel.clear();
                self.sel.insert(i);
            }
            None => self.share_msg = Some((false, "Type some text first (New text).".into())),
        }
    }
}

impl PlumSpiritApp {
    pub(super) fn open_spiderweb(&mut self) {
        self.spiderweb.open = true;
    }

    fn spiderweb_ppq(&self) -> u32 {
        self.doc()
            .and_then(|d| d.division().musical_ppqn())
            .map_or(960, |p| p.clamp(1, 65535) as u32)
    }

    /// The window (once per frame).
    pub(super) fn spiderweb_ui(&mut self, ctx: &egui::Context) {
        if !self.spiderweb.open {
            self.spiderweb.hovered = false;
            if self.spiderweb.playhead.take().is_some() {
                // B2-F26: however the window closed, Listen's notes end.
                self.spiderweb.scrub = None;
                self.spiderweb.transport.clear();
                self.spiderweb_silence();
            }
            return;
        }
        let ppq = self.spiderweb_ppq();
        // v0.9: the board spans the project's keys (256 in 256-Key Mode).
        let keys = self.doc().map_or(KEYS, |d| d.project().keys.keys() as i64);
        self.spiderweb.set_keys(keys);
        self.spiderweb.update(ctx, ppq);
        self.spiderweb.keys(ctx);
        let ti = self.active_track();
        // "At the cursor": the edit cursor on the roll's axis (V09-S11); the
        // tempo is read at the song position.
        let cursor = self.edit_cursor();
        let song_cursor = self.roll.cursor_tick;
        let skip10 = self.settings.skip_channel_10;
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            self.spiderweb.open = false;
            return;
        };
        let dest = doc.track(ti).map(|t| t.id());
        self.spiderweb_playhead(ppq);
        self.spiderweb.project_bpm = Some(doc.conductor().bpm_at(song_cursor));
        if self.spiderweb.ui_family.is_empty() {
            self.spiderweb.ui_family = self.font.family.clone();
        }
        if std::mem::take(&mut self.spiderweb.take_notes) {
            self.spiderweb_take_notes(&doc, self.edit_target(), ppq);
        }
        if let (Some(r), Some(dest)) = (&self.spiderweb.result, dest) {
            let spec = self.spiderweb.paste.spec(dest, "Spiderweb", skip10);
            let t = doc.track(ti).expect("checked");
            let key = format!(
                "{:p}|{:?}|{}|{}|{}",
                Arc::as_ptr(&r.art),
                spec,
                t.subchannels_revision(),
                t.notes.revision(),
                doc.track_count()
            );
            if key != self.spiderweb.plan_key {
                let plan = paste::plan(&doc, &r.art, &spec);
                self.spiderweb.preview = paste_ui::preview_notes(&r.art, &plan, spec.at);
                self.spiderweb.plan = Some(plan);
                self.spiderweb.plan_key = key;
            }
        } else if self.spiderweb.result.is_none() {
            self.spiderweb.plan = None;
            self.spiderweb.preview.clear();
        }
        let mut open = true;
        let mut apply = false;
        let info =
            plumspirit_engine::plugins::info(plumspirit_engine::plugins::PluginId::Spiderweb);
        let screen = ctx.content_rect();
        let shown = egui::Window::new("Spiderweb — Plugins")
            .id(egui::Id::new("plumspirit-spiderweb"))
            .open(&mut open)
            .resizable(true)
            .default_size([
                (screen.width() * 0.8).min(1200.0),
                (screen.height() * 0.75).min(780.0),
            ])
            .default_pos(screen.left_top() + egui::vec2(screen.width() * 0.08, 60.0))
            .show(ctx, |ui| {
                let w = &mut self.spiderweb;
                egui::Panel::top("sw-top").show(ui, |ui| {
                    ui.label(RichText::new(super::plugins::credit_line(info)).small());
                    w.toolbar(ui);
                });
                egui::Panel::bottom("sw-bottom").show(ui, |ui| {
                    w.status_ui(ui, &mut apply);
                });
                egui::Panel::right("sw-side")
                    .resizable(true)
                    .default_size(310.0)
                    .size_range(220.0..=520.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("sw-side")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                w.side_ui(ui);
                                w.paste_section(ui, &doc, ti, cursor);
                            });
                    });
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    let size = ui.available_size().max(egui::vec2(160.0, 120.0));
                    w.canvas_ui(ui, size, ppq);
                });
            });
        self.spiderweb.window_rect = shown.map(|s| s.response.rect);
        self.spiderweb.hovered = match (self.spiderweb.window_rect, ctx.pointer_hover_pos()) {
            (Some(r), Some(p)) => r.contains(p),
            _ => false,
        };
        if !open {
            self.spiderweb.open = false;
        }
        drop(doc);
        for a in std::mem::take(&mut self.spiderweb.transport) {
            self.spiderweb_transport(a, ppq);
        }
        if apply {
            self.apply_spiderweb();
        }
        if std::mem::take(&mut self.spiderweb.listen) {
            self.spiderweb_listen();
        }
    }

    /// B2-F26: where the shared transport is, on the board's axis (an opened
    /// clip's content ticks), with the transport bar's position and time.
    fn spiderweb_playhead(&mut self, ppq: u32) {
        let pos = self.transport.position();
        self.spiderweb.playhead = Some(board_beat(pos, self.edit_shift(), ppq));
        self.spiderweb.playing = self.transport.state().is_running();
        let lc = self.loop_config();
        self.spiderweb.looping = lc.enabled && lc.range.is_some();
        let at = self.meter_map().map(|m| m.describe(pos));
        let clock = self
            .display_tempo()
            .map(|t| super::format_clock(t.seconds_at_tick(pos)));
        self.spiderweb.playhead_label = match (at, clock) {
            (Some(a), Some(c)) => format!("{a} · {c}"),
            (Some(a), None) => a,
            (None, Some(c)) => c,
            (None, None) => String::new(),
        };
    }

    /// B2-F26: one transport request of the window, through the commands the
    /// main transport uses (one playhead; never an undo step).
    pub(super) fn spiderweb_transport(&mut self, a: TransportAction, ppq: u32) {
        match a {
            TransportAction::PlayPause => self.play_pause(),
            TransportAction::Stop => {
                self.stop_playback();
                self.spiderweb_silence();
            }
            TransportAction::Start => self.seek_to(0),
            TransportAction::End => {
                if let Some(end) = self.doc().map(|d| d.duration_ticks()) {
                    self.seek_to(end);
                }
            }
            TransportAction::ToggleLoop => self.toggle_loop(),
            TransportAction::Seek(b) => {
                let tick = song_tick(b, self.edit_shift(), ppq);
                self.seek_to(tick);
            }
        }
    }

    /// Ends Listen's notes and any held audition (Stop, closing the window).
    fn spiderweb_silence(&mut self) {
        self.scrub_stop();
        self.transport.audition_all_off();
    }

    /// The active track's selected notes become one shape (pasted notes).
    fn spiderweb_take_notes(
        &mut self,
        doc: &plumspirit_engine::model::PlumDocument,
        ti: plumspirit_engine::editor::NoteTarget,
        ppq: u32,
    ) {
        const MOST: usize = 2_000_000;
        // The roll's selected notes (an opened clip's content; V09-S11).
        let Ok(t) = plumspirit_engine::editor::note_view(doc, ti) else {
            return;
        };
        let mut subs: Vec<u32> = Vec::new();
        let mut rows: Vec<[i64; 5]> = Vec::new();
        let mut more = 0usize;
        t.notes.for_each_selected(|n| {
            if rows.len() >= MOST {
                more += 1;
                return;
            }
            rows.push([
                n.start_tick.min(i64::MAX as u64 / 4) as i64,
                n.duration_ticks.min(i64::MAX as u64 / 4) as i64,
                i64::from(n.attrs.pitch()),
                i64::from(n.attrs.velocity()),
                i64::from(n.sub.0),
            ]);
            subs.push(n.sub.0);
        });
        if rows.is_empty() {
            self.set_status("Select notes in the Piano Roll first.", StatusKind::Info);
            return;
        }
        subs.sort_unstable();
        subs.dedup();
        for r in &mut rows {
            r[4] = subs.binary_search(&(r[4] as u32)).unwrap_or(0) as i64;
        }
        match sw::edit::notes_shape(&rows, ppq, "Notes") {
            Some(sh) => {
                let w = &mut self.spiderweb;
                w.checkpoint();
                let i = w.board.push(sh);
                w.sel.clear();
                w.sel.insert(i);
                self.set_status(
                    if more > 0 {
                        format!(
                            "Spiderweb took the first {} selected notes ({} more were left out).",
                            plumspirit_engine::midi::group_digits(MOST as u64),
                            plumspirit_engine::midi::group_digits(more as u64)
                        )
                    } else {
                        format!(
                            "Spiderweb took {} notes as one shape.",
                            plumspirit_engine::midi::group_digits(rows.len() as u64)
                        )
                    },
                    StatusKind::Info,
                );
            }
            None => self.set_status("Those notes could not become a shape.", StatusKind::Warning),
        }
    }

    /// Plays the previewed notes through the audition synthesizer.
    fn spiderweb_listen(&mut self) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let tempo = plumspirit_engine::model::TempoMap::for_document_at(
            &doc,
            plumspirit_engine::transform::TEMPO_RATE,
            plumspirit_engine::model::Speed::NORMAL,
        );
        let mut v = Vec::new();
        let mut skipped = 0usize;
        for n in &self.spiderweb.preview {
            if v.len() >= super::scrub::MAX_PLAY_NOTES {
                skipped += 1;
                continue;
            }
            let at = tempo.seconds_at_tick(n.start);
            let len = (tempo.seconds_at_tick(n.start + n.dur) - at).max(0.01);
            v.push(super::scrub::Planned {
                at,
                len,
                key: n.pitch,
                vel: n.vel,
                ch: n.ch,
                tick: n.start,
            });
        }
        self.scrub_play_planned(v, skipped);
    }

    fn apply_spiderweb(&mut self) {
        // Spiderweb pastes into a track's own notes (and may add subtracks);
        // an opened clip's own notes are not a destination (V09-S11).
        if matches!(
            self.edit_target(),
            plumspirit_engine::editor::NoteTarget::Content { .. }
        ) {
            self.set_status(
                "Spiderweb pastes into a track's own notes: select the track in the track panel (instead of the open clip) and apply again.",
                StatusKind::Info,
            );
            return;
        }
        let ti = self.active_track();
        let skip10 = self.settings.skip_channel_10;
        let Some(r) = &self.spiderweb.result else {
            return;
        };
        let art = r.art.clone();
        let Some(doc) = self.doc() else {
            return;
        };
        let Some(dest) = doc.track(ti).map(|t| t.id()) else {
            return;
        };
        let spec = self.spiderweb.paste.spec(dest, "Spiderweb", skip10);
        let plan = paste::plan(doc, &art, &spec);
        let started = Instant::now();
        match paste::build(doc, &art, &spec, &plan, "Spiderweb") {
            Ok(op) => {
                if self.apply_op(op).is_some() {
                    self.tasks.record(
                        "Spiderweb",
                        started.elapsed(),
                        super::tasks::TaskOutcome::Done,
                    );
                    self.set_status(format!("Spiderweb: {}", plan.summary()), StatusKind::Info);
                    self.spiderweb.plan_key.clear();
                }
            }
            Err(e) => self.show_error("Spiderweb was not applied.", e.user_message()),
        }
    }
}

/// The Spiderweb preview on the active track.
pub fn spiderweb_preview(w: &Spiderweb) -> Option<crate::piano_roll::ToolPreview<'_>> {
    if !w.open || !w.show_preview || w.preview.is_empty() {
        return None;
    }
    Some(crate::piano_roll::ToolPreview::Run {
        notes: &w.preview,
        extra: &[],
        max_dur: w.preview.iter().map(|n| n.dur).max().unwrap_or(0),
        all: false,
        rev: 0,
    })
}

/// Song tick `pos` as a board beat (`shift`: song tick of the board's tick 0;
/// before it the beat is negative, off the board).
pub(super) fn board_beat(pos: u64, shift: i128, ppq: u32) -> f64 {
    (i128::from(pos) - shift) as f64 / f64::from(ppq.max(1))
}

/// Board beat `b` as a song tick (the board starts at beat 0).
pub(super) fn song_tick(b: f64, shift: i128, ppq: u32) -> u64 {
    let c = (b.max(0.0) * f64::from(ppq.max(1))).round();
    let c = if c.is_finite() {
        c.min(u64::MAX as f64 / 2.0) as u64
    } else {
        0
    };
    super::clip_session::shifted(c, shift)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::canvas::Draft;
    use super::*;
    use plumspirit_engine::json;

    fn line(w: &mut Spiderweb, a: [f64; 2], b: [f64; 2]) {
        w.draft = Some(Draft::Two {
            kind: "line",
            a,
            b,
            follow: false,
        });
        w.commit_draft();
    }

    #[test]
    fn a_drawn_line_is_one_undo_step_and_comes_back_with_redo() {
        let mut w = Spiderweb::default();
        line(&mut w, [0.0, 60.0], [1.0, 64.0]);
        assert_eq!(w.board.len(), 1);
        assert_eq!(sw::kind(w.board.get(0)), "line");
        assert_eq!(w.sel.len(), 1);
        w.undo();
        assert!(w.board.is_empty());
        w.redo();
        assert_eq!(w.board.len(), 1);
    }

    #[test]
    fn box_tools_place_circles_polygons_and_hz_bass() {
        let mut w = Spiderweb::default();
        w.project_bpm = Some(140.0);
        for tool in [Tool::Circle, Tool::Polygon, Tool::Hz] {
            w.draft = Some(Draft::Box {
                tool,
                a: [0.0, 40.0],
                b: [2.0, 50.0],
            });
            w.commit_draft();
        }
        assert_eq!(w.board.len(), 3);
        assert_eq!(sw::kind(w.board.get(0)), "custom");
        assert!(w.board.get(1).has("polygon"));
        let hz = w.board.get(2).get("hz").unwrap();
        assert_eq!(hz.get("bpm").and_then(Value::as_f64), Some(140.0));
        // a click without a box makes nothing
        w.draft = Some(Draft::Box {
            tool: Tool::Circle,
            a: [1.0, 40.0],
            b: [1.0, 40.0],
        });
        w.commit_draft();
        assert_eq!(w.board.len(), 3);
    }

    #[test]
    fn a_dragged_funnel_gets_its_line_and_wall() {
        let mut w = Spiderweb::default();
        w.draft = Some(Draft::Two {
            kind: "funnel",
            a: [0.0, 60.0],
            b: [2.0, 66.0],
            follow: false,
        });
        w.commit_draft();
        let f = w.board.get(0);
        assert_eq!(sw::kind(f), "funnel");
        assert_eq!(sw::shape_pts(f).len(), 4);
        assert!(!sw::shape_notes(f, 96, 128).is_empty());
    }

    #[test]
    fn flips_and_turns_apply_to_the_selection_as_one_undo_step() {
        let mut w = Spiderweb::default();
        line(&mut w, [0.0, 60.0], [1.0, 64.0]);
        line(&mut w, [2.0, 60.0], [3.0, 64.0]);
        w.sel = (0..2).collect();
        let before = w.board.values();
        w.transform_selected(&Xform::flip_x());
        let p0 = sw::shape_pts(w.board.get(0));
        // the group mirrors around its middle (beat 1.5): the first line moves right
        assert!((p0[0][0] - 3.0).abs() < 1e-9, "{p0:?}");
        w.transform_selected(&Xform::rotation(90.0));
        w.undo();
        w.undo();
        assert_eq!(w.board.values(), before);
    }

    #[test]
    fn live_shapes_split_back_and_undo() {
        let mut w = Spiderweb::default();
        line(&mut w, [0.0, 60.0], [1.0, 64.0]);
        line(&mut w, [1.0, 64.0], [2.0, 60.0]);
        w.sel = (0..2).collect();
        w.make_live();
        assert_eq!(w.board.len(), 1);
        assert_eq!(sw::kind(w.board.get(0)), "custom");
        w.split_selected();
        assert_eq!(w.board.len(), 2);
        assert_eq!(sw::kind(w.board.get(0)), "line");
        w.undo();
        w.undo();
        assert_eq!(w.board.len(), 2);
        assert_eq!(sw::kind(w.board.get(1)), "line");
    }

    #[test]
    fn a_run_of_changes_to_one_setting_is_one_undo_step() {
        let mut w = Spiderweb::default();
        w.draft = Some(Draft::Box {
            tool: Tool::Circle,
            a: [0.0, 40.0],
            b: [2.0, 50.0],
        });
        w.commit_draft();
        let steps = w.undo.len();
        for f in ["fill", "spam", "fill"] {
            w.apply_edits("fill", vec![("", "fill", Value::Str(f.into()))], |_| true);
        }
        assert_eq!(w.undo.len(), steps + 1);
        assert_eq!(sw::gets(w.board.get(0), "fill"), Some("fill"));
        // a change that changes nothing takes no step
        w.last_control = None;
        w.apply_edits(
            "fill",
            vec![("", "fill", Value::Str("fill".into()))],
            |_| true,
        );
        assert_eq!(w.undo.len(), steps + 1);
        w.undo();
        assert_eq!(sw::gets(w.board.get(0), "fill"), Some("empty"));
    }

    #[test]
    fn double_clicking_a_curve_adds_a_point_and_alt_click_makes_a_corner() {
        let mut w = Spiderweb::default();
        w.draft = Some(Draft::Two {
            kind: "curve",
            a: [0.0, 60.0],
            b: [2.0, 70.0],
            follow: false,
        });
        w.commit_draft();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let pts = sw::shape_pts(w.board.get(0));
        let mid = sw::curves::seg_point(pts[0], pts[1], pts[2], pts[3], 0.5);
        let notes = sw::shape_notes(w.board.get(0), 96, 128);
        assert!(w.curve_add_anchor(r, w.view.pos(r, mid)));
        assert_eq!(sw::shape_pts(w.board.get(0)).len(), 7);
        // the same curve, so the same notes
        assert_eq!(sw::shape_notes(w.board.get(0), 96, 128), notes);
        w.toggle_sharp(0, 3);
        assert!(w.board.get(0).has("sharp"));
        w.toggle_sharp(0, 3);
        assert!(!w.board.get(0).has("sharp"));
    }

    #[test]
    fn painting_an_area_colours_its_notes() {
        let mut w = Spiderweb::default();
        w.draft = Some(Draft::Box {
            tool: Tool::Circle,
            a: [0.0, 40.0],
            b: [4.0, 60.0],
        });
        w.commit_draft();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        w.view.top = 70.0;
        w.paint_colour = 4;
        w.paint_area(r, w.view.pos(r, [2.0, 50.0]));
        let sh = w.board.get(0);
        assert_eq!(sw::gets(sh, "fill"), Some("fill"));
        let (_, tracks) = sw::notes_tracks(sh, 96, 128);
        assert!(tracks.is_some_and(|t| t.contains(&4)));
    }

    /// v0.9 256-Key Mode (S21 step 4b): the board spans the project's keys —
    /// a line drawn up to key 220 renders its notes above 127 on a 256-key
    /// board (and in its 256-row density picture) and stops at 127 on a
    /// standard one; the canvas's top key follows; a key change re-renders.
    #[test]
    fn the_board_spans_the_projects_keys() {
        let line =
            sw::clean_shape(&json::parse(r#"{"kind":"line","pts":[[0,100],[4,220]]}"#).unwrap())
                .unwrap();
        let top = |r: &Rendered| r.notes.iter().map(|n| n[2]).max().unwrap();
        let wide = render_job(
            vec![Arc::new(line.clone())],
            96,
            Channels::default(),
            &AtomicBool::new(false),
            1,
            256,
        )
        .unwrap();
        assert!(top(&wide) > 127 && top(&wide) <= 220, "{}", top(&wide));
        assert!(wide.art.notes.iter().any(|n| n.pitch > 127));
        assert_eq!(wide.density.as_ref().unwrap().image.size[1], 256);
        let std = render_job(
            vec![Arc::new(line)],
            96,
            Channels::default(),
            &AtomicBool::new(false),
            2,
            KEYS,
        )
        .unwrap();
        assert!(top(&std) <= 127);
        assert_eq!(std.density.as_ref().unwrap().image.size[1], 128);
        let mut w = Spiderweb::default();
        assert_eq!((w.keys, w.view.top_key), (128, 127.0));
        w.set_keys(256);
        assert_eq!((w.keys, w.view.top_key), (256, 255.0));
    }

    #[test]
    fn the_board_renders_on_the_worker_path_with_fixed_colours() {
        let mut a =
            sw::clean_shape(&json::parse(r#"{"kind":"line","pts":[[0,60],[1,64]]}"#).unwrap())
                .unwrap();
        sw::edit::set_fixed_colour(&mut a, Some(3));
        let b = sw::clean_shape(&json::parse(r#"{"kind":"line","pts":[[0,70],[1,74]]}"#).unwrap())
            .unwrap();
        let r = render_job(
            vec![Arc::new(a), Arc::new(b)],
            96,
            Channels::default(),
            &AtomicBool::new(false),
            1,
            KEYS,
        )
        .unwrap();
        assert!(r.slots >= 4);
        assert!(r.art.notes.iter().any(|n| n.color == 3));
        assert!(r.density.is_some());
        // a stopped run gives no notes
        let stop = AtomicBool::new(true);
        assert!(render_job(Vec::new(), 96, Channels::default(), &stop, 2, KEYS).is_err());
    }

    // ------------------------------------------- B2-F26: the shared playhead

    #[test]
    fn board_beats_and_song_ticks_map_both_ways_including_an_opened_clip() {
        assert_eq!(board_beat(1920, 0, 960), 2.0);
        assert_eq!(song_tick(2.0, 0, 960), 1920);
        assert_eq!(song_tick(2.0004, 0, 960), 1920);
        // an opened clip whose content starts at song tick 4800
        assert_eq!(board_beat(4800 + 960, 4800, 960), 1.0);
        assert_eq!(song_tick(1.0, 4800, 960), 5760);
        // before the clip: off the board (negative); the board starts at 0
        assert!(board_beat(0, 4800, 960) < 0.0);
        assert_eq!(song_tick(-3.0, 4800, 960), 4800);
        assert_eq!(song_tick(f64::NAN, 4800, 960), 4800);
        for t in [0u64, 1, 959, 960, 123_457, 9_999_999] {
            assert_eq!(song_tick(board_beat(t, 0, 960), 0, 960), t);
            assert_eq!(song_tick(board_beat(t + 480, 480, 96), 480, 96), t + 480);
        }
    }

    fn canvas_frame(ctx: &egui::Context, w: &mut Spiderweb, mut events: Vec<egui::Event>) {
        // the frame's modifiers are the buttons' (egui keeps them from events)
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                egui::Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| w.canvas_ui(ui, egui::vec2(900.0, 600.0), 960));
        out.textures_delta.clear();
    }

    fn button(at: egui::Pos2, pressed: bool, shift: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        }
    }

    /// Ruler clicks and drags queue seeks (free, or snapped with Shift) and
    /// never reach the board's tools: no shape, no draft, no undo step.
    #[test]
    fn the_ruler_seeks_the_transport_and_the_board_tools_get_nothing() {
        let ctx = egui::Context::default();
        let mut w = Spiderweb::default();
        w.tool = Tool::Line;
        w.playhead = Some(0.0);
        canvas_frame(&ctx, &mut w, Vec::new());
        let full = w.canvas_rect.expect("the board was shown");
        let r = egui::Rect::from_min_max(
            egui::pos2(full.left() + canvas::KEYS_W, full.top() + canvas::RULER_H),
            full.max,
        );
        let y = full.top() + canvas::RULER_H / 2.0;
        let a = egui::pos2(r.left() + 137.0, y);
        canvas_frame(&ctx, &mut w, vec![egui::Event::PointerMoved(a)]);
        canvas_frame(&ctx, &mut w, vec![button(a, true, false)]);
        let free = w.seek_beat(r, a.x, false);
        assert!(free.fract() != 0.0, "{free}");
        assert_eq!(w.transport, vec![TransportAction::Seek(free)]);
        w.transport.clear();
        // dragging seeks again; the same spot does not repeat the seek
        let b = egui::pos2(r.left() + 301.0, y + 40.0);
        canvas_frame(&ctx, &mut w, vec![egui::Event::PointerMoved(b)]);
        canvas_frame(&ctx, &mut w, Vec::new());
        assert_eq!(
            w.transport,
            vec![TransportAction::Seek(w.seek_beat(r, b.x, false))]
        );
        w.transport.clear();
        canvas_frame(&ctx, &mut w, vec![button(b, false, false)]);
        assert!(w.transport.is_empty() && w.scrub.is_none());
        // Shift snaps to the Snap step
        let c = egui::pos2(r.left() + 211.0, y);
        canvas_frame(&ctx, &mut w, vec![egui::Event::PointerMoved(c)]);
        canvas_frame(&ctx, &mut w, vec![button(c, true, true)]);
        canvas_frame(&ctx, &mut w, vec![button(c, false, true)]);
        let step = canvas::SNAPS[w.snap].1;
        let step = if step > 0.0 { step } else { 1.0 };
        match w.transport.as_slice() {
            [TransportAction::Seek(s)] => {
                assert!(((s / step) - (s / step).round()).abs() < 1e-9, "{s}");
                assert!((s - w.seek_beat(r, c.x, false)).abs() <= step / 2.0);
            }
            other => panic!("{other:?}"),
        }
        assert!(w.board.is_empty() && w.draft.is_none() && w.undo.is_empty());
        // without a song there is no playhead and the ruler does nothing
        w.transport.clear();
        w.playhead = None;
        canvas_frame(&ctx, &mut w, vec![button(a, true, false)]);
        canvas_frame(&ctx, &mut w, vec![button(a, false, false)]);
        assert!(w.transport.is_empty());
    }

    fn song() -> super::super::test_support::Harness {
        use plumspirit_engine::model::{
            Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
        };
        let mut d = PlumDocument::new_project("b2s38".into(), Bpm::DEFAULT, 960);
        let v: Vec<NoteRecord> = (0..16u64)
            .map(|i| NoteRecord {
                start_tick: i * 960,
                duration_ticks: 480,
                attrs: NoteAttrs::new(60 + (i % 12) as u8, 100, 0, 64, NoteOffKind::NoteOff)
                    .unwrap(),
                sub: SubchannelId(0),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        let mut h = super::super::test_support::Harness::new(d);
        h.app.main_view = super::super::track_list::MainView::PianoRoll;
        h
    }

    fn undo_len(h: &super::super::test_support::Harness) -> usize {
        h.app.open.as_ref().map_or(0, |o| o.history.undo_len())
    }

    /// The window's transport is the song's: seeks, begin / end and loop go
    /// to the one transport (the roll's cursor follows), and none of it is an
    /// undo step or changes the document.
    #[test]
    fn spiderweb_seeks_the_shared_transport_without_undo_steps() {
        let mut h = song();
        h.app.spiderweb.open = true;
        h.app_frame(egui::vec2(1400.0, 900.0), 1.0);
        let before = (undo_len(&h), h.doc().fingerprint());
        h.app.spiderweb_transport(TransportAction::Seek(2.5), 960);
        assert_eq!(h.app.transport.position(), 2400);
        assert_eq!(h.app.roll.cursor_tick, 2400);
        h.app_frame(egui::vec2(1400.0, 900.0), 1.0);
        assert_eq!(h.app.spiderweb.playhead, Some(2.5));
        assert!(!h.app.spiderweb.playhead_label.is_empty());
        h.app.spiderweb_transport(TransportAction::End, 960);
        assert_eq!(h.app.transport.position(), h.doc().duration_ticks());
        h.app.spiderweb_transport(TransportAction::Start, 960);
        assert_eq!(h.app.transport.position(), 0);
        h.app.set_loop_range(Some((0, 3840)), false);
        h.app.spiderweb_transport(TransportAction::ToggleLoop, 960);
        assert!(h.app.loop_config().enabled);
        h.app_frame(egui::vec2(1400.0, 900.0), 1.0);
        assert!(h.app.spiderweb.looping);
        h.app.spiderweb_transport(TransportAction::ToggleLoop, 960);
        assert!(!h.app.loop_config().enabled);
        h.app.spiderweb_transport(TransportAction::Stop, 960);
        assert!(!h.app.transport.state().is_running());
        assert_eq!((undo_len(&h), h.doc().fingerprint()), before);
        assert!(h.app.spiderweb.undo.is_empty());
    }

    /// A ruler drag in the real window moves the song's playhead frame by
    /// frame; the document and both undo histories stay as they were.
    #[test]
    fn dragging_the_spiderweb_ruler_moves_the_song_playhead() {
        let mut h = song();
        h.app.spiderweb.open = true;
        let size = egui::vec2(1400.0, 900.0);
        for _ in 0..3 {
            h.app_frame(size, 1.0);
        }
        let before = (undo_len(&h), h.doc().fingerprint());
        let full = h.app.spiderweb.canvas_rect.expect("the board is shown");
        let r = egui::Rect::from_min_max(
            egui::pos2(full.left() + canvas::KEYS_W, full.top() + canvas::RULER_H),
            full.max,
        );
        let y = full.top() + canvas::RULER_H / 2.0;
        let a = egui::pos2(r.left() + 120.0, y);
        let b = egui::pos2(r.left() + 260.0, y);
        h.app_frame_events(size, 1.0, vec![egui::Event::PointerMoved(a)]);
        h.app_frame_events(size, 1.0, vec![button(a, true, false)]);
        let want = song_tick(h.app.spiderweb.seek_beat(r, a.x, false), 0, 960);
        assert!(want > 0);
        assert_eq!(h.app.transport.position(), want);
        h.app_frame_events(size, 1.0, vec![egui::Event::PointerMoved(b)]);
        let want = song_tick(h.app.spiderweb.seek_beat(r, b.x, false), 0, 960);
        assert_eq!(h.app.transport.position(), want);
        h.app_frame_events(size, 1.0, vec![button(b, false, false)]);
        assert_eq!(h.app.transport.position(), want);
        assert_eq!((undo_len(&h), h.doc().fingerprint()), before);
        assert!(h.app.spiderweb.board.is_empty() && h.app.spiderweb.undo.is_empty());
    }

    /// Closing the window (any way) ends the notes Listen plays and leaves
    /// the song's position alone; reopening shows the same playhead.
    #[test]
    fn closing_spiderweb_ends_listen_and_keeps_the_song_position() {
        let mut h = song();
        let size = egui::vec2(1400.0, 900.0);
        h.app.spiderweb_transport(TransportAction::Seek(3.0), 960);
        h.app.spiderweb.open = true;
        h.app_frame(size, 1.0);
        h.app.spiderweb.preview = vec![TNote {
            start: 0,
            dur: 960,
            pitch: 64,
            vel: 100,
            ..TNote::default()
        }];
        h.app.spiderweb_listen();
        assert!(h.app.scrub_is_playing());
        h.app.spiderweb.open = false;
        h.app_frame(size, 1.0);
        assert!(!h.app.scrub_is_playing());
        assert_eq!(h.app.spiderweb.playhead, None);
        assert_eq!(h.app.transport.position(), 2880);
        h.app.spiderweb.open = true;
        h.app_frame(size, 1.0);
        assert_eq!(h.app.spiderweb.playhead, Some(3.0));
        // Stop ends Listen too
        h.app.spiderweb.preview = vec![TNote {
            start: 0,
            dur: 960,
            pitch: 64,
            vel: 100,
            ..TNote::default()
        }];
        h.app.spiderweb_listen();
        assert!(h.app.scrub_is_playing());
        h.app.spiderweb_transport(TransportAction::Stop, 960);
        assert!(!h.app.scrub_is_playing());
    }
}
