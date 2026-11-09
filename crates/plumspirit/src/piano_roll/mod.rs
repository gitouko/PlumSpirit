//! The piano-roll editor widget.
//!
//! The widget owns only *view* state (scroll, zoom, tool, the gesture in progress and
//! render caches). It reads the document and reports edits as [`RollAction`]s, which
//! the application applies to the authoritative document. Drags are previewed from
//! transient gesture state and committed once, on release.
//!
//! Above the ruler, the marker band shows the Conductor's markers and chapters;
//! time signatures are labelled in the ruler. Both use the same horizontal axis as
//! the notes and the lanes below.

pub mod dense;
pub mod graph;
pub mod handles;
pub mod hit_test;
pub mod markers;
pub mod render;
pub mod ruler;
pub mod velocity;
#[cfg(test)]
mod view_frame_tests;
pub mod viewport;

use crate::diagnostics::RenderStats;
use crate::ui::theme::Palette;
use egui::{
    CursorIcon, Event, Key, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Stroke, Vec2,
};
use hit_test::{Hit, hit_note, note_rect};
use markers::{BandLayout, MarkerPart};
use plumspirit_engine::editor::gate::DefaultGate;
use plumspirit_engine::editor::{clamp_move_delta, min_resize_delta};
use plumspirit_engine::model::note::pitch_labels;
use plumspirit_engine::model::timing::{snap_floor, snap_nearest};

/// Snaps content tick `c` to the song's grid (`docs/CLIP_PIANO_ROLL.md`
/// Decision 2): content tick `c` is song tick `c + shift`, so the result is
/// `snap(c + shift) − shift` (nearest, or `floor`). `shift == 0` is plain
/// snapping, exactly as before clips.
/// Tick `t` moved by `shift` (content → song with the opened clip's shift,
/// song → content with its negation), clamped to the tick range.
fn song_of(t: u64, shift: i128) -> u64 {
    (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64
}

fn snap_content(c: u64, unit: u64, shift: i128, floor: bool) -> u64 {
    if shift == 0 {
        return if floor {
            snap_floor(c, unit)
        } else {
            snap_nearest(c, unit)
        };
    }
    if unit <= 1 {
        return c;
    }
    let s = i128::from(c) + shift;
    let u = i128::from(unit);
    let down = s.div_euclid(u) * u;
    let snapped = if floor || (s - down) * 2 < u {
        down
    } else {
        down + u
    };
    (snapped - shift).clamp(0, i128::from(u64::MAX)) as u64
}
use plumspirit_engine::model::{
    NoteAttrs, NoteLoc, NoteRecord, NoteStore, PlumDocument, SelectionBounds, Snap,
};
use plumspirit_engine::view::grid::MeterMap;
use render::{NotePass, OnionCache, OnionPass, OnionSource, RenderCache};
use std::collections::HashSet;
use viewport::{FollowStyle, RollView};

pub const RULER_HEIGHT: f32 = 22.0;
pub const KEYS_WIDTH: f32 = 52.0;
pub const SCROLLBAR: f32 = 12.0;
/// Pointer movement (points) below which a press counts as a click.
const CLICK_SLOP: f32 = 3.0;
/// Extra reach of the eraser around the pointer path, in points.
const ERASE_REACH: f32 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Draw,
    Erase,
}

/// B1-0065: right-aligned labels in the ruler strip (each on its own
/// background, the first rightmost), truncated so together they use at most
/// two thirds of the ruler; nothing is drawn inside the note area.
/// B1-0115: the ruler banner of a scale-handle drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleDragNote {
    /// Duplicate Notes in this mode (words for it).
    Duplicating(&'static str),
    /// Duplicate Notes is on, but Alt keeps this drag N notes → N notes.
    AltOff,
}

impl ScaleDragNote {
    pub fn text(self) -> String {
        match self {
            ScaleDragNote::Duplicating(mode) => {
                format!(
                    "Scale handles: Duplicate Notes — {mode} (Alt on an edge: N notes → N notes)"
                )
            }
            ScaleDragNote::AltOff => "Scale handles: N notes → N notes for this drag (Alt)".into(),
        }
    }

    fn into_label(self, pal: &crate::ui::theme::Palette) -> (String, egui::Color32) {
        let c = match self {
            ScaleDragNote::Duplicating(_) => pal.accent,
            ScaleDragNote::AltOff => pal.text_weak,
        };
        (self.text(), c)
    }
}

pub(crate) fn ruler_banners(
    p: &egui::Painter,
    ruler: Rect,
    labels: &[(String, egui::Color32)],
    pal: &crate::ui::theme::Palette,
) -> Vec<Rect> {
    let mut out = Vec::new();
    let font = egui::FontId::proportional(crate::ui::theme::size::SMALL);
    let mut right = ruler.right() - 6.0;
    let budget = ruler.width() * 2.0 / 3.0;
    let left_limit = ruler.right() - budget;
    for (text, color) in labels {
        let room = right - left_limit - 8.0;
        if room < 24.0 {
            break;
        }
        let mut job = egui::text::LayoutJob::simple_singleline(text.clone(), font.clone(), *color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let galley = p.layout_job(job);
        let size = galley.size();
        let pos = Pos2::new(right - size.x, ruler.center().y - size.y / 2.0);
        let bg = Rect::from_min_size(pos, size).expand2(Vec2::new(4.0, 1.0));
        p.rect_filled(bg, 0.0, pal.overlay_bg);
        p.galley(pos, galley, *color);
        out.push(bg);
        right = bg.left() - 6.0;
    }
    out
}

/// Screen layout of the widget for one frame.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// Marker band (Conductor markers and chapters), above the ruler.
    pub band: Rect,
    pub ruler: Rect,
    pub keys: Rect,
    pub notes: Rect,
    pub vscroll: Rect,
    pub hscroll: Rect,
    pub view: RollView,
    /// Physical pixels per point.
    pub ppp: f32,
}

impl Geometry {
    /// The geometry of song time (`docs/CLIP_PIANO_ROLL.md` Decision 2):
    /// the roll's axis is content ticks and song tick `s` is drawn at
    /// content tick `s − shift`, so this is the same view with the origin
    /// moved by `shift`. When song tick 0 is right of the view's left edge,
    /// the band, ruler and note rectangles start where it is drawn (there
    /// is no song time left of it). `shift == 0`: `self`, unchanged.
    pub fn song(&self, shift: i128) -> Geometry {
        if shift == 0 {
            return *self;
        }
        let mut g = *self;
        let o = i128::from(self.view.time.origin_tick) + shift;
        if o >= 0 {
            g.view.time.origin_tick = u64::try_from(o).unwrap_or(u64::MAX);
        } else {
            g.view.time.origin_tick = 0;
            let dx = ((-o) as f64 * self.view.time.pixels_per_tick).min(1e7) as f32;
            for r in [&mut g.band, &mut g.ruler, &mut g.notes] {
                r.min.x = (r.min.x + dx).min(r.max.x);
            }
        }
        g
    }

    pub(crate) fn new(full: Rect, view: RollView, ppp: f32) -> Self {
        let top = full.top() + markers::BAND_HEIGHT + RULER_HEIGHT;
        let notes = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, top),
            Pos2::new(full.right() - SCROLLBAR, full.bottom() - SCROLLBAR),
        );
        Geometry {
            band: Rect::from_min_max(
                Pos2::new(notes.left(), full.top()),
                Pos2::new(notes.right(), full.top() + markers::BAND_HEIGHT),
            ),
            ruler: Rect::from_min_max(
                Pos2::new(notes.left(), full.top() + markers::BAND_HEIGHT),
                Pos2::new(notes.right(), notes.top()),
            ),
            keys: Rect::from_min_max(
                Pos2::new(full.left(), notes.top()),
                Pos2::new(notes.left(), notes.bottom()),
            ),
            notes,
            vscroll: Rect::from_min_max(
                Pos2::new(notes.right(), notes.top()),
                Pos2::new(full.right(), notes.bottom()),
            ),
            hscroll: Rect::from_min_max(
                Pos2::new(notes.left(), notes.bottom()),
                Pos2::new(notes.right(), full.bottom()),
            ),
            view,
            ppp,
        }
    }

    fn tick_at(&self, x: f32) -> u64 {
        self.view.time.x_to_tick(f64::from(x - self.notes.left()))
    }

    fn pitch_at_clamped(&self, y: f32) -> u8 {
        self.view
            .pitch
            .y_to_pitch_clamped(f64::from(y - self.notes.top()))
    }

    /// Fractional pitch row under `y` (row `p` covers `[p, p + 1)`), clamped.
    fn pitch_f_at(&self, y: f32) -> f64 {
        let row = f64::from(y - self.notes.top()) / self.view.pitch.row_height
            + self.view.pitch.scroll_rows;
        let rows = self.view.pitch.rows();
        (rows - row).clamp(0.0, rows - 0.001)
    }

    /// Screen position of an absolute (tick, fractional pitch) point.
    fn point_of(&self, tick: u64, pitch: f64) -> Pos2 {
        let x = self.notes.left() + self.view.time.tick_to_x(tick) as f32;
        let row = self.view.pitch.rows() - pitch;
        let y = self.notes.top()
            + ((row - self.view.pitch.scroll_rows) * self.view.pitch.row_height) as f32;
        Pos2::new(x, y)
    }
}

/// How a tool window captures the pointer in the note area (v0.7 stroke
/// tools: Draw / Paint strokes, Select lassos).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// A drag records a freehand stroke.
    Freehand,
    /// Custom tools retain a bounded ordered egui event batch in the same stroke buffer.
    FreehandOrdered,
    /// Each click adds a corner.
    Polygon,
}

/// Edits and selection changes requested by the widget.
#[derive(Clone, Debug, PartialEq)]
pub enum RollAction {
    /// B1-0047: open a plugin's custom tool (plugin id, tool id).
    CustomTool(String, String),
    InsertNote(NoteRecord),
    SelectOnly(NoteLoc),
    ToggleSelected(NoteLoc),
    AddSelected(NoteLoc),
    ClearSelection,
    SelectRegion {
        t0: u64,
        t1: u64,
        pitch_lo: u8,
        pitch_hi: u8,
    },
    MoveSelected {
        dt: i128,
        dp: i32,
    },
    ResizeSelected {
        delta: i128,
    },
    /// Delete these notes of the active track (one Erase gesture).
    Erase(Vec<NoteLoc>),
    /// Move the transport / edit position (ruler click or drag, marker click).
    Seek(u64),
    /// Set the note-on velocity of every selected note (Velocity Lane).
    SetVelocity(u8),
    /// Marker band / ruler requests (applied as Conductor edits by the app).
    MoveMarker {
        id: u32,
        start: u64,
        end: Option<u64>,
    },
    EditMarker(u32),
    DeleteMarker(u32),
    AddMarkerAt(u64),
    EditSignature(u32),
    DeleteSignature(u32),
    AddSignatureAt(u64),
    /// Drag on the ruler (v0.8): the time selection (`None`: cleared).
    SetTimeSelection(Option<(u64, u64)>),
    /// Ruler menu: the loop becomes the time selection.
    LoopTimeSelection,
    /// Ruler menu: select the active track's notes starting in the time
    /// selection.
    SelectInTimeSelection,
    /// Shift+drag on the ruler, or its context menu: the loop range `[start, end)`.
    SetLoop(u64, u64),
    SetLoopStart(u64),
    SetLoopEnd(u64),
    ClearLoop,
    /// v0.6 note audition: a piano key is pressed (or the press moved to
    /// another key) — `Some(pitch)` — or released — `None`.
    AuditionKey(Option<u8>),
    /// v0.6 note audition: a note is being placed on this pitch (sent once
    /// when the Draw gesture starts, never while dragging its length), at the
    /// note's start (the instrument there is used).
    AuditionNote {
        pitch: u8,
        tick: u64,
    },
    /// v0.7 stroke tools: the stroke drawn so far (tick, fractional pitch,
    /// seconds since it began); `done` when the button was released.
    StrokeCanceled,
    Stroke {
        points: Vec<(u64, f64, f64)>,
        done: bool,
    },
    /// v0.7 polygon lasso: a corner clicked at (tick, fractional pitch).
    StrokeVertex(u64, f64),
    /// v0.7 scale handles: scale the selection's box `from` onto `to` (one undo
    /// step; the app commits the same kernel it previewed).
    ScaleBox {
        from: plumspirit_engine::transform::geom::GeomBox,
        to: plumspirit_engine::transform::geom::GeomBox,
        /// v0.8 §191: Duplicate / Fill rows — only while Alt is held during
        /// this drag (never a remembered state).
        fill: bool,
    },
    /// v0.9 §084 / §085: a Ctrl (Skew) or Ctrl+Shift (Perspective) corner
    /// drag of the selection's box `from` (one undo step; the app commits the
    /// same exact kernel it previewed).
    WarpBox {
        from: plumspirit_engine::transform::geom::GeomBox,
        warp: plumspirit_engine::transform::geom::CornerWarp,
        fill: bool,
    },
    /// v0.8 note menu: run an application command (Repeat Last Tool, Mute,
    /// Delete, …).
    Run(crate::app::commands::Command),
    /// B2-F18: a press on a note of another clip of the aggregate: that clip
    /// becomes the edited one and the note is selected (`add`: added to the
    /// selection, Shift / Ctrl).
    FocusClip {
        clip: plumspirit_engine::model::ClipId,
        loc: NoteLoc,
        add: bool,
    },
}

/// B2-F18: another clip of the picked track in the aggregate view.
#[derive(Clone, Copy, Debug)]
pub struct RollPart<'a> {
    pub clip: plumspirit_engine::model::ClipId,
    pub store: &'a NoteStore,
    /// Cache identity of its content ([`render::host_key`]).
    pub key: u64,
    /// Song tick of its content tick 0 (`start − offset`).
    pub shift: i128,
    /// Its content window `[a, b)` (only notes starting in it play).
    pub window: (u64, u64),
}

/// B2-F18: the aggregate part whose note is under `pos` (the front one:
/// parts are drawn in order), with the note's location in its store.
fn part_hit(parts: &[render::PlacedPart<'_>], g: &Geometry, pos: Pos2) -> Option<(usize, NoteLoc)> {
    parts.iter().enumerate().rev().find_map(|(i, q)| {
        let h = hit_note(q.store, &g.song(-q.place.shift), pos)?;
        let (a, b) = q.place.window;
        (h.note.start_tick >= a && h.note.start_tick < b).then_some((i, h.loc))
    })
}

impl RollAction {
    /// B2-F15 / F19: whether the action edits or selects notes (refused
    /// while the roll is locked); seeking, markers, signatures, the loop,
    /// the time selection, auditioning and commands are not.
    pub fn touches_notes(&self) -> bool {
        !matches!(
            self,
            RollAction::Seek(_)
                | RollAction::MoveMarker { .. }
                | RollAction::EditMarker(_)
                | RollAction::DeleteMarker(_)
                | RollAction::AddMarkerAt(_)
                | RollAction::EditSignature(_)
                | RollAction::DeleteSignature(_)
                | RollAction::AddSignatureAt(_)
                | RollAction::SetTimeSelection(_)
                | RollAction::LoopTimeSelection
                | RollAction::SetLoop(..)
                | RollAction::SetLoopStart(_)
                | RollAction::SetLoopEnd(_)
                | RollAction::ClearLoop
                | RollAction::AuditionKey(_)
                | RollAction::AuditionNote { .. }
                | RollAction::Run(_)
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Gesture {
    None,
    /// A stroke for the open stroke tool (samples in `PianoRoll::stroke_buf`).
    Stroke,
    /// Dragging a selection scale handle (edge mask, source box, target box).
    ScaleBox {
        mask: u8,
        from: plumspirit_engine::transform::geom::GeomBox,
        to: plumspirit_engine::transform::geom::GeomBox,
        /// Alt held (Duplicate / Fill rows for this drag only).
        fill: bool,
        /// Where the handle was pressed.
        origin: Pos2,
        /// v0.9: a corner dragged with Ctrl (Skew) or Ctrl+Shift
        /// (Perspective) — the modifiers are read live, like Shift / Alt.
        warp: Option<plumspirit_engine::transform::geom::CornerWarp>,
        /// The Skew axis, fixed by the drag's first clear direction.
        axis: Option<plumspirit_engine::transform::geom::WarpKind>,
    },
    Draw {
        pitch: u8,
        start: u64,
        end: u64,
        press: Pos2,
        dragged: bool,
    },
    BoxSelect {
        origin: Pos2,
        current: Pos2,
    },
    Move {
        press: Pos2,
        grab_start: u64,
        grab_pitch: u8,
        bounds: Option<SelectionBounds>,
        dt: i128,
        dp: i32,
    },
    Resize {
        press: Pos2,
        grab_end: u64,
        bounds: Option<SelectionBounds>,
        delta: i128,
    },
    /// Erase tool: every note the pointer path crosses (see `PianoRoll::erasing`).
    Erase {
        last: Pos2,
    },
    Pan {
        last: Pos2,
    },
    /// Pressed on the ruler: a click moves the edit position, a drag
    /// selects time, Shift+drag sets the loop (shared with the Track List,
    /// `ruler::RulerDrag`).
    Ruler(ruler::RulerDrag),
    Marker {
        id: u32,
        part: MarkerPart,
        press: Pos2,
        grab: u64,
        start: u64,
        end: Option<u64>,
        /// Current preview (start, end).
        now: (u64, Option<u64>),
        moved: bool,
    },
    HScroll {
        grab: f32,
    },
    VScroll {
        grab: f32,
    },
    /// A piano key held for audition (follows the pointer across keys).
    Keys {
        pitch: u8,
    },
}

/// Information for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct PointerInfo {
    /// Tick under the pointer on the roll's axis (content ticks of the
    /// opened clip; song ticks for a track's own content).
    pub tick: Option<u64>,
    pub pitch: Option<u8>,
    /// The same position in song time (`tick + shift`; what the status bar
    /// describes as bar and beat).
    pub song_tick: Option<u64>,
}

/// Notes about to be added (drawn as highlighted ghosts).
#[derive(Clone, Copy)]
pub struct AddedGhosts<'a> {
    pub stores: &'a [&'a plumspirit_engine::model::NoteStore],
    pub records: &'a [NoteRecord],
    pub longest: u64,
}

/// Inputs for one frame.
pub struct RollInput<'a> {
    pub doc: &'a PlumDocument,
    /// The hosting track (its subchannels, channel and Onion identity).
    pub track_index: usize,
    /// v0.9 V09-S11 (`docs/CLIP_PIANO_ROLL.md`): an owned content edited on
    /// that track (an opened clip), else the track's own notes.
    pub content: Option<plumspirit_engine::model::ContentId>,
    /// Song tick of content tick 0 for the opened clip (`start − offset`):
    /// the notes are drawn and edited in content ticks, everything in song
    /// time (ruler, grid, Conductor band, playhead, loop, selections,
    /// Onion) is shifted by it. 0 = a track's own content at the identity,
    /// exactly the roll before clips.
    pub shift: i128,
    /// The opened clip's name ("clip “Intro” on Track 2"), shown in the
    /// roll's corner while a clip is open.
    pub title: Option<&'a str>,
    /// B2-A05 / F15: the roll is locked (no clip to edit): this hint is
    /// shown, no own notes are drawn and nothing edits or selects notes.
    pub locked: Option<&'a str>,
    /// B2-F18 (Track Aggregate): the picked track's other clips, drawn with
    /// the edited one at their song ticks; empty outside the aggregate.
    pub parts: &'a [RollPart<'a>],
    pub meter: &'a MeterMap,
    pub palette: &'a Palette,
    pub dark: bool,
    /// False while a background job owns the document: navigation still works, edits
    /// and selection changes do not.
    pub editable: bool,
    /// Keyboard shortcuts for the roll are allowed (no text field / dialog focused).
    pub keyboard: bool,
    /// Onion Skin sources (other tracks drawn as ghosts), in drawing order.
    pub onion: &'a [OnionSource<'a>],
    /// Ghost opacity 0–255.
    pub onion_alpha: u8,
    /// Ghost outlines (v0.7 setting, default on) at a darkness in percent
    /// (v0.8 §121: 0 % white … 100 % black); `None`: no outlines.
    pub onion_outlines: Option<u8>,
    /// v0.7: a stroke tool captures the pointer in the note area.
    pub capture: Option<CaptureMode>,
    /// v0.7: the open stroke tool's current stroke / lasso (drawn on top).
    pub stroke_overlay: &'a [(u64, f64)],
    /// Playback position while playing (drawn instead of the edit cursor).
    pub playhead: Option<u64>,
    /// The Conductor is selected: the track's notes are shown faintly for
    /// orientation and cannot be edited; markers and signatures can.
    pub conductor_active: bool,
    /// Channel, length and subchannel of newly drawn notes.
    pub new_note_channel: u8,
    pub new_note_gate: DefaultGate,
    pub new_note_sub: plumspirit_engine::model::SubchannelId,
    /// Live Preview of a batch operation on the active track's selection.
    pub preview: Option<(
        &'a plumspirit_engine::editor::preview::NoteKernel,
        plumspirit_engine::editor::preview::PreviewScope,
    )>,
    /// v0.7: the open tool's preview on the active track.
    pub tool_preview: Option<ToolPreview<'a>>,
    /// Notes an import would add (import preview).
    pub added: Option<AddedGhosts<'a>>,
    /// Loop range and whether the loop is on (drawn in the ruler).
    pub loop_range: Option<(u64, u64)>,
    pub loop_enabled: bool,
    /// Shown when the active track's playback / export differ from what is drawn
    /// (Track Properties corrections).
    pub corrections_note: Option<&'a str>,
    /// The last custom range (Select Range), shaded faintly.
    pub range: Option<(u64, u64)>,
    /// v0.6: what each piano key shows (live keyboard colours), if on.
    pub key_colors: Option<&'a [crate::app::keys::KeyDisplay; crate::app::keys::KEYS]>,
    /// v0.6: clicking keys / placing notes asks for an audition.
    pub audition: bool,
    /// v0.9 §057: View → 88-Key Guide (lines at A0 = 21 and C8 = 108).
    pub key_guide: bool,
    /// v0.8 §156: the right-click note menu (None: no menu, e.g. Conductor).
    pub note_menu: Option<&'a crate::app::note_menu::NoteMenu>,
    /// The readouts' chips and caret (last frame): presses there belong to
    /// the control, never to the notes under it.
    pub overlay_controls: crate::app::progressor::OverlayControls,
}

/// What the open v0.7 tool previews on the active track.
pub enum ToolPreview<'a> {
    /// An already-staged immutable native result, queried only in the viewport.
    Store {
        notes: &'a plumspirit_engine::model::NoteStore,
        all: bool,
        rev: u64,
    },
    /// The computed result (sorted by start) and its longest note.
    Run {
        notes: &'a [plumspirit_engine::transform::TNote],
        /// Results for other tracks (drawn too, sorted by start).
        extra: &'a [plumspirit_engine::transform::TNote],
        max_dur: u64,
        all: bool,
        /// Identity of the content (0: not cached); the drawing is cached per
        /// identity, track revision and view.
        rev: u64,
    },
    /// A pointwise kernel evaluated over the viewport (result still computing).
    Lazy {
        kernel: &'a dyn plumspirit_engine::transform::PointKernel,
        boundary: plumspirit_engine::transform::Boundary,
        slot: u16,
        all: bool,
        rev: u64,
    },
}

impl RollInput<'_> {
    /// What the roll edits.
    pub fn target(&self) -> plumspirit_engine::editor::NoteTarget {
        match self.content {
            None => plumspirit_engine::editor::NoteTarget::Track(self.track_index),
            Some(content) => plumspirit_engine::editor::NoteTarget::Content {
                host: self.track_index,
                content,
            },
        }
    }
}

impl ToolPreview<'_> {
    /// The tool replaces every note of the track (not only the selection).
    pub fn all(&self) -> bool {
        match self {
            ToolPreview::Run { all, .. }
            | ToolPreview::Lazy { all, .. }
            | ToolPreview::Store { all, .. } => *all,
        }
    }
}

pub struct PianoRoll {
    pub view: RollView,
    /// The opened clip's `start − offset` this frame (0: a track's own
    /// content at the identity); see [`RollInput::shift`].
    pub shift: i128,
    pub tool: Tool,
    pub snap: Snap,
    pub cursor_tick: u64,
    /// The Edit / Anchor playhead while it is on (B1-0080; set by the app
    /// from the transport each frame, like `cursor_tick`).
    pub anchor: Option<u64>,
    pub pointer: PointerInfo,
    pub stats: RenderStats,
    /// Marker / time signature selected in the band or ruler (view state).
    pub selected_marker: Option<u32>,
    pub selected_signature: Option<u32>,
    /// The user scrolled the view horizontally this frame (wheel, scrollbar,
    /// middle-drag): Follow Playhead pauses instead of fighting them.
    pub user_panned: bool,
    gesture: Gesture,
    /// Notes hit by the Erase gesture in progress, in hit order (and as a set).
    erase_hits: Vec<NoteLoc>,
    erase_set: HashSet<NoteLoc>,
    /// Selection box cache: ((note host, note revision), box).
    sel_box: Option<(
        (plumspirit_engine::model::ContentRef, u64),
        Option<plumspirit_engine::transform::geom::GeomBox>,
    )>,
    /// Shift held this frame (scale handles snap).
    shift_held: bool,
    alt_held: bool,
    /// Ctrl (Command) held this frame (corner handles skew / Perspective).
    ctrl_held: bool,
    /// B1-0115: Transform → Scale Handles: Duplicate Notes is on, and the
    /// words for its mode — the ruler names what a handle drag does.
    pub scale_duplicate: (bool, &'static str),
    /// B1-0091: an Alt + corner drag rotates about the selection's center
    /// (true) or the corner opposite the dragged one.
    pub rotate_about_center: bool,
    cache: RenderCache,
    labels: Vec<String>,
    /// Document generation the view was initialised for.
    initialized_for: Option<u64>,
    reset_requested: bool,
    /// Zoom X / Y percent of a new view (Settings, B1-0059; set by the app).
    pub default_zoom: (f64, f64),
    /// L-F02: a new view fits all keys instead of the default Zoom Y.
    pub default_fit: bool,
    /// B1-0087: Y Zoom Beyond the Keys (Settings; set by the app). Off: the
    /// keys always fill the note area's height at least.
    pub y_beyond_keys: bool,
    /// B1-0089: Lock Fit Range (Settings; set by the app): after Fit 128 /
    /// 256 Keys the same number of keys stays fitted when the note area's
    /// height changes (graph panels, the window).
    pub lock_fit: bool,
    /// B1-0088: the number of key rows the last Fit Keys fitted (cleared by
    /// a Y zoom).
    fit_rows: Option<f64>,
    onion_cache: OnionCache,
    /// v0.8 dense-note tiles (active track and Onion).
    dense: dense::DenseRenderer,
    band_layout: BandLayout,
    /// Signature labels drawn in the ruler last frame (id, rect) for picking.
    signature_rects: Vec<(u32, Rect)>,
    /// Size of the note area last frame (for zoom and follow outside `show`).
    notes_size: Vec2,
    /// What a right-click menu opened on (kept while the menu is open).
    context: Option<ContextTarget>,
    /// Channel and gate of new notes, and the meter they are measured with
    /// (copied from the input each frame).
    new_note: (u8, DefaultGate),
    new_note_sub: plumspirit_engine::model::SubchannelId,
    meter: Option<MeterMap>,
    /// Note audition requested this frame (copied from the input).
    audition: bool,
    /// Note area of the last frame (overlays drawn by the app).
    notes_rect: Option<Rect>,
    capture_view: bool,
    view_frame: Option<viewport::ViewFrame>,
    overlay_layer: Option<(egui::Painter, egui::layers::ShapeIdx)>,
    /// The stroke being drawn (tick, fractional pitch, seconds) and when it
    /// began.
    stroke_buf: Vec<(u64, f64, f64)>,
    stroke_began: Option<std::time::Instant>,
    stroke_sent: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ContextTarget {
    Marker(u32),
    Signature(u32),
    Band(u64),
    Ruler(u64),
    /// v0.8: right-click in the note area (the note under the pointer, if
    /// any, was targeted first).
    Notes,
}

impl PianoRoll {
    /// The tile service, shared with the Track List's clip mini notes (v0.9
    /// V09-S09: one worker, one cache and one budget for both views).
    /// B1-0170: at exit, the tile caches are left to the system (an inline
    /// renderer without threads takes their place).
    pub fn abandon_caches(&mut self) {
        std::mem::replace(&mut self.dense, dense::DenseRenderer::new(true)).abandon();
    }

    pub fn dense_mut(&mut self) -> &mut dense::DenseRenderer {
        &mut self.dense
    }

    /// The note area as drawn last frame (for overlays such as the Progressor).
    pub fn notes_rect(&self) -> Option<Rect> {
        self.notes_rect
    }
    pub fn capture_view(&mut self, enabled: bool) {
        self.capture_view = enabled;
        if !enabled {
            self.view_frame = None;
            self.overlay_layer = None;
        }
    }
    pub fn view_frame(&self) -> Option<viewport::ViewFrame> {
        self.view_frame
    }
    pub fn take_overlay_layer(&mut self) -> Option<(egui::Painter, egui::layers::ShapeIdx)> {
        self.overlay_layer.take()
    }

    /// A piano key is held for audition.
    pub fn audition_key_held(&self) -> bool {
        matches!(self.gesture, Gesture::Keys { .. })
    }

    /// The piano key held for audition.
    pub fn audition_pitch(&self) -> Option<u8> {
        match self.gesture {
            Gesture::Keys { pitch } => Some(pitch),
            _ => None,
        }
    }
}

impl Default for PianoRoll {
    fn default() -> Self {
        PianoRoll {
            view: RollView::default(),
            shift: 0,
            tool: Tool::Select,
            snap: Snap::Sixteenth,
            cursor_tick: 0,
            anchor: None,
            pointer: PointerInfo::default(),
            stats: RenderStats::default(),
            selected_marker: None,
            selected_signature: None,
            user_panned: false,
            gesture: Gesture::None,
            erase_hits: Vec::new(),
            erase_set: HashSet::new(),
            sel_box: None,
            shift_held: false,
            alt_held: false,
            ctrl_held: false,
            scale_duplicate: (false, ""),
            rotate_about_center: true,
            cache: RenderCache::default(),
            labels: pitch_labels(),
            initialized_for: None,
            reset_requested: false,
            default_zoom: (100.0, 100.0),
            default_fit: false,
            y_beyond_keys: false,
            lock_fit: true,
            fit_rows: None,
            onion_cache: OnionCache::default(),
            dense: dense::DenseRenderer::default(),
            band_layout: BandLayout::default(),
            signature_rects: Vec::new(),
            notes_size: Vec2::new(800.0, 400.0),
            context: None,
            new_note: (0, DefaultGate::Grid),
            new_note_sub: plumspirit_engine::model::SubchannelId::MAIN,
            meter: None,
            audition: false,
            notes_rect: None,
            capture_view: false,
            view_frame: None,
            overlay_layer: None,
            stroke_buf: Vec::new(),
            stroke_began: None,
            stroke_sent: 0,
        }
    }
}

/// Liang–Barsky: does the segment `a`–`b` touch `r`?
/// What hovering the Piano Roll keyboard shows (v0.9 §063): the key number
/// and name, plus how many notes sound on it when live keyboard colours are
/// on.
fn key_hover_text(
    pitch: u8,
    colors: Option<&[crate::app::keys::KeyDisplay; crate::app::keys::KEYS]>,
) -> String {
    let mut text = plumspirit_engine::model::note::key_label(pitch);
    if let Some(total) = colors.map(|kc| kc[usize::from(pitch)].total)
        && total > 0
    {
        text.push_str(&format!(
            "\n{} note(s) sounding",
            plumspirit_engine::midi::group_digits(u64::from(total))
        ));
    }
    text
}

fn segment_hits_rect(a: Pos2, b: Pos2, r: Rect) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-dx, a.x - r.left()),
        (dx, r.right() - a.x),
        (-dy, a.y - r.top()),
        (dy, r.bottom() - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return false;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return false;
                }
                t1 = t1.min(t);
            }
        }
    }
    true
}

/// Adds every note of `store` touched by the pointer path `a`–`b` to the erase
/// set. Only notes in the path's tick range and pitch rows are visited.
fn erase_along(
    store: &NoteStore,
    g: &Geometry,
    a: Pos2,
    b: Pos2,
    hits: &mut Vec<NoteLoc>,
    set: &mut HashSet<NoteLoc>,
) {
    let clamp = |p: Pos2| {
        Pos2::new(
            p.x.clamp(g.notes.left(), g.notes.right()),
            p.y.clamp(g.notes.top(), g.notes.bottom()),
        )
    };
    let (a, b) = (clamp(a), clamp(b));
    let x0 = a.x.min(b.x) - ERASE_REACH - hit_test::MIN_NOTE_WIDTH;
    let x1 = a.x.max(b.x) + ERASE_REACH;
    let t0 = g.tick_at(x0);
    let t1 = g.tick_at(x1);
    let p_hi = g.pitch_at_clamped(a.y.min(b.y) - ERASE_REACH);
    let p_lo = g.pitch_at_clamped(a.y.max(b.y) + ERASE_REACH);
    store.query(t0, t1, |loc, n| {
        let pitch = n.attrs.pitch();
        if pitch < p_lo || pitch > p_hi || set.contains(&loc) {
            return;
        }
        let r = note_rect(g, n.start_tick, n.end_tick(), pitch).expand(ERASE_REACH);
        if segment_hits_rect(a, b, r) {
            set.insert(loc);
            hits.push(loc);
        }
    });
}

impl PianoRoll {
    /// Called when a new document is opened.
    pub fn document_changed(&mut self, snap_valid: bool) {
        self.initialized_for = None;
        self.cancel_gesture();
        self.cursor_tick = 0;
        self.selected_marker = None;
        self.selected_signature = None;
        if !snap_valid {
            self.snap = Snap::Off;
        }
    }

    pub fn request_reset_view(&mut self) {
        self.reset_requested = true;
    }

    /// Aborts any gesture in progress (e.g. the document changed underneath it).
    pub fn cancel_gesture(&mut self) {
        self.gesture = Gesture::None;
        self.erase_hits.clear();
        self.erase_set.clear();
        self.context = None;
    }

    /// The scale-handle drag in progress: (source box, target box, Fill).
    pub fn scale_drag(
        &self,
    ) -> Option<(
        plumspirit_engine::transform::geom::GeomBox,
        plumspirit_engine::transform::geom::GeomBox,
        bool,
    )> {
        match self.gesture {
            Gesture::ScaleBox {
                from,
                to,
                fill,
                warp: None,
                ..
            } if from != to => Some((from, to, fill)),
            _ => None,
        }
    }

    /// B1-0115: what a scale-handle drag in progress does with the notes
    /// (Duplicate Notes on, or turned off for this drag by Alt).
    pub fn scale_drag_note(&self) -> Option<ScaleDragNote> {
        let Gesture::ScaleBox {
            fill,
            warp: None,
            mask,
            ..
        } = self.gesture
        else {
            return None;
        };
        if fill && handles::corner_of(mask).is_some() {
            // B1-0091: Alt on a corner rotates (no scaling).
            return None;
        }
        let (toggle, mode) = self.scale_duplicate;
        match (toggle != fill, toggle) {
            (true, _) => Some(ScaleDragNote::Duplicating(mode)),
            (false, true) => Some(ScaleDragNote::AltOff),
            (false, false) => None,
        }
    }

    /// B1-0091: the ruler's words for an Alt + corner rotation in progress.
    pub fn rotation_note(&self) -> Option<String> {
        match self.gesture {
            Gesture::ScaleBox { warp: Some(w), .. } if w.kind.is_rotation() => {
                let deg = w.degrees();
                // −180° … 180° reads better than 0 … 360°.
                let deg = if deg > 180.0 { deg - 360.0 } else { deg };
                let about = if w.kind == plumspirit_engine::transform::geom::WarpKind::RotateCenter
                {
                    "the selection's center"
                } else {
                    "the opposite corner"
                };
                Some(format!("Rotate {deg:.1}° about {about} (Shift: 15° steps)"))
            }
            _ => None,
        }
    }

    /// The Ctrl / Ctrl+Shift corner drag in progress: (source box, gesture,
    /// Fill).
    pub fn corner_drag(
        &self,
    ) -> Option<(
        plumspirit_engine::transform::geom::GeomBox,
        plumspirit_engine::transform::geom::CornerWarp,
        bool,
    )> {
        match self.gesture {
            Gesture::ScaleBox {
                from,
                warp: Some(w),
                fill,
                ..
            } if !w.is_identity() => Some((from, w, fill)),
            _ => None,
        }
    }

    /// The selection's box of `track`, cached per note revision (computing it
    /// walks the selection).
    fn selection_box(
        &mut self,
        src: &plumspirit_engine::editor::HostView<'_>,
    ) -> Option<plumspirit_engine::transform::geom::GeomBox> {
        let key = (src.host(), src.notes.revision());
        match self.sel_box {
            Some((k, b)) if k == key => b,
            _ => {
                let b = src.notes.selection_bounds().map(|b| handles::geom_box(&b));
                self.sel_box = Some((key, b));
                b
            }
        }
    }

    pub fn is_dragging(&self) -> bool {
        !matches!(self.gesture, Gesture::None)
    }

    /// True while the user drags on the ruler (the app must not overwrite the
    /// cursor with the transport position then).
    pub fn is_scrubbing(&self) -> bool {
        matches!(self.gesture, Gesture::Ruler(d) if d.is_press())
    }

    /// Width of the note area (points), as last drawn.
    pub fn view_width(&self) -> f64 {
        f64::from(self.notes_size.x.max(1.0))
    }

    /// Height of the note area (points), as last drawn.
    pub fn view_height(&self) -> f64 {
        f64::from(self.notes_size.y.max(1.0))
    }

    /// v0.7 Zoom tool: shows ticks `[t0, t1)` and pitches `lo..=hi` across the
    /// note area (a small margin around), within the zoom limits.
    pub fn fit_box(&mut self, t0: u64, t1: u64, lo: u8, hi: u8) {
        let (w, h) = (self.view_width(), self.view_height());
        let span = t1.saturating_sub(t0).max(1) as f64;
        let ppt = plumspirit_engine::view::clamp_zoom(w * 0.92 / span);
        let margin = (w * 0.04 / ppt) as u64;
        self.view.time = plumspirit_engine::view::TimeAxis::new(t0.saturating_sub(margin), ppt);
        let rows = f64::from(hi.max(lo) - lo.min(hi)) + 3.0;
        let rh = (h / rows)
            .clamp(
                plumspirit_engine::view::MIN_ROW_HEIGHT,
                plumspirit_engine::view::MAX_ROW_HEIGHT,
            )
            .max(self.row_floor(h));
        self.fit_rows = None;
        self.view.pitch.row_height = rh;
        self.view
            .center_pitch((f64::from(lo) + f64::from(hi)) / 2.0, h);
        self.user_panned = true;
    }

    /// v0.7 Zoom tool: replaces the view (history, named regions, animation).
    pub fn set_view(&mut self, v: RollView) {
        let top = self.view.pitch.top;
        let h = self.view_height();
        self.view = v;
        // A view kept from before 256-Key Mode changed stays on its keys.
        self.view.set_top(top, h);
        self.view.pitch.clamp_scroll(h);
        self.user_panned = true;
        self.fit_rows = None;
    }

    /// Follows the playhead during playback (see [`viewport::follow_origin`]).
    pub fn follow_playhead(&mut self, playhead: u64, end: u64, style: FollowStyle, anchor: f64) {
        if let Some(o) = viewport::follow_origin(
            &self.view.time,
            self.view_width(),
            playhead,
            end,
            style,
            anchor,
        ) {
            self.view.time.origin_tick = o;
        }
    }

    /// Makes `tick` visible (after a seek): if it is off screen, it is placed near
    /// the left edge.
    pub fn reveal(&mut self, tick: u64, end: u64) {
        let x = self.view.time.tick_to_x(tick);
        if x < 0.0 || x > self.view_width() {
            self.follow_playhead(tick, end.max(tick), FollowStyle::Page, 1.0);
        }
    }

    pub fn zoom_by(&mut self, factor: f64, doc: &PlumDocument, width: f32) {
        self.view.zoom_time(
            f64::from(width) / 2.0,
            factor,
            viewport::zoom_span(doc, self.cursor_tick),
            f64::from(width),
        );
    }

    /// X anchor for typed zoom values: the edit / playback position when it is on
    /// screen, else the centre of the view.
    fn zoom_anchor_x(&self, position: u64) -> f64 {
        let x = self.view.time.tick_to_x(position);
        let w = self.view_width();
        if (0.0..=w).contains(&x) { x } else { w / 2.0 }
    }

    /// Sets Zoom X (percent of the reference; see `viewport`).
    pub fn set_zoom_x_percent(
        &mut self,
        percent: f64,
        doc: &PlumDocument,
        position: u64,
    ) -> Result<(), String> {
        let ppt = viewport::zoom_x_from_percent(percent, doc.division())?;
        let anchor = self.zoom_anchor_x(position);
        self.view.set_zoom_x(anchor, ppt);
        Ok(())
    }

    /// Y-Zoom − / + (v0.8 §153–§155): the same kernel as the typed value and
    /// Ctrl+Shift+wheel (`RollView::zoom_pitch`), around the centre row.
    pub fn zoom_y_by(&mut self, factor: f64) {
        let h = f64::from(self.notes_size.y.max(1.0));
        self.zoom_pitch_at(h / 2.0, factor, h);
    }

    /// Every Y zoom of the roll: it ends a key fit and stops at the
    /// [`Self::row_floor`].
    fn zoom_pitch_at(&mut self, anchor_y: f64, factor: f64, h: f64) {
        self.fit_rows = None;
        let floor = self.row_floor(h);
        self.view.zoom_pitch_floor(anchor_y, factor, h, floor);
    }

    /// B1-0087: the smallest row height a Y zoom reaches — every key fills
    /// the note area's height `h`, unless Y Zoom Beyond the Keys is on.
    pub fn row_floor(&self, h: f64) -> f64 {
        use plumspirit_engine::view::{MAX_ROW_HEIGHT, MIN_ROW_HEIGHT};
        if self.y_beyond_keys {
            MIN_ROW_HEIGHT
        } else {
            (h / self.view.pitch.rows()).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT)
        }
    }

    /// B1-0088: Fit 128 Keys (`lo..=hi` = 0..=127) and Fit All 256 Keys:
    /// exactly `hi − lo + 1` rows fill the note area's height (a fractional
    /// row height in points; on a high-DPI screen the rows are the same
    /// points, finer pixels). A window too small for half a point per row
    /// keeps half a point and centers the range. With Lock Fit Range the fit
    /// is kept when the height changes.
    pub fn fit_keys(&mut self, lo: u8, hi: u8) {
        use plumspirit_engine::view::{MAX_ROW_HEIGHT, MIN_ROW_HEIGHT};
        let (lo, hi) = (lo.min(hi), hi.max(lo).min(self.view.pitch.top));
        let h = self.view_height();
        let rows = f64::from(hi - lo) + 1.0;
        let rh = h / rows;
        self.view.pitch.row_height = rh.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        if rh >= MIN_ROW_HEIGHT {
            self.view.pitch.scroll_rows = f64::from(self.view.pitch.top) - f64::from(hi);
            self.view.pitch.clamp_scroll(h);
        } else {
            self.view
                .center_pitch((f64::from(lo) + f64::from(hi)) / 2.0, h);
        }
        self.fit_rows = Some(rows);
    }

    /// The key rows a Fit Keys is keeping fitted (Lock Fit Range), if any.
    #[cfg(test)]
    pub fn fitted_rows(&self) -> Option<f64> {
        self.fit_rows.filter(|_| self.lock_fit)
    }

    /// Once per frame with the note area's height: Lock Fit Range keeps the
    /// fitted rows (the top row stays), then the scroll limits apply. The
    /// [`Self::row_floor`] belongs to the Y zooms only: a window or UI-scale
    /// change never changes the zoom by itself (B1-0062).
    fn keep_y_policy(&mut self, h: f64) {
        use plumspirit_engine::view::{MAX_ROW_HEIGHT, MIN_ROW_HEIGHT};
        if !self.lock_fit {
            self.fit_rows = None;
        }
        if let Some(rows) = self.fit_rows {
            self.view.pitch.row_height = (h / rows).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        }
        self.view.pitch.clamp_scroll(h);
    }

    /// Sets Zoom Y (percent of the reference row height), keeping the centre row.
    pub fn set_zoom_y_percent(&mut self, percent: f64) -> Result<(), String> {
        let h = f64::from(self.notes_size.y.max(1.0));
        let rh = viewport::zoom_y_from_percent(percent)?.max(self.row_floor(h));
        self.fit_rows = None;
        self.view.set_zoom_y(h / 2.0, rh, h);
        Ok(())
    }

    pub fn zoom_x_percent(&self, doc: &PlumDocument) -> f64 {
        self.view.zoom_x_percent(doc.division())
    }

    pub fn zoom_y_percent(&self) -> f64 {
        self.view.zoom_y_percent()
    }

    /// Reset Zoom: X and Y back to 100 %, around the same anchors as typed values.
    pub fn reset_zoom(&mut self, doc: &PlumDocument, position: u64) {
        let _ = self.set_zoom_x_percent(100.0, doc, position);
        let _ = self.set_zoom_y_percent(100.0);
    }

    /// Fit Song: the whole song (notes, events, Conductor) across the width.
    pub fn fit_song(&mut self, doc: &PlumDocument) {
        let span = viewport::zoom_span(doc, 0).max(doc.duration_ticks());
        self.view.fit(span, self.view_width());
    }

    fn snap_unit(&self, doc: &PlumDocument) -> Option<u64> {
        self.snap.unit_ticks(doc.division())
    }

    /// Length of a note drawn at `at` without dragging (the default Gate).
    fn new_note_length(&self, doc: &PlumDocument, at: u64) -> u64 {
        match &self.meter {
            Some(m) => self.new_note.1.ticks(doc.division(), self.snap, m, at),
            None => {
                plumspirit_engine::model::timing::default_note_length(doc.division(), self.snap)
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        input: RollInput<'_>,
        generation: u64,
    ) -> Vec<RollAction> {
        self.show_with_extent(ui, input, generation, None)
    }

    /// Private immutable import browser: native navigation only, no emitted actions.
    /// Its content extent avoids scanning the whole staged project's placements.
    pub(crate) fn show_staged(
        &mut self,
        ui: &mut egui::Ui,
        mut input: RollInput<'_>,
        generation: u64,
        extent: u64,
    ) {
        input.editable = false;
        input.keyboard = false;
        input.audition = false;
        input.capture = None;
        input.note_menu = None;
        let _ = self.show_with_extent(ui, input, generation, Some(extent));
    }

    fn show_with_extent(
        &mut self,
        ui: &mut egui::Ui,
        input: RollInput<'_>,
        generation: u64,
        extent: Option<u64>,
    ) -> Vec<RollAction> {
        self.overlay_layer = None;
        self.view_frame = None;
        let mut actions = Vec::new();
        self.user_panned = false;
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        if full.width() < KEYS_WIDTH + SCROLLBAR + 40.0
            || full.height() < RULER_HEIGHT + markers::BAND_HEIGHT + SCROLLBAR + 40.0
        {
            return actions;
        }
        let doc = input.doc;
        let conductor = doc.conductor();
        // The notes edited: the hosting track's own, or the opened clip's
        // owned content (resolved once per frame; `editor::note_view`).
        let Ok(src) = plumspirit_engine::editor::note_view(doc, input.target()) else {
            return actions;
        };
        // B2-F18: the aggregate's other clips on this roll's axis.
        let parts: Vec<render::PlacedPart<'_>> = input
            .parts
            .iter()
            .map(|q| render::PlacedPart {
                store: q.store,
                key: q.key,
                place: plumspirit_engine::view::tiles::SourcePlace {
                    shift: q.shift - input.shift,
                    window: q.window,
                },
            })
            .collect();
        // v0.9: the rows follow the project's key range (256-Key Mode on /
        // off keeps the pitch at the top of the view where it was).
        let h_last = self.view_height();
        self.view.set_top(doc.project().keys.max_pitch(), h_last);
        self.shift = input.shift;
        self.new_note = (input.new_note_channel, input.new_note_gate);
        self.new_note_sub = input.new_note_sub;
        self.audition = input.audition;
        if self.meter.as_ref() != Some(input.meter) {
            self.meter = Some(input.meter.clone());
        }
        let ppp = ui.ctx().pixels_per_point();
        let g0 = Geometry::new(full, self.view, ppp);
        self.notes_size = g0.notes.size();
        let (w, h) = (f64::from(g0.notes.width()), f64::from(g0.notes.height()));
        if self.initialized_for != Some(generation) || self.reset_requested {
            self.view = RollView::initial(doc, input.track_index, self.default_zoom, w, h);
            self.initialized_for = Some(generation);
            self.reset_requested = false;
            self.fit_rows = None;
            if self.default_fit {
                // L-F02: the remembered all-keys overview (128 / 256 rows).
                self.fit_keys(0, self.view.pitch.top);
            }
        }
        self.keep_y_policy(h);
        // The scrollable extent on the roll's axis: the song (with the edit
        // cursor) moved into content ticks, and an opened content's notes.
        let content_end = if let Some(extent) = extent {
            extent.max(self.cursor_tick)
        } else if input.shift == 0 && input.content.is_none() {
            doc.content_end_tick().max(self.cursor_tick)
        } else {
            song_of(doc.content_end_tick().max(self.cursor_tick), -input.shift)
                .max(src.notes.max_end().unwrap_or(0))
        };
        // B2-F15: a locked roll shows no notes of its own (only ghosts).
        let own_notes = !input.conductor_active && input.locked.is_none();
        let note_edits = input.editable && own_notes;
        // Selections of Conductor entries that no longer exist are dropped.
        if self
            .selected_marker
            .is_some_and(|id| conductor.marker_by_id(id).is_none())
        {
            self.selected_marker = None;
        }
        if self
            .selected_signature
            .is_some_and(|id| conductor.signature_by_id(id).is_none())
        {
            self.selected_signature = None;
        }

        // ----------------------------------------------------------- input
        let id = ui.id().with("piano_roll");
        let notes_resp = ui.interact(g0.notes, id.with("notes"), Sense::click_and_drag());
        let ruler_resp = ui.interact(g0.ruler, id.with("ruler"), Sense::click_and_drag());
        let band_resp = ui.interact(g0.band, id.with("band"), Sense::click_and_drag());
        let h_resp = ui.interact(g0.hscroll, id.with("hscroll"), Sense::click_and_drag());
        let v_resp = ui.interact(g0.vscroll, id.with("vscroll"), Sense::click_and_drag());
        let keys_resp = ui.interact(g0.keys, id.with("keys"), Sense::click_and_drag());
        let hovered_full = ui.rect_contains_pointer(full);
        let on_control = ui
            .input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()))
            .is_some_and(|p| {
                input
                    .overlay_controls
                    .iter()
                    .flatten()
                    .any(|r| r.contains(p))
            });
        // The notes area under the pointer (not a readout's control on it).
        let over_notes = notes_resp.contains_pointer() && !on_control;

        let (
            events,
            pointer_pos,
            pressed,
            released,
            primary_down,
            middle_pressed,
            secondary_pressed,
            modifiers,
            escape,
        ) = ui.input(|i| {
            (
                i.events.clone(),
                i.pointer.hover_pos().or(i.pointer.interact_pos()),
                i.pointer.button_pressed(PointerButton::Primary),
                i.pointer.button_released(PointerButton::Primary),
                i.pointer.button_down(PointerButton::Primary),
                i.pointer.button_pressed(PointerButton::Middle),
                i.pointer.button_pressed(PointerButton::Secondary),
                i.modifiers,
                i.key_pressed(Key::Escape),
            )
        });

        // Wheel: scroll / zoom (only while the pointer is over the roll).
        if hovered_full {
            for e in &events {
                if let Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: m,
                    ..
                } = e
                {
                    let scale = match unit {
                        MouseWheelUnit::Point => 1.0,
                        MouseWheelUnit::Line => 40.0,
                        MouseWheelUnit::Page => h as f32,
                    };
                    let d = *delta * scale;
                    let anchor = pointer_pos.unwrap_or(g0.notes.center());
                    if m.command && m.shift {
                        let f = viewport::wheel_factor(f64::from(d.y + d.x));
                        self.zoom_pitch_at(f64::from(anchor.y - g0.notes.top()), f, h);
                    } else if m.command {
                        let f = viewport::wheel_factor(f64::from(d.y + d.x));
                        self.view.zoom_time(
                            f64::from(anchor.x - g0.notes.left()),
                            f,
                            extent.map_or_else(
                                || viewport::zoom_span(doc, self.cursor_tick),
                                |e| {
                                    e.max(self.cursor_tick)
                                        .max(doc.division().musical_ppqn().unwrap_or(480) * 16)
                                },
                            ),
                            w,
                        );
                    } else if m.shift {
                        self.view.time.scroll_pixels(-f64::from(d.y + d.x));
                        self.user_panned |= d.y + d.x != 0.0;
                    } else {
                        self.view.time.scroll_pixels(-f64::from(d.x));
                        self.user_panned |= d.x != 0.0;
                        self.view
                            .scroll_rows(-f64::from(d.y) / self.view.pitch.row_height, h);
                    }
                }
            }
        }

        let g = Geometry::new(full, self.view, ppp);
        // Song time (ruler, Conductor band, playhead, loop, selections,
        // Onion): the same view shifted for an opened clip.
        let gs = g.song(input.shift);
        let unit = self.snap_unit(doc);

        // Escape / right-click cancel the transient gesture (nothing is committed).
        if escape && input.keyboard && !matches!(self.gesture, Gesture::None) {
            if matches!(self.gesture, Gesture::Stroke) {
                actions.push(RollAction::StrokeCanceled);
            }
            self.cancel_gesture();
        }
        if secondary_pressed
            && matches!(
                self.gesture,
                Gesture::Draw { .. } | Gesture::Erase { .. } | Gesture::Marker { .. }
            )
        {
            self.cancel_gesture();
        }

        // Custom tools extend the existing native stroke engine with ordered input.
        // Polling only the final pointer position loses fast down/move/up batches.
        let mut ordered_handled = false;
        if input.capture == Some(CaptureMode::FreehandOrdered) {
            let overflow = events.len() > 256;
            if overflow && matches!(self.gesture, Gesture::Stroke) {
                self.cancel_gesture();
                actions.push(RollAction::StrokeCanceled);
                ordered_handled = true;
            }
            if !overflow && !escape {
                for event in &events {
                    match event {
                        Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: true,
                            ..
                        } if matches!(self.gesture, Gesture::None)
                            && input.editable
                            && g.notes.contains(*pos)
                            && ui
                                .ctx()
                                .layer_id_at(*pos)
                                .is_none_or(|l| l == ui.layer_id()) =>
                        {
                            self.stroke_buf.clear();
                            self.stroke_sent = 0;
                            self.stroke_buf
                                .push((g.tick_at(pos.x), g.pitch_f_at(pos.y), 0.));
                            self.stroke_began = Some(std::time::Instant::now());
                            self.gesture = Gesture::Stroke;
                            ordered_handled = true;
                        }
                        Event::PointerMoved(pos) if matches!(self.gesture, Gesture::Stroke) => {
                            ordered_handled = true;
                            let moved = self
                                .stroke_buf
                                .last()
                                .is_none_or(|p| (g.point_of(p.0, p.1) - *pos).length() >= 2.);
                            if moved
                                && self.stroke_buf.len()
                                    < plumspirit_engine::transform::MAX_STROKE_POINTS
                            {
                                let time =
                                    self.stroke_began.map_or(0., |b| b.elapsed().as_secs_f64());
                                self.stroke_buf
                                    .push((g.tick_at(pos.x), g.pitch_f_at(pos.y), time));
                            }
                        }
                        Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: false,
                            ..
                        } if matches!(self.gesture, Gesture::Stroke) => {
                            ordered_handled = true;
                            let p = (g.tick_at(pos.x), g.pitch_f_at(pos.y));
                            if self
                                .stroke_buf
                                .last()
                                .is_none_or(|q| q.0 != p.0 || q.1 != p.1)
                                && self.stroke_buf.len()
                                    < plumspirit_engine::transform::MAX_STROKE_POINTS
                            {
                                self.stroke_buf.push((
                                    p.0,
                                    p.1,
                                    self.stroke_began.map_or(0., |b| b.elapsed().as_secs_f64()),
                                ));
                            }
                            self.stroke_began = None;
                            self.gesture = Gesture::None;
                            actions.push(RollAction::Stroke {
                                points: std::mem::take(&mut self.stroke_buf),
                                done: true,
                            });
                        }
                        Event::WindowFocused(false) | Event::PointerGone
                            if matches!(self.gesture, Gesture::Stroke) =>
                        {
                            ordered_handled = true;
                            self.cancel_gesture();
                            actions.push(RollAction::StrokeCanceled);
                        }
                        Event::PointerButton {
                            button: PointerButton::Middle | PointerButton::Secondary,
                            pressed: true,
                            ..
                        } if matches!(self.gesture, Gesture::Stroke) => {
                            ordered_handled = true;
                            self.cancel_gesture();
                            actions.push(RollAction::StrokeCanceled);
                        }
                        _ => {}
                    }
                }
            }
            if matches!(self.gesture, Gesture::Stroke) && self.stroke_sent != self.stroke_buf.len()
            {
                self.stroke_sent = self.stroke_buf.len();
                actions.push(RollAction::Stroke {
                    points: self.stroke_buf.clone(),
                    done: false,
                });
            }
        }

        // Middle-drag pans (always allowed).
        if middle_pressed
            && hovered_full
            && let Some(p) = pointer_pos
        {
            self.gesture = Gesture::Pan { last: p };
        }

        // Marker band layout (used for picking now and drawing later).
        let preview = match self.gesture {
            Gesture::Marker {
                id,
                now,
                moved: true,
                ..
            } => Some(markers::Preview {
                id,
                start: now.0,
                end: now.1,
            }),
            _ => None,
        };
        let previous = std::mem::take(&mut self.band_layout);
        self.band_layout = markers::layout(conductor, &gs.view.time, gs.band, preview);
        self.band_layout.inherit_labels(&previous);

        // Right-click on the band or a signature label: remember the target for
        // the context menu.
        if secondary_pressed && let Some(pos) = pointer_pos {
            if g.notes.contains(pos)
                && over_notes
                && note_edits
                && input.note_menu.is_some()
                && matches!(self.gesture, Gesture::None)
            {
                // §156: an unselected note under the pointer becomes the
                // selection first (standard behaviour); a selected one keeps
                // the whole selection as the target.
                if let Some(h) = hit_note(src.notes, &g, pos)
                    && !h.note.selected
                {
                    actions.push(RollAction::SelectOnly(h.loc));
                }
                self.context = Some(ContextTarget::Notes);
            } else if g.band.contains(pos) {
                self.context = Some(
                    match markers::hit(&self.band_layout, pos, self.selected_marker) {
                        Some((id, _)) => {
                            self.selected_marker = Some(id);
                            ContextTarget::Marker(id)
                        }
                        None => ContextTarget::Band(gs.tick_at(pos.x.max(gs.band.left()))),
                    },
                );
            } else if g.ruler.contains(pos) {
                self.context = Some(
                    match self.signature_rects.iter().find(|(_, r)| r.contains(pos)) {
                        Some(&(id, _)) => {
                            self.selected_signature = Some(id);
                            ContextTarget::Signature(id)
                        }
                        None => ContextTarget::Ruler(gs.tick_at(pos.x.max(gs.ruler.left()))),
                    },
                );
            }
        }

        // Presses start gestures.
        if pressed
            && !ordered_handled
            && !(input.capture == Some(CaptureMode::FreehandOrdered)
                && pointer_pos.is_some_and(|p| g.notes.contains(p)))
            && matches!(self.gesture, Gesture::None)
            && let Some(pos) = pointer_pos
        {
            if over_notes
                && g.notes.contains(pos)
                && input.editable
                && let Some(mode) = input.capture
            {
                let pt = (g.tick_at(pos.x), g.pitch_f_at(pos.y));
                match mode {
                    CaptureMode::Freehand | CaptureMode::FreehandOrdered => {
                        self.stroke_buf.clear();
                        self.stroke_buf.push((pt.0, pt.1, 0.0));
                        self.stroke_began = Some(std::time::Instant::now());
                        self.stroke_sent = 0;
                        self.gesture = Gesture::Stroke;
                    }
                    CaptureMode::Polygon => actions.push(RollAction::StrokeVertex(pt.0, pt.1)),
                }
            } else if over_notes && g.notes.contains(pos) && note_edits {
                // B2-F18: a press on another clip's note focuses that clip.
                match (self.tool, hit_note(src.notes, &g, pos)) {
                    (Tool::Erase, _) | (_, Some(_)) => {
                        self.begin_press(&g, &src, doc, pos, modifiers, unit, &mut actions);
                    }
                    (_, None) => match part_hit(&parts, &g, pos) {
                        Some((i, loc)) => actions.push(RollAction::FocusClip {
                            clip: input.parts[i].clip,
                            loc,
                            add: modifiers.shift || modifiers.command,
                        }),
                        None => self.begin_press(&g, &src, doc, pos, modifiers, unit, &mut actions),
                    },
                }
            } else if keys_resp.contains_pointer() && g.keys.contains(pos) && input.audition {
                let pitch = g.pitch_at_clamped(pos.y);
                self.gesture = Gesture::Keys { pitch };
                actions.push(RollAction::AuditionKey(Some(pitch)));
            } else if band_resp.contains_pointer() && g.band.contains(pos) {
                match markers::hit(&self.band_layout, pos, self.selected_marker) {
                    Some((id, part)) if input.editable => {
                        if let Some((_, m)) = conductor.marker_by_id(id) {
                            self.selected_marker = Some(id);
                            self.selected_signature = None;
                            self.gesture = Gesture::Marker {
                                id,
                                part,
                                press: pos,
                                grab: gs.tick_at(pos.x.max(gs.band.left())),
                                start: m.start,
                                end: m.end,
                                now: (m.start, m.end),
                                moved: false,
                            };
                        }
                    }
                    _ => self.selected_marker = None,
                }
            } else if ruler_resp.contains_pointer() && g.ruler.contains(pos) && modifiers.shift {
                // Shift+drag on the ruler: set the loop range.
                let t = gs.tick_at(pos.x.clamp(gs.ruler.left(), gs.ruler.right()));
                let t = unit.map_or(t, |u| snap_nearest(t, u));
                self.gesture = Gesture::Ruler(ruler::RulerDrag::press(t, pos.x, true));
            } else if ruler_resp.contains_pointer() && g.ruler.contains(pos) {
                if let Some(&(sid, _)) = self.signature_rects.iter().find(|(_, r)| r.contains(pos))
                {
                    self.selected_signature = Some(sid);
                    self.selected_marker = None;
                }
                let t = gs.tick_at(pos.x.clamp(gs.ruler.left(), gs.ruler.right()));
                self.cursor_tick = unit.map_or(t, |u| snap_nearest(t, u));
                self.gesture =
                    Gesture::Ruler(ruler::RulerDrag::press(self.cursor_tick, pos.x, false));
            } else if h_resp.contains_pointer() && g.hscroll.contains(pos) {
                let extent = self.view.scroll_extent(content_end, w) as f64;
                let vis = w / self.view.time.pixels_per_tick;
                let thumb = render::scrollbar_thumb(
                    g.hscroll,
                    true,
                    self.view.time.origin_tick as f64 / extent,
                    vis / extent,
                );
                let grab = if thumb.contains(pos) {
                    pos.x - thumb.left()
                } else {
                    thumb.width() / 2.0
                };
                self.gesture = Gesture::HScroll { grab };
            } else if v_resp.contains_pointer() && g.vscroll.contains(pos) {
                let rows_vis = h / self.view.pitch.row_height;
                let thumb = render::scrollbar_thumb(
                    g.vscroll,
                    false,
                    self.view.pitch.scroll_rows / self.view.pitch.rows(),
                    rows_vis / self.view.pitch.rows(),
                );
                let grab = if thumb.contains(pos) {
                    pos.y - thumb.top()
                } else {
                    thumb.height() / 2.0
                };
                self.gesture = Gesture::VScroll { grab };
            }
        }
        // Double-clicks on the band / ruler labels open the edit dialogs.
        if band_resp.double_clicked()
            && input.editable
            && let Some(pos) = pointer_pos
        {
            match markers::hit(&self.band_layout, pos, self.selected_marker) {
                Some((id, _)) => actions.push(RollAction::EditMarker(id)),
                None => {
                    let t = gs.tick_at(pos.x.max(gs.band.left()));
                    actions.push(RollAction::AddMarkerAt(
                        unit.map_or(t, |u| snap_nearest(t, u)),
                    ))
                }
            }
            self.gesture = Gesture::None;
        }
        if ruler_resp.double_clicked()
            && input.editable
            && let Some(pos) = pointer_pos
            && let Some(&(sid, _)) = self.signature_rects.iter().find(|(_, r)| r.contains(pos))
        {
            actions.push(RollAction::EditSignature(sid));
        }

        // Drag updates.
        self.shift_held = modifiers.shift;
        self.alt_held = modifiers.alt;
        self.ctrl_held = modifiers.command;
        if let Some(pos) = pointer_pos {
            if matches!(self.gesture, Gesture::Stroke)
                && input.capture != Some(CaptureMode::FreehandOrdered)
            {
                let t = self.stroke_began.map_or(0.0, |b| b.elapsed().as_secs_f64());
                let pt = (g.tick_at(pos.x), g.pitch_f_at(pos.y));
                let moved = self.stroke_buf.last().is_none_or(|l| {
                    let a = g.point_of(l.0, l.1);
                    (a - pos).length() >= 2.0
                });
                if moved && self.stroke_buf.len() < plumspirit_engine::transform::MAX_STROKE_POINTS
                {
                    self.stroke_buf.push((pt.0, pt.1, t));
                }
                if self.stroke_buf.len() != self.stroke_sent {
                    self.stroke_sent = self.stroke_buf.len();
                    actions.push(RollAction::Stroke {
                        points: self.stroke_buf.clone(),
                        done: false,
                    });
                }
            }
            let keys = doc.project().keys;
            self.update_gesture(&g, &gs, &src, keys, pos, unit, content_end, w, h);
            // Glissando: the held key follows the pointer, one change per key.
            if let Gesture::Keys { pitch } = &mut self.gesture {
                let p = g.pitch_at_clamped(pos.y);
                if p != *pitch {
                    *pitch = p;
                    actions.push(RollAction::AuditionKey(Some(p)));
                }
            }
        }
        let middle_down = ui.input(|i| i.pointer.button_down(PointerButton::Middle));
        if let Gesture::Pan { .. } = self.gesture {
            if !middle_down {
                self.gesture = Gesture::None;
            }
        } else if released || !primary_down {
            self.finish_gesture(&g, doc, &mut actions);
        }

        // Autoscroll while dragging notes / box / markers beyond the edges.
        if let (Some(pos), true) = (
            pointer_pos,
            matches!(
                self.gesture,
                Gesture::Move { .. }
                    | Gesture::Draw { .. }
                    | Gesture::BoxSelect { .. }
                    | Gesture::Resize { .. }
                    | Gesture::Erase { .. }
                    | Gesture::Marker { .. }
                    | Gesture::Ruler(
                        ruler::RulerDrag::Loop { .. } | ruler::RulerDrag::Range { .. }
                    )
            ),
        ) {
            let mut moved = false;
            if pos.x > g.notes.right() {
                self.view
                    .time
                    .scroll_pixels(f64::from((pos.x - g.notes.right()).min(60.0)) * 0.5);
                moved = true;
                self.user_panned = true;
            } else if pos.x < g.notes.left() {
                self.view
                    .time
                    .scroll_pixels(-f64::from((g.notes.left() - pos.x).min(60.0)) * 0.5);
                moved = true;
                self.user_panned = true;
            }
            if !matches!(self.gesture, Gesture::Marker { .. } | Gesture::Ruler(_)) {
                if pos.y > g.notes.bottom() {
                    self.view.scroll_rows(0.25, h);
                    moved = true;
                } else if pos.y < g.notes.top() {
                    self.view.scroll_rows(-0.25, h);
                    moved = true;
                }
            }
            if moved {
                ui.ctx().request_repaint();
            }
        }

        // Keep the origin inside the scrollable extent after zoom/scroll.
        let extent = self.view.scroll_extent(content_end, w);
        let vis_ticks = (w / self.view.time.pixels_per_tick) as u64;
        if self.view.time.origin_tick > extent.saturating_sub(vis_ticks / 2) {
            self.view.time.origin_tick = extent.saturating_sub(vis_ticks / 2);
        }

        // ---------------------------------------------------------- hover / cursor
        let g = Geometry::new(full, self.view, ppp);
        let gs = g.song(input.shift);
        let hover_hit: Option<Hit> = match (pointer_pos, &self.gesture) {
            (Some(pos), Gesture::None) if over_notes && !self.stats.lod && note_edits => {
                hit_note(src.notes, &g, pos)
            }
            _ => None,
        };
        self.pointer = match pointer_pos {
            Some(pos) if g.notes.contains(pos) || g.keys.contains(pos) => {
                let tick = g.notes.x_range().contains(pos.x).then(|| g.tick_at(pos.x));
                PointerInfo {
                    tick,
                    pitch: g.view.pitch.y_to_pitch(f64::from(pos.y - g.notes.top())),
                    song_tick: tick.map(|t| song_of(t, input.shift)),
                }
            }
            Some(pos) if g.ruler.contains(pos) || g.band.contains(pos) => {
                let song = gs.tick_at(pos.x.max(gs.ruler.left()));
                PointerInfo {
                    tick: Some(song_of(song, -input.shift)),
                    pitch: None,
                    song_tick: Some(song),
                }
            }
            _ => PointerInfo::default(),
        };
        let band_hover = match (pointer_pos, &self.gesture) {
            (Some(pos), Gesture::None) if band_resp.contains_pointer() => {
                markers::hit(&self.band_layout, pos, self.selected_marker)
            }
            _ => None,
        };
        let handle_hover = match (pointer_pos, &self.gesture) {
            (Some(pos), Gesture::None) if over_notes && note_edits && self.tool == Tool::Select => {
                self.selection_box(&src).and_then(|b| {
                    let r = handles::screen_rect(&g, &b);
                    handles::visible(r, g.notes)
                        .then(|| handles::hit(r, pos))
                        .flatten()
                })
            }
            _ => None,
        };
        let icon = match (&self.gesture, &hover_hit) {
            (Gesture::ScaleBox { mask, .. }, _) => Some(handles::cursor(*mask)),
            (Gesture::None, _) if handle_hover.is_some() => handle_hover.map(handles::cursor),
            (Gesture::Move { .. }, _) => Some(CursorIcon::Grabbing),
            (Gesture::Resize { .. }, _) => Some(CursorIcon::ResizeHorizontal),
            (Gesture::Pan { .. }, _) => Some(CursorIcon::AllScroll),
            (Gesture::Erase { .. }, _) => Some(CursorIcon::Crosshair),
            (Gesture::Marker { part, .. }, _) => Some(match part {
                MarkerPart::Start | MarkerPart::End => CursorIcon::ResizeHorizontal,
                _ => CursorIcon::Grabbing,
            }),
            (Gesture::None, _) if band_hover.is_some() && input.editable => {
                Some(match band_hover.map(|h| h.1) {
                    Some(MarkerPart::Start | MarkerPart::End) => CursorIcon::ResizeHorizontal,
                    _ => CursorIcon::PointingHand,
                })
            }
            (Gesture::None, Some(_)) if self.tool == Tool::Erase => Some(CursorIcon::Crosshair),
            (Gesture::None, Some(hh)) if note_edits => Some(if hh.on_edge {
                CursorIcon::ResizeHorizontal
            } else {
                CursorIcon::Grab
            }),
            (Gesture::None, None)
                if over_notes && note_edits && matches!(self.tool, Tool::Draw | Tool::Erase) =>
            {
                Some(CursorIcon::Crosshair)
            }
            _ => None,
        };
        if let Some(icon) = icon {
            ui.ctx().set_cursor_icon(icon);
        }

        // ---------------------------------------------------------------- paint
        let started = std::time::Instant::now();
        let painter = ui.painter_at(full);
        let pal = input.palette;
        let notes_painter = painter.with_clip_rect(g.notes);
        render::fill_background(&notes_painter, &g, pal);
        let grid_lines = render::draw_grid(
            &notes_painter,
            &gs,
            pal,
            input.meter,
            unit,
            &mut self.cache.grid,
        );
        let (move_preview, resize_preview) = match self.gesture {
            Gesture::Move { dt, dp, .. } if dt != 0 || dp != 0 => (Some((dt, dp)), None),
            Gesture::Resize { delta, .. } if delta != 0 => (None, Some(delta)),
            _ => (None, None),
        };
        let onion_started = std::time::Instant::now();
        let mut stats = RenderStats::default();
        self.dense.begin_frame();
        // v0.9 §068: the one note palette (application preferences).
        let note_palette = crate::ui::theme::note_palette(ui.ctx());
        if !input.onion.is_empty() {
            let pass = OnionPass {
                sources: input.onion,
                dark: input.dark,
                palette: &note_palette,
                alpha: input.onion_alpha,
                outlines: input.onion_outlines,
            };
            // L-G05: the tile workers wake the UI when a tile is done.
            render::draw_onion(
                &notes_painter,
                ui.ctx(),
                &gs,
                &pass,
                &mut self.onion_cache,
                &mut self.dense,
                &mut stats,
            );
        }
        stats.onion_time = onion_started.elapsed();
        let notes_started = std::time::Instant::now();
        let erasing = matches!(self.gesture, Gesture::Erase { .. });
        // B2-D01: the Conductor owns no notes. Its roll shows the timing grid,
        // the markers / tempo band and — when the Onion Skin is on — the
        // musical tracks as Onion sources in their own colours (drawn above);
        // never a faint copy of the track that was active before.
        let pass = NotePass {
            parts: &parts,
            store: src.notes,
            key: render::host_key(src.host()),
            pal,
            dark: input.dark,
            palette: &note_palette,
            hover: hover_hit.map(|h| h.rect),
            move_preview,
            resize_preview,
            erasing: erasing.then_some(&self.erase_set),
            hover_color: if self.tool == Tool::Erase {
                pal.error
            } else {
                pal.note_hover_border
            },
            previewing: input.preview.is_some() || input.tool_preview.is_some(),
            preview_all: input
                .preview
                .is_some_and(|p| p.1 == plumspirit_engine::editor::preview::PreviewScope::All)
                || input.tool_preview.as_ref().is_some_and(ToolPreview::all),
        };
        if own_notes {
            render::draw_notes(
                &notes_painter,
                ui.ctx(),
                &g,
                &pass,
                &mut self.cache,
                &mut self.dense,
                &mut stats,
            );
        }
        self.overlay_layer = self
            .capture_view
            .then(|| (notes_painter.clone(), notes_painter.add(egui::Shape::Noop)));
        // Preview layer: above the committed notes, below selection overlays and
        // the playhead; never part of the document.
        if let Some((k, scope)) = input.preview
            && own_notes
        {
            render::draw_preview(
                &notes_painter,
                &g,
                src,
                k,
                scope,
                pal,
                input.dark,
                &mut self.cache.subs,
            );
        }
        if let Some(tp) = input.tool_preview.as_ref()
            && own_notes
        {
            render::draw_tool_preview(
                &notes_painter,
                &g,
                src,
                tp,
                pal,
                input.dark,
                &mut self.cache.subs,
                &mut self.cache.tool_preview,
            );
        }
        if let Some(a) = input.added {
            render::draw_added(&notes_painter, &g, a.stores, a.records, a.longest, pal);
        }
        // v0.9 §057 / §229: the 88-Key Guide (guidance only: nothing is
        // cropped or refused outside it).
        if input.key_guide {
            render::draw_key_guide(&notes_painter, &g, g.notes, pal);
        }
        // Stroke tools: the stroke / lasso of the tool window, and the one
        // being drawn (bounded: at most `MAX_STROKE_POINTS` samples).
        if input.capture.is_some() {
            let live: Vec<(u64, f64)> = self.stroke_buf.iter().map(|q| (q.0, q.1)).collect();
            let pts: &[(u64, f64)] = if live.is_empty() {
                input.stroke_overlay
            } else {
                &live
            };
            let line: Vec<Pos2> = pts.iter().map(|&(t, p)| g.point_of(t, p)).collect();
            let stroke = egui::Stroke::new(1.5, pal.accent);
            if line.len() >= 2 {
                notes_painter.add(egui::Shape::line(line.clone(), stroke));
            }
            if input.capture == Some(CaptureMode::Polygon) {
                if line.len() >= 3 {
                    notes_painter.line_segment([line[line.len() - 1], line[0]], stroke);
                }
                for q in &line {
                    notes_painter.circle_filled(*q, 3.0, pal.accent);
                }
            }
        }
        stats.grid_lines = grid_lines;
        stats.notes_time = notes_started.elapsed();
        let overlay_started = std::time::Instant::now();
        // Selection box with scale handles (Select tool), or the dragged box.
        let shown_box = match self.gesture {
            Gesture::ScaleBox { to, .. } => Some(to),
            Gesture::None | Gesture::BoxSelect { .. }
                if self.tool == Tool::Select && note_edits =>
            {
                self.selection_box(&src)
            }
            _ => None,
        };
        let warped = match self.gesture {
            Gesture::ScaleBox {
                from,
                warp: Some(w),
                ..
            } => Some(handles::warp_outline(&g, &from, &w)),
            _ => None,
        };
        if let Some(q) = warped {
            // The warped box: its outline and the corner handles where they
            // are now.
            let line = [q[0], q[1], q[2], q[3], q[0]];
            notes_painter.add(egui::Shape::line(
                line.to_vec(),
                Stroke::new(1.0, pal.accent),
            ));
            for c in q {
                let hr = handles::handle_rect(c);
                notes_painter.rect_filled(hr, 0.0, pal.roll_bg);
                notes_painter.rect_stroke(
                    hr,
                    0.0,
                    Stroke::new(1.0, pal.accent),
                    egui::StrokeKind::Inside,
                );
            }
        } else if let Some(b) = shown_box {
            let r = handles::screen_rect(&g, &b);
            if handles::visible(r, g.notes) {
                let dragging = matches!(self.gesture, Gesture::ScaleBox { .. });
                let c = if dragging {
                    pal.accent
                } else {
                    pal.box_select_stroke.gamma_multiply(0.7)
                };
                notes_painter.rect_stroke(r, 0.0, Stroke::new(1.0, c), egui::StrokeKind::Outside);
                for (_, centre) in handles::handles(r) {
                    let hr = handles::handle_rect(centre);
                    notes_painter.rect_filled(hr, 0.0, pal.roll_bg);
                    notes_painter.rect_stroke(
                        hr,
                        0.0,
                        Stroke::new(1.0, c),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        // Gesture overlays.
        match self.gesture {
            Gesture::Draw {
                pitch, start, end, ..
            } => {
                let r = note_rect(&g, start, end, pitch);
                // B1-0092: the note being drawn takes the selected look at
                // the user's Selection Opacity.
                let (fill, border) =
                    crate::ui::theme::note_palette(notes_painter.ctx()).selection_paint(pal);
                notes_painter.rect_filled(r, 0.0, fill);
                notes_painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
            }
            Gesture::BoxSelect { origin, current } => {
                let r = Rect::from_two_pos(origin, current);
                notes_painter.rect_filled(r, 0.0, pal.box_select_fill);
                notes_painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, pal.box_select_stroke),
                    egui::StrokeKind::Inside,
                );
            }
            Gesture::Erase { .. } if stats.lod => {
                // The density raster cannot hide single notes: outline the hit ones.
                for &loc in self.erase_hits.iter().take(20_000) {
                    if let Some(n) = src.notes.get(loc) {
                        let r = note_rect(&g, n.start_tick, n.end_tick(), n.attrs.pitch());
                        notes_painter.rect_stroke(
                            r,
                            0.0,
                            Stroke::new(1.0, pal.error),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
            Gesture::Move {
                dt,
                dp,
                bounds: Some(b),
                ..
            } if stats.lod && (dt != 0 || dp != 0) => {
                // In LOD mode individual previews would be unreadable: show the
                // selection's bounding box at its target position instead.
                let s = (i128::from(b.min_start) + dt) as u64;
                let e = (i128::from(b.max_end) + dt) as u64;
                let hi = (i32::from(b.max_pitch) + dp) as u8;
                let lo = (i32::from(b.min_pitch) + dp) as u8;
                let r = note_rect(&g, s, e, hi).union(note_rect(&g, s, e, lo));
                notes_painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.5, pal.box_select_stroke),
                    egui::StrokeKind::Inside,
                );
            }
            _ => {}
        }
        // Chapter spans are shaded faintly behind the playhead line, so the
        // selected chapter's range is visible over the notes too.
        if let Some(id) = self.selected_marker
            && let Some((_, m)) = conductor.marker_by_id(id)
        {
            let (s, e) = match self.gesture {
                Gesture::Marker { id: gid, now, .. } if gid == id => now,
                _ => (m.start, m.end),
            };
            let x0 = gs.notes.left() + gs.view.time.tick_to_x(s) as f32;
            let x1 = e.map_or(x0, |e| gs.notes.left() + gs.view.time.tick_to_x(e) as f32);
            let r = Rect::from_min_max(
                Pos2::new(x0.max(gs.notes.left() - 2.0), g.notes.top()),
                Pos2::new(
                    x1.min(g.notes.right() + 2.0).max(x0 + 1.0),
                    g.notes.bottom(),
                ),
            );
            if r.width() > 1.5 {
                notes_painter.rect_filled(r, 0.0, pal.accent.gamma_multiply(0.08));
            }
            notes_painter.vline(
                x0.round() + 0.5,
                g.notes.y_range(),
                Stroke::new(1.0, markers::drag_color(pal)),
            );
        }
        // Loop range (restrained: a faint wash, edges marked) and the time
        // selection, after a ruler drag in progress (shared with the Track
        // List).
        let ruler_drag = match &self.gesture {
            Gesture::Ruler(d) => Some(d),
            _ => None,
        };
        let spans = ruler::Spans::with_drag(
            input.loop_range,
            input.loop_enabled,
            input.range,
            ruler_drag,
        );
        ruler::draw_area_spans(
            &notes_painter,
            gs.notes,
            gs.notes.left(),
            &gs.view.time,
            &spans,
            pal,
        );
        match input.playhead {
            Some(t) => render::draw_playhead(
                &notes_painter,
                gs.notes,
                gs.notes.left(),
                &gs.view.time,
                pal,
                t,
            ),
            None => render::draw_edit_cursor(&notes_painter, &gs, pal, self.cursor_tick),
        }
        // The anchor where it differs from the line just drawn (B1-0080).
        if let Some(a) = self.anchor
            && input.playhead.unwrap_or(self.cursor_tick) != a
        {
            render::draw_anchor_line(
                &notes_painter,
                gs.notes,
                gs.notes.left(),
                &gs.view.time,
                pal,
                a,
                None,
            );
        }
        stats.overlay_time = overlay_started.elapsed();
        let chrome_started = std::time::Instant::now();
        // Ruler with time-signature labels, then the marker band.
        render::draw_ruler_background(&painter, g.ruler, pal);
        let ruler_painter = painter.with_clip_rect(g.ruler);
        let sigs = if conductor.timing_editable() {
            markers::draw_signatures(
                &ruler_painter,
                gs.ruler,
                &gs.view.time,
                conductor,
                input.meter,
                pal,
                self.selected_signature,
            )
        } else {
            markers::SignatureLabels {
                rects: Vec::new(),
                bar_labels: Vec::new(),
            }
        };
        self.signature_rects = sigs.rects;
        let avoid: Vec<Rect> = self
            .signature_rects
            .iter()
            .map(|(_, r)| *r)
            .chain(sigs.bar_labels)
            .collect();
        render::draw_ruler(
            &ruler_painter,
            gs.ruler,
            &gs.view.time,
            pal,
            input.meter,
            &mut self.cache.grid,
            input.playhead.unwrap_or(self.cursor_tick),
            input.playhead.is_some(),
            &avoid,
        );
        ruler::draw_spans(&ruler_painter, gs.ruler, &gs.view.time, &spans, pal);
        if let Some(a) = self.anchor
            && input.playhead.unwrap_or(self.cursor_tick) != a
        {
            render::draw_anchor_line(
                &ruler_painter,
                gs.ruler,
                gs.ruler.left(),
                &gs.view.time,
                pal,
                a,
                Some(gs.ruler),
            );
        }
        // B1-0065: the roll's text banners (what is edited, the corrections
        // in effect, a non-musical timing note) sit at the right end of the
        // ruler strip — never over the notes.
        {
            let mut labels: Vec<(String, egui::Color32)> = Vec::new();
            if input.conductor_active {
                labels.push((
                    "Conductor — tempo, time signatures and markers for the whole song".into(),
                    pal.text,
                ));
            } else if let Some(hint) = input.locked {
                labels.push((hint.to_owned(), pal.warning));
            } else if let Some(title) = input.title {
                labels.push((format!("Editing {title}"), pal.text));
            }
            if let Some(note) = input.corrections_note {
                labels.push((note.to_owned(), pal.warning));
            }
            if let Some(note) = input.meter.non_musical_note() {
                labels.push((note.to_owned(), pal.text_weak));
            }
            if let Some(note) = self.scale_drag_note() {
                labels.push(note.into_label(pal));
            }
            if let Some(text) = self.rotation_note() {
                labels.push((text, pal.accent));
            }
            ruler_banners(&ruler_painter, g.ruler, &labels, pal);
        }
        markers::draw(
            &painter.with_clip_rect(g.band),
            gs.band,
            conductor,
            &mut self.band_layout,
            pal,
            self.selected_marker,
            band_hover.map(|h| h.0),
        );
        render::draw_keys(
            &painter.with_clip_rect(g.keys),
            &g,
            pal,
            &self.labels,
            self.pointer.pitch,
            input.key_colors,
        );
        if input.key_guide {
            render::draw_key_guide(&painter.with_clip_rect(g.keys), &g, g.keys, pal);
        }
        // v0.9 §063: the hovered key's number and name (0–127, or 0–255 in
        // 256-Key Mode), with the live keyboard colours' sounding-note count.
        if let Some(pos) = pointer_pos
            && keys_resp.hovered()
            && g.keys.contains(pos)
            && matches!(self.gesture, Gesture::None)
            && let Some(pitch) = g.view.pitch.y_to_pitch(f64::from(pos.y - g.notes.top()))
        {
            keys_resp
                .clone()
                .on_hover_text_at_pointer(key_hover_text(pitch, input.key_colors));
        }
        self.notes_rect = Some(g.notes);
        if self.capture_view {
            self.view_frame = input
                .doc
                .track(input.track_index)
                .map(|track| viewport::ViewFrame {
                    rect: g.notes,
                    view: g.view,
                    shift: input.shift,
                    track: track.id(),
                    content: input.content,
                    locked: input.locked.is_some() || !input.editable,
                    conductor: input.conductor_active,
                    pixels_per_point: ppp,
                    document: std::ptr::from_ref(input.doc) as usize,
                    revisions: [
                        input.doc.structure_revision(),
                        input.doc.analysis_revision(),
                        input.doc.layout_revision(),
                        input.doc.tempo_revision(),
                    ],
                });
        }
        // Scrollbars.
        let extent = self.view.scroll_extent(content_end, w) as f64;
        let hthumb = render::scrollbar_thumb(
            g.hscroll,
            true,
            self.view.time.origin_tick as f64 / extent,
            (w / self.view.time.pixels_per_tick) / extent,
        );
        render::draw_scrollbar(
            &painter,
            g.hscroll,
            hthumb,
            h_resp.hovered() || matches!(self.gesture, Gesture::HScroll { .. }),
            pal,
        );
        let vthumb = render::scrollbar_thumb(
            g.vscroll,
            false,
            self.view.pitch.scroll_rows / self.view.pitch.rows(),
            (h / self.view.pitch.row_height) / self.view.pitch.rows(),
        );
        render::draw_scrollbar(
            &painter,
            g.vscroll,
            vthumb,
            v_resp.hovered() || matches!(self.gesture, Gesture::VScroll { .. }),
            pal,
        );
        // Top-left corner (beside band and ruler): labels.
        let corner_tl = Rect::from_min_max(full.min, Pos2::new(g.keys.right(), g.ruler.bottom()));
        painter.rect_filled(corner_tl, 0.0, pal.ruler_bg);
        painter.hline(
            corner_tl.x_range(),
            corner_tl.bottom() - 0.5,
            Stroke::new(1.0, pal.panel_border),
        );
        painter.hline(
            corner_tl.x_range(),
            g.band.bottom() - 0.5,
            Stroke::new(1.0, pal.panel_border),
        );
        painter.text(
            Pos2::new(corner_tl.left() + 5.0, g.band.center().y),
            egui::Align2::LEFT_CENTER,
            "Markers",
            egui::FontId::proportional(crate::ui::theme::size::SMALL),
            pal.text_weak,
        );
        let corner_bl = Rect::from_min_max(
            Pos2::new(full.left(), g.hscroll.top()),
            Pos2::new(g.keys.right(), full.bottom()),
        );
        painter.rect_filled(corner_bl, 0.0, pal.scrollbar_track);
        let corner_br = Rect::from_min_max(g.vscroll.left_bottom(), full.right_bottom());
        painter.rect_filled(corner_br, 0.0, pal.scrollbar_track);
        // Above the vertical scrollbar, beside the band and ruler.
        let corner_tr = Rect::from_min_max(
            Pos2::new(g.vscroll.left(), full.top()),
            Pos2::new(full.right(), g.ruler.bottom()),
        );
        painter.rect_filled(corner_tr, 0.0, pal.ruler_bg);
        painter.hline(
            corner_tr.x_range(),
            corner_tr.bottom() - 0.5,
            Stroke::new(1.0, pal.panel_border),
        );
        stats.chrome_time = chrome_started.elapsed();
        stats.frame_time = started.elapsed();
        self.stats = stats;

        // ------------------------------------------------------------ tooltips / menus
        if let Some((mid, _)) = band_hover
            && let Some((_, m)) = conductor.marker_by_id(mid)
        {
            let text = markers::describe(m, |t| {
                format!(
                    "{} (tick {})",
                    input.meter.describe(t),
                    plumspirit_engine::midi::group_digits(t)
                )
            });
            band_resp.clone().on_hover_text_at_pointer(text);
        } else if band_resp.hovered() && matches!(self.gesture, Gesture::None) {
            band_resp.clone().on_hover_text_at_pointer(
                "Markers and chapters of the song. Double-click to add one; right-click for more.",
            );
        }
        if let Some(pos) = pointer_pos
            && ruler_resp.hovered()
            && let Some(&(sid, _)) = self.signature_rects.iter().find(|(_, r)| r.contains(pos))
            && let Some((_, s)) = conductor.signature_by_id(sid)
        {
            ruler_resp.clone().on_hover_text_at_pointer(format!(
                "Time signature {} from {} (tick {}). Double-click to edit; right-click for more.",
                s.label(),
                input.meter.describe(s.tick),
                plumspirit_engine::midi::group_digits(s.tick)
            ));
        }
        let editable = input.editable;
        let timing = conductor.timing_editable();
        let markers_ok = conductor.markers_editable();
        let target = self.context;
        let mut menu_action: Option<RollAction> = None;
        let mut close_context = false;
        for resp in [&band_resp, &ruler_resp] {
            resp.context_menu(|ui| match target {
                Some(ContextTarget::Marker(id)) => {
                    if ui
                        .add_enabled(editable, egui::Button::new("Edit Marker…"))
                        .clicked()
                    {
                        menu_action = Some(RollAction::EditMarker(id));
                        ui.close();
                    }
                    if ui.button("Go to Marker").clicked() {
                        if let Some((_, m)) = conductor.marker_by_id(id) {
                            menu_action = Some(RollAction::Seek(m.start));
                        }
                        ui.close();
                    }
                    if ui
                        .add_enabled(editable, egui::Button::new("Delete Marker"))
                        .clicked()
                    {
                        menu_action = Some(RollAction::DeleteMarker(id));
                        ui.close();
                    }
                }
                Some(ContextTarget::Signature(id)) => {
                    if ui
                        .add_enabled(editable, egui::Button::new("Edit Time Signature…"))
                        .clicked()
                    {
                        menu_action = Some(RollAction::EditSignature(id));
                        ui.close();
                    }
                    let first = conductor.time_signatures().first().map(|s| s.id) == Some(id);
                    if ui
                        .add_enabled(
                            editable && !first,
                            egui::Button::new("Delete Time Signature"),
                        )
                        .on_disabled_hover_text(
                            "The song needs a time signature at its start; edit it instead.",
                        )
                        .clicked()
                    {
                        menu_action = Some(RollAction::DeleteSignature(id));
                        ui.close();
                    }
                }
                Some(ContextTarget::Band(t) | ContextTarget::Ruler(t)) => {
                    let t = unit.map_or(t, |u| snap_nearest(t, u));
                    if ui
                        .add_enabled(
                            editable && markers_ok,
                            egui::Button::new("Add Marker Here…"),
                        )
                        .clicked()
                    {
                        menu_action = Some(RollAction::AddMarkerAt(t));
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            editable && timing,
                            egui::Button::new("Add Time Signature Here…"),
                        )
                        .clicked()
                    {
                        menu_action = Some(RollAction::AddSignatureAt(t));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button("Set Loop Start Here")
                        .on_hover_text("Shift+drag on the ruler sets both ends at once.")
                        .clicked()
                    {
                        menu_action = Some(RollAction::SetLoopStart(t));
                        ui.close();
                    }
                    if ui.button("Set Loop End Here").clicked() {
                        menu_action = Some(RollAction::SetLoopEnd(t));
                        ui.close();
                    }
                    if ui
                        .add_enabled(input.loop_range.is_some(), egui::Button::new("Clear Loop"))
                        .clicked()
                    {
                        menu_action = Some(RollAction::ClearLoop);
                        ui.close();
                    }
                    ui.separator();
                    let has = input.range.is_some();
                    if ui
                        .add_enabled(has, egui::Button::new("Loop Selected Time"))
                        .on_hover_text(
                            "The loop becomes the time selection (drag on the ruler to make one).",
                        )
                        .clicked()
                    {
                        menu_action = Some(RollAction::LoopTimeSelection);
                        ui.close();
                    }
                    if ui
                        .add_enabled(has, egui::Button::new("Select Notes in Time Selection"))
                        .on_hover_text("The active track's notes that start in the time selection.")
                        .clicked()
                    {
                        menu_action = Some(RollAction::SelectInTimeSelection);
                        ui.close();
                    }
                    if ui
                        .add_enabled(has, egui::Button::new("Clear Time Selection"))
                        .clicked()
                    {
                        menu_action = Some(RollAction::SetTimeSelection(None));
                        ui.close();
                    }
                }
                Some(ContextTarget::Notes) | None => {
                    close_context = true;
                    ui.close();
                }
            });
        }
        // v0.8 §156–§157: the note menu (categories, Repeat Last Tool, Recent
        // Tools, Mute, Delete).
        if let Some(menu) = input.note_menu {
            notes_resp.context_menu(|ui| {
                if target != Some(ContextTarget::Notes) {
                    close_context = true;
                    ui.close();
                    return;
                }
                note_menu_ui(ui, menu, &mut menu_action);
            });
        }
        if close_context {
            self.context = None;
        }
        if let Some(a) = menu_action {
            self.context = None;
            actions.push(a);
        }
        actions
    }

    #[allow(clippy::too_many_arguments)] // internal helper with one call site
    fn begin_press(
        &mut self,
        g: &Geometry,
        src: &plumspirit_engine::editor::HostView<'_>,
        doc: &PlumDocument,
        pos: Pos2,
        modifiers: egui::Modifiers,
        unit: Option<u64>,
        actions: &mut Vec<RollAction>,
    ) {
        if self.tool == Tool::Erase {
            // Erasing never selects: every note the pointer touches is removed on
            // release, as one undo step.
            self.erase_hits.clear();
            self.erase_set.clear();
            erase_along(
                src.notes,
                g,
                pos,
                pos,
                &mut self.erase_hits,
                &mut self.erase_set,
            );
            self.gesture = Gesture::Erase { last: pos };
            return;
        }
        // Scale handles of the selection's box (Select tool).
        if self.tool == Tool::Select
            && let Some(b) = self.selection_box(src)
        {
            let r = handles::screen_rect(g, &b);
            if handles::visible(r, g.notes)
                && let Some(mask) = handles::hit(r, pos)
            {
                self.gesture = Gesture::ScaleBox {
                    mask,
                    from: b,
                    to: b,
                    fill: self.alt_held,
                    origin: pos,
                    warp: None,
                    axis: None,
                };
                return;
            }
        }
        // At extreme density (LOD mode) one pixel covers many notes; the hit test still
        // picks the deterministic topmost note under the pointer.
        if let Some(h) = hit_note(src.notes, g, pos) {
            if modifiers.command {
                actions.push(RollAction::ToggleSelected(h.loc));
                return;
            }
            if modifiers.shift {
                actions.push(RollAction::AddSelected(h.loc));
            } else if !h.note.selected {
                actions.push(RollAction::SelectOnly(h.loc));
            }
            self.gesture = if h.on_edge {
                Gesture::Resize {
                    press: pos,
                    grab_end: h.note.end_tick(),
                    bounds: None,
                    delta: 0,
                }
            } else {
                Gesture::Move {
                    press: pos,
                    grab_start: h.note.start_tick,
                    grab_pitch: h.note.attrs.pitch(),
                    bounds: None,
                    dt: 0,
                    dp: 0,
                }
            };
            return;
        }
        match self.tool {
            Tool::Select | Tool::Erase => {
                if !(modifiers.command || modifiers.shift) {
                    actions.push(RollAction::ClearSelection);
                }
                self.gesture = Gesture::BoxSelect {
                    origin: pos,
                    current: pos,
                };
            }
            Tool::Draw => {
                let pitch = g.pitch_at_clamped(pos.y);
                let tick = g.tick_at(pos.x);
                let start = unit.map_or(tick, |u| snap_content(tick, u, self.shift, true));
                if self.audition {
                    actions.push(RollAction::AuditionNote { pitch, tick: start });
                }
                let len = self.new_note_length(doc, start);
                self.gesture = Gesture::Draw {
                    pitch,
                    start,
                    end: start.saturating_add(len),
                    press: pos,
                    dragged: false,
                };
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // internal helper with one call site
    #[allow(clippy::too_many_arguments)]
    fn update_gesture(
        &mut self,
        g: &Geometry,
        gs: &Geometry,
        src: &plumspirit_engine::editor::HostView<'_>,
        keys: plumspirit_engine::model::project::KeyRange,
        pos: Pos2,
        unit: Option<u64>,
        content_end: u64,
        w: f64,
        h: f64,
    ) {
        let ppt = self.view.time.pixels_per_tick;
        let shift = self.shift_held;
        let alt = self.alt_held;
        // Content ticks snap to the song's grid (the opened clip's shift).
        let sh = self.shift;
        let ctrl = self.ctrl_held;
        let about_center = self.rotate_about_center;
        match &mut self.gesture {
            Gesture::ScaleBox {
                mask,
                from,
                to,
                fill,
                origin,
                warp,
                axis,
            } => {
                *fill = alt;
                let corner = handles::corner_of(*mask);
                match corner.filter(|_| ctrl) {
                    // B1-0091: Alt+corner (no Ctrl) rotates about the pivot;
                    // Shift snaps to 15° steps.
                    None if alt && corner.is_some() => {
                        *to = *from;
                        if let Some(c) = corner
                            && let Some(w) = handles::rotation_warp(
                                g,
                                from,
                                c,
                                about_center,
                                *origin,
                                pos,
                                shift,
                            )
                        {
                            *warp = Some(w);
                        }
                    }
                    // Ctrl+corner: Skew, Ctrl+Shift+corner: Perspective.
                    Some(corner) => {
                        *to = *from;
                        let kind = if shift {
                            Some(plumspirit_engine::transform::geom::WarpKind::Perspective)
                        } else {
                            if axis.is_none() {
                                *axis = handles::skew_axis(*origin, pos);
                            }
                            *axis
                        };
                        match kind {
                            Some(kind) => {
                                let w = handles::corner_warp(g, corner, kind, *origin, pos);
                                // A Perspective corner dragged across the
                                // diagonal would fold the plane: the last
                                // valid shape stays.
                                if w.valid(from) {
                                    *warp = Some(w);
                                } else if warp.is_some_and(|o| o.kind != kind) {
                                    *warp = None;
                                }
                            }
                            None => *warp = None,
                        }
                    }
                    None => {
                        *warp = None;
                        *to =
                            handles::dragged(g, from, *mask, pos, if shift { unit } else { None });
                    }
                }
            }
            Gesture::Ruler(d) => {
                let t = gs.tick_at(pos.x.clamp(gs.ruler.left(), gs.ruler.right()));
                d.drag(unit.map_or(t, |u| snap_nearest(t, u)), pos.x);
            }
            Gesture::None | Gesture::Stroke => {}
            Gesture::Draw {
                start,
                end,
                press,
                dragged,
                ..
            } => {
                if (pos.x - press.x).abs() >= CLICK_SLOP {
                    *dragged = true;
                }
                if *dragged {
                    let t = g.tick_at(pos.x);
                    let min_len = unit.unwrap_or(1).max(1);
                    let e = unit.map_or(t, |u| snap_content(t, u, sh, false));
                    *end = e.max(start.saturating_add(min_len));
                }
            }
            Gesture::BoxSelect { current, .. } => *current = pos,
            Gesture::Erase { last } => {
                let a = *last;
                *last = pos;
                erase_along(
                    src.notes,
                    g,
                    a,
                    pos,
                    &mut self.erase_hits,
                    &mut self.erase_set,
                );
            }
            Gesture::Move {
                press,
                grab_start,
                grab_pitch,
                bounds,
                dt,
                dp,
            } => {
                // A click never moves notes (v0.8 Wave Q fix): until the
                // pointer leaves the click slop, nothing is computed — before,
                // the first held frame snapped an off-grid grabbed note to the
                // grid, so a plain click on a selected note moved the whole
                // selection (one "Move Notes" undo step).
                if *dt == 0 && *dp == 0 && (pos - *press).length() < CLICK_SLOP {
                    return;
                }
                if bounds.is_none() {
                    *bounds = src.notes.selection_bounds();
                }
                let Some(b) = bounds else {
                    return;
                };
                let raw_dt = (f64::from(pos.x - press.x) / ppt).round() as i128;
                let target = (i128::from(*grab_start) + raw_dt).max(0) as u64;
                let snapped = unit.map_or(target, |u| snap_content(target, u, sh, false));
                let want_dt = i128::from(snapped) - i128::from(*grab_start);
                let want_dp = i32::from(g.pitch_at_clamped(pos.y)) - i32::from(*grab_pitch);
                let (cdt, cdp) = clamp_move_delta(b, want_dt, want_dp, keys);
                *dt = cdt;
                *dp = cdp;
            }
            Gesture::Resize {
                press,
                grab_end,
                bounds,
                delta,
            } => {
                // As for Move: a click on a note's edge never resizes it.
                if *delta == 0 && (pos - *press).length() < CLICK_SLOP {
                    return;
                }
                if bounds.is_none() {
                    *bounds = src.notes.selection_bounds();
                }
                let Some(b) = bounds else {
                    return;
                };
                let raw = (f64::from(pos.x - press.x) / ppt).round() as i128;
                let target = (i128::from(*grab_end) + raw).max(0) as u64;
                let snapped = unit.map_or(target, |u| snap_content(target, u, sh, false));
                let want = i128::from(snapped) - i128::from(*grab_end);
                let max = i128::from(u64::MAX) - i128::from(b.max_end);
                *delta = want.clamp(min_resize_delta(b), max);
            }
            Gesture::Marker {
                part,
                press,
                grab,
                start,
                end,
                now,
                moved,
                ..
            } => {
                if (pos.x - press.x).abs() >= CLICK_SLOP {
                    *moved = true;
                }
                if !*moved {
                    return;
                }
                let t = gs.tick_at(pos.x.max(gs.band.left()));
                let delta = i128::from(t) - i128::from(*grab);
                let shift = |v: u64| -> u64 {
                    let x = (i128::from(v) + delta).clamp(0, i128::from(u64::MAX)) as u64;
                    unit.map_or(x, |u| snap_nearest(x, u))
                };
                *now = match (*part, *end) {
                    (MarkerPart::Point, _) => (shift(*start), None),
                    (MarkerPart::Start, Some(e)) => (shift(*start).min(e), Some(e)),
                    (MarkerPart::End, Some(e)) => (*start, Some(shift(e).max(*start))),
                    (MarkerPart::Body, Some(e)) => {
                        let s2 = shift(*start);
                        let len = e - *start;
                        match s2.checked_add(len) {
                            Some(e2) => (s2, Some(e2)),
                            None => *now,
                        }
                    }
                    (_, None) => (shift(*start), None),
                };
            }
            Gesture::Pan { last } => {
                let d = pos - *last;
                *last = pos;
                self.view.time.scroll_pixels(-f64::from(d.x));
                self.user_panned |= d.x != 0.0;
                self.view
                    .scroll_rows(-f64::from(d.y) / self.view.pitch.row_height, h);
            }
            Gesture::HScroll { grab } => {
                let extent = self.view.scroll_extent(content_end, w) as f64;
                let vis = w / ppt;
                let len_frac = (vis / extent).clamp(0.0, 1.0);
                let thumb_len = ((f64::from(g.hscroll.width()) * len_frac) as f32)
                    .max(24.0f32.min(g.hscroll.width()));
                let free = (g.hscroll.width() - thumb_len).max(1.0);
                let frac = f64::from(((pos.x - *grab - g.hscroll.left()) / free).clamp(0.0, 1.0));
                let o = (frac * (1.0 - len_frac) * extent) as u64;
                self.user_panned |= o != self.view.time.origin_tick;
                self.view.time.origin_tick = o;
            }
            Gesture::VScroll { grab } => {
                let rows_vis = h / self.view.pitch.row_height;
                let len_frac = (rows_vis / self.view.pitch.rows()).clamp(0.0, 1.0);
                let thumb_len = ((f64::from(g.vscroll.height()) * len_frac) as f32)
                    .max(24.0f32.min(g.vscroll.height()));
                let free = (g.vscroll.height() - thumb_len).max(1.0);
                let frac = f64::from(((pos.y - *grab - g.vscroll.top()) / free).clamp(0.0, 1.0));
                self.view.pitch.scroll_rows = frac * (self.view.pitch.rows() - rows_vis).max(0.0);
                self.view.pitch.clamp_scroll(h);
            }
            // Followed in `show` (it emits audition actions).
            Gesture::Keys { .. } => {}
        }
    }

    fn finish_gesture(&mut self, g: &Geometry, doc: &PlumDocument, actions: &mut Vec<RollAction>) {
        let gesture = std::mem::replace(&mut self.gesture, Gesture::None);
        match gesture {
            Gesture::Stroke => {
                self.stroke_began = None;
                actions.push(RollAction::Stroke {
                    points: std::mem::take(&mut self.stroke_buf),
                    done: true,
                });
            }
            Gesture::Draw {
                pitch,
                start,
                end,
                dragged,
                ..
            } => {
                let end = if dragged {
                    end
                } else {
                    start.saturating_add(self.new_note_length(doc, start))
                };
                if end > start
                    && let Ok(attrs) = NoteAttrs::default_for_pitch(pitch)
                    && let Ok(attrs) = attrs.with_channel(self.new_note.0)
                {
                    actions.push(RollAction::InsertNote(NoteRecord {
                        start_tick: start,
                        duration_ticks: end - start,
                        attrs,
                        sub: self.new_note_sub,
                        group: 0,
                    }));
                }
            }
            Gesture::BoxSelect { origin, current } if (current - origin).length() >= CLICK_SLOP => {
                let r = Rect::from_two_pos(origin, current);
                let t0 = g.tick_at(r.left());
                let t1 = g.tick_at(r.right());
                let pitch_hi = g.pitch_at_clamped(r.top());
                let pitch_lo = g.pitch_at_clamped(r.bottom());
                actions.push(RollAction::SelectRegion {
                    t0,
                    t1,
                    pitch_lo,
                    pitch_hi,
                });
            }
            Gesture::Move { dt, dp, .. } if dt != 0 || dp != 0 => {
                actions.push(RollAction::MoveSelected { dt, dp });
            }
            Gesture::ScaleBox {
                from,
                warp: Some(warp),
                fill,
                ..
            } if !warp.is_identity() => {
                actions.push(RollAction::WarpBox { from, warp, fill });
            }
            // An identity warp consumes the gesture without ordinary scaling.
            Gesture::ScaleBox { warp: Some(_), .. } => {}
            Gesture::ScaleBox { from, to, fill, .. } if from != to => {
                actions.push(RollAction::ScaleBox { from, to, fill });
            }
            Gesture::Resize { delta, .. } if delta != 0 => {
                actions.push(RollAction::ResizeSelected { delta });
            }
            Gesture::Keys { .. } => actions.push(RollAction::AuditionKey(None)),
            Gesture::Erase { .. } => {
                self.erase_set.clear();
                let hits = std::mem::take(&mut self.erase_hits);
                if !hits.is_empty() {
                    actions.push(RollAction::Erase(hits));
                }
            }
            Gesture::Marker {
                id,
                start,
                end,
                now,
                moved,
                ..
            } => {
                if moved && now != (start, end) {
                    actions.push(RollAction::MoveMarker {
                        id,
                        start: now.0,
                        end: now.1,
                    });
                } else if !moved {
                    // A click goes to the marker.
                    actions.push(RollAction::Seek(start));
                }
            }
            Gesture::Ruler(d) => actions.extend(d.release()),
            _ => {}
        }
    }
}

/// The right-click note menu (v0.8 §156–§157).
fn note_menu_ui(
    ui: &mut egui::Ui,
    menu: &crate::app::note_menu::NoteMenu,
    out: &mut Option<RollAction>,
) {
    use crate::app::commands::Command;
    fn run(
        ui: &mut egui::Ui,
        out: &mut Option<RollAction>,
        enabled: bool,
        label: &str,
        cmd: Command,
    ) {
        let shortcut = cmd.shortcut();
        if ui
            .add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut))
            .clicked()
        {
            *out = Some(RollAction::Run(cmd));
            ui.close();
        }
    }
    let repeat = menu
        .repeat
        .clone()
        .unwrap_or_else(|| "Repeat Last Tool".to_owned());
    run(
        ui,
        out,
        menu.repeat.is_some(),
        &repeat,
        Command::RepeatLastTool,
    );
    run(
        ui,
        out,
        menu.repeat.is_some(),
        "Repeat with New Seed",
        Command::RepeatLastToolNewSeed,
    );
    ui.add_enabled_ui(!menu.recent.is_empty(), |ui| {
        ui.menu_button("Recent Tools", |ui| {
            for (id, name) in &menu.recent {
                if ui.button(*name).clicked() {
                    *out = Some(RollAction::Run(Command::OpenTool(id)));
                    ui.close();
                }
            }
        });
    });
    ui.separator();
    ui.menu_button("Simple", |ui| {
        for cmd in crate::ui::menu::SIMPLE_COMMANDS {
            run(ui, out, menu.can_copy, cmd.label(), cmd);
        }
    });
    for (cat, tools) in crate::app::note_menu::categories() {
        ui.menu_button(*cat, |ui| {
            for (id, name) in tools {
                if ui.button(*name).clicked() {
                    *out = Some(RollAction::Run(Command::OpenTool(id)));
                    ui.close();
                }
            }
        });
    }
    // B1-0047: plugins' custom tools.
    if !menu.custom.is_empty() {
        ui.menu_button("Custom", |ui| {
            for (plugin, name, tools) in &menu.custom {
                ui.label(egui::RichText::new(name.as_str()).small().weak());
                for (id, label, tip) in tools {
                    let mut r = ui.button(label.as_str());
                    if !tip.is_empty() {
                        r = r.on_hover_text(tip.as_str());
                    }
                    if r.clicked() {
                        *out = Some(RollAction::CustomTool(plugin.clone(), id.clone()));
                        ui.close();
                    }
                }
            }
        });
    }
    ui.separator();
    ui.menu_button("Mute", |ui| {
        run(ui, out, menu.can_mute, "Mute Notes", Command::MuteNotes);
        run(
            ui,
            out,
            menu.can_unmute,
            "Unmute Notes",
            Command::UnmuteNotes,
        );
    });
    run(ui, out, menu.can_slice, "Slice Notes", Command::SliceNotes);
    ui.menu_button("Duplicate One Octave", |ui| {
        run(
            ui,
            out,
            menu.can_slice,
            "Duplicate Up One Octave",
            Command::DuplicateOctaveUp,
        );
        run(
            ui,
            out,
            menu.can_slice,
            "Duplicate Down One Octave",
            Command::DuplicateOctaveDown,
        );
    });
    ui.menu_button("Delete", |ui| {
        run(ui, out, menu.can_copy, "Delete Notes", Command::Delete);
        if ui
            .button("Delete Tool (rules)…")
            .on_hover_text("Delete by rules (every n-th, pitch range, velocity, …)")
            .clicked()
        {
            *out = Some(RollAction::Run(Command::OpenTool("delete")));
            ui.close();
        }
    });
    ui.separator();
    run(ui, out, menu.can_copy, "Copy", Command::Copy);
    run(ui, out, menu.can_copy, "Cut (Copy + Delete)", Command::Cut);
}

#[cfg(test)]
mod tests {
    /// B1-0088: a roll under 64 points tall cannot give 128 keys half a
    /// point each: Fit 128 Keys keeps half a point per row and centers the
    /// range (keys 60 – 67 in the middle); a roll exactly 128 points tall
    /// gets one point per key.
    #[test]
    fn fit_keys_in_a_tiny_roll_keeps_half_a_point_and_centers() {
        use plumspirit_engine::view::MIN_ROW_HEIGHT;
        let mut r = PianoRoll {
            notes_size: Vec2::new(800.0, 40.0),
            ..PianoRoll::default()
        };
        r.fit_keys(0, 127);
        assert_eq!(r.view.pitch.row_height, MIN_ROW_HEIGHT);
        let mid = r.view.pitch.y_to_pitch(20.0).unwrap();
        assert!((60..=67).contains(&mid), "{mid}");
        r.notes_size = Vec2::new(800.0, 128.0);
        r.fit_keys(0, 127);
        assert_eq!(r.view.pitch.row_height, 1.0);
        assert_eq!(r.view.pitch.scroll_rows, 0.0);
    }

    use super::*;

    /// V09-S10 (CONTEXT_MATRIX.md fix 8): Delete Notes, Copy and Cut are
    /// offered only with notes selected (they were enabled and did nothing).
    #[test]
    fn the_note_menus_delete_copy_and_cut_need_a_selection() {
        use crate::app::commands::Command;
        use crate::app::note_menu::NoteMenu;
        for (can, want) in [(false, None), (true, Some(RollAction::Run(Command::Copy)))] {
            let menu = NoteMenu {
                can_copy: can,
                ..Default::default()
            };
            let build = |ui: &mut egui::Ui| {
                let mut out = None;
                note_menu_ui(ui, &menu, &mut out);
                out
            };
            let (got, labels) = crate::ui::testkit::run_menu(&build, Some("Copy"));
            assert_eq!(got, want, "can_copy {can}");
            assert!(labels.iter().any(|l| l == "Cut (Copy + Delete)"));
        }
    }

    #[test]
    fn geometry_regions_do_not_overlap() {
        let g = Geometry::new(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0)),
            RollView::default(),
            1.0,
        );
        assert_eq!(g.notes.left(), KEYS_WIDTH);
        assert_eq!(g.notes.top(), RULER_HEIGHT + markers::BAND_HEIGHT);
        for r in [g.band, g.ruler, g.keys, g.vscroll, g.hscroll] {
            assert!(r.intersect(g.notes).area() <= 0.0);
        }
        assert!(g.band.intersect(g.ruler).area() <= 0.0);
        // Band, ruler and notes share the same horizontal extent (alignment).
        assert_eq!(g.band.x_range(), g.notes.x_range());
        assert_eq!(g.ruler.x_range(), g.notes.x_range());
    }

    /// V09-S11 (`docs/CLIP_PIANO_ROLL.md` Decision 2): the song geometry is
    /// the view moved by the shift; left of song tick 0 its rectangles start
    /// where tick 0 is drawn; shift 0 is the geometry itself.
    #[test]
    fn the_song_geometry_moves_the_origin_or_the_rectangles() {
        let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 500.0));
        let view = RollView {
            time: plumspirit_engine::view::TimeAxis::new(1_000, 0.5),
            ..RollView::default()
        };
        let g = Geometry::new(full, view, 1.0);
        let same = g.song(0);
        assert_eq!(
            (
                same.view.time.origin_tick,
                same.notes,
                same.ruler,
                same.band
            ),
            (g.view.time.origin_tick, g.notes, g.ruler, g.band)
        );
        let s = g.song(4_000);
        assert_eq!(s.view.time.origin_tick, 5_000);
        assert_eq!((s.notes, s.ruler, s.band), (g.notes, g.ruler, g.band));
        // Content tick c and song tick c + shift land on the same x.
        for c in [1_000u64, 1_234, 9_999] {
            let xc = g.notes.left() + g.view.time.tick_to_x(c) as f32;
            let xs = s.notes.left() + s.view.time.tick_to_x(c + 4_000) as f32;
            assert!((xc - xs).abs() < 1e-3, "{c}");
        }
        // shift −1,200 with origin 1,000: song 0 is content 1,200, 100 px in.
        let n = g.song(-1_200);
        assert_eq!(n.view.time.origin_tick, 0);
        assert!((n.notes.left() - (g.notes.left() + 100.0)).abs() < 1e-3);
        assert_eq!(n.ruler.left(), n.notes.left());
        assert_eq!(n.notes.right(), g.notes.right());
        let xc = g.notes.left() + g.view.time.tick_to_x(3_000) as f32;
        let xs = n.notes.left() + n.view.time.tick_to_x(1_800) as f32;
        assert!((xc - xs).abs() < 1e-3);
        // Song time entirely right of the view: an empty strip at the right.
        let far = g.song(-10_000_000);
        assert_eq!(far.notes.width(), 0.0);
    }

    #[test]
    fn content_ticks_snap_to_the_song_grid() {
        for c in [0u64, 1, 119, 120, 121, 1_000, 99_999] {
            for u in [1u64, 240, 960] {
                assert_eq!(snap_content(c, u, 0, false), snap_nearest(c, u));
                assert_eq!(snap_content(c, u, 0, true), snap_floor(c, u));
                // A shift on the grid snaps like the content's own ticks.
                assert_eq!(snap_content(c, u, 4 * 960, false), snap_nearest(c, u));
                for shift in [100i128, -100, 9_700, -3_333] {
                    for floor in [false, true] {
                        let r = snap_content(c, u, shift, floor);
                        let song = i128::from(r) + shift;
                        if r > 0 {
                            assert_eq!(song.rem_euclid(i128::from(u)), 0, "{c} {u} {shift}");
                        }
                        let dist = (song - (i128::from(c) + shift)).abs();
                        if r > 0 {
                            assert!(dist <= i128::from(u), "{c} {u} {shift}");
                        }
                        if floor && r > 0 {
                            assert!(r <= c);
                        }
                    }
                }
            }
        }
    }

    /// The roll's input for the session tests.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn session_input<'a>(
        doc: &'a PlumDocument,
        meter: &'a MeterMap,
        track: usize,
        content: Option<plumspirit_engine::model::ContentId>,
        shift: i128,
    ) -> RollInput<'a> {
        RollInput {
            doc,
            track_index: track,
            content,
            shift,
            meter,
            palette: &crate::ui::theme::DARK,
            dark: true,
            editable: true,
            keyboard: false,
            onion: &[],
            onion_alpha: 255,
            onion_outlines: Some(75),
            capture: None,
            stroke_overlay: &[],
            playhead: None,
            conductor_active: false,
            new_note_channel: 0,
            new_note_gate: DefaultGate::Grid,
            new_note_sub: Default::default(),
            preview: None,
            tool_preview: None,
            added: None,
            loop_range: None,
            loop_enabled: false,
            corrections_note: None,
            range: None,
            key_colors: None,
            key_guide: false,
            audition: false,
            note_menu: None,
            overlay_controls: [None; 3],
            title: None,
            locked: None,
            parts: &[],
        }
    }

    /// B1-0059: a new view (new document generation) and Reset View start at
    /// the default zoom the app copies from Settings; zooming in between is
    /// the user's and is replaced only by those two.
    #[test]
    fn a_new_view_and_reset_view_start_at_the_default_zoom() {
        use plumspirit_engine::model::Bpm;
        let doc = PlumDocument::new_project("zoom".into(), Bpm::DEFAULT, 960);
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let run = |roll: &mut PianoRoll, generation: u64| {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                roll.show(ui, session_input(&doc, &meter, 0, None, 0), generation);
            });
            o.textures_delta.clear();
        };
        let reference = viewport::reference_ppt(doc.division());
        let mut roll = PianoRoll::default();
        run(&mut roll, 1);
        assert!((roll.view.time.pixels_per_tick - reference).abs() < 1e-12);
        assert_eq!(roll.view.pitch.row_height, viewport::DEFAULT_ROW_HEIGHT);
        roll.default_zoom = (250.0, 50.0);
        run(&mut roll, 1);
        assert!(
            (roll.view.time.pixels_per_tick - reference).abs() < 1e-12,
            "the open view keeps its zoom"
        );
        run(&mut roll, 2);
        assert!((roll.view.time.pixels_per_tick - reference * 2.5).abs() < 1e-12);
        assert_eq!(roll.view.pitch.row_height, 6.0);
        roll.view.time.pixels_per_tick = reference * 9.0;
        roll.request_reset_view();
        run(&mut roll, 2);
        assert!((roll.view.time.pixels_per_tick - reference * 2.5).abs() < 1e-12);
        assert_eq!(roll.view.pitch.row_height, 6.0);
    }

    /// V09-S11 step 3a: an opened clip's owned content is drawn, hit and
    /// edited in content ticks while the ruler works in song time, at the
    /// four display scales. The content is track 1's notes copied to song
    /// tick 9,700 (shift 9,700, off the sixteenth grid) plus a note only it
    /// has (content tick 4,800, key 64).
    #[test]
    fn an_opened_clip_is_edited_in_content_ticks_with_the_ruler_in_song_time() {
        use plumspirit_engine::editor::NoteTarget;
        use plumspirit_engine::editor::clips::duplicate_clips;
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::{Bpm, ClipId, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("session".into(), Bpm::DEFAULT, 960);
        let a = |p| NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(960, 960, a(60))], false)
            .unwrap();
        let mut h = History::default();
        let t0 = doc.track(0).unwrap().id();
        let op = duplicate_clips(&doc, &[(ClipId::Primary(t0), 0, 9_700)], false).unwrap();
        h.apply(&mut doc, op).unwrap();
        let c = doc.clips().contents()[0].id;
        let target = NoteTarget::Content {
            host: 0,
            content: c,
        };
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: target,
                record: NoteRecord::new(4_800, 480, a(64)),
            },
        )
        .unwrap();
        plumspirit_engine::editor::notes_mut(&mut doc, target)
            .unwrap()
            .clear_selection();
        let shift = 9_700i128;
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let full = std::cell::Cell::new(Rect::NOTHING);
            type Content = Option<plumspirit_engine::model::ContentId>;
            let run = |roll: &mut PianoRoll, content: Content, events: Vec<egui::Event>| {
                let mut out = Vec::new();
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                    events,
                    ..Default::default()
                };
                let mut o = ctx.run_ui(raw, |ui| {
                    full.set(ui.available_rect_before_wrap());
                    let sh = if content.is_some() { shift } else { 0 };
                    out = roll.show(ui, session_input(&doc, &meter, 0, content, sh), 1);
                });
                o.textures_delta.clear();
                out
            };
            let mut roll = PianoRoll::default();
            run(&mut roll, Some(c), Vec::new());
            roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.1);
            roll.view.pitch.scroll_rows = 127.0 - 72.0;
            roll.view.pitch.row_height = 12.0;
            let g = Geometry::new(full.get(), roll.view, scale);
            let click = |roll: &mut PianoRoll, content: Content, pos: Pos2| {
                let b = |pressed| egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                let mut v = run(roll, content, vec![egui::Event::PointerMoved(pos)]);
                v.extend(run(roll, content, vec![b(true)]));
                v.extend(run(roll, content, vec![b(false)]));
                v.extend(run(roll, content, Vec::new()));
                v
            };
            // The content's own note (content tick 4,800, key 64) is hit…
            let on_note = note_rect(&g, 4_800, 5_280, 64).center();
            let acts = click(&mut roll, Some(c), on_note);
            assert!(
                acts.iter().any(|x| matches!(x, RollAction::SelectOnly(_))),
                "{scale}: {acts:?}"
            );
            // …where the track's own notes have none.
            let acts = click(&mut PianoRoll::default(), None, on_note);
            assert!(
                !acts.iter().any(|x| matches!(x, RollAction::SelectOnly(_))),
                "{scale}: {acts:?}"
            );
            // The pointer reports content and song ticks.
            run(&mut roll, Some(c), vec![egui::Event::PointerMoved(on_note)]);
            let (t, s) = (roll.pointer.tick.unwrap(), roll.pointer.song_tick.unwrap());
            assert_eq!(i128::from(s) - i128::from(t), shift, "{scale}");
            // A ruler click at content tick 960 seeks the song to 960 + shift
            // (snapped to the song's sixteenth grid: 10,660 → 10,560).
            let x = g.ruler.left() + roll.view.time.tick_to_x(960) as f32;
            let acts = click(&mut roll, Some(c), Pos2::new(x, g.ruler.center().y));
            assert!(
                acts.contains(&RollAction::Seek(10_560)),
                "{scale}: {acts:?}"
            );
            // Drawing at content tick 2,000 starts on the song's grid: song
            // 11,700 floors to 11,520, content 1,820.
            roll.tool = Tool::Draw;
            let pos = Pos2::new(
                g.notes.left() + roll.view.time.tick_to_x(2_000) as f32,
                note_rect(&g, 2_000, 2_100, 70).center().y,
            );
            let acts = click(&mut roll, Some(c), pos);
            let drawn: Vec<u64> = acts
                .iter()
                .filter_map(|x| match x {
                    RollAction::InsertNote(r) => Some(r.start_tick),
                    _ => None,
                })
                .collect();
            assert_eq!(drawn, vec![1_820], "{scale}: {acts:?}");
        }
    }

    /// v1.0-beta1 B1-0151 (PLAYTEST_ISSUES row 6): a Hypernova-like burst
    /// (the active track and six Onion tracks dense in bars 20–24) entering
    /// the roll while panning a sixth of a bar at a time: at every step the
    /// roll's own frames make every visible tile of the track and the Onion
    /// ready (the worker builds; the frames receive) — nothing stays
    /// unrendered until a bar is fully on screen.
    #[test]
    fn a_dense_burst_renders_in_the_roll_at_every_pan_step() {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let bar = 960 * 4u64;
        let mut doc = PlumDocument::new_project("burst".into(), Bpm::DEFAULT, 960);
        for t in 0..7usize {
            if t > 0 {
                History::default()
                    .apply(&mut doc, Operation::AddTrack)
                    .unwrap();
            }
            let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ t as u64;
            let mut next = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            // PLUMSPIRIT_BURST_SCALE=10 (release): a 10.2 M-note burst.
            let scale: usize = std::env::var("PLUMSPIRIT_BURST_SCALE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let n = scale * if t == 0 { 300_000 } else { 120_000 };
            let mut recs: Vec<NoteRecord> = (0..n)
                .map(|_| {
                    let a = NoteAttrs::new((next() % 128) as u8, 100, 0, 0, NoteOffKind::NoteOff)
                        .unwrap();
                    NoteRecord::new(20 * bar + next() % (4 * bar), 1 + next() % 120, a)
                })
                .collect();
            recs.sort_by_key(|r| r.start_tick);
            doc.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let scale_report = std::env::var("PLUMSPIRIT_BURST_SCALE").is_ok();
        pan_until_ready(&doc, 0, 17 * bar, bar, 30, scale_report);
    }

    /// Pans the roll over `doc` (track `active` edited, every other track an
    /// Onion source) two bars wide from `from`, a sixth of a `bar` per step,
    /// and runs the roll's own frames at each step until every visible tile
    /// of the track and the Onion is ready (B1-0151); `report` prints each
    /// step's wait.
    fn pan_until_ready(
        doc: &PlumDocument,
        active: usize,
        from: u64,
        bar: u64,
        steps: u64,
        report: bool,
    ) {
        pan_wide(doc, active, from, bar, 2, steps, report, false);
    }

    /// One roll frame of `doc` (track `active`, the others as Onion) at
    /// 1280 × 700 points.
    fn roll_frame(
        ctx: &egui::Context,
        roll: &mut PianoRoll,
        doc: &PlumDocument,
        active: usize,
        meter: &MeterMap,
        sources: &[render::OnionSource<'_>],
    ) {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
            ..Default::default()
        };
        let mut o = ctx.run_ui(raw, |ui| {
            roll.show(
                ui,
                RollInput {
                    doc,
                    track_index: active,
                    meter,
                    palette: &crate::ui::theme::DARK,
                    dark: true,
                    editable: true,
                    keyboard: false,
                    onion: sources,
                    onion_alpha: 90,
                    onion_outlines: Some(75),
                    capture: None,
                    stroke_overlay: &[],
                    playhead: None,
                    conductor_active: false,
                    new_note_channel: 0,
                    new_note_gate: DefaultGate::Grid,
                    new_note_sub: Default::default(),
                    preview: None,
                    tool_preview: None,
                    added: None,
                    loop_range: None,
                    loop_enabled: false,
                    corrections_note: None,
                    range: None,
                    key_colors: None,
                    key_guide: false,
                    audition: false,
                    note_menu: None,
                    overlay_controls: [None; 3],
                    content: None,
                    shift: 0,
                    title: None,
                    locked: None,
                    parts: &[],
                },
                1,
            );
        });
        o.textures_delta.clear();
    }

    /// Runs `roll`'s frames until every visible tile of the track and the
    /// Onion is ready; the wait and the frames it took.
    fn frames_until_ready(
        ctx: &egui::Context,
        roll: &mut PianoRoll,
        doc: &PlumDocument,
        active: usize,
        meter: &MeterMap,
        sources: &[render::OnionSource<'_>],
        label: &str,
    ) -> (std::time::Duration, u32) {
        let t = std::time::Instant::now();
        let mut frames = 0;
        loop {
            roll_frame(ctx, roll, doc, active, meter, sources);
            frames += 1;
            let st = &roll.stats;
            let done = |d: &dense::DenseStats| d.ready == d.visible_tiles && d.pending == 0;
            if (!st.lod || done(&st.dense))
                && (st.onion_dense.visible_tiles == 0 || done(&st.onion_dense))
            {
                return (t.elapsed(), frames);
            }
            assert!(
                t.elapsed() < std::time::Duration::from_secs(120),
                "{label}: never ready: track {:?} onion {:?}",
                st.dense,
                st.onion_dense
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// [`pan_until_ready`] with the view `wide` bars across, panned a sixth
    /// of the view per step. `check`: at every step the picture (the content
    /// hashes of the drawn tiles of the track and the Onion) must equal the
    /// picture a fresh roll draws at the same view — what panning reuses or
    /// builds is never stale, partial or missing (B1-0151).
    #[allow(clippy::too_many_arguments)]
    fn pan_wide(
        doc: &PlumDocument,
        active: usize,
        from: u64,
        bar: u64,
        wide: u64,
        steps: u64,
        report: bool,
        check: bool,
    ) -> std::time::Duration {
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let others: Vec<usize> = (0..doc.tracks().len()).filter(|&t| t != active).collect();
        let ghosts: Vec<_> = others
            .iter()
            .map(|&t| doc.track(t).unwrap().notes.clone())
            .collect();
        let sources: Vec<render::OnionSource<'_>> = ghosts
            .iter()
            .zip(&others)
            .map(|(s, &t)| render::OnionSource {
                id: doc.track(t).unwrap().id(),
                store: s,
                color: None,
                place: plumspirit_engine::view::tiles::SourcePlace::IDENTITY,
            })
            .collect();
        roll_frame(&ctx, &mut roll, doc, active, &meter, &sources);
        let width = f64::from(roll.notes_rect().unwrap().width());
        let span = wide * bar;
        let mut slowest = std::time::Duration::ZERO;
        for step in 0..steps {
            let view =
                plumspirit_engine::view::TimeAxis::new(from + step * span / 6, width / span as f64);
            roll.view.time = view;
            let (wait, frames) = frames_until_ready(
                &ctx,
                &mut roll,
                doc,
                active,
                &meter,
                &sources,
                &format!("{wide} bars, step {step}"),
            );
            slowest = slowest.max(wait);
            let st = roll.stats;
            if report {
                println!(
                    "{wide} bars, step {step}: ready after {wait:?} ({frames} frames), track {}/{} tiles (lod {} level {}), onion {}/{} (level {}); estimates {} / {}, view {:?}",
                    st.dense.ready,
                    st.dense.visible_tiles,
                    st.lod,
                    st.dense.level,
                    st.onion_dense.ready,
                    st.onion_dense.visible_tiles,
                    st.onion_dense.level,
                    st.visible_estimate,
                    st.onion_estimate,
                    roll.view.time.visible_range(width)
                );
            }
            if check {
                let fresh_ctx = egui::Context::default();
                let mut fresh = PianoRoll::default();
                roll_frame(&fresh_ctx, &mut fresh, doc, active, &meter, &sources);
                fresh.view.time = view;
                frames_until_ready(
                    &fresh_ctx,
                    &mut fresh,
                    doc,
                    active,
                    &meter,
                    &sources,
                    &format!("fresh {wide} bars, step {step}"),
                );
                let f = &fresh.stats;
                assert_eq!(
                    (
                        st.lod,
                        st.dense.level,
                        st.dense.visible_tiles,
                        st.dense.picture
                    ),
                    (f.lod, f.dense.level, f.dense.visible_tiles, f.dense.picture),
                    "{wide} bars, step {step}: the panned roll's track tiles differ from a fresh roll's"
                );
                assert_eq!(
                    (
                        st.onion_dense.level,
                        st.onion_dense.visible_tiles,
                        st.onion_dense.picture
                    ),
                    (
                        f.onion_dense.level,
                        f.onion_dense.visible_tiles,
                        f.onion_dense.picture
                    ),
                    "{wide} bars, step {step}: the panned roll's Onion tiles differ from a fresh roll's"
                );
            }
        }
        slowest
    }

    /// B1-0151 on a real file: `PLUMSPIRIT_ROLL_MIDI=song.mid cargo test
    /// --release -p plumspirit real_dense_file -- --ignored --nocapture`.
    /// The densest bar (most notes starting) and its densest track are found;
    /// the roll pans from two bars before it with every other track as Onion.
    #[test]
    #[ignore = "needs a large MIDI file (PLUMSPIRIT_ROLL_MIDI)"]
    fn a_real_dense_file_renders_in_the_roll_at_every_pan_step() {
        let Ok(path) = std::env::var("PLUMSPIRIT_ROLL_MIDI") else {
            println!("PLUMSPIRIT_ROLL_MIDI is not set");
            return;
        };
        let t = std::time::Instant::now();
        let (doc, _) =
            plumspirit_engine::midi::import_file(std::path::Path::new(&path), Default::default())
                .unwrap();
        // Metrical files (the encoding is the PPQN).
        let bar = u64::from(doc.division().encoded()) * 4;
        let mut per_bar: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        for tr in doc.tracks() {
            for v in tr.notes.iter() {
                *per_bar.entry(v.start_tick / bar).or_default() += 1;
            }
        }
        let (&densest, &count) = per_bar
            .iter()
            .max_by_key(|(b, n)| (**n, std::cmp::Reverse(**b)))
            .unwrap();
        let active = (0..doc.tracks().len())
            .max_by_key(|&i| {
                doc.tracks()[i]
                    .notes
                    .estimate_in_range(densest * bar, densest * bar + bar)
            })
            .unwrap();
        println!(
            "{path}: {} notes in {} tracks, imported in {:?}; densest bar {densest} ({count} notes start there), active track {active}",
            doc.tracks().iter().map(|t| t.notes.len()).sum::<usize>(),
            doc.tracks().len(),
            t.elapsed()
        );
        pan_until_ready(&doc, active, densest.saturating_sub(2) * bar, bar, 30, true);
        // Wider views (the community report: "a good 10M notes on screen"),
        // each picture checked against a fresh roll's.
        let widths: Vec<u64> = std::env::var("PLUMSPIRIT_ROLL_WIDTHS")
            .ok()
            .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_else(|| vec![4, 8, 16, 32]);
        for wide in widths {
            let from = (densest * bar).saturating_sub(wide * bar);
            let slowest = pan_wide(&doc, active, from, bar, wide, 13, true, true);
            println!("{wide} bars: slowest step {slowest:?}");
        }
    }

    /// Pans the roll continuously over `doc` (as a mouse drag does): every
    /// frame moves the view `1 / per_view` of its width and lasts at least
    /// 16 ms, the Onion ordered by `order` over the visible range exactly as
    /// the app orders it (`app::hierarchy::OnionOrderCache`: re-ranked when
    /// the view moved 1/64 of its width, `OnionRanking`'s hysteresis).
    /// Returns (frames, frames with a part of the view only a placeholder,
    /// Onion family changes).
    #[allow(clippy::too_many_arguments)]
    fn pan_continuously(
        doc: &PlumDocument,
        active: usize,
        frames: u64,
        view_at: impl Fn(u64) -> (u64, u64),
        order: plumspirit_engine::model::project::OnionOrder,
    ) -> (u64, u64, u64, f32, f32) {
        // Returns (frames before the first frame without a placeholder — the
        // initial load —, frames with a placeholder after it, Onion family
        // changes, the mean and the largest share of the view under the
        // placeholder after the first half second — the cold start).
        use plumspirit_engine::model::project::DensityBasis;
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let others: Vec<usize> = (0..doc.tracks().len()).filter(|&t| t != active).collect();
        // The app's Onion order (`OnionOrderCache`: hysteresis, held while
        // the view moves).
        let cfg = plumspirit_engine::model::OnionConfig {
            order,
            density: DensityBasis::VisibleRange,
            ..Default::default()
        };
        let mut cache = crate::app::hierarchy::OnionOrderCache::default();
        let mut ordered: Vec<usize> = others.clone();
        let sources_of = |ord: &[usize]| -> Vec<render::OnionSource<'_>> {
            ord.iter()
                .map(|&t| render::OnionSource {
                    id: doc.track(t).unwrap().id(),
                    store: &doc.track(t).unwrap().notes,
                    color: None,
                    place: plumspirit_engine::view::tiles::SourcePlace::IDENTITY,
                })
                .collect()
        };
        roll_frame(&ctx, &mut roll, doc, active, &meter, &sources_of(&ordered));
        let width = f64::from(roll.notes_rect().unwrap().width());
        let (mut gaps, mut changes) = (0u64, 0u64);
        let mut first: Option<u64> = None;
        const COLD_FRAMES: u64 = 30;
        let (mut share_sum, mut share_max) = (0.0f32, 0.0f32);
        let mut last_family = None;
        let report_gaps = std::env::var("PLUMSPIRIT_ROLL_GAPS").is_ok();
        let trace = std::env::var("PLUMSPIRIT_ROLL_TRACE").is_ok();
        let mut marks: Vec<u64> = Vec::new();
        for f in 0..frames {
            let t = std::time::Instant::now();
            let (origin, span) = view_at(f);
            roll.view.time = plumspirit_engine::view::TimeAxis::new(origin, width / span as f64);
            let (t0, t1) = (origin, origin + span);
            ordered = cache.order(doc, &others, &cfg, (t0, t1), None);
            let sources = sources_of(&ordered);
            roll_frame(&ctx, &mut roll, doc, active, &meter, &sources);
            let st = &roll.stats;
            // The renderer's generation counts family changes (the track's
            // family is fixed while panning: its notes do not change).
            let fam = st.onion_dense.generation.max(st.dense.generation);
            if last_family.is_some_and(|l| l != fam) {
                changes += 1;
            }
            last_family = Some(fam);
            if trace {
                println!(
                    "      {f:3} span {span:8} level {:3}/{:3} track r{} s{} c{} {:.0}% onion r{} s{} c{} {:.0}% gen {} pend {} cached {}",
                    st.dense.level,
                    st.onion_dense.level,
                    st.dense.ready,
                    st.dense.stand_ins,
                    st.dense.coarse,
                    st.dense.placeholder * 100.0,
                    st.onion_dense.ready,
                    st.onion_dense.stand_ins,
                    st.onion_dense.coarse,
                    st.onion_dense.placeholder * 100.0,
                    st.onion_dense.generation,
                    st.onion_dense.pending,
                    st.onion_dense.cached
                );
            }
            if f >= COLD_FRAMES {
                let share = st.dense.placeholder.max(st.onion_dense.placeholder);
                share_sum += share;
                share_max = share_max.max(share);
            }
            if st.dense.coarse > 0 || st.onion_dense.coarse > 0 {
                if first.is_some() {
                    gaps += 1;
                }
                if report_gaps {
                    marks.push(f);
                }
            } else if first.is_none() {
                first = Some(f);
            }
            let spent = t.elapsed();
            if spent < std::time::Duration::from_millis(16) {
                std::thread::sleep(std::time::Duration::from_millis(16) - spent);
            }
        }
        if report_gaps {
            // Runs of frames with a placeholder (first-last).
            let mut runs: Vec<String> = Vec::new();
            let mut i = 0;
            while i < marks.len() {
                let mut j = i;
                while j + 1 < marks.len() && marks[j + 1] == marks[j] + 1 {
                    j += 1;
                }
                runs.push(format!("{}-{}", marks[i], marks[j]));
                i = j + 1;
            }
            println!("    placeholder frames: {}", runs.join(" "));
        }
        let load = first.unwrap_or(frames);
        let mean = share_sum / frames.saturating_sub(COLD_FRAMES).max(1) as f32;
        (load, gaps, changes, mean, share_max)
    }

    /// B1-0151 on a real file, panned continuously across its densest
    /// region (`PLUMSPIRIT_ROLL_MIDI`, as `a_real_dense_file_renders_…`):
    /// Project order and Dense First ghosts, 8 and 32 bars across.
    #[test]
    #[ignore = "needs a large MIDI file (PLUMSPIRIT_ROLL_MIDI)"]
    fn a_real_dense_file_pans_continuously_without_missing_regions() {
        use plumspirit_engine::model::project::OnionOrder;
        let Ok(path) = std::env::var("PLUMSPIRIT_ROLL_MIDI") else {
            println!("PLUMSPIRIT_ROLL_MIDI is not set");
            return;
        };
        let (doc, _) =
            plumspirit_engine::midi::import_file(std::path::Path::new(&path), Default::default())
                .unwrap();
        let bar = u64::from(doc.division().encoded()) * 4;
        let mut per_bar: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        for tr in doc.tracks() {
            for v in tr.notes.iter() {
                *per_bar.entry(v.start_tick / bar).or_default() += 1;
            }
        }
        let (&densest, _) = per_bar
            .iter()
            .max_by_key(|(b, n)| (**n, std::cmp::Reverse(**b)))
            .unwrap();
        let active = (0..doc.tracks().len())
            .max_by_key(|&i| {
                doc.tracks()[i]
                    .notes
                    .estimate_in_range(densest * bar, densest * bar + bar)
            })
            .unwrap();
        let mut worst = 0;
        // PLUMSPIRIT_ROLL_CASE: only the cases whose label contains it
        // ("DenseFirst zoom", "Project pan 32", …).
        let case = std::env::var("PLUMSPIRIT_ROLL_CASE").unwrap_or_default();
        for order in [OnionOrder::Project, OnionOrder::DenseFirst] {
            for wide in [8u64, 32] {
                if !format!("{order:?} pan {wide}").contains(&case) {
                    continue;
                }
                let span = wide * bar;
                let from = (densest * bar).saturating_sub(span);
                // Two views' worth of panning, three seconds per view.
                let view = |f: u64| (from + f * span / 180, span);
                let (load, gaps, changes, mean, max) =
                    pan_continuously(&doc, active, 360, view, order);
                println!(
                    "{order:?}, {wide} bars: first full frame after {load} frames, then {gaps} of {} frames with a placeholder (after 0.5 s, view share under it: mean {:.1} %, max {:.1} %), {changes} Onion family changes",
                    360 - load,
                    mean * 100.0,
                    max * 100.0
                );
                worst = worst.max(gaps);
            }
            // Zoom out from 4 to 64 bars around the densest bar and back in
            // (a smooth wheel zoom, 1.5 % per frame). Zooming out uncovers
            // ticks no level has tiles for yet: the placeholder band shows
            // there until they arrive (counted, informative), never an empty
            // column.
            if !format!("{order:?} zoom").contains(&case) {
                continue;
            }
            let centre = densest * bar + bar / 2;
            let zoom = |f: u64| {
                let k = if f < 190 { f } else { 380 - f } as f64;
                let span = ((4 * bar) as f64 * 1.015f64.powf(k)) as u64;
                (centre.saturating_sub(span / 2), span)
            };
            let (load, gaps, changes, mean, max) = pan_continuously(&doc, active, 380, zoom, order);
            println!(
                "{order:?}, zoom 4 - 64 bars and back: first full frame after {load} frames, then {gaps} of {} frames with a placeholder (after 0.5 s, view share under it: mean {:.1} %, max {:.1} %), {changes} Onion family changes",
                380 - load,
                mean * 100.0,
                max * 100.0
            );
        }
        println!("worst pan: {worst} frames with a placeholder");
    }

    /// v1.0-beta1 B1-0155 (contract §022-A): the selection and the playhead
    /// come after the primary fills and outlines — in the frame's shape
    /// order the unselected notes' mesh, then the selected notes' mesh (its
    /// own Selection Opacity colours), then the playhead line, both in exact
    /// and in dense mode (where the tiles put the selection in front of their
    /// texels, `dense_texels_show_the_note_drawn_last_with_the_selection_in_front`).
    #[test]
    fn the_selection_and_the_playhead_are_drawn_after_the_notes() {
        use plumspirit_engine::model::{Bpm, NoteLoc, NoteOffKind, NoteRecord};
        for dense_notes in [0usize, 30_000] {
            let mut doc = PlumDocument::new_project("order".into(), Bpm::DEFAULT, 960);
            let a = |p| NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
            let mut recs = vec![
                NoteRecord::new(960, 960, a(60)),
                NoteRecord::new(1_200, 480, a(60)),
            ];
            recs.extend(
                (0..dense_notes as u64).map(|k| NoteRecord::new(k % 3_000, 40, a((k % 100) as u8))),
            );
            recs.sort_by_key(|r| r.start_tick);
            let notes = &mut doc.track_mut(0).unwrap().notes;
            notes.insert_sorted_batch(&recs, false).unwrap();
            let mut sel = None;
            notes.query(1_200, 1_200, |loc: NoteLoc, v| {
                if v.start_tick == 1_200 {
                    sel = Some(loc);
                }
            });
            notes.set_selected(sel.unwrap(), true);
            let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
            let ctx = egui::Context::default();
            let mut roll = PianoRoll::default();
            roll.capture_view(true);
            roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.3);
            let mut shapes = Vec::new();
            for _ in 0..200 {
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                    ..Default::default()
                };
                let mut o = ctx.run_ui(raw, |ui| {
                    roll.show(
                        ui,
                        RollInput {
                            doc: &doc,
                            track_index: 0,
                            meter: &meter,
                            palette: &crate::ui::theme::DARK,
                            dark: true,
                            editable: true,
                            keyboard: false,
                            onion: &[],
                            onion_alpha: 90,
                            onion_outlines: Some(75),
                            capture: None,
                            stroke_overlay: &[],
                            playhead: Some(1_500),
                            conductor_active: false,
                            new_note_channel: 0,
                            new_note_gate: DefaultGate::Grid,
                            new_note_sub: Default::default(),
                            preview: None,
                            tool_preview: None,
                            added: None,
                            loop_range: None,
                            loop_enabled: false,
                            corrections_note: None,
                            range: None,
                            key_colors: None,
                            key_guide: false,
                            audition: false,
                            note_menu: None,
                            overlay_controls: [None; 3],
                            content: None,
                            shift: 0,
                            title: None,
                            locked: None,
                            parts: &[],
                        },
                        1,
                    );
                    let (painter, slot) = roll.take_overlay_layer().expect("native reserved layer");
                    let rect = roll.notes_rect().unwrap();
                    painter.set(
                        slot,
                        egui::Shape::line_segment(
                            [rect.left_center(), rect.right_center()],
                            egui::Stroke::new(3.0, egui::Color32::from_rgb(7, 19, 43)),
                        ),
                    );
                });
                o.textures_delta.clear();
                shapes = o.shapes;
                let st = &roll.stats;
                if !st.lod || (st.dense.ready == st.dense.visible_tiles && st.dense.pending == 0) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(roll.stats.lod, dense_notes > 0);
            let playhead = shapes
                .iter()
                .rposition(|c| {
                    matches!(&c.shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == crate::ui::theme::DARK.playhead)
                })
                .expect("the playhead line");
            let overlay = shapes.iter().position(|c| matches!(&c.shape,egui::Shape::LineSegment{stroke,..} if stroke.color==egui::Color32::from_rgb(7,19,43))).expect("native reserved overlay is filled");
            assert!(
                overlay < playhead,
                "reserved overlay remains below playhead"
            );
            // Every note mesh (exact meshes or tile meshes) comes before it.
            let meshes: Vec<usize> = shapes
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(&c.shape, egui::Shape::Mesh(m) if !m.vertices.is_empty()))
                .map(|(i, _)| i)
                .collect();
            assert!(!meshes.is_empty());
            let notes_area = roll.notes_rect().unwrap();
            for &i in &meshes {
                if shapes[i].clip_rect == notes_area {
                    assert!(
                        i < overlay,
                        "{dense_notes}: a native note mesh after the overlay"
                    );
                    assert!(
                        i < playhead,
                        "{dense_notes}: a note mesh after the playhead"
                    );
                }
            }
            if dense_notes > 0 {
                // No shadow gradients (§022-A): every textured tile quad is
                // drawn with white vertices — the texels' own flat colours.
                let mut textured = 0;
                for &i in &meshes {
                    if let egui::Shape::Mesh(m) = &shapes[i].shape
                        && m.texture_id != egui::TextureId::default()
                    {
                        assert!(m.vertices.iter().all(|v| v.color == egui::Color32::WHITE));
                        textured += 1;
                    }
                }
                assert!(textured > 0, "the dense tiles were drawn");
            }
            if dense_notes == 0 {
                // Exact: the selected mesh (selected fill) after the normal one.
                let vp = crate::ui::theme::velocity_palette(
                    crate::ui::theme::default_note_palette(),
                    true,
                );
                let has = |i: usize, c: egui::Color32| match &shapes[i].shape {
                    egui::Shape::Mesh(m) => m.vertices.iter().any(|v| v.color == c),
                    _ => false,
                };
                let normal = meshes
                    .iter()
                    .position(|&i| has(i, vp.fill[100]))
                    .expect("normal mesh");
                let selected = meshes
                    .iter()
                    .position(|&i| has(i, vp.selected_fill[100]))
                    .expect("selected mesh");
                assert!(normal < selected && meshes[selected] < playhead);
            }
        }
    }

    /// Clicks once at a note of track 0 while `active` is the edited track and
    /// track 0 (if different) is an Onion source at `alpha`; returns the actions.
    fn click_on_track0_note(active: usize, alpha: u8) -> Vec<RollAction> {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("hit".into(), Bpm::DEFAULT, 960);
        History::default()
            .apply(&mut doc, Operation::AddTrack)
            .unwrap();
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(960, 960, a)], false)
            .unwrap();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let full = std::cell::Cell::new(Rect::NOTHING);
        let ghost_store = doc.track(0).unwrap().notes.clone();
        let sources = [render::OnionSource {
            id: doc.track(0).unwrap().id(),
            store: &ghost_store,
            color: None,
            place: plumspirit_engine::view::tiles::SourcePlace::IDENTITY,
        }];
        let onion: &[render::OnionSource<'_>] = if active == 0 { &[] } else { &sources };
        let run = |roll: &mut PianoRoll, events: Vec<egui::Event>| {
            let mut out = Vec::new();
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                full.set(ui.available_rect_before_wrap());
                out = roll.show(
                    ui,
                    RollInput {
                        doc: &doc,
                        track_index: active,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion,
                        onion_alpha: alpha,
                        onion_outlines: Some(75),
                        capture: None,
                        stroke_overlay: &[],
                        playhead: None,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: None,
                        loop_enabled: false,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: false,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts: &[],
                    },
                    1,
                );
            });
            o.textures_delta.clear();
            out
        };
        run(&mut roll, Vec::new());
        roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.2);
        roll.view.pitch.scroll_rows = 127.0 - 70.0;
        roll.view.pitch.row_height = 12.0;
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let pos = note_rect(&g, 960, 1920, 60).center();
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut actions = run(&mut roll, vec![egui::Event::PointerMoved(pos)]);
        actions.extend(run(&mut roll, vec![button(true)]));
        actions.extend(run(&mut roll, vec![button(false)]));
        actions.extend(run(&mut roll, Vec::new()));
        actions
    }

    /// Presses on the ruler at tick `from`, drags to tick `to` (in steps) and
    /// releases, with the roll scrolled to `origin` at `ppt` pixels per tick
    /// and Snap `snap`; returns the actions and the ruler geometry used.
    fn ruler_drag(origin: u64, ppt: f64, snap: Snap, from: u64, to: u64) -> Vec<RollAction> {
        use plumspirit_engine::model::Bpm;
        let doc = PlumDocument::new_project("ruler".into(), Bpm::DEFAULT, 960);
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll {
            snap,
            ..PianoRoll::default()
        };
        let full = std::cell::Cell::new(Rect::NOTHING);
        let run = |roll: &mut PianoRoll, events: Vec<egui::Event>| {
            let mut out = Vec::new();
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                full.set(ui.available_rect_before_wrap());
                out = roll.show(
                    ui,
                    RollInput {
                        doc: &doc,
                        track_index: 0,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion: &[],
                        onion_alpha: 255,
                        onion_outlines: Some(75),
                        capture: None,
                        stroke_overlay: &[],
                        playhead: None,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: None,
                        loop_enabled: false,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: false,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts: &[],
                    },
                    1,
                );
            });
            o.textures_delta.clear();
            out
        };
        run(&mut roll, Vec::new());
        roll.view.time = plumspirit_engine::view::TimeAxis::new(origin, ppt);
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let y = g.ruler.center().y;
        let x = |t: u64| g.ruler.left() + roll.view.time.tick_to_x(t) as f32;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (p0, p1) = (Pos2::new(x(from), y), Pos2::new(x(to), y));
        let mut actions = run(&mut roll, vec![egui::Event::PointerMoved(p0)]);
        actions.extend(run(&mut roll, vec![button(p0, true)]));
        for i in 1..=8 {
            let p = p0.lerp(p1, i as f32 / 8.0);
            actions.extend(run(&mut roll, vec![egui::Event::PointerMoved(p)]));
        }
        actions.extend(run(&mut roll, vec![button(p1, false)]));
        actions.extend(run(&mut roll, Vec::new()));
        actions
    }

    fn time_selection(a: &[RollAction]) -> Option<(u64, u64)> {
        a.iter().find_map(|x| match x {
            RollAction::SetTimeSelection(r) => *r,
            _ => None,
        })
    }

    #[test]
    fn ruler_drag_selects_snapped_time_in_both_directions_at_any_zoom() {
        // 960 PPQ, Sixteenth snap = 240 ticks; 0.2 px / tick.
        let sixteenth = Snap::Sixteenth;
        // Left → right: 1000 → 2950 snaps to 960 → 2880.
        let a = ruler_drag(0, 0.2, sixteenth, 1000, 2950);
        assert_eq!(time_selection(&a), Some((960, 2880)), "{a:?}");
        assert!(a.contains(&RollAction::Seek(960)));
        // Right → left: the same interval, start first.
        let a = ruler_drag(0, 0.2, sixteenth, 2950, 1000);
        assert_eq!(time_selection(&a), Some((960, 2880)), "{a:?}");
        // Snap off: exact ticks (one pixel = 1 tick at 1 px / tick).
        let a = ruler_drag(0, 1.0, Snap::Off, 123, 777);
        assert_eq!(time_selection(&a), Some((123, 777)), "{a:?}");
        // Zoomed far out on a long song (ticks beyond u32): still the snap grid.
        let origin = 5_000_000_000u64;
        let a = ruler_drag(
            origin,
            0.0005,
            Snap::Quarter,
            origin + 100_000,
            origin + 1_000_000,
        );
        let (s, e) = time_selection(&a).expect("a range");
        assert_eq!((s % 960, e % 960), (0, 0), "{s} {e}");
        assert!(s.abs_diff(origin + 100_000) <= 960 && e.abs_diff(origin + 1_000_000) <= 960);
        // Zoomed in: Custom snap of 7 ticks.
        let a = ruler_drag(10_000, 4.0, Snap::Custom(7), 10_050, 10_100);
        assert_eq!(time_selection(&a), Some((10_052, 10_101)), "{a:?}");
        // A click (no drag) only moves the edit position.
        let a = ruler_drag(0, 0.2, sixteenth, 1000, 1001);
        assert_eq!(time_selection(&a), None, "{a:?}");
        assert!(a.contains(&RollAction::Seek(960)), "{a:?}");
        // A drag that snaps to an empty interval selects nothing.
        let a = ruler_drag(0, 1.0, Snap::Quarter, 1000, 1100);
        assert_eq!(time_selection(&a), None, "{a:?}");
    }

    /// Presses on the selected note at 1000 – 2000 (off the 1/16 grid) in its
    /// middle or on its right edge, moves the pointer by `path` offsets (one
    /// frame each, button held) and releases; returns the actions. A second
    /// selected note (key 64, 3000 – 3500) makes the selection box larger
    /// than the pressed note, so the scale handles on the box are elsewhere.
    fn press_hold_release(on_edge: bool, path: &[Vec2]) -> Vec<RollAction> {
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("click".into(), Bpm::DEFAULT, 960);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let t = doc.track_mut(0).unwrap();
        let a64 = NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        t.notes
            .insert_sorted_batch(
                &[
                    NoteRecord::new(1000, 1000, a),
                    NoteRecord::new(3000, 500, a64),
                ],
                false,
            )
            .unwrap();
        t.notes.select_all();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll {
            snap: Snap::Sixteenth,
            ..PianoRoll::default()
        };
        let full = std::cell::Cell::new(Rect::NOTHING);
        let run = |roll: &mut PianoRoll, events: Vec<egui::Event>| {
            let mut out = Vec::new();
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                full.set(ui.available_rect_before_wrap());
                out = roll.show(
                    ui,
                    RollInput {
                        doc: &doc,
                        track_index: 0,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion: &[],
                        onion_alpha: 255,
                        onion_outlines: Some(75),
                        capture: None,
                        stroke_overlay: &[],
                        playhead: None,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: None,
                        loop_enabled: false,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: false,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts: &[],
                    },
                    1,
                );
            });
            o.textures_delta.clear();
            out
        };
        run(&mut roll, Vec::new());
        roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.2);
        roll.view.pitch.scroll_rows = 127.0 - 70.0;
        roll.view.pitch.row_height = 12.0;
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let r = note_rect(&g, 1000, 2000, 60);
        let at = if on_edge {
            Pos2::new(r.right() - 1.0, r.center().y)
        } else {
            r.center()
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut actions = run(&mut roll, vec![egui::Event::PointerMoved(at)]);
        actions.extend(run(&mut roll, vec![button(at, true)]));
        let mut last = at;
        for d in path {
            last = at + *d;
            actions.extend(run(&mut roll, vec![egui::Event::PointerMoved(last)]));
        }
        actions.extend(run(&mut roll, vec![button(last, false)]));
        actions.extend(run(&mut roll, Vec::new()));
        actions
    }

    #[test]
    fn a_click_on_an_off_grid_selected_note_never_moves_or_resizes_it() {
        let edits = |a: &[RollAction]| {
            a.iter()
                .filter(|x| {
                    matches!(
                        x,
                        RollAction::MoveSelected { .. } | RollAction::ResizeSelected { .. }
                    )
                })
                .count()
        };
        let still = [Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0)];
        for on_edge in [false, true] {
            // Held for three frames with a pixel of jitter: a click.
            let a = press_hold_release(on_edge, &still);
            assert_eq!(edits(&a), 0, "edge {on_edge}: {a:?}");
        }
        // A real drag (40 px = 200 ticks → snapped to the grid) still moves.
        let drag: Vec<Vec2> = (1..=8).map(|i| Vec2::new(5.0 * i as f32, 0.0)).collect();
        let a = press_hold_release(false, &drag);
        assert!(
            a.iter()
                .any(|x| matches!(x, RollAction::MoveSelected { dt, dp: 0 } if *dt != 0)),
            "{a:?}"
        );
        let a = press_hold_release(true, &drag);
        assert!(
            a.iter()
                .any(|x| matches!(x, RollAction::ResizeSelected { delta } if *delta != 0)),
            "{a:?}"
        );
    }

    /// Presses corner / edge handle `mask` of the selection's box (notes
    /// 60 at 0 – 960 and 71 at 2880 – 3840: box 0 – 3840 × 60 – 71) with
    /// `mods` held, moves by the cumulative `path` offsets (one frame each)
    /// and releases. Returns the actions and the corner drag the app saw on
    /// the last frame before the release (what it previews).
    fn drag_handle(
        mask: u8,
        mods: egui::Modifiers,
        path: &[Vec2],
    ) -> (
        Vec<RollAction>,
        Option<(
            plumspirit_engine::transform::geom::GeomBox,
            plumspirit_engine::transform::geom::CornerWarp,
            bool,
        )>,
    ) {
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("corner".into(), Bpm::DEFAULT, 960);
        let at = |p| NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let t = doc.track_mut(0).unwrap();
        t.notes
            .insert_sorted_batch(
                &[
                    NoteRecord::new(0, 960, at(60)),
                    NoteRecord::new(2880, 960, at(71)),
                ],
                false,
            )
            .unwrap();
        t.notes.select_all();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let full = std::cell::Cell::new(Rect::NOTHING);
        let run = |roll: &mut PianoRoll, mut events: Vec<egui::Event>| {
            let mut out = Vec::new();
            events.insert(0, egui::Event::ModifiersChanged(mods));
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                full.set(ui.available_rect_before_wrap());
                out = roll.show(
                    ui,
                    RollInput {
                        doc: &doc,
                        track_index: 0,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion: &[],
                        onion_alpha: 255,
                        onion_outlines: Some(75),
                        capture: None,
                        stroke_overlay: &[],
                        playhead: None,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: None,
                        loop_enabled: false,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: false,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts: &[],
                    },
                    1,
                );
            });
            o.textures_delta.clear();
            out
        };
        run(&mut roll, Vec::new());
        roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.2);
        roll.view.pitch.scroll_rows = 127.0 - 80.0;
        roll.view.pitch.row_height = 12.0;
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let b = plumspirit_engine::transform::geom::GeomBox {
            t0: 0,
            t1: 3840,
            p0: 60,
            p1: 71,
        };
        let r = handles::screen_rect(&g, &b);
        let (_, start) = handles::handles(r)
            .into_iter()
            .find(|(m, _)| *m == mask)
            .unwrap();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: mods,
        };
        let mut actions = run(&mut roll, vec![egui::Event::PointerMoved(start)]);
        actions.extend(run(&mut roll, vec![button(start, true)]));
        let mut last = start;
        for d in path {
            last += *d;
            actions.extend(run(&mut roll, vec![egui::Event::PointerMoved(last)]));
        }
        let seen = roll.corner_drag();
        actions.extend(run(&mut roll, vec![button(last, false)]));
        actions.extend(run(&mut roll, Vec::new()));
        (actions, seen)
    }

    /// v0.9 §084 / §085 / §258 (S25): Ctrl+corner drag skews (the drag's first
    /// clear direction picks time or pitch), Ctrl+Shift+corner drag moves the
    /// one corner (Perspective), at all four corners; the preview and the
    /// committed action carry the same gesture; edges, plain drags and clicks
    /// keep their old meaning.
    #[test]
    fn ctrl_and_ctrl_shift_corner_drags_skew_and_warp_in_perspective() {
        use plumspirit_engine::transform::geom::{Corner, CornerWarp, WarpKind};
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let ctrl_shift = egui::Modifiers {
            shift: true,
            ..ctrl
        };
        let warps = |a: &[RollAction]| -> Vec<CornerWarp> {
            a.iter()
                .filter_map(|x| match x {
                    RollAction::WarpBox { warp, .. } => Some(*warp),
                    _ => None,
                })
                .collect()
        };
        let scaled = |a: &[RollAction]| a.iter().any(|x| matches!(x, RollAction::ScaleBox { .. }));
        // 0.2 px per tick, 12 px per row: 40 px = 200 ticks, 36 px = 3 rows.
        let right = [Vec2::new(10.0, 0.0); 4];
        let up = [Vec2::new(0.0, -12.0); 3];
        let diag = [Vec2::new(10.0, -12.0), Vec2::new(10.0, -12.0)];
        let corners = [
            (handles::LEFT | handles::BOTTOM, Corner::BottomLeft),
            (handles::RIGHT | handles::BOTTOM, Corner::BottomRight),
            (handles::RIGHT | handles::TOP, Corner::TopRight),
            (handles::LEFT | handles::TOP, Corner::TopLeft),
        ];
        for (mask, corner) in corners {
            assert_eq!(handles::corner_of(mask), Some(corner));
            let (a, seen) = drag_handle(mask, ctrl, &right);
            let x = CornerWarp {
                kind: WarpKind::SkewX,
                corner,
                dt: 200,
                dr: 0,
            };
            assert_eq!(warps(&a), vec![x], "{corner:?}: {a:?}");
            assert_eq!(seen.map(|s| s.1), Some(x), "the preview is the commit");
            assert!(!scaled(&a));
            let (a, _) = drag_handle(mask, ctrl, &up);
            let y = CornerWarp {
                kind: WarpKind::SkewY,
                corner,
                dt: 0,
                dr: 3,
            };
            assert_eq!(warps(&a), vec![y], "{corner:?}: {a:?}");
            let (a, seen) = drag_handle(mask, ctrl_shift, &diag);
            let p = CornerWarp {
                kind: WarpKind::Perspective,
                corner,
                dt: 100,
                dr: 2,
            };
            assert_eq!(warps(&a), vec![p], "{corner:?}: {a:?}");
            assert_eq!(seen.map(|s| (s.0.t1, s.0.p1, s.1)), Some((3840, 71, p)));
            // The first clear direction fixes the Skew axis: a drag that turns
            // upward afterwards still skews in time.
            let (a, _) = drag_handle(mask, ctrl, &[Vec2::new(10.0, 0.0), Vec2::new(0.0, -36.0)]);
            let w = warps(&a);
            assert_eq!(w.len(), 1, "{a:?}");
            assert_eq!((w[0].kind, w[0].dt), (WarpKind::SkewX, 50));
            // Without Ctrl a corner still scales.
            let (a, seen) = drag_handle(mask, egui::Modifiers::NONE, &right);
            assert!(
                scaled(&a) && warps(&a).is_empty() && seen.is_none(),
                "{a:?}"
            );
            // A Ctrl / Ctrl+Shift click changes nothing.
            for m in [ctrl, ctrl_shift] {
                let (a, _) = drag_handle(mask, m, &[]);
                assert!(warps(&a).is_empty() && !scaled(&a), "{a:?}");
            }
        }
        // Edge handles keep scaling with Ctrl held.
        let (a, _) = drag_handle(handles::RIGHT, ctrl, &right);
        assert!(scaled(&a) && warps(&a).is_empty(), "{a:?}");
    }

    /// B1-0091 (S40): Alt + corner drag rotates about the selection's center
    /// at all four corners (counter-clockwise on screen is positive), Shift
    /// snaps to 15° steps, the preview is the commit; edges with Alt keep
    /// scaling; Ctrl keeps skewing.
    #[test]
    fn alt_corner_drags_rotate_about_the_selection_center() {
        use plumspirit_engine::transform::geom::WarpKind;
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let alt_shift = egui::Modifiers { shift: true, ..alt };
        let warps = |a: &[RollAction]| -> Vec<plumspirit_engine::transform::geom::CornerWarp> {
            a.iter()
                .filter_map(|x| match x {
                    RollAction::WarpBox { warp, .. } => Some(*warp),
                    _ => None,
                })
                .collect()
        };
        let scaled = |a: &[RollAction]| a.iter().any(|x| matches!(x, RollAction::ScaleBox { .. }));
        // Box 768 × 144 px; counter-clockwise from each corner.
        let up = [Vec2::new(0.0, -12.0); 3];
        let down = [Vec2::new(0.0, 12.0); 3];
        for (mask, path) in [
            (handles::RIGHT | handles::TOP, &up),
            (handles::RIGHT | handles::BOTTOM, &up),
            (handles::LEFT | handles::TOP, &down),
            (handles::LEFT | handles::BOTTOM, &down),
        ] {
            let (a, seen) = drag_handle(mask, alt, path);
            let w = warps(&a);
            assert_eq!(w.len(), 1, "{mask}: {a:?}");
            assert_eq!(w[0].kind, WarpKind::RotateCenter);
            let deg = w[0].degrees();
            assert!(deg > 1.0 && deg < 20.0, "{mask}: {deg}");
            assert_eq!(seen.map(|s| s.1), Some(w[0]), "the preview is the commit");
            assert!(!scaled(&a));
        }
        // Shift: 15° steps (≈ 24.7° → 30°).
        let (a, _) = drag_handle(
            handles::RIGHT | handles::TOP,
            alt_shift,
            &[Vec2::new(0.0, -50.0); 4],
        );
        let w = warps(&a);
        assert_eq!(w.len(), 1, "{a:?}");
        assert!((w[0].degrees() - 30.0).abs() < 1e-9, "{}", w[0].degrees());
        // Alt on an edge scales (Duplicate Notes reversed), never rotates.
        let (a, _) = drag_handle(handles::TOP, alt, &up);
        assert!(scaled(&a) && warps(&a).is_empty(), "{a:?}");
    }

    /// v0.9 §058 / §063 (S21 step 3b): a 256-Key Mode project shows 256
    /// rows with 255 on top; turning the mode on keeps the pitch at the top of
    /// the view; the pointer, hit test and selection reach keys above 127;
    /// hovering a key names its number.
    #[test]
    fn a_256_key_project_shows_and_edits_every_key() {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::project::KeyRange;
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("wide".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.apply(&mut doc, Operation::AddTrack).unwrap();
        let rec = |t: u64, p: u8| {
            NoteRecord::new(
                t,
                960,
                NoteAttrs::new_wide(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            )
        };
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(960, 60)], false)
            .unwrap();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let full = std::cell::Cell::new(Rect::NOTHING);
        let run = |roll: &mut PianoRoll, doc: &PlumDocument, events: Vec<egui::Event>| {
            let mut out = Vec::new();
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut o = ctx.run_ui(raw, |ui| {
                full.set(ui.available_rect_before_wrap());
                out = roll.show(
                    ui,
                    RollInput {
                        doc,
                        track_index: 0,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion: &[],
                        onion_alpha: 0,
                        onion_outlines: None,
                        capture: None,
                        stroke_overlay: &[],
                        playhead: None,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: None,
                        loop_enabled: false,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: true,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts: &[],
                    },
                    1,
                );
            });
            o.textures_delta.clear();
            out
        };
        // Standard: 128 rows, 127 on top.
        run(&mut roll, &doc, Vec::new());
        roll.view.pitch.row_height = 12.0;
        roll.view.pitch.scroll_rows = 127.0 - 90.0;
        run(&mut roll, &doc, Vec::new());
        assert_eq!((roll.view.pitch.top, roll.view.pitch.rows()), (127, 128.0));
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let top_pitch = g.pitch_at_clamped(g.notes.top() + 1.0);
        assert_eq!(top_pitch, 90);
        // 256-Key Mode on: the same pitch stays on top; rows reach 255.
        h.apply(
            &mut doc,
            Operation::SetKeyRange {
                keys: KeyRange::Extended256,
            },
        )
        .unwrap();
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(1920, 200),
            },
        )
        .unwrap();
        run(&mut roll, &doc, Vec::new());
        assert_eq!((roll.view.pitch.top, roll.view.pitch.rows()), (255, 256.0));
        let g = Geometry::new(full.get(), roll.view, 1.0);
        assert_eq!(g.pitch_at_clamped(g.notes.top() + 1.0), top_pitch);
        assert_eq!(g.pitch_f_at(g.notes.top() - 99_999.0), 255.999);
        let p = g.point_of(0, 200.5);
        assert_eq!(g.pitch_at_clamped(p.y), 200);
        // Scroll up to key 200's note: hovering names the key, a click
        // selects the note (drawn selected: clear that first).
        doc.track_mut(0).unwrap().notes.clear_selection();
        roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.2);
        roll.view.pitch.scroll_rows = 255.0 - 210.0;
        run(&mut roll, &doc, Vec::new());
        let g = Geometry::new(full.get(), roll.view, 1.0);
        let pos = note_rect(&g, 1920, 2880, 200).center();
        run(&mut roll, &doc, vec![egui::Event::PointerMoved(pos)]);
        assert_eq!(roll.pointer.pitch, Some(200));
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut actions = run(&mut roll, &doc, vec![button(true)]);
        actions.extend(run(&mut roll, &doc, vec![button(false)]));
        actions.extend(run(&mut roll, &doc, Vec::new()));
        let wide = doc
            .track(0)
            .unwrap()
            .notes
            .iter_with_loc()
            .find(|(_, v)| v.attrs.pitch() == 200);
        let loc = wide.unwrap().0;
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, RollAction::SelectOnly(l) if *l == loc)),
            "{actions:?}"
        );
        assert_eq!(key_hover_text(200, None), "Key 200 · G#15");
        assert_eq!(key_hover_text(60, None), "Key 60 · C4");
        // Off again (the note at 200 deleted first): back to 128 rows with
        // the same pitch on top, clamped into 0–127.
        h.undo(&mut doc).unwrap();
        h.undo(&mut doc).unwrap();
        run(&mut roll, &doc, Vec::new());
        assert_eq!(
            (roll.view.pitch.top, roll.view.pitch.scroll_rows),
            (127, 0.0)
        );
    }

    /// The pitch axis follows the key range in the view helpers (centering,
    /// saved zoom views) without moving what the user sees.
    #[test]
    fn the_view_keeps_its_keys_when_the_key_range_changes() {
        let mut v = RollView::default();
        v.pitch.row_height = 10.0;
        v.center_pitch(60.0, 200.0);
        let before = v.pitch.y_to_pitch(100.0);
        v.set_top(255, 200.0);
        assert_eq!((v.pitch.top, v.pitch.y_to_pitch(100.0)), (255, before));
        v.center_pitch(200.0, 200.0);
        assert_eq!(v.pitch.y_to_pitch(100.0), Some(200));
        v.set_top(127, 200.0);
        assert_eq!(
            v.pitch.scroll_rows, 0.0,
            "clamped: 200 does not exist any more"
        );
        assert_eq!(v.pitch.y_to_pitch(0.0), Some(127));
    }

    /// B2-D01 (owner screenshot 060457: Conductor selected, one track, flat
    /// purple blocks): the Conductor owns no notes, so the roll's own note
    /// pass draws nothing there — no faint copy of the track active before —
    /// and the track appears only through the Onion pass (its own colours),
    /// in the exact and the dense renderer alike.
    #[test]
    fn the_conductor_draws_no_track_notes_only_its_onion_sources() {
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        // `steps` columns of 64 stacked keys from tick 0: 2 long blocks (bars 1
        // and 3, as in the screenshot) for the exact renderer, 256 short ones
        // (16,384 notes, above the 12,000-note exact limit) for the dense one.
        let song = |steps: u64, len: u64, gap: u64| {
            let mut doc = PlumDocument::new_project("conductor".into(), Bpm::DEFAULT, 960);
            let mut recs: Vec<NoteRecord> = Vec::new();
            for s in 0..steps {
                for k in 0..64u8 {
                    let a = NoteAttrs::new(40 + k, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
                    recs.push(NoteRecord::new(s * (len + gap), len, a));
                }
            }
            doc.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
            doc
        };
        let ctx = egui::Context::default();
        for (dense, doc) in [(false, song(2, 3_840, 3_840)), (true, song(256, 30, 0))] {
            let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
            let ghost = doc.track(0).unwrap().notes.clone();
            let sources = [render::OnionSource {
                id: doc.track(0).unwrap().id(),
                store: &ghost,
                color: None,
                place: plumspirit_engine::view::tiles::SourcePlace::IDENTITY,
            }];
            let mut roll = PianoRoll::default();
            let mut frame = |conductor: bool, onion: &[render::OnionSource<'_>]| {
                // A few frames: the dense tiles build on a worker.
                for _ in 0..200 {
                    let raw = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1280.0, 700.0),
                        )),
                        ..Default::default()
                    };
                    let mut o = ctx.run_ui(raw, |ui| {
                        let mut input = session_input(&doc, &meter, 0, None, 0);
                        input.conductor_active = conductor;
                        input.onion = onion;
                        roll.show(ui, input, 1);
                    });
                    o.textures_delta.clear();
                    let st = roll.stats;
                    if st.dense.ready == st.dense.visible_tiles
                        && st.onion_dense.ready == st.onion_dense.visible_tiles
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                roll.stats
            };
            let track = frame(false, &[]);
            assert_eq!(track.lod, dense, "renderer under test: {track:?}");
            assert!(
                track.primitives > 0 || track.dense.visible_tiles > 0,
                "dense {dense}: the track's own pass draws its notes: {track:?}"
            );
            let conductor = frame(true, &[]);
            assert_eq!(conductor.primitives, 0, "dense {dense}: {conductor:?}");
            assert_eq!(conductor.notes_queried, 0, "dense {dense}: {conductor:?}");
            assert_eq!(
                conductor.dense.visible_tiles, 0,
                "dense {dense}: {conductor:?}"
            );
            assert_eq!(
                conductor.onion_primitives + conductor.onion_dense.visible_tiles,
                0,
                "Onion off on the Conductor: no notes at all"
            );
            let onion = frame(true, &sources);
            assert_eq!(
                onion.primitives, 0,
                "dense {dense}: no faint copy: {onion:?}"
            );
            assert_eq!(onion.dense.visible_tiles, 0, "dense {dense}: {onion:?}");
            assert!(
                onion.onion_primitives > 0 || onion.onion_dense.visible_tiles > 0,
                "dense {dense}: the track is drawn as an Onion source: {onion:?}"
            );
        }
    }

    /// B2-D02 end to end: a dense active track of 64 stacked keys of one
    /// subchannel (the fused-slab case of screenshot 003928) gets 1-point
    /// borders on its rows at 8-pt rows, none at 3-pt rows (the exact
    /// renderer's 4-pt rule) — and the same tiles serve both row heights.
    #[test]
    fn dense_rows_get_their_borders_at_the_exact_renderers_threshold() {
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("rows".into(), Bpm::DEFAULT, 960);
        let mut recs: Vec<NoteRecord> = Vec::new();
        for s in 0..256u64 {
            for k in 0..64u8 {
                let a = NoteAttrs::new(40 + k, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
                recs.push(NoteRecord::new(s * 30, 30, a));
            }
        }
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let mut roll = PianoRoll::default();
        let frame = |roll: &mut PianoRoll, row_h: Option<f64>| {
            for _ in 0..200 {
                if let Some(h) = row_h {
                    roll.view.pitch.row_height = h;
                }
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                    ..Default::default()
                };
                let mut o = ctx.run_ui(raw, |ui| {
                    roll.show(ui, session_input(&doc, &meter, 0, None, 0), 1);
                });
                o.textures_delta.clear();
                let st = roll.stats.dense;
                if st.visible_tiles > 0 && st.ready == st.visible_tiles {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            roll.stats
        };
        frame(&mut roll, None);
        // Show keys 40 - 103 at 8-pt rows.
        roll.view.pitch.scroll_rows = 127.0 - 100.0;
        let tall = frame(&mut roll, Some(8.0));
        assert!(tall.lod, "dense renderer: {tall:?}");
        assert!(tall.dense.bordered_rows >= 40, "{:?}", tall.dense);
        let picture = tall.dense.picture;
        let short = frame(&mut roll, Some(3.0));
        assert!(short.lod);
        assert_eq!(short.dense.bordered_rows, 0, "{:?}", short.dense);
        assert_eq!(
            short.dense.picture, picture,
            "the same tiles at both row heights"
        );
    }

    /// B2-F18: in the dense renderer the aggregate's other clips are part of
    /// the track's tiles (one family with places): the picture changes with
    /// a part and stays put while paused.
    #[test]
    fn dense_tiles_draw_the_aggregate_parts() {
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("parts".into(), Bpm::DEFAULT, 960);
        let mut recs: Vec<NoteRecord> = Vec::new();
        for s in 0..256u64 {
            for k in 0..64u8 {
                let a = NoteAttrs::new(40 + k, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
                recs.push(NoteRecord::new(s * 30, 30, a));
            }
        }
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let store = doc.track(0).unwrap().notes.clone();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let picture = |parts: &[RollPart<'_>]| {
            let mut roll = PianoRoll::default();
            let mut st = roll.stats;
            for i in 0..200 {
                if i == 1 {
                    roll.view.pitch.scroll_rows = 127.0 - 100.0;
                }
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
                    ..Default::default()
                };
                let mut o = ctx.run_ui(raw, |ui| {
                    let input = RollInput {
                        parts,
                        ..session_input(&doc, &meter, 0, None, 0)
                    };
                    roll.show(ui, input, 1);
                });
                o.textures_delta.clear();
                st = roll.stats;
                if i > 1 && st.dense.visible_tiles > 0 && st.dense.ready == st.dense.visible_tiles {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(st.lod, "dense: {st:?}");
            st.dense.picture
        };
        let alone = picture(&[]);
        let part = [RollPart {
            clip: plumspirit_engine::model::ClipId::Placed(1),
            store: &store,
            key: 77,
            shift: 1_920 + 15,
            window: (0, u64::MAX),
        }];
        let with = picture(&part);
        assert_ne!(alone, with, "the part is in the tiles");
        assert_eq!(picture(&part), with, "deterministic");
    }

    /// B2-F18: a press on a note of another clip of the aggregate finds that
    /// clip's note at its place (front part first), never one outside the
    /// clip's window.
    #[test]
    fn part_hit_finds_another_clips_note_at_its_place() {
        use plumspirit_engine::model::{NoteOffKind, NoteRecord};
        use plumspirit_engine::view::tiles::SourcePlace;
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let store = NoteStore::from_sorted_records(
            &[NoteRecord::new(0, 480, a), NoteRecord::new(3_000, 480, a)],
            false,
        )
        .unwrap();
        let view = RollView {
            time: plumspirit_engine::view::TimeAxis::new(0, 0.1),
            pitch: plumspirit_engine::view::PitchAxis {
                scroll_rows: 127.0 - 72.0,
                row_height: 12.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0)),
            view,
            1.0,
        );
        // The clip plays content 0 – 1,920 at roll tick 7,680.
        let parts = [render::PlacedPart {
            store: &store,
            key: 9,
            place: SourcePlace {
                shift: 7_680,
                window: (0, 1_920),
            },
        }];
        let y = g.notes.top() + g.view.pitch.pitch_top_y(60) as f32 + 6.0;
        let at =
            |tick: u64| Pos2::new(g.notes.left() + g.view.time.tick_to_x(tick) as f32 + 5.0, y);
        let (i, loc) = part_hit(&parts, &g, at(7_680)).expect("the clip's note");
        assert_eq!((i, store.get(loc).map(|n| n.start_tick)), (0, Some(0)));
        // Its content tick 3,000 is past the window: not played, not hit.
        assert!(part_hit(&parts, &g, at(7_680 + 3_000)).is_none());
        // Nothing where the content would be without the place.
        assert!(part_hit(&parts, &g, at(0)).is_none());
    }

    /// B2-D02 (S05): at 100 / 150 / 200 % the dense rows keep their 1-point
    /// borders (drawn in points from the tiles, so a scale changes only the
    /// tiles' time resolution: their level follows the physical pixels per
    /// tick), and a paused view draws the same picture with the same borders
    /// frame after frame, rebuilding nothing — with 128 and 256 keys.
    #[test]
    fn dense_row_borders_stay_put_while_paused_and_at_every_ui_scale() {
        use plumspirit_engine::model::project::KeyRange;
        use plumspirit_engine::model::{Bpm, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("rows".into(), Bpm::DEFAULT, 960);
        let mut recs: Vec<NoteRecord> = Vec::new();
        for s in 0..256u64 {
            for k in 0..64u8 {
                let a = NoteAttrs::new(40 + k, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
                recs.push(NoteRecord::new(s * 30, 30, a));
            }
        }
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            doc.project_mut().keys = keys;
            let mut pictures = Vec::new();
            for scale in [1.0f32, 1.5, 2.0] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let mut roll = PianoRoll::default();
                let once = |roll: &mut PianoRoll| {
                    let raw = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1280.0, 700.0),
                        )),
                        ..Default::default()
                    };
                    let mut o = ctx.run_ui(raw, |ui| {
                        roll.show(ui, session_input(&doc, &meter, 0, None, 0), 1);
                    });
                    o.textures_delta.clear();
                    roll.stats
                };
                once(&mut roll);
                roll.view.pitch.scroll_rows = keys.keys() as f64 - 1.0 - 100.0;
                roll.view.pitch.row_height = 8.0;
                let mut st = once(&mut roll);
                for _ in 0..200 {
                    if st.dense.visible_tiles > 0 && st.dense.ready == st.dense.visible_tiles {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    st = once(&mut roll);
                }
                assert!(st.lod, "{keys:?} {scale}: dense renderer: {st:?}");
                assert!(st.dense.bordered_rows >= 40, "{scale}: {:?}", st.dense);
                assert_ne!(st.dense.picture, 0, "{scale}: every tile ready");
                for _ in 0..5 {
                    let again = once(&mut roll);
                    assert_eq!(again.dense.picture, st.dense.picture, "{scale}: paused");
                    assert_eq!(again.dense.bordered_rows, st.dense.bordered_rows);
                    assert_eq!(again.dense.generation, st.dense.generation);
                    // (`pending` may hold the prefetch beside the view.)
                    assert_eq!(again.dense.ready, again.dense.visible_tiles);
                    assert_eq!(again.dense.stand_ins, 0);
                }
                let ppt = roll.view.time.pixels_per_tick * f64::from(scale);
                assert_eq!(
                    st.dense.level,
                    plumspirit_engine::view::tiles::level_for(ppt)
                );
                pictures.push((st.dense.level, st.dense.bordered_rows));
            }
            // The same visible rows bordered in every tile across the view at
            // every scale; 200 % one octave (4 levels) finer in time than 100 %.
            assert!(
                pictures.iter().all(|p| p.1 % pictures[0].1 == 0),
                "{pictures:?}"
            );
            assert_eq!(pictures[2].0 - pictures[0].0, 4, "{pictures:?}");
        }
    }

    #[test]
    fn onion_ghosts_never_intercept_clicks_even_when_opaque() {
        let selects = |a: &[RollAction]| {
            a.iter().any(|x| {
                matches!(
                    x,
                    RollAction::SelectOnly(_)
                        | RollAction::ToggleSelected(_)
                        | RollAction::AddSelected(_)
                )
            })
        };
        // Sanity: clicking the note on its own track selects it.
        assert!(
            selects(&click_on_track0_note(0, 0)),
            "the click simulation works"
        );
        // The same spot with the note only as a ghost (any opacity) selects nothing.
        for alpha in [0u8, 64, 128, 204, 255] {
            let a = click_on_track0_note(1, alpha);
            assert!(!selects(&a), "alpha {alpha}: {a:?}");
        }
    }

    #[test]
    fn the_eraser_follows_the_pointer_path_and_counts_each_note_once() {
        use plumspirit_engine::model::{NoteOffKind, NoteRecord};
        // Notes on pitches 60–64, 100 ticks each, one per 200 ticks; 1 px per tick.
        let recs: Vec<NoteRecord> = (0..20u64)
            .map(|i| NoteRecord {
                start_tick: i * 200,
                duration_ticks: 100,
                attrs: NoteAttrs::new(60 + (i % 5) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        let store = NoteStore::from_sorted_records(&recs, false).unwrap();
        let view = RollView {
            time: plumspirit_engine::view::TimeAxis::new(0, 1.0),
            pitch: plumspirit_engine::view::PitchAxis {
                scroll_rows: 127.0 - 70.0,
                row_height: 10.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(4200.0, 400.0)),
            view,
            1.0,
        );
        let y_of = |p: u8| g.notes.top() + g.view.pitch.pitch_top_y(p) as f32 + 5.0;
        let x_of = |t: u64| g.notes.left() + t as f32;
        let (mut hits, mut set) = (Vec::new(), HashSet::new());
        // A single click on note 0.
        let p = Pos2::new(x_of(50), y_of(60));
        erase_along(&store, &g, p, p, &mut hits, &mut set);
        assert_eq!(hits.len(), 1);
        // A horizontal drag along pitch 60 over notes 0, 5, 10, 15 (0 again).
        erase_along(
            &store,
            &g,
            p,
            Pos2::new(x_of(3050), y_of(60)),
            &mut hits,
            &mut set,
        );
        assert_eq!(hits.len(), 4);
        // A fast diagonal stroke crossing pitch rows 61–64 between ticks 200 and 900.
        erase_along(
            &store,
            &g,
            Pos2::new(x_of(250), y_of(61)),
            Pos2::new(x_of(850), y_of(64)),
            &mut hits,
            &mut set,
        );
        let mut starts: Vec<u64> = hits
            .iter()
            .map(|&l| store.get(l).unwrap().start_tick)
            .collect();
        starts.sort_unstable();
        assert_eq!(starts, vec![0, 200, 400, 600, 800, 1000, 2000, 3000]);
        // Empty space hits nothing.
        let e = Pos2::new(x_of(150), y_of(60));
        let before = hits.len();
        erase_along(&store, &g, e, e, &mut hits, &mut set);
        assert_eq!(hits.len(), before);
    }

    #[test]
    fn segment_rect_intersection() {
        let r = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(20.0, 20.0));
        assert!(segment_hits_rect(
            Pos2::new(0.0, 15.0),
            Pos2::new(30.0, 15.0),
            r
        ));
        assert!(segment_hits_rect(
            Pos2::new(15.0, 15.0),
            Pos2::new(15.0, 15.0),
            r
        ));
        assert!(!segment_hits_rect(
            Pos2::new(0.0, 0.0),
            Pos2::new(9.0, 30.0),
            r
        ));
        assert!(segment_hits_rect(
            Pos2::new(0.0, 0.0),
            Pos2::new(30.0, 30.0),
            r
        ));
        assert!(!segment_hits_rect(
            Pos2::new(21.0, 0.0),
            Pos2::new(40.0, 30.0),
            r
        ));
    }

    /// Frame cost of the whole piano roll (notes, grid, ruler, marker band) with
    /// a 1M-note track — alone (comparable with v0.2), then with 10,000 tempo
    /// points, 100,000 markers and a signature change: static views, and following
    /// the playhead (only the origin moves).
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn roll_frame_cost_with_v03_features() {
        use plumspirit_engine::fixtures::{
            FixtureShape, generate, set_dense_tempo, set_many_markers,
        };
        use plumspirit_engine::history::{History, Operation};
        let doc = generate(FixtureShape::Dense, 1_000_000, 960, 42);
        measure_roll("1M notes", &doc, &[]);
        let mut doc = doc;
        let end = doc.content_end_tick();
        set_dense_tempo(&mut doc, 10_000, end / 10_000, 5);
        set_many_markers(&mut doc, 100_000, end / 100_000);
        let edit = doc
            .conductor()
            .prepare_set_signature(None, end / 3, 7, 3)
            .unwrap();
        History::default()
            .apply(&mut doc, Operation::Conductor { edit, label: "s" })
            .unwrap();
        measure_roll("1M notes + 10k tempo points + 100k markers", &doc, &[]);
    }

    /// B2-F18 (B2-S12): Track Aggregate frame cost — a track of 600k notes
    /// with a second clip of 600k notes after it, against one track of 1.2M
    /// notes (the same views as `roll_frame_cost_with_v03_features`).
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit aggregate_frame_cost -- --ignored --nocapture --test-threads=1"]
    fn aggregate_frame_cost() {
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        let one = generate(FixtureShape::Dense, 1_200_000, 960, 42);
        measure_roll("one track, 1.2M notes", &one, &[]);
        let half = generate(FixtureShape::Dense, 600_000, 960, 42);
        let other = generate(FixtureShape::Dense, 600_000, 960, 43);
        let t = other.track(1).unwrap();
        let parts = [RollPart {
            clip: plumspirit_engine::model::ClipId::Primary(t.id()),
            store: &t.notes,
            key: 7,
            shift: i128::from(half.content_end_tick()),
            window: (0, u64::MAX),
        }];
        measure_roll("aggregate, 2 clips x 600k notes", &half, &parts);
    }

    fn measure_roll(what: &str, doc: &PlumDocument, parts: &[RollPart<'_>]) {
        use std::time::{Duration, Instant};
        // The song with the aggregate's parts.
        let end = parts.iter().fold(doc.content_end_tick(), |e, q| {
            let last = i128::from(q.store.max_end().unwrap_or(0)) + q.shift;
            e.max(u64::try_from(last).unwrap_or(0))
        });
        let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 700.0))),
            ..Default::default()
        };
        let mut roll = PianoRoll::default();
        let frame = |roll: &mut PianoRoll, playhead: Option<u64>| -> Duration {
            let mut took = Duration::ZERO;
            let _ = ctx.run_ui(input(), |ui| {
                let t = Instant::now();
                roll.show(
                    ui,
                    RollInput {
                        doc,
                        track_index: 1,
                        meter: &meter,
                        palette: &crate::ui::theme::DARK,
                        dark: true,
                        editable: true,
                        keyboard: false,
                        onion: &[],
                        onion_alpha: 90,
                        onion_outlines: Some(75),
                        capture: None,
                        stroke_overlay: &[],
                        playhead,
                        conductor_active: false,
                        new_note_channel: 0,
                        new_note_gate: DefaultGate::Grid,
                        new_note_sub: Default::default(),
                        preview: None,
                        tool_preview: None,
                        added: None,
                        loop_range: Some((0, 3840)),
                        loop_enabled: true,
                        corrections_note: None,
                        range: None,
                        key_colors: None,
                        key_guide: false,
                        audition: false,
                        note_menu: None,
                        overlay_controls: [None; 3],
                        content: None,
                        shift: 0,
                        title: None,
                        locked: None,
                        parts,
                    },
                    1,
                );
                took = t.elapsed();
            });
            took
        };
        let report = |name: &str, times: &mut Vec<Duration>| {
            let first = times[0];
            times.sort();
            println!(
                "roll ({what}), {name}: first {:.2} ms, median {:.2} ms, slowest {:.2} ms ({} frames)",
                first.as_secs_f64() * 1e3,
                times[times.len() / 2].as_secs_f64() * 1e3,
                times[times.len() - 1].as_secs_f64() * 1e3,
                times.len()
            );
        };
        // 100% zoom at the song's middle, then the whole song.
        frame(&mut roll, None);
        roll.view.time.origin_tick = end / 2;
        let mut times: Vec<Duration> = (0..60).map(|_| frame(&mut roll, None)).collect();
        report("100% zoom, static", &mut times);
        roll.fit_song(doc);
        roll.view.time.pixels_per_tick *= doc.content_end_tick().max(1) as f64 / end.max(1) as f64;
        let mut times: Vec<Duration> = (0..60).map(|_| frame(&mut roll, None)).collect();
        report("whole song, static", &mut times);
        // Following the playhead at 100% zoom: 800 frames of audio (1/60 s) per
        // frame at the song's tempo map, starting mid-song.
        for (style, name) in [
            (FollowStyle::Smooth, "follow smooth, playing"),
            (FollowStyle::Page, "follow page, playing"),
        ] {
            roll.reset_zoom(doc, end / 2);
            frame(&mut roll, None);
            let map = plumspirit_engine::model::TempoMap::for_document(doc, 48_000);
            let mut playhead = end / 2;
            roll.view.time.origin_tick = playhead;
            let mut times = Vec::new();
            for _ in 0..240 {
                let f = map.frame_at_tick(playhead) + 800;
                playhead = map.tick_at_frame(f).max(playhead + 1);
                roll.follow_playhead(playhead, end, style, 0.5);
                times.push(frame(&mut roll, Some(playhead)));
            }
            report(name, &mut times);
        }
    }
}

#[cfg(test)]
mod banner_tests {
    use super::*;

    /// B1-0065: ruler banners stay inside the ruler strip, right-aligned, the
    /// first rightmost, and give up (truncate / drop) before the left third.
    #[test]
    fn ruler_banners_stay_in_the_ruler_strip() {
        let ctx = egui::Context::default();
        let ruler = Rect::from_min_size(Pos2::new(100.0, 40.0), Vec2::new(600.0, 18.0));
        let mut got = Vec::new();
        let mut o = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.painter().with_clip_rect(ruler);
            let long = "x".repeat(400);
            got = ruler_banners(
                &p,
                ruler,
                &[
                    ("Editing clip".into(), egui::Color32::WHITE),
                    (long, egui::Color32::YELLOW),
                    ("never reached".into(), egui::Color32::GRAY),
                ],
                &crate::ui::theme::DARK,
            );
        });
        o.textures_delta.clear();
        assert!(!got.is_empty());
        for r in &got {
            assert!(
                r.top() >= ruler.top() - 1.5 && r.bottom() <= ruler.bottom() + 1.5,
                "{r:?}"
            );
            assert!(
                r.left() >= ruler.right() - ruler.width() * 2.0 / 3.0 - 1.0,
                "{r:?}"
            );
            assert!(r.right() <= ruler.right(), "{r:?}");
        }
        assert!(
            got[0].left() > got.get(1).map_or(0.0, |r| r.right()),
            "first is rightmost"
        );
        assert!(got.len() <= 2, "the third does not fit");
    }
}
