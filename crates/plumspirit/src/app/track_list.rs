//! The Track List (v0.9 §037 / §038 / §220, V09-S08, `docs/TRACK_LIST.md`):
//! the primary window — every track's clips on a timeline, beside the left
//! track panel, which is its row-header column (the panel reports where it
//! drew each row, `track_panel::RowGeometry`, and the timeline draws the row
//! at exactly that y; one vertical scroll for both).
//!
//! The timeline has its own horizontal zoom and scroll; the ruler, the loop,
//! the time selection, the edit cursor and the playhead are the transport's
//! (one transport, the ruler gestures shared with the Piano Roll:
//! `piano_roll::ruler`). Clip rectangles come from the cached
//! `view::track_list::TrackListModel` — per visible clip, never per note.
//! The widget owns view state only and reports what it wants as
//! [`TrackListOutput`]; the app applies it.

use crate::app::TreeRowView;
use crate::piano_roll::dense::{self, ClipTiles, DenseRenderer};
use crate::piano_roll::markers::{self, BandLayout};
use crate::piano_roll::render;
use crate::piano_roll::ruler::{self, RulerDrag, Spans};
use crate::piano_roll::viewport::{self, FollowStyle, RollView};
use crate::piano_roll::{RULER_HEIGHT, RollAction, SCROLLBAR};
use crate::ui::theme::{self, Palette};
use crate::ui::track_panel::{ROW_H, RowGeometry};
use egui::{
    Color32, CursorIcon, Event, FontId, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    Sense, Stroke, Vec2,
};
use plumspirit_engine::editor::clips;
use plumspirit_engine::model::timing::snap_nearest;
use plumspirit_engine::model::{ClipId, PlumDocument};
use plumspirit_engine::model::{ContentRef, NoteStore};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::grid::{GridLine, LineKind, MeterMap};
use plumspirit_engine::view::mini::{self, MiniLod};
use plumspirit_engine::view::tiles;
use plumspirit_engine::view::track_list::{ClipHit, ClipRect, TrackListModel};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Row height of the Track List (points): default and limits.
pub const DEFAULT_ROW_H: f32 = 34.0;
pub const MIN_ROW_H: f32 = plumspirit_engine::model::project::TRACK_LIST_MIN_ROW;
pub const MAX_ROW_H: f32 = plumspirit_engine::model::project::TRACK_LIST_MAX_ROW;
/// Empty space above and below a clip inside its row.
const CLIP_INSET: f32 = 2.0;
/// Most Paste Repeat copy outlines drawn per frame (V09-S13).
const MAX_REPEAT_OUTLINES: usize = 2_000;
/// Narrowest a clip is drawn (points), so a short clip stays visible.
const MIN_CLIP_W: f32 = 3.0;
/// Pointer movement (points) below which a press on the timeline is a click.
const CLICK_SLOP: f32 = ruler::CLICK_SLOP;
/// Width of a clip's resize edges (points; never more than a third of the
/// clip, the rule of `view::track_list::hit`).
const EDGE_PX: f32 = 6.0;

/// A clip edit the Track List asks for. The app plans it with
/// `editor::clips` ([`plan`]) and applies it through the history: one
/// gesture or menu command = one undo step.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipIntent {
    /// v0.9 §041 (V09-0030): Add Piano Roll Clip — a new empty clip (its own
    /// content) on track `track` at `start`, `length` ticks long.
    Add {
        track: usize,
        start: u64,
        length: u64,
    },
    /// Each clip to (track index, start).
    Move(Vec<(ClipId, usize, u64)>),
    /// The window of one clip (left-edge trim moves start and offset).
    Resize {
        id: ClipId,
        start: u64,
        offset: u64,
        length: Option<u64>,
    },
    /// Copies of each clip at (track index, start).
    Duplicate {
        copies: Vec<(ClipId, usize, u64)>,
        linked: bool,
    },
    Mute(Vec<ClipId>, bool),
    /// `None` clears the name.
    Rename(ClipId, Option<String>),
    /// `None`: the track's colour.
    Color(Vec<ClipId>, Option<[u8; 3]>),
    Delete(Vec<ClipId>),
    /// Show the clip's notes in the Piano Roll.
    Open(ClipId),
    /// v0.9 §047 (V09-S12): open the merge Preview for the selected clips.
    Merge(plumspirit_engine::editor::clip_merge::MergeKind),
    /// v0.9 §088 (V09-S13): open the Paste Repeat dialog for the selected
    /// clips.
    PasteRepeat,
    /// B1-0097: Slice these clips at the edit cursor (the app knows it).
    Slice(Vec<ClipId>),
    /// B1-0097: Copy / Cut these clips to the app's clip clipboard.
    Copy(Vec<ClipId>),
    Cut(Vec<ClipId>),
    /// B1-0097: Paste the clip clipboard on track `track` at `at` (`None`:
    /// the active track and the edit cursor).
    Paste {
        track: Option<usize>,
        at: Option<u64>,
    },
}

/// The undoable operation for `intent` (`Ok(None)`: not an edit — Open).
/// The clip clipboard intents (Copy, Cut, Paste) and Slice need the app's
/// state and are applied by `PlumSpiritApp::apply_clip_intent`.
pub fn plan(
    doc: &PlumDocument,
    intent: &ClipIntent,
) -> Result<Option<plumspirit_engine::history::Operation>, clips::ClipEditError> {
    Ok(Some(match intent {
        ClipIntent::Add {
            track,
            start,
            length,
        } => clips::add_clip(doc, *track, *start, *length)?,
        ClipIntent::Move(m) => clips::move_clips(doc, m)?,
        ClipIntent::Resize {
            id,
            start,
            offset,
            length,
        } => clips::resize_clip(doc, *id, *start, *offset, *length)?,
        ClipIntent::Duplicate { copies, linked } => clips::duplicate_clips(doc, copies, *linked)?,
        ClipIntent::Mute(ids, on) => clips::set_muted(doc, ids, *on)?,
        ClipIntent::Rename(id, name) => clips::rename_clip(doc, *id, name.clone())?,
        ClipIntent::Color(ids, c) => clips::recolor_clips(doc, ids, *c)?,
        ClipIntent::Delete(ids) => clips::delete_clips(doc, ids)?,
        ClipIntent::Open(_)
        | ClipIntent::Merge(_)
        | ClipIntent::PasteRepeat
        | ClipIntent::Slice(_)
        | ClipIntent::Copy(_)
        | ClipIntent::Cut(_)
        | ClipIntent::Paste { .. } => return Ok(None),
    }))
}

/// Notes of the owned contents that deleting clips `ids` removes with them
/// (no other clip shows them); a track's own notes always stay.
fn deleted_notes(doc: &PlumDocument, ids: &[ClipId]) -> usize {
    use plumspirit_engine::model::ContentRef;
    let mut gone = std::collections::BTreeSet::new();
    for &id in ids {
        if let Some(p) = doc.clips().placement(id)
            && let ContentRef::Owned(c) = p.content
            && doc
                .clips()
                .placements_of(p.content)
                .iter()
                .all(|q| ids.contains(&q.id))
        {
            gone.insert(c);
        }
    }
    gone.iter()
        .filter_map(|&c| doc.clips().content(c))
        .map(|c| c.notes.len())
        .sum()
}

/// The window of a clip added for `tick` (Add Piano Roll Clip, V09-0030):
/// it starts at `tick` snapped down to the snap unit (so it covers the
/// clicked spot) and is one bar long — the bar it starts in; four quarter
/// notes (two seconds with SMPTE timing) when the timing is not musical.
pub fn new_clip_window(
    meter: &MeterMap,
    division: plumspirit_engine::model::Division,
    snap: Option<u64>,
    tick: u64,
) -> (u64, u64) {
    use plumspirit_engine::model::TimingKind;
    let start = match snap {
        Some(u) if u > 1 => tick / u * u,
        _ => tick,
    };
    let fallback = match division.kind() {
        TimingKind::Ppqn(p) => 4 * u64::from(p),
        TimingKind::Smpte {
            frames_per_second,
            ticks_per_frame,
        } => 2 * u64::from(frames_per_second) * u64::from(ticks_per_frame),
    };
    let length = meter
        .measure_length_at(start)
        .filter(|&l| l > 0)
        .unwrap_or(fallback)
        .max(1);
    (start, length)
}

/// Where Ctrl+D and the clip menu's Duplicate put copies of `sel`: right
/// after the selection as one block, each copy on its clip's track. Every
/// clip moves by the block's length — its first start to its last end —
/// rounded up to the snap unit, so a loop whose last note ends just before a
/// bar line is copied onto that bar line, and clips that follow each other
/// on a row are never copied on top of one another (found in the S08 GUI
/// journey: copies at the raw content end drifted off the grid).
pub fn copies_after(sel: &[ClipRect], snap: Option<u64>) -> Vec<(ClipId, usize, u64)> {
    let (Some(first), Some(last)) = (
        sel.iter().map(|r| r.start).min(),
        sel.iter().map(|r| r.end).max(),
    ) else {
        return Vec::new();
    };
    let len = last.saturating_sub(first).max(1);
    let step = match snap {
        Some(u) if u > 1 => len.div_ceil(u).saturating_mul(u),
        _ => len,
    };
    sel.iter()
        .map(|r| (r.id, r.track, r.start.saturating_add(step)))
        .collect()
}

/// What the Track List's right-click menu is for (`docs/v0_9/CONTEXT_MATRIX.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuTarget {
    /// A clip (the selection holds it): the clip menu.
    Clip(ClipId),
    /// The empty timeline of a track row: Add Piano Roll Clip at `tick`, and
    /// the row's track menu.
    Row { row: usize, track: usize, tick: u64 },
    /// A folder row: the folder header's menu.
    Folder {
        id: u32,
        collapsed: bool,
        count: u32,
    },
    /// The Conductor lane: the Conductor's menu at `tick`.
    Conductor { tick: u64 },
    /// The empty space below the rows: Add Track, New Folder.
    Empty,
}

/// A clip being moved: where it is now (its visible row, if shown).
#[derive(Clone, Copy, Debug)]
struct Moving {
    id: ClipId,
    row: Option<usize>,
    track: usize,
    start: u64,
    end: u64,
}

/// The Rename Clip window.
#[derive(Clone, Debug)]
struct RenameState {
    id: ClipId,
    text: String,
    frames: u8,
}

/// The Clip Color window.
#[derive(Clone, Debug)]
struct ColorState {
    ids: Vec<ClipId>,
    rgb: [u8; 3],
    track: bool,
}

/// Which view the central area shows (application state; not undoable).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainView {
    /// The primary window (every project opens in it).
    #[default]
    TrackList,
    /// The note editor for the active track's clip.
    PianoRoll,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Gesture {
    #[default]
    None,
    Ruler(RulerDrag),
    /// Pressed on empty timeline: a click clears the selection, a drag is a
    /// box selection (`add`: Ctrl / Shift held — added to the selection).
    Box {
        origin: Pos2,
        current: Pos2,
        add: bool,
        dragged: bool,
    },
    /// Dragging the horizontal scrollbar's thumb (grab offset in points).
    HScroll {
        grab: f32,
    },
    /// Dragging the selected clips by clip `grab` (`self.moving`): `dt`
    /// ticks and `drow` visible rows; Alt held = linked duplicates.
    Move {
        grab: ClipId,
        press: Pos2,
        press_tick: u64,
        grab_start: u64,
        grab_row: usize,
        dt: i128,
        drow: i64,
        dragged: bool,
        dup: bool,
        /// A plain press on a clip of a larger selection: a click (no drag)
        /// selects that clip alone.
        reselect: bool,
    },
    /// Dragging an edge of clip `id`: `left` trims (start and offset move
    /// together), else the length; `now` is the dragged edge's tick.
    Resize {
        id: ClipId,
        track: usize,
        left: bool,
        press_x: f32,
        start: u64,
        end: u64,
        offset: u64,
        length: Option<u64>,
        now: u64,
        dragged: bool,
    },
}

/// The screen areas of one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Areas {
    pub ruler: Rect,
    /// The Conductor lane (beside the panel's Conductor row): markers.
    pub conductor: Rect,
    /// The rows (beside the panel's scrolling list).
    pub list: Rect,
    pub hscroll: Rect,
}

impl Default for Areas {
    fn default() -> Self {
        Areas {
            ruler: Rect::ZERO,
            conductor: Rect::ZERO,
            list: Rect::ZERO,
            hscroll: Rect::ZERO,
        }
    }
}

/// Counters for tests and the dev overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackListStats {
    pub rows_drawn: usize,
    pub clips_drawn: usize,
    pub model_builds: u64,
    /// Merged member spans drawn on folder header rows.
    pub folder_spans_drawn: usize,
    /// Clip mini notes (V09-S09): notes drawn exactly, clips drawn from
    /// tiles, clips whose tiles are not all ready (stand-ins drawn).
    pub mini_exact_notes: usize,
    pub mini_raster_clips: usize,
    pub mini_waiting_clips: usize,
    /// V09-S13: Paste Repeat copy outlines drawn in the last frame.
    pub repeat_outlines: usize,
}

/// What the Track List reads each frame.
pub struct TrackListInput<'a> {
    pub doc: &'a PlumDocument,
    /// The document generation (a new or reopened project resets the view).
    pub generation: u64,
    /// The visible rows of the track tree (the panel's table).
    pub rows: &'a [TreeRowView],
    /// Where the panel drew them this frame.
    pub geometry: &'a RowGeometry,
    pub meter: &'a MeterMap,
    pub palette: &'a Palette,
    pub dark: bool,
    /// Keyboard input goes to the Track List (no dialog, no text field).
    pub keyboard: bool,
    pub playhead: Option<u64>,
    /// The edit position (drawn when not playing).
    pub cursor: u64,
    /// The Edit / Anchor playhead while it is on (B1-0080).
    pub anchor: Option<u64>,
    pub loop_range: Option<(u64, u64)>,
    pub loop_enabled: bool,
    /// The time selection.
    pub range: Option<(u64, u64)>,
    /// Snap unit for ruler gestures (`None`: off).
    pub snap: Option<u64>,
    /// The active track (`None`: the Conductor).
    pub active: Option<usize>,
    /// Clip edits are possible (no background job, no dialog).
    pub editable: bool,
    /// V09-S09: the tile service shared with the Piano Roll (clip mini
    /// notes); `None`: rectangles only.
    pub dense: Option<&'a mut DenseRenderer>,
    /// V09-S09 §217: Display Note Colors (else one colour per clip).
    pub note_colors: bool,
    /// B1-0098: the clip body's transparency in percent (Settings → Track
    /// List; 80 = 20 % opacity).
    pub clip_transparency: u8,
    /// The panel's multi-selection (the row menu's track actions act on it
    /// as the panel's do).
    pub selected_tracks: &'a BTreeSet<plumspirit_engine::model::TrackId>,
    /// V09-S12 (§048): while the merge Preview is open, where the merged
    /// clip will be — (track index, start, end) — drawn as a dashed outline.
    pub merge_outline: Option<(usize, u64, u64)>,
    /// V09-S13 (§089): while the Paste Repeat dialog is open, the copies it
    /// would add, drawn as dashed outlines (those in view, capped).
    pub repeat_outline: Option<super::clip_repeat_ui::RepeatOutline>,
    /// B1-0097: the clip clipboard holds clips (the row menu offers Paste
    /// Here).
    pub can_paste: bool,
    /// B1-0107: the readouts' chips and caret (last frame): presses there
    /// belong to the control (collapse / move / charts), not to the clips
    /// under it.
    pub overlay_chips: super::progressor::OverlayControls,
}

/// What the Track List asks the app to do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackListOutput {
    /// Ruler requests, applied exactly like the Piano Roll's.
    pub roll: Vec<RollAction>,
    /// A clicked clip's track becomes the active track.
    pub activate: Option<usize>,
    /// Scroll the shared track list to this offset (points) next frame.
    pub scroll_to: Option<f32>,
    /// Clip edits (each one undo step).
    pub edits: Vec<ClipIntent>,
    /// A track action from a row's menu (applied by the panel's dispatcher).
    pub track: Option<crate::ui::track_panel::TrackAction>,
    /// B2-F29: a command from the clip menu (Nudge, Zoom to Selection), run
    /// exactly as its key runs it.
    pub command: Option<crate::app::commands::Command>,
}

pub struct TrackList {
    /// Horizontal axis (only `time` is used: rows are the panel's).
    pub view: RollView,
    pub row_h: f32,
    /// Selected clips.
    pub selected: BTreeSet<ClipId>,
    /// The last clip clicked without Shift (Shift+click range anchor).
    anchor: Option<ClipId>,
    gesture: Gesture,
    model: Option<((u64, u64), Arc<TrackListModel>)>,
    initialized_for: Option<u64>,
    lines: Vec<GridLine>,
    band: BandLayout,
    /// The edit position moved by a ruler press (shown until released).
    pressed_cursor: Option<u64>,
    /// Fit the song again when the width changes (the width it was fitted
    /// to; NaN: not yet) — until the view is changed.
    auto_fit: Option<f64>,
    fitted: Option<TimeAxis>,
    width: f64,
    /// The user scrolled horizontally this frame (pauses Follow).
    pub user_panned: bool,
    pub stats: TrackListStats,
    /// Last frame's areas and drawn clips (screen rectangles), for tests and
    /// hit testing.
    pub areas: Areas,
    pub drawn: Vec<(ClipId, Rect)>,
    /// V09-S12: where the last frame drew the merge Preview's outline.
    pub merge_drawn: Option<Rect>,
    /// The clips of a Move gesture.
    moving: Vec<Moving>,
    /// What a right-click opened the menu for (V09-S10: a clip, or a row's
    /// empty timeline).
    context: Option<MenuTarget>,
    rename: Option<RenameState>,
    color: Option<ColorState>,
    /// The snap unit of the last frame (Ctrl+D / Duplicate place their copies
    /// with it; Paste Repeat's End to end rounds to it).
    pub snap: Option<u64>,
    /// Per-pixel coverage of a folder header's member clips (reused).
    cover: Vec<i32>,
    /// Clip mini notes (V09-S09): used keys per content revision, the last
    /// tile family of each content (it stands in after an edit), the colours
    /// of exactly drawn notes.
    mini: MiniState,
}

/// The notes clip `c` shows (its content's one store — a track's own notes
/// or an owned content; never a copy) and the content's key for the mini
/// notes' caches. The one resolution the mini notes use (V09-S11 §042: the
/// Piano Roll edits this very store).
pub(super) fn clip_store<'a>(doc: &'a PlumDocument, c: &ClipRect) -> Option<(&'a NoteStore, u64)> {
    match c.content {
        ContentRef::Track(t) => match c.content_track.and_then(|i| doc.track(i)) {
            Some(tr) if tr.id() == t => Some((&tr.notes, u64::from(t.0))),
            _ => doc
                .tracks()
                .iter()
                .find(|tr| tr.id() == t)
                .map(|tr| (&tr.notes, u64::from(t.0))),
        },
        ContentRef::Owned(id) => doc
            .clips()
            .content(id)
            .map(|o| (&o.notes, (1 << 40) | u64::from(id.0))),
    }
}

/// What the clip mini notes keep between frames (V09-S09).
#[derive(Default)]
struct MiniState {
    keys: UsedKeys,
    last_family: HashMap<u64, dense::FamilyId>,
    /// Colours of exactly drawn notes by colour key (one with Display Note
    /// Colors, one per clip colour without; their tables fill on use).
    colors: HashMap<u64, dense::SourceColors>,
}

/// A content (id, revision) and its used keys (`None`: no notes).
type KeysOf = ((u64, u64), Option<(u8, u8)>);

/// The used keys of each content, per content revision: small contents
/// right away, big ones on a short-lived thread (the whole key range until
/// it answers) — never a walk of millions of notes in a frame.
struct UsedKeys {
    known: HashMap<(u64, u64), Option<(u8, u8)>>,
    pending: HashSet<(u64, u64)>,
    tx: Sender<KeysOf>,
    rx: Receiver<KeysOf>,
    /// Walk inline up to this many notes (tests: always inline).
    inline: usize,
}

impl Default for UsedKeys {
    fn default() -> Self {
        let (tx, rx) = channel();
        UsedKeys {
            known: HashMap::new(),
            pending: HashSet::new(),
            tx,
            rx,
            inline: 250_000,
        }
    }
}

impl UsedKeys {
    /// The used keys of content `key` (content id, revision) — `None` while
    /// a big content is being walked.
    fn get(&mut self, key: (u64, u64), store: &NoteStore) -> Option<Option<(u8, u8)>> {
        while let Ok((k, v)) = self.rx.try_recv() {
            self.pending.remove(&k);
            self.known.insert(k, v);
        }
        if let Some(v) = self.known.get(&key) {
            return Some(*v);
        }
        if self.known.len() > 4096 {
            self.known.clear();
        }
        if store.len() <= self.inline {
            let v = mini::used_keys(store);
            self.known.insert(key, v);
            return Some(v);
        }
        if self.pending.insert(key) {
            let (tx, store) = (self.tx.clone(), store.clone());
            let spawned = std::thread::Builder::new()
                .name("plumspirit-used-keys".into())
                .spawn(move || {
                    let _ = tx.send((key, mini::used_keys(&store)));
                });
            if spawned.is_err() {
                self.pending.remove(&key);
            }
        }
        None
    }
}

/// What one frame's clip mini notes share.
struct MiniFrame<'d> {
    dense: &'d mut DenseRenderer,
    palette: &'d crate::ui::theme::NotePalette,
    note_colors: bool,
    dark: bool,
    ppp: f32,
    keys: plumspirit_engine::model::project::KeyRange,
    /// Exact rectangles left this frame.
    exact_left: usize,
}

/// What the pointer handlers of one frame read and write.
struct Press<'m, 'o> {
    doc: &'m PlumDocument,
    snap: Option<u64>,
    content_end: u64,
    model: &'m TrackListModel,
    rows: &'m [TreeRowView],
    geo: &'m RowGeometry,
    /// Modifiers held now (Alt during a move = duplicate).
    mods: egui::Modifiers,
    editable: bool,
    out: &'o mut TrackListOutput,
}

impl Default for TrackList {
    fn default() -> Self {
        TrackList {
            view: RollView::default(),
            row_h: DEFAULT_ROW_H,
            selected: BTreeSet::new(),
            anchor: None,
            gesture: Gesture::None,
            model: None,
            initialized_for: None,
            lines: Vec::new(),
            band: BandLayout::default(),
            pressed_cursor: None,
            auto_fit: None,
            fitted: None,
            width: 1.0,
            user_panned: false,
            stats: TrackListStats::default(),
            areas: Areas::default(),
            drawn: Vec::new(),
            merge_drawn: None,
            moving: Vec::new(),
            context: None,
            rename: None,
            color: None,
            snap: None,
            cover: Vec::new(),
            mini: MiniState::default(),
        }
    }
}

/// Height of a clip's header bar (B1-0098: the track color at 100 %
/// opacity with the clip's name, FL's pattern header).
const LABEL_STRIP: f32 = 13.0;
/// Clips at least this tall get the full header with the name; lower ones
/// a thin header bar without text, so the notes keep the rest.
const STRIP_ROW: f32 = 24.0;
/// The header bar of clips lower than `STRIP_ROW`.
const THIN_HEADER: f32 = 3.0;

/// B1-0098: where a clip's parts go. The header bar runs across the top at
/// full opacity; the notes area starts under it (notes are never under the
/// header) and stays inside the 1-point border. Corners are square.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ClipLayout {
    header: Rect,
    /// Where the mini notes go.
    notes: Rect,
    /// The header is tall enough for the clip's name.
    label: bool,
}

fn clip_layout(rect: Rect) -> ClipLayout {
    let label = rect.height() >= STRIP_ROW;
    let h = if label {
        LABEL_STRIP
    } else {
        THIN_HEADER.min(rect.height())
    };
    let header = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + h));
    let notes = Rect::from_min_max(
        Pos2::new(rect.left() + 1.0, header.bottom()),
        Pos2::new(
            rect.right() - 1.0,
            (rect.bottom() - 1.0).max(header.bottom()),
        ),
    );
    ClipLayout {
        header,
        notes,
        label,
    }
}

/// B1-0098: a clip body's color — the track color at `100 - transparency`
/// percent opacity (the default 80 % transparent = 20 % opacity, alpha 51).
fn body_color(fill: Color32, transparency: u8) -> Color32 {
    let a = (f32::from(100 - transparency.min(100)) * 2.55).round() as u8;
    Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), a)
}

/// `top` (premultiplied, as egui keeps it) composited over the opaque `bg`.
fn over(bg: Color32, top: Color32) -> Color32 {
    let k = 1.0 - f32::from(top.a()) / 255.0;
    let mix = |t: u8, b: u8| t.saturating_add((f32::from(b) * k).round() as u8);
    Color32::from_rgb(
        mix(top.r(), bg.r()),
        mix(top.g(), bg.g()),
        mix(top.b(), bg.b()),
    )
}

/// The one colour of mini notes with Display Note Colors off: the clip's
/// colour, lighter on a dark body and darker on a light one (`behind`: the
/// body as it shows — B1-0098: a 20 % body over a dark list is dark even for
/// a light track color).
fn mono_note_color(fill: Color32, behind: Color32) -> [u8; 4] {
    let rgb = [behind.r(), behind.g(), behind.b()];
    let c = if theme::contrast_ratio([255, 255, 255], rgb) >= theme::contrast_ratio([0, 0, 0], rgb)
    {
        fill.lerp_to_gamma(Color32::WHITE, 0.55)
    } else {
        fill.gamma_multiply(0.45)
    };
    let [r, g, b, _] = c.to_array();
    [r, g, b, 255]
}

/// Text colour readable on `fill`: near black or near white, whichever has
/// the higher WCAG contrast (B1-0056 / B1-0069: white on a light turquoise
/// header was 2 : 1 by the old luma threshold).
fn text_on(fill: Color32) -> Color32 {
    const DARK_TEXT: [u8; 3] = [20, 20, 24];
    const LIGHT_TEXT: [u8; 3] = [245, 245, 248];
    let rgb = [fill.r(), fill.g(), fill.b()];
    let [r, g, b] =
        if theme::contrast_ratio(DARK_TEXT, rgb) >= theme::contrast_ratio(LIGHT_TEXT, rgb) {
            DARK_TEXT
        } else {
            LIGHT_TEXT
        };
    Color32::from_rgb(r, g, b)
}

impl TrackList {
    /// The clip model for `doc`, rebuilt only when a placement or a content
    /// changed (O(tracks + clips); never per frame otherwise).
    pub fn model(&mut self, doc: &PlumDocument, generation: u64) -> Arc<TrackListModel> {
        let key = (generation, doc.analysis_revision());
        if let Some((k, m)) = &self.model
            && *k == key
        {
            return m.clone();
        }
        let m = Arc::new(TrackListModel::build(doc));
        self.stats.model_builds += 1;
        // Clips that no longer exist leave the selection.
        if !self.selected.is_empty() {
            let alive: std::collections::HashSet<ClipId> = (0..doc.track_count())
                .flat_map(|t| m.row(t).iter().map(|r| r.id))
                .collect();
            self.selected.retain(|id| alive.contains(id));
            if self.anchor.is_some_and(|a| !alive.contains(&a)) {
                self.anchor = None;
            }
        }
        self.model = Some((key, m.clone()));
        m
    }

    /// Follows the playhead during playback (the roll's rule,
    /// `viewport::follow_origin`).
    pub fn follow_playhead(&mut self, playhead: u64, end: u64, style: FollowStyle, anchor: f64) {
        if let Some(o) =
            viewport::follow_origin(&self.view.time, self.width, playhead, end, style, anchor)
        {
            self.view.time.origin_tick = o;
        }
    }

    /// The whole song across the width.
    pub fn fit_song(&mut self, doc: &PlumDocument) {
        let span = viewport::zoom_span(doc, 0).max(doc.duration_ticks());
        self.view.fit(span, self.width);
    }

    pub fn is_dragging(&self) -> bool {
        !matches!(self.gesture, Gesture::None)
    }

    pub fn cancel_gesture(&mut self) {
        self.gesture = Gesture::None;
        self.pressed_cursor = None;
        self.moving.clear();
    }

    /// A Rename or Clip Color window is open (it has the keyboard).
    pub fn has_window(&self) -> bool {
        self.rename.is_some() || self.color.is_some()
    }

    /// The view to save with document generation `generation` (V09-0118):
    /// `None` when the Track List has not been shown for it (the project's
    /// saved view stays as it is).
    pub fn view_for(
        &self,
        generation: u64,
    ) -> Option<plumspirit_engine::model::project::TrackListView> {
        let v = plumspirit_engine::model::project::TrackListView::new(
            self.view.time.origin_tick,
            self.view.time.pixels_per_tick,
            self.row_h,
        );
        (self.initialized_for == Some(generation) && v.is_valid()).then_some(v)
    }

    /// The selected clips as last modelled, in draw order.
    pub fn selection(&self) -> Vec<ClipRect> {
        self.model
            .as_ref()
            .map_or_else(Vec::new, |(_, m)| self.selected_rects(m))
    }

    /// Selects clip `id` alone (the anchor of Shift+click ranges).
    pub fn select_only(&mut self, id: ClipId) {
        self.selected.clear();
        self.selected.insert(id);
        self.anchor = Some(id);
    }

    /// B2-F29: Select All in the Track List — every clip it draws (not the
    /// empty windows of tracks without notes); returns how many.
    pub fn select_all(&mut self, doc: &PlumDocument, generation: u64) -> usize {
        let m = self.model(doc, generation);
        self.selected = (0..doc.track_count())
            .flat_map(|t| m.row(t).iter().filter(|r| !r.empty).map(|r| r.id))
            .collect();
        self.anchor = self.selected.iter().next().copied();
        self.selected.len()
    }

    /// B2-F29: the Move that nudges the selected clips `step` ticks later
    /// (`right`) or earlier — never before tick 0: the earliest clip stops
    /// there (`None`: nothing selected, or it already starts at 0).
    pub fn nudge_moves(&self, step: u64, right: bool) -> Option<Vec<(ClipId, usize, u64)>> {
        let sel = self.selection();
        let first = sel.iter().map(|r| r.start).min()?;
        let d = if right { step } else { step.min(first) };
        if d == 0 {
            return None;
        }
        Some(
            sel.iter()
                .map(|r| {
                    let start = if right {
                        r.start.saturating_add(d)
                    } else {
                        r.start - d
                    };
                    (r.id, r.track, start)
                })
                .collect(),
        )
    }

    /// The timeline's width (points) as last drawn (tests).
    #[cfg(test)]
    pub fn width(&self) -> f64 {
        self.width
    }

    /// B2-F29: Zoom to Selection — ticks `[t0, t1)` across the width with a
    /// small margin (the Piano Roll's Zoom tool rule), within the zoom limits.
    pub fn zoom_to(&mut self, t0: u64, t1: u64) {
        let w = self.width.max(1.0);
        let span = t1.saturating_sub(t0).max(1) as f64;
        let ppt = plumspirit_engine::view::clamp_zoom(w * 0.92 / span);
        let margin = (w * 0.04 / ppt) as u64;
        self.view.time = plumspirit_engine::view::TimeAxis::new(t0.saturating_sub(margin), ppt);
        self.auto_fit = None;
    }

    /// B2-F29: Jump to Playhead — when `tick` is off screen it is brought in
    /// near the left edge (the Piano Roll's `reveal`).
    pub fn reveal(&mut self, tick: u64, end: u64) {
        let x = self.view.time.tick_to_x(tick);
        if x < 0.0 || x > self.width {
            self.follow_playhead(tick, end.max(tick), FollowStyle::Page, 1.0);
        }
    }

    /// The clip Enter opens: the last clicked one if it is selected, else the
    /// first selected.
    pub fn open_target(&self) -> Option<ClipId> {
        self.anchor
            .filter(|a| self.selected.contains(a))
            .or_else(|| self.selection().first().map(|c| c.id))
    }

    /// What a table chord's command means while the Track List is shown
    /// (V09-0142): Delete deletes the selected clips; Ctrl+D (the Duplicate
    /// tool's chord) duplicates them linked, each right after itself.
    pub fn key_intent(&self, c: crate::app::commands::Command) -> Option<ClipIntent> {
        use crate::app::commands::Command;
        if self.has_window() || self.selected.is_empty() {
            return None;
        }
        let sel = self.selection();
        match c {
            Command::Delete => Some(ClipIntent::Delete(sel.iter().map(|r| r.id).collect())),
            // B1-0097: Copy / Cut and Ctrl+K (Slice) on the selected clips
            // (by id: they need no drawn rectangle).
            Command::Copy => Some(ClipIntent::Copy(self.selected.iter().copied().collect())),
            Command::Cut => Some(ClipIntent::Cut(self.selected.iter().copied().collect())),
            Command::SliceNotes => Some(ClipIntent::Slice(self.selected.iter().copied().collect())),
            Command::OpenTool("duplicate") => Some(ClipIntent::Duplicate {
                copies: copies_after(&sel, self.snap),
                linked: true,
            }),
            _ => None,
        }
    }

    fn areas(full: Rect, geo: &RowGeometry) -> Areas {
        // The panel has not been drawn yet (first frame): a plausible stack.
        let fallback = geo.list.height() <= 0.0;
        let conductor = if fallback {
            Rect::from_min_size(
                full.min + Vec2::new(0.0, RULER_HEIGHT + 8.0),
                Vec2::new(full.width(), ROW_H),
            )
        } else {
            Rect::from_x_y_ranges(full.x_range(), geo.conductor.y_range())
        };
        let list = if fallback {
            Rect::from_min_max(
                Pos2::new(full.left(), conductor.bottom() + 5.0),
                Pos2::new(
                    full.right(),
                    (full.bottom() - SCROLLBAR - 60.0).max(conductor.bottom() + 6.0),
                ),
            )
        } else {
            Rect::from_x_y_ranges(full.x_range(), geo.list.y_range())
        };
        // The ruler fills the space beside the panel's "Tracks" heading.
        let ruler = Rect::from_min_max(
            full.min,
            Pos2::new(full.right(), conductor.top().max(full.top() + 12.0)),
        );
        let hscroll = Rect::from_min_size(
            Pos2::new(full.left(), list.bottom() + 1.0),
            Vec2::new(full.width(), SCROLLBAR),
        );
        Areas {
            ruler,
            conductor,
            list,
            hscroll,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, input: TrackListInput<'_>) -> TrackListOutput {
        let mut out = TrackListOutput::default();
        self.user_panned = false;
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let pal = input.palette;
        let doc = input.doc;
        let geo = input.geometry;
        self.snap = input.snap;
        let a = Self::areas(full, geo);
        self.areas = a;
        self.width = f64::from(a.list.width()).max(1.0);
        let w = self.width;
        if self.initialized_for != Some(input.generation) {
            self.initialized_for = Some(input.generation);
            self.selected.clear();
            self.anchor = None;
            self.cancel_gesture();
            self.rename = None;
            self.color = None;
            self.context = None;
            // V09-0118: the view saved with the project, else the whole song.
            match doc.project().track_list.filter(|v| v.is_valid()) {
                Some(v) => {
                    self.view.time = TimeAxis::new(v.origin_tick, v.pixels_per_tick());
                    self.row_h = v.row_height();
                    self.auto_fit = None;
                }
                None => {
                    self.row_h = DEFAULT_ROW_H;
                    self.auto_fit = Some(f64::NAN);
                }
            }
        }
        // A freshly opened project shows the whole song, and keeps doing so
        // while the window settles (resizes) until the view is changed.
        if self.auto_fit.is_some_and(|fw| !fw.is_nan()) && self.fitted != Some(self.view.time) {
            self.auto_fit = None;
        }
        if let Some(fw) = self.auto_fit
            && (fw.is_nan() || (fw - w).abs() > 0.5)
        {
            self.fit_song(doc);
            self.auto_fit = Some(w);
            self.fitted = Some(self.view.time);
        }
        let model = self.model(doc, input.generation);
        // The Paste Repeat Preview's copies count as content while it is
        // open: zooming out and scrolling reach the last outline.
        let content_end = viewport::zoom_span(doc, input.cursor)
            .max(doc.duration_ticks())
            .max(input.repeat_outline.as_ref().map_or(0, |o| o.end()));

        // ----------------------------------------------------------- input
        let id = ui.id().with("track_list");
        let ruler_resp = ui.interact(a.ruler, id.with("ruler"), Sense::click_and_drag());
        let list_resp = ui.interact(a.list, id.with("list"), Sense::click_and_drag());
        let lane_resp = ui.interact(a.conductor, id.with("conductor"), Sense::click());
        let h_resp = ui.interact(a.hscroll, id.with("hscroll"), Sense::click_and_drag());
        let hovered = ui.rect_contains_pointer(Rect::from_min_max(a.ruler.min, a.hscroll.max));
        let (events, pointer, escape, mods) = ui.input(|i| {
            (
                i.events.clone(),
                i.pointer.hover_pos().or(i.pointer.interact_pos()),
                i.key_pressed(Key::Escape),
                i.modifiers,
            )
        });
        if hovered {
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
                        MouseWheelUnit::Page => a.list.height(),
                    };
                    let d = *delta * scale;
                    let anchor = pointer.unwrap_or(a.list.center());
                    if m.command && m.shift {
                        // L-F11 (contract §008): Ctrl+Shift+wheel / two-finger
                        // scroll zooms the row height (the Track List's Y)
                        // around the pointer, as it zooms the pitch rows in
                        // the Piano Roll; fractional deltas stay fractional.
                        let f = viewport::wheel_factor(f64::from(d.y + d.x)) as f32;
                        let from = out.scroll_to.unwrap_or(geo.scroll);
                        let rows = if geo.row_h > 0.0 {
                            (geo.content_h / geo.row_h).round() as usize
                        } else {
                            0
                        };
                        if let Some(scroll) = self.zoom_rows_at(
                            anchor.y - geo.list.top(),
                            f,
                            from,
                            rows,
                            geo.list.height(),
                        ) {
                            out.scroll_to = Some(scroll);
                        }
                    } else if m.command {
                        let f = viewport::wheel_factor(f64::from(d.y + d.x));
                        self.view
                            .zoom_time(f64::from(anchor.x - a.list.left()), f, content_end, w);
                    } else if m.shift {
                        self.view.time.scroll_pixels(-f64::from(d.y + d.x));
                        self.user_panned |= d.y + d.x != 0.0;
                    } else {
                        self.view.time.scroll_pixels(-f64::from(d.x));
                        self.user_panned |= d.x != 0.0;
                        if d.y != 0.0 {
                            let from = out.scroll_to.unwrap_or(geo.scroll);
                            out.scroll_to = Some((from - d.y).clamp(0.0, geo.max_scroll()));
                        }
                    }
                }
            }
        }
        if escape && input.keyboard && !self.has_window() {
            if self.is_dragging() {
                self.cancel_gesture();
            } else {
                self.selected.clear();
                self.anchor = None;
            }
        }
        // Pointer events in order (a quick click and drag can arrive in one
        // frame): every press uses its own position and modifiers. A press
        // under a menu or window (another layer) is not ours.
        let layer = ui.layer_id();
        let ctx = ui.ctx().clone();
        let chips = input.overlay_chips;
        let ours = |pos: Pos2| {
            ctx.layer_id_at(pos).is_none_or(|l| l == layer)
                && !chips.iter().flatten().any(|r| r.contains(pos))
        };
        let mut press = Press {
            doc,
            snap: input.snap,
            content_end,
            model: &model,
            rows: input.rows,
            geo,
            mods,
            editable: input.editable && !self.has_window(),
            out: &mut out,
        };
        for e in &events {
            match *e {
                Event::PointerMoved(pos) => self.pointer_to(pos, &press),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers,
                } if ours(pos) => self.press(pos, modifiers, &mut press),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: true,
                    ..
                } if ours(pos) && a.list.contains(pos) => self.secondary(pos, &mut press),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: true,
                    ..
                } if ours(pos) && a.conductor.contains(pos) => {
                    let tick = self.view.time.x_to_tick(f64::from(pos.x - a.list.left()));
                    self.context = Some(MenuTarget::Conductor { tick });
                }
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    ..
                } => {
                    self.pointer_to(pos, &press);
                    self.release(&mut press);
                }
                _ => {}
            }
        }
        // Clips dragged past the list's edges scroll it.
        if let (Gesture::Move { .. } | Gesture::Resize { .. }, Some(pos)) = (self.gesture, pointer)
        {
            let dx = if pos.x > a.list.right() {
                (pos.x - a.list.right()).min(60.0) * 0.5
            } else if pos.x < a.list.left() {
                -(a.list.left() - pos.x).min(60.0) * 0.5
            } else {
                0.0
            };
            if dx != 0.0 {
                self.view.time.scroll_pixels(f64::from(dx));
                self.user_panned = true;
                self.pointer_to(pos, &press);
            }
            if matches!(self.gesture, Gesture::Move { .. }) {
                let step = self.row_h_of(geo) * 0.25;
                let dy = if pos.y > a.list.bottom() {
                    step
                } else if pos.y < a.list.top() {
                    -step
                } else {
                    0.0
                };
                if dy != 0.0 {
                    press.out.scroll_to = Some((geo.scroll + dy).clamp(0.0, geo.max_scroll()));
                }
            }
        }
        // A release that never arrived (focus lost) ends the gesture.
        if self.is_dragging() && !ui.input(|i| i.pointer.primary_down()) {
            self.release(&mut press);
        }
        // The right-click menu (docs/v0_9/CONTEXT_MATRIX.md): a clip's (it
        // acts on the selection), a track row's (Add Piano Roll Clip here,
        // the row's track menu), a folder row's, the Conductor lane's, or
        // the empty space's.
        let target = self.context;
        let clip = match target {
            Some(MenuTarget::Clip(id)) => model.find(id).cloned(),
            _ => None,
        };
        let mut choice: Option<MenuChoice> = None;
        let mut row_choice: Option<RowChoice> = None;
        let mut close_menu = false;
        let can_edit = input.editable;
        let several = self.selected.len() > 1;
        let active_id = input.active.and_then(|i| doc.track(i)).map(|t| t.id());
        let track_menu_for = |track: usize| {
            input
                .rows
                .iter()
                .find(|r| !r.header && r.index == track)
                .map(|row| crate::ui::track_panel::TrackMenu {
                    doc,
                    index: track,
                    row,
                    editable: can_edit,
                    selected: input.selected_tracks,
                    active: active_id,
                    add_clip: false,
                })
        };
        let conductor = doc.conductor();
        let selection: Vec<ClipId> = self.selected.iter().copied().collect();
        // B2-F29: Nudge Left has no room when a selected clip starts at 0.
        let at_start = self.selection().iter().any(|r| r.start == 0);
        let mut menu = |ui: &mut egui::Ui| match (target, &clip) {
            (Some(MenuTarget::Clip(_)), Some(c)) => {
                // V09-S12: whether each merge can run on the selection (no
                // note visited; the menu is open only now).
                use plumspirit_engine::editor::clip_merge::{MergeKind, check_merge};
                let merge = [MergeKind::Sequence, MergeKind::Stack]
                    .map(|k| (k, check_merge(doc, &selection, k).map_err(|e| e.0)));
                choice = clip_menu_ui(
                    ui,
                    c,
                    several,
                    can_edit,
                    at_start,
                    track_menu_for(c.track).as_ref(),
                    &merge,
                );
            }
            (Some(MenuTarget::Row { track, .. }), _) => {
                row_choice =
                    track_menu_for(track).and_then(|m| row_menu_ui(ui, &m, input.can_paste));
            }
            (
                Some(MenuTarget::Folder {
                    id,
                    collapsed,
                    count,
                }),
                _,
            ) => {
                row_choice =
                    crate::ui::track_panel::folder_menu(ui, id, collapsed, count, can_edit)
                        .map(RowChoice::Track);
            }
            (Some(MenuTarget::Conductor { tick }), _) => {
                row_choice = crate::ui::track_panel::conductor_menu(
                    ui,
                    can_edit,
                    conductor.timing_editable(),
                    conductor.markers_editable(),
                    Some(tick),
                )
                .map(RowChoice::Track);
            }
            (Some(MenuTarget::Empty), _) => {
                row_choice = crate::ui::track_panel::empty_menu(ui, can_edit).map(RowChoice::Track);
            }
            _ => {
                close_menu = true;
                ui.close();
            }
        };
        list_resp.context_menu(&mut menu);
        lane_resp.context_menu(&mut menu);
        if close_menu {
            self.context = None;
        }
        if let (Some(ch), Some(c)) = (choice, clip) {
            self.context = None;
            match ch {
                MenuChoice::Track(a) => out.track = Some(a),
                ch => self.menu_choice(
                    ch,
                    &c,
                    &model,
                    doc,
                    &theme::note_palette(ui.ctx()),
                    &mut out,
                ),
            }
        }
        if let Some(ch) = row_choice {
            self.context = None;
            match ch {
                RowChoice::AddClip => {
                    if let Some(MenuTarget::Row { track, tick, .. }) = target {
                        let (start, length) =
                            new_clip_window(input.meter, doc.division(), self.snap, tick);
                        out.edits.push(ClipIntent::Add {
                            track,
                            start,
                            length,
                        });
                    }
                }
                RowChoice::Paste => {
                    if let Some(MenuTarget::Row { track, tick, .. }) = target {
                        let at = match self.snap {
                            Some(u) if u > 1 => tick / u * u,
                            _ => tick,
                        };
                        out.edits.push(ClipIntent::Paste {
                            track: Some(track),
                            at: Some(at),
                        });
                    }
                }
                RowChoice::Track(a) => out.track = Some(a),
            }
        }
        // A double-click on a clip opens it in the Piano Roll.
        if ui.input(|i| i.pointer.button_double_clicked(PointerButton::Primary))
            && let Some(pos) = pointer
            && a.list.contains(pos)
            && ours(pos)
            && let Some((hit, rect, _)) = self.clip_at(&model, input.rows, geo, pos)
            && edge_part(rect, pos) == ClipHit::Body
        {
            out.edits.push(ClipIntent::Open(hit.id));
        }
        self.windows(ui.ctx(), &mut out);
        if ruler_resp.hovered() || matches!(self.gesture, Gesture::Ruler(_)) {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        match self.gesture {
            Gesture::Resize { .. } => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            Gesture::Move { dragged: true, .. } => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            Gesture::None if input.editable && list_resp.hovered() => {
                if let Some(pos) = pointer
                    && let Some((_, rect, _)) = self.clip_at(&model, input.rows, geo, pos)
                    && edge_part(rect, pos) != ClipHit::Body
                {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            }
            _ => {}
        }

        // ------------------------------------------------------------ draw
        let time = self.view.time;
        let (t0, t1) = time.visible_range(w);
        let painter = ui.painter_at(full);
        let list_p = painter.with_clip_rect(a.list);
        painter.rect_filled(full, 0.0, pal.roll_bg);
        // Rows: bands, folder headers, the active / selected track.
        self.stats.rows_drawn = 0;
        for &(r, _) in &geo.rows {
            let Some(row) = input.rows.get(r) else {
                continue;
            };
            let y = geo.y_of(r);
            let band = Rect::from_min_size(
                Pos2::new(a.list.left(), y),
                Vec2::new(a.list.width(), self.row_h_of(geo)),
            );
            if row.header {
                list_p.rect_filled(band, 0.0, pal.panel_bg);
            } else if input.active == Some(row.index) {
                list_p.rect_filled(band, 0.0, pal.accent_weak.gamma_multiply(0.35));
            } else if r % 2 == 1 {
                list_p.rect_filled(band, 0.0, pal.roll_black_row.gamma_multiply(0.5));
            }
            list_p.hline(
                band.x_range(),
                band.bottom() - 0.5,
                Stroke::new(1.0, pal.grid_minor),
            );
            self.stats.rows_drawn += 1;
        }
        // Bar lines across the rows (the ruler's lines).
        input
            .meter
            .lines(t0, t1, time.pixels_per_tick, 7.0, None, &mut self.lines);
        for l in &self.lines {
            let c = match l.kind {
                LineKind::Major => pal.grid_major,
                LineKind::Minor => pal.grid_minor,
                LineKind::Sub => continue,
            };
            let x = (a.list.left() + time.tick_to_x(l.tick) as f32).round() + 0.5;
            list_p.vline(x, a.list.y_range(), Stroke::new(1.0, c));
        }
        let spans = Spans::with_drag(
            input.loop_range,
            input.loop_enabled,
            input.range,
            match &self.gesture {
                Gesture::Ruler(d) => Some(d),
                _ => None,
            },
        );
        ruler::draw_area_spans(&list_p, a.list, a.list.left(), &time, &spans, pal);
        // Clips: per visible row, the clips inside the view.
        self.drawn.clear();
        self.stats.folder_spans_drawn = 0;
        self.stats.mini_exact_notes = 0;
        self.stats.mini_raster_clips = 0;
        self.stats.mini_waiting_clips = 0;
        // Clip mini notes (V09-S09) through the tile service shared with the
        // roll.
        let note_palette = theme::note_palette(ui.ctx());
        let mut mini_frame = input.dense.map(|d| {
            d.begin_clips(ui.ctx());
            MiniFrame {
                dense: d,
                palette: &note_palette,
                note_colors: input.note_colors,
                dark: input.dark,
                ppp: ui.ctx().pixels_per_point(),
                keys: doc.project().keys,
                exact_left: mini::FRAME_EXACT,
            }
        });
        let font = FontId::proportional(theme::size::SMALL);
        for &(r, _) in &geo.rows {
            let Some(row) = input.rows.get(r) else {
                continue;
            };
            if row.header {
                self.draw_folder_spans(&list_p, doc, &model, row, geo.y_of(r), geo, t0, t1, pal);
                continue;
            }
            let Some(track) = doc.track(row.index) else {
                continue;
            };
            let y = geo.y_of(r);
            let colour_of = |t: &plumspirit_engine::model::Track| {
                note_palette.track_color(t.id().0, t.props().onion_color, input.dark, 255)
            };
            let track_color = colour_of(track);
            for c in model.visible(row.index, t0, t1).filter(|c| !c.empty) {
                // A clip showing another track's notes is named and coloured
                // after that track.
                let source = c
                    .content_track
                    .filter(|&i| i != row.index)
                    .and_then(|i| doc.track(i).map(|t| (i, t)));
                let rect = self.clip_rect(&time, a.list.left(), y, geo, c);
                if !rect.intersects(a.list) {
                    continue;
                }
                let fill = match (c.color, source) {
                    (Some(rgb), _) => {
                        Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
                    }
                    (None, Some((_, t))) => colour_of(t),
                    (None, None) => track_color,
                };
                // B1-0098: the opaque header in the track color (muted:
                // faded toward the list), the body at the set transparency.
                let header_color = if c.muted {
                    fill.lerp_to_gamma(pal.roll_bg, 0.6)
                } else {
                    fill
                };
                let body = body_color(header_color, input.clip_transparency);
                let lay = clip_layout(rect);
                list_p.rect_filled(rect, 0.0, body);
                if let Some(f) = mini_frame.as_mut() {
                    let vis = lay.notes.intersect(a.list);
                    if vis.width() >= 1.0 && vis.height() >= 1.0 && lay.notes.height() >= 3.0 {
                        let behind = over(pal.roll_bg, body);
                        let p = list_p.with_clip_rect(vis);
                        self.draw_mini(&p, f, doc, c, rect, lay.notes, vis, fill, behind);
                        if c.muted {
                            list_p.rect_filled(vis, 0.0, Color32::from_black_alpha(110));
                        }
                    }
                }
                if c.muted {
                    // Hatching over the notes area, only where it is visible.
                    let vis = lay.notes.intersect(a.list);
                    let p = list_p.with_clip_rect(vis);
                    let step = 8.0;
                    let mut x = vis.left() - rect.height() - (vis.left() - rect.left()) % step;
                    while x < vis.right() {
                        p.line_segment(
                            [
                                Pos2::new(x, rect.bottom()),
                                Pos2::new(x + rect.height(), rect.top()),
                            ],
                            Stroke::new(1.0, pal.text_weak.gamma_multiply(0.5)),
                        );
                        x += step;
                    }
                }
                list_p.rect_filled(lay.header, 0.0, header_color);
                list_p.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(1.0, header_color),
                    egui::StrokeKind::Inside,
                );
                if lay.label && rect.width() >= 18.0 {
                    let label = c.name.clone().unwrap_or_else(|| match source {
                        Some((i, t)) => input
                            .rows
                            .iter()
                            .find(|x| !x.header && x.index == i)
                            .map_or_else(|| t.display_name(i), |x| t.label_with(x.ordinal)),
                        None => track.label_with(row.ordinal),
                    });
                    let label = if c.linked {
                        format!("\u{221E} {label}")
                    } else {
                        label
                    };
                    let tp = list_p
                        .with_clip_rect(lay.header.shrink2(Vec2::new(2.0, 0.0)).intersect(a.list));
                    tp.text(
                        Pos2::new(rect.left().max(a.list.left()) + 4.0, lay.header.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        font.clone(),
                        text_on(header_color),
                    );
                }
                if self.selected.contains(&c.id) {
                    list_p.rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(2.0, pal.note_selected_border),
                        egui::StrokeKind::Inside,
                    );
                }
                self.drawn.push((c.id, rect));
            }
        }
        self.stats.clips_drawn = self.drawn.len();
        // L-G05: the tile workers wake the UI when a tile is done.
        if let Some(f) = mini_frame {
            f.dense.end_clips(ui.ctx());
        }
        self.draw_ghosts(&list_p, input.rows, geo, pal);
        self.merge_drawn = input
            .merge_outline
            .and_then(|o| self.draw_merge_outline(&list_p, input.rows, geo, pal, o));
        self.stats.repeat_outlines = input.repeat_outline.as_ref().map_or(0, |o| {
            self.draw_repeat_outline(&list_p, input.rows, geo, pal, o)
        });
        // Box selection.
        if let Gesture::Box {
            origin,
            current,
            dragged: true,
            ..
        } = self.gesture
        {
            let r = Rect::from_two_pos(origin, current);
            list_p.rect_filled(r, 0.0, pal.box_select_fill);
            list_p.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, pal.box_select_stroke),
                egui::StrokeKind::Inside,
            );
        }
        // Conductor lane: the markers and chapters (the roll's band).
        let cp = painter.with_clip_rect(a.conductor);
        self.band = markers::layout(doc.conductor(), &time, a.conductor, None);
        markers::draw(
            &cp,
            a.conductor,
            doc.conductor(),
            &mut self.band,
            pal,
            None,
            None,
        );
        // Ruler: bars, the edit position / playhead marker, loop and range.
        let rp = painter.with_clip_rect(a.ruler);
        render::draw_ruler_background(&rp, a.ruler, pal);
        let cursor = self.pressed_cursor.unwrap_or(input.cursor);
        render::draw_ruler(
            &rp,
            a.ruler,
            &time,
            pal,
            input.meter,
            &mut self.lines,
            input.playhead.unwrap_or(cursor),
            input.playhead.is_some(),
            &[],
        );
        ruler::draw_spans(&rp, a.ruler, &time, &spans, pal);
        // The edit position or the playhead through the lanes.
        let lanes = Rect::from_min_max(a.conductor.min, a.list.max);
        let lp = painter.with_clip_rect(lanes);
        match input.playhead {
            Some(t) => render::draw_playhead(&lp, lanes, a.list.left(), &time, pal, t),
            None => render::draw_cursor_line(&lp, lanes, a.list.left(), &time, pal, cursor),
        }
        // The anchor where it differs from that line (B1-0080).
        if let Some(t) = input.anchor
            && input.playhead.unwrap_or(cursor) != t
        {
            render::draw_anchor_line(&lp, lanes, a.list.left(), &time, pal, t, None);
            render::draw_anchor_line(&rp, a.ruler, a.ruler.left(), &time, pal, t, Some(a.ruler));
        }
        // Horizontal scrollbar.
        let thumb = self.hthumb(a.hscroll, content_end);
        render::draw_scrollbar(
            &painter,
            a.hscroll,
            thumb,
            h_resp.hovered() || matches!(self.gesture, Gesture::HScroll { .. }),
            pal,
        );
        if self.is_dragging() || out.scroll_to.is_some() {
            ui.ctx().request_repaint();
        }
        out
    }

    /// The tick under screen x `x` (clamped to the ruler), snapped.
    fn ruler_tick(&self, x: f32, snap: Option<u64>) -> u64 {
        let a = &self.areas;
        let t = self.view.time.x_to_tick(f64::from(
            x.clamp(a.ruler.left(), a.ruler.right()) - a.list.left(),
        ));
        snap.map_or(t, |u| snap_nearest(t, u))
    }

    fn press(&mut self, pos: Pos2, m: egui::Modifiers, p: &mut Press<'_, '_>) {
        let a = self.areas;
        if a.ruler.contains(pos) {
            let t = self.ruler_tick(pos.x, p.snap);
            let d = RulerDrag::press(t, pos.x, m.shift);
            if d.is_press() {
                self.pressed_cursor = Some(t);
            }
            self.gesture = Gesture::Ruler(d);
        } else if a.hscroll.contains(pos) {
            let thumb = self.hthumb(a.hscroll, p.content_end);
            let grab = if thumb.contains(pos) {
                pos.x - thumb.left()
            } else {
                thumb.width() / 2.0
            };
            self.gesture = Gesture::HScroll { grab };
            self.pointer_to(pos, p);
        } else if a.list.contains(pos) {
            match self.clip_at(p.model, p.rows, p.geo, pos) {
                Some((hit, rect, row)) => {
                    p.out.activate = Some(hit.track);
                    let plain = !m.command && !m.shift;
                    let part = edge_part(rect, pos);
                    if plain
                        && p.editable
                        && part != ClipHit::Body
                        && let Some(pl) = p.doc.clips().placement(hit.id)
                    {
                        // An edge resizes this clip alone.
                        self.selected.clear();
                        self.selected.insert(hit.id);
                        self.anchor = Some(hit.id);
                        let left = part == ClipHit::LeftEdge;
                        self.gesture = Gesture::Resize {
                            id: hit.id,
                            track: hit.track,
                            left,
                            press_x: pos.x,
                            start: hit.start,
                            end: hit.end,
                            offset: pl.offset,
                            length: pl.length,
                            now: if left { hit.start } else { hit.end },
                            dragged: false,
                        };
                        return;
                    }
                    let was_selected = self.selected.contains(&hit.id);
                    let (grab, grab_start) = (hit.id, hit.start);
                    self.click_clip(p.model, hit, m);
                    if plain && p.editable && self.selected.contains(&grab) {
                        let row_of: std::collections::HashMap<usize, usize> = p
                            .rows
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| !r.header)
                            .map(|(i, r)| (r.index, i))
                            .collect();
                        self.moving = self
                            .selected_rects(p.model)
                            .into_iter()
                            .map(|c| Moving {
                                id: c.id,
                                row: row_of.get(&c.track).copied(),
                                track: c.track,
                                start: c.start,
                                end: c.end,
                            })
                            .collect();
                        let press_tick = self.view.time.x_to_tick(f64::from(pos.x - a.list.left()));
                        self.gesture = Gesture::Move {
                            grab,
                            press: pos,
                            press_tick,
                            grab_start,
                            grab_row: row,
                            dt: 0,
                            drow: 0,
                            dragged: false,
                            dup: m.alt,
                            reselect: was_selected && self.selected.len() > 1,
                        };
                    }
                }
                None => {
                    self.gesture = Gesture::Box {
                        origin: pos,
                        current: pos,
                        add: m.command || m.shift,
                        dragged: false,
                    };
                }
            }
        }
    }

    fn pointer_to(&mut self, pos: Pos2, p: &Press<'_, '_>) {
        let a = self.areas;
        let snap = p.snap;
        match self.gesture {
            Gesture::Ruler(mut d) => {
                d.drag(self.ruler_tick(pos.x, snap), pos.x);
                self.gesture = Gesture::Ruler(d);
            }
            Gesture::Box {
                origin,
                ref mut current,
                ref mut dragged,
                ..
            } => {
                *current = pos;
                *dragged |= (pos - origin).length() >= CLICK_SLOP;
            }
            Gesture::HScroll { grab } => {
                let extent = self.view.scroll_extent(p.content_end, self.width) as f64;
                let frac = f64::from(pos.x - grab - a.hscroll.left())
                    / f64::from(a.hscroll.width().max(1.0));
                self.view.time.origin_tick = (frac.max(0.0) * extent) as u64;
                self.user_panned = true;
            }
            Gesture::Move {
                press,
                press_tick,
                grab_start,
                grab_row,
                ref mut dt,
                ref mut drow,
                ref mut dragged,
                ref mut dup,
                ..
            } => {
                *dragged |= (pos - press).length() >= CLICK_SLOP;
                *dup = p.mods.alt;
                if !*dragged {
                    return;
                }
                // The grabbed clip's start follows the pointer, snapped; no
                // clip goes before the song's start.
                let now = self.view.time.x_to_tick(f64::from(pos.x - a.list.left()));
                let raw = i128::from(grab_start) + i128::from(now) - i128::from(press_tick);
                let target = raw.clamp(0, i128::from(u64::MAX)) as u64;
                let snapped = snap.map_or(target, |u| snap_nearest(target, u));
                let lowest = self.moving.iter().map(|m| m.start).min().unwrap_or(0);
                *dt = (i128::from(snapped) - i128::from(grab_start)).max(-i128::from(lowest));
                // Rows: every clip must land on a track row (not a folder
                // header, not past the ends); otherwise the rows stay.
                let n = p.rows.len();
                let row_h = if p.geo.row_h > 0.0 {
                    p.geo.row_h
                } else {
                    self.row_h
                };
                let r = ((pos.y - p.geo.list.top() + p.geo.scroll) / row_h).floor();
                let r = (r.max(0.0) as usize).min(n.saturating_sub(1));
                let want = r as i64 - grab_row as i64;
                let fits = |d: i64| {
                    d == 0
                        || self.moving.iter().all(|m| {
                            m.row.is_some_and(|row| {
                                let t = row as i64 + d;
                                t >= 0 && (t as usize) < n && !p.rows[t as usize].header
                            })
                        })
                };
                if fits(want) {
                    *drow = want;
                }
            }
            Gesture::Resize {
                left,
                press_x,
                start,
                end,
                offset,
                ref mut now,
                ref mut dragged,
                ..
            } => {
                *dragged |= (pos.x - press_x).abs() >= CLICK_SLOP;
                if !*dragged {
                    return;
                }
                let t = self.view.time.x_to_tick(f64::from(pos.x - a.list.left()));
                let t = snap.map_or(t, |u| snap_nearest(t, u));
                *now = if left {
                    // Trim: not before the content's first tick, at least one
                    // tick left.
                    t.clamp(
                        start.saturating_sub(offset),
                        end.saturating_sub(1).max(start),
                    )
                } else {
                    t.max(start.saturating_add(1))
                };
            }
            Gesture::None => {}
        }
    }

    fn release(&mut self, p: &mut Press<'_, '_>) {
        match std::mem::take(&mut self.gesture) {
            Gesture::Ruler(d) => p.out.roll.extend(d.release()),
            Gesture::Box {
                origin,
                current,
                add,
                dragged,
            } => {
                if dragged {
                    self.box_select(
                        p.model,
                        p.rows,
                        p.geo,
                        Rect::from_two_pos(origin, current),
                        add,
                    );
                } else if !add {
                    self.selected.clear();
                    self.anchor = None;
                }
            }
            Gesture::Move {
                grab,
                dt,
                drow,
                dragged,
                dup,
                reselect,
                ..
            } => {
                let moving = std::mem::take(&mut self.moving);
                if dragged {
                    let to: Vec<(ClipId, usize, u64)> = moving
                        .iter()
                        .map(|m| {
                            let track = match m.row {
                                Some(r) if drow != 0 => p
                                    .rows
                                    .get((r as i64 + drow) as usize)
                                    .map_or(m.track, |row| row.index),
                                _ => m.track,
                            };
                            let start = (i128::from(m.start) + dt).max(0) as u64;
                            (m.id, track, start)
                        })
                        .collect();
                    if dt != 0 || drow != 0 {
                        p.out.edits.push(if dup {
                            ClipIntent::Duplicate {
                                copies: to,
                                linked: true,
                            }
                        } else {
                            ClipIntent::Move(to)
                        });
                    }
                } else if reselect {
                    self.selected.clear();
                    self.selected.insert(grab);
                    self.anchor = Some(grab);
                }
            }
            Gesture::Resize {
                id,
                left,
                start,
                end,
                offset,
                length,
                now,
                dragged,
                ..
            } => {
                if dragged && left && now != start {
                    p.out.edits.push(ClipIntent::Resize {
                        id,
                        start: now,
                        offset: (i128::from(offset) + i128::from(now) - i128::from(start)).max(0)
                            as u64,
                        // A window to the content's end stays one.
                        length: length.map(|_| end.saturating_sub(now).max(1)),
                    });
                } else if dragged && !left && now != end {
                    p.out.edits.push(ClipIntent::Resize {
                        id,
                        start,
                        offset,
                        length: Some(now.saturating_sub(start).max(1)),
                    });
                }
            }
            Gesture::HScroll { .. } | Gesture::None => {}
        }
        self.moving.clear();
        self.pressed_cursor = None;
    }

    /// Right-click in the list: the clip under the pointer (selected first
    /// if it was not) is the menu's target; on a track row's empty timeline
    /// the row (Add Piano Roll Clip there, its track menu); elsewhere the
    /// menu closes.
    fn secondary(&mut self, pos: Pos2, p: &mut Press<'_, '_>) {
        if let Some((hit, _, _)) = self.clip_at(p.model, p.rows, p.geo, pos) {
            if !self.selected.contains(&hit.id) {
                self.select_only(hit.id);
            }
            p.out.activate = Some(hit.track);
            self.context = Some(MenuTarget::Clip(hit.id));
            return;
        }
        let tick = self
            .view
            .time
            .x_to_tick(f64::from(pos.x - self.areas.list.left()));
        self.context = match p.geo.row_at(pos.y, p.rows.len()) {
            Some(r) => p.rows.get(r).map(|row| {
                if row.header {
                    MenuTarget::Folder {
                        id: row.folder,
                        collapsed: row.collapsed,
                        count: row.ordinal,
                    }
                } else {
                    MenuTarget::Row {
                        row: r,
                        track: row.index,
                        tick,
                    }
                }
            }),
            // Below the last row (the list is shorter than the window).
            None if pos.y >= p.geo.y_of(p.rows.len()) => Some(MenuTarget::Empty),
            None => None,
        };
    }

    /// The selected clips, in draw order (O(clips)).
    fn selected_rects(&self, model: &TrackListModel) -> Vec<ClipRect> {
        if self.selected.is_empty() {
            return Vec::new();
        }
        (0..model.row_count())
            .flat_map(|t| model.row(t))
            .filter(|c| self.selected.contains(&c.id))
            .cloned()
            .collect()
    }

    /// Runs a clip-menu choice for target `c` (the selection holds it).
    fn menu_choice(
        &mut self,
        ch: MenuChoice,
        c: &ClipRect,
        model: &TrackListModel,
        doc: &PlumDocument,
        palette: &theme::NotePalette,
        out: &mut TrackListOutput,
    ) {
        let sel = self.selected_rects(model);
        let ids: Vec<ClipId> = sel.iter().map(|r| r.id).collect();
        match ch {
            MenuChoice::Open => out.edits.push(ClipIntent::Open(c.id)),
            MenuChoice::Duplicate(linked) => out.edits.push(ClipIntent::Duplicate {
                copies: copies_after(&sel, self.snap),
                linked,
            }),
            MenuChoice::Mute(on) => out.edits.push(ClipIntent::Mute(ids, on)),
            MenuChoice::Rename => {
                self.rename = Some(RenameState {
                    id: c.id,
                    text: c.name.clone().unwrap_or_default(),
                    frames: 0,
                });
            }
            MenuChoice::Color => {
                let rgb = c.color.map_or_else(
                    || {
                        doc.track(c.track).map_or([128, 128, 128], |t| {
                            let c = palette.track_color(t.id().0, t.props().onion_color, true, 255);
                            [c.r(), c.g(), c.b()]
                        })
                    },
                    |v| [(v >> 16) as u8, (v >> 8) as u8, v as u8],
                );
                self.color = Some(ColorState {
                    ids,
                    rgb,
                    track: c.color.is_none(),
                });
            }
            MenuChoice::Delete => out.edits.push(ClipIntent::Delete(ids)),
            MenuChoice::Merge(kind) => out.edits.push(ClipIntent::Merge(kind)),
            MenuChoice::PasteRepeat => out.edits.push(ClipIntent::PasteRepeat),
            MenuChoice::Slice => out.edits.push(ClipIntent::Slice(ids)),
            MenuChoice::Copy => out.edits.push(ClipIntent::Copy(ids)),
            MenuChoice::Cut => out.edits.push(ClipIntent::Cut(ids)),
            MenuChoice::Command(cmd) => out.command = Some(cmd),
            MenuChoice::Track(a) => out.track = Some(a),
        }
    }

    /// The Rename Clip and Clip Color windows.
    fn windows(&mut self, ctx: &egui::Context, out: &mut TrackListOutput) {
        if let Some(st) = &mut self.rename {
            st.frames = st.frames.saturating_add(1);
            // The Enter that may have opened it must not also confirm it.
            let enter_ok = st.frames > 2;
            let (mut ok, mut cancel) = (false, false);
            egui::Window::new("Rename Clip")
                .id(egui::Id::new("track_list_rename"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Name (empty: the clip shows its track's or content's name):");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.text)
                            .char_limit(plumspirit_engine::model::clip::MAX_CLIP_NAME_CHARS)
                            .desired_width(260.0),
                    );
                    if st.frames == 1 {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        ok |= ui.button("Rename").clicked();
                        cancel |= ui.button("Cancel").clicked();
                    });
                    // The keys are consumed: the app's shortcuts run after the
                    // Track List, and the Enter that confirms the name must not
                    // also open the clip (nor Escape clear the selection).
                    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    ok |= enter_ok && enter;
                    cancel |= ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                });
            if ok {
                let name = Some(st.text.trim().to_string()).filter(|t| !t.is_empty());
                out.edits.push(ClipIntent::Rename(st.id, name));
            }
            if ok || cancel {
                self.rename = None;
            }
        }
        if let Some(st) = &mut self.color {
            let (mut ok, mut cancel) = (false, false);
            egui::Window::new("Clip Color")
                .id(egui::Id::new("track_list_color"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.checkbox(&mut st.track, "The track's color");
                    ui.add_enabled_ui(!st.track, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Color:");
                            egui::color_picker::color_edit_button_srgb(ui, &mut st.rgb);
                        });
                    });
                    ui.horizontal(|ui| {
                        ok |= ui.button("Apply").clicked();
                        cancel |= ui.button("Cancel").clicked();
                    });
                    cancel |= ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    // Enter belongs to the window too (never Open Clip behind it).
                    ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                });
            if ok {
                out.edits.push(ClipIntent::Color(
                    st.ids.clone(),
                    (!st.track).then_some(st.rgb),
                ));
            }
            if ok || cancel {
                self.color = None;
            }
        }
    }

    /// Where a Move or Resize would put the clips (outlines over the list).
    fn draw_ghosts(
        &self,
        p: &egui::Painter,
        rows: &[TreeRowView],
        geo: &RowGeometry,
        pal: &Palette,
    ) {
        let time = self.view.time;
        let left = self.areas.list.left();
        let drawn = |r: usize| {
            geo.rows
                .first()
                .is_some_and(|&(f, _)| r >= f && r < f + geo.rows.len())
        };
        let ghost = |rect: Rect| {
            p.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.25));
            p.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.5, pal.accent),
                egui::StrokeKind::Inside,
            );
        };
        match self.gesture {
            Gesture::Move {
                dt,
                drow,
                dragged: true,
                ..
            } => {
                for m in &self.moving {
                    let Some(r) = m.row.map(|r| (r as i64 + drow) as usize) else {
                        continue;
                    };
                    if r >= rows.len() || !drawn(r) {
                        continue;
                    }
                    let c = ClipRect {
                        id: m.id,
                        track: m.track,
                        start: (i128::from(m.start) + dt).max(0) as u64,
                        end: (i128::from(m.end) + dt).max(1) as u64,
                        empty: false,
                        name: None,
                        color: None,
                        muted: false,
                        linked: false,
                        owned: false,
                        content_track: None,
                        content: ContentRef::Owned(plumspirit_engine::model::ContentId(0)),
                        offset: 0,
                    };
                    ghost(self.clip_rect(&time, left, geo.y_of(r), geo, &c));
                }
            }
            Gesture::Resize {
                id,
                track,
                left: l,
                start,
                end,
                now,
                dragged: true,
                ..
            } => {
                let Some(r) = rows
                    .iter()
                    .position(|row| !row.header && row.index == track)
                else {
                    return;
                };
                if !drawn(r) {
                    return;
                }
                let (a, b) = if l { (now, end) } else { (start, now) };
                let c = ClipRect {
                    id,
                    track,
                    start: a,
                    end: b.max(a + 1),
                    empty: false,
                    name: None,
                    color: None,
                    muted: false,
                    linked: false,
                    owned: false,
                    content_track: None,
                    content: ContentRef::Owned(plumspirit_engine::model::ContentId(0)),
                    offset: 0,
                };
                ghost(self.clip_rect(&time, left, geo.y_of(r), geo, &c));
            }
            _ => {}
        }
    }

    /// The merge Preview's result (§048): a dashed outline over `(track,
    /// start, end)` on the track's row, if that row is drawn. Returns where.
    fn draw_merge_outline(
        &self,
        p: &egui::Painter,
        rows: &[TreeRowView],
        geo: &RowGeometry,
        pal: &Palette,
        (track, start, end): (usize, u64, u64),
    ) -> Option<Rect> {
        let r = rows
            .iter()
            .position(|row| !row.header && row.index == track)?;
        let (first, _) = *geo.rows.first()?;
        if r < first || r >= first + geo.rows.len() {
            return None;
        }
        let rect = self.span_rect(
            &self.view.time,
            self.areas.list.left(),
            geo.y_of(r),
            geo,
            start,
            end,
        );
        p.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.12));
        // Just outside the result, so it stays visible where it coincides
        // with the selected source clips (their border is drawn inside).
        let o = rect.expand(2.0);
        let pts = [
            o.left_top(),
            o.right_top(),
            o.right_bottom(),
            o.left_bottom(),
            o.left_top(),
        ];
        p.extend(egui::Shape::dashed_line(
            &pts,
            Stroke::new(2.0, pal.accent),
            6.0,
            4.0,
        ));
        Some(rect)
    }

    /// The Paste Repeat dialog's new clips (§089, §094): a dashed outline
    /// for every one in view on a drawn row (muted ones dimmer) — found per
    /// row by binary search in the sorted plan, never by walking all clips —
    /// at most [`MAX_REPEAT_OUTLINES`] per frame. Returns how many were
    /// drawn.
    fn draw_repeat_outline(
        &self,
        p: &egui::Painter,
        rows: &[TreeRowView],
        geo: &RowGeometry,
        pal: &Palette,
        o: &super::clip_repeat_ui::RepeatOutline,
    ) -> usize {
        let Some(&(first, _)) = geo.rows.first() else {
            return 0;
        };
        let time = self.view.time;
        let t0 = time.origin_tick;
        let t1 = t0.saturating_add((self.width / time.pixels_per_tick).ceil() as u64);
        let left = self.areas.list.left();
        let stroke = Stroke::new(1.5, pal.accent);
        let muted = Stroke::new(1.0, pal.accent.gamma_multiply(0.45));
        let mut drawn = 0;
        for (r, row) in rows.iter().enumerate() {
            if row.header || r < first || r >= first + geo.rows.len() {
                continue;
            }
            for c in o.in_view(row.index, t0, t1) {
                if c.end <= t0 {
                    continue;
                }
                if drawn >= MAX_REPEAT_OUTLINES {
                    return drawn;
                }
                let rect = self.span_rect(&time, left, geo.y_of(r), geo, c.start, c.end);
                let (fill, line) = if c.muted {
                    (pal.accent.gamma_multiply(0.04), muted)
                } else {
                    (pal.accent.gamma_multiply(0.10), stroke)
                };
                p.rect_filled(rect, 0.0, fill);
                p.extend(egui::Shape::dashed_line(
                    &[
                        rect.left_top(),
                        rect.right_top(),
                        rect.right_bottom(),
                        rect.left_bottom(),
                        rect.left_top(),
                    ],
                    line,
                    5.0,
                    4.0,
                ));
                drawn += 1;
            }
        }
        drawn
    }

    fn row_h_of(&self, geo: &RowGeometry) -> f32 {
        if geo.row_h > 0.0 {
            geo.row_h
        } else {
            self.row_h
        }
    }

    fn hthumb(&self, track: Rect, content_end: u64) -> Rect {
        let extent = self.view.scroll_extent(content_end, self.width) as f64;
        let vis = self.width / self.view.time.pixels_per_tick;
        render::scrollbar_thumb(
            track,
            true,
            self.view.time.origin_tick as f64 / extent,
            vis / extent,
        )
    }

    /// The screen rectangle of clip `c` on the row whose top is `y`.
    /// A folder header row shows where its members' clips are (faint, no
    /// editing there; docs/TRACK_LIST.md): the visible clips of its run of
    /// top-level members and their subtracks, merged per pixel column —
    /// O(visible member clips + width) per frame, never per note. Collapsed
    /// folders show it too (their member rows are hidden).
    #[allow(clippy::too_many_arguments)]
    fn draw_folder_spans(
        &mut self,
        p: &egui::Painter,
        doc: &PlumDocument,
        model: &TrackListModel,
        row: &TreeRowView,
        y: f32,
        geo: &RowGeometry,
        t0: u64,
        t1: u64,
        pal: &Palette,
    ) {
        let time = self.view.time;
        let list = self.areas.list;
        let w = list.width().ceil().max(1.0) as usize;
        self.cover.clear();
        self.cover.resize(w + 1, 0);
        let folders = &doc.project().folders;
        let mut any = false;
        let mut k = row.index;
        while let Some(t) = doc.track(k) {
            if t.parent().is_some() || folders.folder_of(t.id()) != row.folder {
                break;
            }
            let end = doc.subtree_end(k).max(k + 1);
            for m in k..end {
                for c in model.visible(m, t0, t1).filter(|c| !c.empty) {
                    let x0 = time.tick_to_x(c.start).floor().clamp(0.0, w as f64) as usize;
                    let x1 = time.tick_to_x(c.end).ceil().clamp(0.0, w as f64) as usize;
                    let x1 = x1.max(x0 + 1).min(w);
                    if x0 < x1 {
                        self.cover[x0] += 1;
                        self.cover[x1] -= 1;
                        any = true;
                    }
                }
            }
            k = end;
        }
        if !any {
            return;
        }
        let h = self.row_h_of(geo);
        let fill = pal.accent.gamma_multiply(0.35);
        let (mut acc, mut run) = (0i32, None);
        for x in 0..=w {
            acc += self.cover[x];
            match (x < w && acc > 0, run) {
                (true, None) => run = Some(x),
                (false, Some(s)) => {
                    let r = Rect::from_min_max(
                        Pos2::new(list.left() + s as f32, y + h * 0.3),
                        Pos2::new(list.left() + x as f32, y + h * 0.7),
                    );
                    p.rect_filled(r, 0.0, fill);
                    self.stats.folder_spans_drawn += 1;
                    run = None;
                }
                _ => {}
            }
        }
    }

    /// A clip's mini notes (V09-S09, docs/TRACK_LIST.md "Mini notes"): the
    /// notes it plays, exact when sparse, else the content's tiles from the
    /// shared tile service (stand-ins while they build — never blank).
    /// `area` is the clip's notes area (B1-0098: under the header; the key
    /// rows divide its whole height, so a row scrolled half out of view keeps
    /// its notes' size), `body` its visible part, `p` clipped to it.
    #[allow(clippy::too_many_arguments)]
    fn draw_mini(
        &mut self,
        p: &egui::Painter,
        f: &mut MiniFrame<'_>,
        doc: &PlumDocument,
        c: &ClipRect,
        rect: Rect,
        area: Rect,
        body: Rect,
        fill: Color32,
        behind: Color32,
    ) {
        let Some((store, content_key)) = clip_store(doc, c) else {
            return;
        };
        if store.is_empty() {
            return;
        }
        let time = self.view.time;
        let left = self.areas.list.left();
        let len = c.end.saturating_sub(c.start);
        let (w0, w1) = (c.offset, c.offset.saturating_add(len));
        // The content ticks under the visible part of the body.
        let tick_at = |x: f32| time.x_to_tick(f64::from(x - left));
        let v0 = tick_at(body.left()).clamp(c.start, c.end);
        let v1 = tick_at(body.right())
            .saturating_add(1)
            .clamp(c.start, c.end);
        let (cv0, cv1) = (w0 + (v0 - c.start), w0 + (v1 - c.start));
        if cv1 <= cv0 {
            return;
        }
        let rev = store.content_revision();
        let used = self.mini.keys.get((content_key, rev), store);
        if used == Some(None) {
            return;
        }
        let rows = mini::row_bucket(f64::from(area.height() * f.ppp));
        let fold = mini::fold_for(used.flatten(), f.keys, rows);
        let width_px = f64::from(body.width() * f.ppp);
        let estimate = store.estimate_in_range(cv0, cv1 - 1);
        // What the colours depend on: the note palette with Display Note
        // Colors (whatever the clip's colour: linked clips of other colours
        // share tiles), else the clip's one colour.
        let mono = mono_note_color(fill, behind);
        let colors_key = if f.note_colors {
            dense::hash_words([1, f.palette.key(), u64::from(f.dark)])
        } else {
            dense::hash_words([0, u64::from(u32::from_le_bytes(mono))])
        };
        let lod = mini::mini_lod(estimate, width_px, f.exact_left);
        if lod == MiniLod::Exact
            && let Some(notes) =
                mini::exact_notes(store, w0, len, c.start, mini::MINI_EXACT.min(f.exact_left))
        {
            if self.mini.colors.len() > 64 && !self.mini.colors.contains_key(&colors_key) {
                self.mini.colors.clear();
            }
            let src = self.mini.colors.entry(colors_key).or_insert_with(|| {
                dense::SourceColors::new(if f.note_colors {
                    dense::Tint::Subchannels {
                        dark: f.dark,
                        palette: Arc::new(f.palette.clone()),
                    }
                } else {
                    dense::Tint::Flat(mono)
                })
            });
            let key_h = area.height() / fold.keys() as f32;
            let min = 1.0 / f.ppp;
            let mut mesh = egui::Mesh::default();
            mesh.reserve_triangles(notes.len() * 2);
            mesh.reserve_vertices(notes.len() * 4);
            for n in &notes {
                if fold.row_of(n.pitch).is_none() {
                    continue;
                }
                let k = f32::from(fold.hi() - n.pitch);
                let x0 = left + time.tick_to_x(n.start) as f32;
                let x1 = (left + time.tick_to_x(n.end) as f32).max(x0 + min);
                let y0 = area.top() + k * key_h;
                let r = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y0 + key_h.max(min)))
                    .intersect(rect)
                    .intersect(body);
                if r.width() <= 0.0 || r.height() <= 0.0 {
                    continue;
                }
                let [r8, g8, b8, a8] = src.get(n.sub, n.velocity).fill;
                mesh.add_colored_rect(r, Color32::from_rgba_unmultiplied(r8, g8, b8, a8));
            }
            f.exact_left = f.exact_left.saturating_sub(notes.len());
            self.stats.mini_exact_notes += notes.len();
            p.add(egui::Shape::mesh(mesh));
            return;
        }
        // Tiles: the content's notes starting in the clip's window, its used
        // keys folded onto the body's rows, on the content's own time axis.
        let opt = tiles::TileOptions {
            keys: f.keys,
            fold: Some(fold),
            window: Some((w0, w1)),
            outline: None,
        };
        let family = dense::hash_words([
            2,
            content_key,
            rev,
            store.len() as u64,
            colors_key,
            u64::from(fold.lo()),
            u64::from(fold.hi()),
            fold.rows() as u64,
            w0,
            w1,
            f.keys.keys() as u64,
        ]);
        let previous = match self.mini.last_family.insert(content_key, family) {
            Some(prev) if prev != family => Some(prev),
            _ => None,
        };
        let tiles_in = ClipTiles {
            family,
            previous,
            level: tiles::level_for(time.pixels_per_tick * f64::from(f.ppp)),
            range: (cv0, cv1),
            origin: w0 as f64,
            x0: left + time.tick_to_x(c.start) as f32,
            ppt: time.pixels_per_tick,
            body: Rect::from_x_y_ranges(rect.x_range(), area.y_range()),
        };
        let (note_colors, dark, palette) = (f.note_colors, f.dark, f.palette);
        let complete = f.dense.draw_clip(p, &tiles_in, || dense::Family {
            stores: vec![Arc::new(store.clone())],
            places: Vec::new(),
            readings: Vec::new(),
            colors: dense::Colors {
                sources: vec![dense::SourceColors::new(if note_colors {
                    dense::Tint::Subchannels {
                        dark,
                        palette: Arc::new(palette.clone()),
                    }
                } else {
                    dense::Tint::Flat(mono)
                })],
                selection: false,
                muted_fill: None,
                muted: 90,
                ranked: false,
            },
            opt,
        });
        self.stats.mini_raster_clips += 1;
        if !complete {
            self.stats.mini_waiting_clips += 1;
        }
    }

    fn clip_rect(
        &self,
        time: &TimeAxis,
        left: f32,
        y: f32,
        geo: &RowGeometry,
        c: &ClipRect,
    ) -> Rect {
        self.span_rect(time, left, y, geo, c.start, c.end)
    }

    /// The screen rectangle of song ticks `start .. end` on the row whose
    /// top is `y` (a clip's, or the merge Preview's outline).
    fn span_rect(
        &self,
        time: &TimeAxis,
        left: f32,
        y: f32,
        geo: &RowGeometry,
        start: u64,
        end: u64,
    ) -> Rect {
        let x0 = left + time.tick_to_x(start) as f32;
        let x1 = (left + time.tick_to_x(end) as f32).max(x0 + MIN_CLIP_W);
        let h = self.row_h_of(geo);
        Rect::from_min_max(
            Pos2::new(x0, y + CLIP_INSET),
            Pos2::new(x1, y + h - CLIP_INSET),
        )
    }

    /// The clip under `pos` (the topmost: the last drawn of its row), its
    /// screen rectangle and its visible row.
    fn clip_at(
        &self,
        model: &TrackListModel,
        rows: &[TreeRowView],
        geo: &RowGeometry,
        pos: Pos2,
    ) -> Option<(ClipRect, Rect, usize)> {
        let r = geo.row_at(pos.y, rows.len())?;
        let row = rows.get(r).filter(|row| !row.header)?;
        let y = geo.y_of(r);
        let time = self.view.time;
        let left = self.areas.list.left();
        // Clips narrower than MIN_CLIP_W are drawn wider: search a few ticks
        // either side, then test the drawn rectangles.
        let t = time.x_to_tick(f64::from(pos.x - left));
        let pad = (f64::from(MIN_CLIP_W) / time.pixels_per_tick).ceil() as u64 + 1;
        model
            .visible(
                row.index,
                t.saturating_sub(pad),
                t.saturating_add(pad).saturating_add(1),
            )
            .filter(|c| !c.empty)
            .map(|c| (c, self.clip_rect(&time, left, y, geo, c)))
            .filter(|(_, rect)| rect.contains(pos))
            .last()
            .map(|(c, rect)| (c.clone(), rect, r))
    }

    /// A click on clip `hit`: select it; Ctrl: toggle; Shift: every clip of
    /// its row from the anchor's start to this one.
    fn click_clip(&mut self, model: &TrackListModel, hit: ClipRect, m: egui::Modifiers) {
        if m.shift
            && let Some(a) = self
                .anchor
                .and_then(|a| model.find(a))
                .filter(|a| a.track == hit.track)
        {
            let (lo, hi) = (a.start.min(hit.start), a.start.max(hit.start));
            if !m.command {
                self.selected.clear();
            }
            self.selected.extend(
                model
                    .row(hit.track)
                    .iter()
                    .filter(|c| c.start >= lo && c.start <= hi)
                    .map(|c| c.id),
            );
            return;
        }
        if m.command {
            if !self.selected.remove(&hit.id) {
                self.selected.insert(hit.id);
            }
        } else if !self.selected.contains(&hit.id) {
            self.selected.clear();
            self.selected.insert(hit.id);
        }
        self.anchor = Some(hit.id);
    }

    /// Every clip whose drawn rectangle touches `area` (the visible rows).
    fn box_select(
        &mut self,
        model: &TrackListModel,
        rows: &[TreeRowView],
        geo: &RowGeometry,
        area: Rect,
        add: bool,
    ) {
        if !add {
            self.selected.clear();
        }
        let time = self.view.time;
        let left = self.areas.list.left();
        let area = area.intersect(self.areas.list);
        let t0 = time.x_to_tick(f64::from(area.left() - left));
        let t1 = time
            .x_to_tick(f64::from(area.right() - left))
            .saturating_add(1);
        let pad = (f64::from(MIN_CLIP_W) / time.pixels_per_tick).ceil() as u64 + 1;
        for &(r, _) in &geo.rows {
            let Some(row) = rows.get(r).filter(|row| !row.header) else {
                continue;
            };
            let y = geo.y_of(r);
            for c in model.visible(row.index, t0.saturating_sub(pad), t1) {
                if !c.empty && self.clip_rect(&time, left, y, geo, c).intersects(area) {
                    self.selected.insert(c.id);
                }
            }
        }
    }
}

impl TrackList {
    /// Zoom X in percent (100 % = the Piano Roll's reference zoom).
    pub fn zoom_x_percent(&self, doc: &PlumDocument) -> f64 {
        self.view.zoom_x_percent(doc.division())
    }

    /// Zoom X typed in the toolbar, around the middle of the timeline.
    pub fn set_zoom_x_percent(&mut self, percent: f64, doc: &PlumDocument) -> Result<(), String> {
        let ppt = viewport::zoom_x_from_percent(percent, doc.division())?;
        self.view.set_zoom_x(self.width / 2.0, ppt);
        Ok(())
    }

    /// The row height in percent of the default.
    pub fn row_percent(&self) -> f64 {
        f64::from(self.row_h / DEFAULT_ROW_H) * 100.0
    }

    /// The row height typed in the toolbar (the toolbar's Y field).
    pub fn set_row_percent(&mut self, percent: f64) -> Result<(), String> {
        // B1-0090: any positive request; beyond the row limits it is clamped.
        if !percent.is_finite() || percent <= 0.0 {
            return Err("The Track List's row height must be a positive number of percent.".into());
        }
        let h = DEFAULT_ROW_H * percent as f32 / 100.0;
        self.row_h = if h.is_finite() {
            h.clamp(MIN_ROW_H, MAX_ROW_H)
        } else {
            MAX_ROW_H
        };
        Ok(())
    }

    /// View → Fit Song / Zoom In / Zoom Out / Reset Zoom (time) and Zoom In
    /// / Out Y (row height) while the Track List is shown.
    /// L-F11: row height × `f` keeping the row under `y` (points from the
    /// list's top) where it is; returns the new scroll offset, or None when
    /// the row height is already at its limit.
    pub fn zoom_rows_at(
        &mut self,
        y: f32,
        f: f32,
        scroll: f32,
        rows: usize,
        list_h: f32,
    ) -> Option<f32> {
        if !f.is_finite() || f <= 0.0 {
            return None;
        }
        let old = self.row_h;
        let new = (old * f).clamp(MIN_ROW_H, MAX_ROW_H);
        if (new - old).abs() < f32::EPSILON {
            return None;
        }
        self.row_h = new;
        let content = (y + scroll) / old * new;
        Some((content - y).clamp(0.0, (rows as f32 * new - list_h).max(0.0)))
    }

    pub fn zoom_command(&mut self, cmd: crate::app::commands::Command, doc: &PlumDocument) {
        use crate::app::commands::Command;
        let w = self.width;
        match cmd {
            Command::FitSong => self.fit_song(doc),
            Command::ZoomIn | Command::ZoomOut => {
                let f = if cmd == Command::ZoomIn { 1.25 } else { 0.8 };
                let span = viewport::zoom_span(doc, 0).max(doc.duration_ticks());
                self.view.zoom_time(w / 2.0, f, span, w);
            }
            Command::ResetZoom => {
                let _ = self.set_zoom_x_percent(100.0, doc);
                self.row_h = DEFAULT_ROW_H;
            }
            Command::ZoomInY | Command::ZoomOutY => {
                let f = viewport::Y_ZOOM_STEP as f32;
                let h = if cmd == Command::ZoomInY {
                    self.row_h * f
                } else {
                    self.row_h / f
                };
                self.row_h = h.clamp(MIN_ROW_H, MAX_ROW_H);
            }
            _ => {}
        }
    }
}

/// Which part of a clip drawn at `rect` the pointer is on: the edges are
/// `EDGE_PX` wide, never more than a third of the clip.
fn edge_part(rect: Rect, pos: Pos2) -> ClipHit {
    let e = EDGE_PX.min(rect.width() / 3.0);
    if pos.x < rect.left() + e {
        ClipHit::LeftEdge
    } else if pos.x >= rect.right() - e {
        ClipHit::RightEdge
    } else {
        ClipHit::Body
    }
}

/// A clip-menu command.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MenuChoice {
    /// An item of the clip's track menu (Track “name” ▸).
    Track(crate::ui::track_panel::TrackAction),
    Open,
    /// `true`: linked.
    Duplicate(bool),
    Mute(bool),
    Rename,
    Color,
    Delete,
    /// V09-S12: open the merge Preview.
    Merge(plumspirit_engine::editor::clip_merge::MergeKind),
    /// V09-S13: open the Paste Repeat dialog.
    PasteRepeat,
    /// B1-0097: Slice at the edit cursor, Copy, Cut.
    Slice,
    Copy,
    Cut,
    /// B2-F29: Nudge Left / Right, Zoom to Selection (the app runs them).
    Command(crate::app::commands::Command),
}

/// Whether each merge can run on the selection (`Err`: why not).
type MergeChecks = [(
    plumspirit_engine::editor::clip_merge::MergeKind,
    Result<(), String>,
); 2];

/// The clip menu for target `c` (the selection holds it; `several`: more
/// clips are selected; `merge`: the two merge commands' eligibility).
#[allow(clippy::too_many_arguments)]
fn clip_menu_ui(
    ui: &mut egui::Ui,
    c: &ClipRect,
    several: bool,
    editable: bool,
    at_start: bool,
    track: Option<&crate::ui::track_panel::TrackMenu<'_>>,
    merge: &MergeChecks,
) -> Option<MenuChoice> {
    use plumspirit_engine::editor::clip_merge::MergeKind;
    let mut out = None;
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, tip: &str, ch: MenuChoice| {
        // B1-0066: the live keys (the user's keybinds), not fixed text.
        use super::commands::Command;
        use super::keybinds::first_key;
        let keys = match ch {
            MenuChoice::Open => first_key(Command::OpenClip),
            MenuChoice::Duplicate(true) => first_key(Command::OpenTool("duplicate")),
            MenuChoice::Delete => first_key(Command::Delete),
            MenuChoice::Merge(MergeKind::Sequence) => {
                first_key(Command::MergeClips(MergeKind::Sequence))
            }
            MenuChoice::Merge(MergeKind::Stack) => first_key(Command::MergeClips(MergeKind::Stack)),
            MenuChoice::PasteRepeat => first_key(Command::PasteRepeat),
            MenuChoice::Slice => first_key(Command::SliceNotes),
            MenuChoice::Copy => first_key(Command::Copy),
            MenuChoice::Cut => first_key(Command::Cut),
            MenuChoice::Command(cmd) => first_key(cmd),
            _ => String::new(),
        };
        if ui
            .add_enabled(enabled, egui::Button::new(label).shortcut_text(keys))
            .on_hover_text(tip)
            .on_disabled_hover_text(tip)
            .clicked()
        {
            out = Some(ch.clone());
            ui.close();
        }
    };
    item(
        ui,
        true,
        "Open in Piano Roll",
        "Edit this clip's notes in the Piano Roll",
        MenuChoice::Open,
    );
    ui.separator();
    let what = if several {
        "the selected clips"
    } else {
        "this clip"
    };
    // B1-0097: the clip clipboard and Slice, as in the Piano Roll.
    item(
        ui,
        true,
        "Copy",
        &format!("Copies {what} (with their notes) to paste on any track"),
        MenuChoice::Copy,
    );
    item(
        ui,
        editable,
        "Cut",
        &format!("Copies {what} and deletes them (one Undo step)"),
        MenuChoice::Cut,
    );
    item(
        ui,
        editable,
        "Slice at Edit Cursor",
        &format!(
            "Splits {what} in two where the edit cursor is; both parts show the same notes and nothing is cut from them (drag an edge back to see everything again)"
        ),
        MenuChoice::Slice,
    );
    ui.separator();
    item(
        ui,
        editable,
        "Duplicate (Linked)",
        &format!("A copy of {what} right after each: the same notes — editing one edits both (∞)"),
        MenuChoice::Duplicate(true),
    );
    item(
        ui,
        editable,
        "Duplicate (Independent Copy)",
        &format!(
            "A copy of {what} right after each with its own notes — editing one never changes the other"
        ),
        MenuChoice::Duplicate(false),
    );
    // V09-S13 (§088): copies of the selection a number of times, end to
    // end or evenly spaced (a dialog first).
    item(
        ui,
        editable,
        "Paste Repeat…",
        &format!(
            "Paste {what} again a number of times, end to end or one every so many bars, beats or seconds (a dialog shows where first)"
        ),
        MenuChoice::PasteRepeat,
    );
    // B2-F29 (§023): the nudges and Zoom to Selection, with their keys.
    {
        use super::commands::Command;
        item(
            ui,
            editable,
            "Nudge Right",
            &format!("Moves {what} one Snap step later (one beat with Snap off); one Undo step"),
            MenuChoice::Command(Command::NudgeClipsRight),
        );
        item(
            ui,
            editable && !at_start,
            "Nudge Left",
            &if at_start && several {
                "A selected clip already starts at the beginning of the song".to_owned()
            } else if at_start {
                "This clip already starts at the beginning of the song".to_owned()
            } else {
                format!(
                    "Moves {what} one Snap step earlier (never before the song's start); one Undo step"
                )
            },
            MenuChoice::Command(Command::NudgeClipsLeft),
        );
        item(
            ui,
            true,
            "Zoom to Selection",
            &format!("Shows {what} across the Track List"),
            MenuChoice::Command(Command::ZoomToSelection),
        );
    }
    ui.separator();
    let (label, tip) = match (c.muted, several) {
        (false, false) => (
            "Mute",
            "A muted clip is shown but does not play, export or count",
        ),
        (false, true) => (
            "Mute Selected Clips",
            "Muted clips are shown but do not play, export or count",
        ),
        (true, false) => ("Unmute", "The clip plays, exports and counts again"),
        (true, true) => (
            "Unmute Selected Clips",
            "The clips play, export and count again",
        ),
    };
    item(ui, editable, label, tip, MenuChoice::Mute(!c.muted));
    item(
        ui,
        editable,
        "Rename…",
        "The clip's name (shown on the clip)",
        MenuChoice::Rename,
    );
    item(
        ui,
        editable,
        "Clip Color…",
        &format!("The color of {what} (or the track's)"),
        MenuChoice::Color,
    );
    ui.separator();
    // V09-S12 (§047): two merges, never one ambiguous "Merge"; each says
    // why it cannot run on this selection.
    for (kind, check) in merge {
        let (label, does) = match kind {
            MergeKind::Sequence => (
                "Merge Clips in Sequence…",
                "The selected clips on this track become one clip, their timing kept (a Preview shows the result first)",
            ),
            MergeKind::Stack => (
                "Merge Clips Stacked…",
                "The selected clips on several tracks that play together become one clip on one of those tracks (a Preview shows the result first)",
            ),
        };
        let tip = match check {
            Ok(()) => does.to_string(),
            Err(why) => why.clone(),
        };
        item(
            ui,
            editable && check.is_ok(),
            label,
            &tip,
            MenuChoice::Merge(*kind),
        );
    }
    ui.separator();
    item(
        ui,
        editable,
        if several {
            "Delete Selected Clips"
        } else {
            "Delete Clip"
        },
        "A track's own clip is removed from the song (the track keeps its notes, unplaced; Undo brings the clip back); an independent copy is deleted with its notes",
        MenuChoice::Delete,
    );
    // V09-S10 (§043): the clip's track from the clip itself.
    if let Some(m) = track
        && let Some(t) = m.doc.track(m.index)
    {
        ui.separator();
        ui.menu_button(
            format!("Track “{}”", t.label_with(m.row.ordinal)),
            |ui| {
                if let Some(a) = crate::ui::track_panel::track_menu(ui, m) {
                    out = Some(MenuChoice::Track(a));
                }
            },
        );
    }
    out
}

/// A row-menu command.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowChoice {
    /// Add Piano Roll Clip at the clicked tick.
    AddClip,
    /// B1-0097: paste the clip clipboard at the clicked tick on this row.
    Paste,
    /// An item of the row's track menu.
    Track(crate::ui::track_panel::TrackAction),
}

/// The menu of a track row's empty timeline (§041, §199): Add Piano Roll
/// Clip Here, then the row's track menu in "Track “name” ▸" — the panel's
/// own builder, so the actions are the same from the row as from the panel.
fn row_menu_ui(
    ui: &mut egui::Ui,
    m: &crate::ui::track_panel::TrackMenu<'_>,
    can_paste: bool,
) -> Option<RowChoice> {
    let mut out = None;
    let track = m.doc.track(m.index)?;
    if ui
        .add_enabled(m.editable, egui::Button::new("Add Piano Roll Clip Here"))
        .on_hover_text("A new empty clip on this track where you clicked (one bar; Insert adds one at the edit cursor)")
        .clicked()
    {
        out = Some(RowChoice::AddClip);
        ui.close();
    }
    // B1-0097: the copied clips, their first one where you clicked.
    let tip = if can_paste {
        "Pastes the copied clips (independent copies with their own notes): the first where you clicked, the others on the rows below as they were"
    } else {
        "Copy or cut clips first"
    };
    if ui
        .add_enabled(
            m.editable && can_paste,
            egui::Button::new("Paste Clips Here")
                .shortcut_text(super::keybinds::first_key(super::commands::Command::Paste)),
        )
        .on_hover_text(tip)
        .on_disabled_hover_text(tip)
        .clicked()
    {
        out = Some(RowChoice::Paste);
        ui.close();
    }
    ui.separator();
    let name = track.label_with(m.row.ordinal);
    ui.menu_button(format!("Track “{name}”"), |ui| {
        if let Some(a) = crate::ui::track_panel::track_menu(ui, m) {
            out = Some(RowChoice::Track(a));
        }
    });
    out
}

impl super::PlumSpiritApp {
    /// Ctrl+↑ / Ctrl+↓ (V09-0142): the previous / next track row of the
    /// track list becomes the active track (folder headers are skipped; from
    /// the Conductor, ↓ goes to the first track), scrolled into view.
    pub(super) fn step_track(&mut self, down: bool) {
        let generation = self.doc_generation();
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let rows: Vec<TreeRowView> = self
            .tree_rows
            .rows(&o.doc, generation, &self.collapsed)
            .to_vec();
        let current = if self.conductor_selected() {
            None
        } else {
            rows.iter()
                .position(|r| !r.header && r.index == self.active_track())
        };
        let next = match (current, down) {
            (None, true) => rows.iter().position(|r| !r.header),
            (None, false) => None,
            (Some(r), true) => (r + 1..rows.len()).find(|&k| !rows[k].header),
            (Some(r), false) => (0..r).rev().find(|&k| !rows[k].header),
        };
        let Some(r) = next else {
            return;
        };
        self.select_track(rows[r].index);
        let g = &self.row_geometry;
        let y = g.y_of(r);
        if g.row_h > 0.0 && (y < g.list.top() || y + g.row_h > g.list.bottom()) {
            let to = r as f32 * g.row_h - (g.list.height() - g.row_h) / 2.0;
            self.track_list_scroll = Some(to.clamp(0.0, g.max_scroll()));
        }
    }

    /// Shows `v` in the central area (gestures of the other view end).
    pub(super) fn set_main_view(&mut self, v: MainView) {
        if self.main_view == v {
            return;
        }
        self.track_list.cancel_gesture();
        self.roll.cancel_gesture();
        self.main_view = v;
    }

    /// One frame of the Track List in the central area (`rows`: the panel's
    /// visible rows, drawn this frame at `self.row_geometry`).
    pub(super) fn track_list_frame(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[TreeRowView],
        pal: &Palette,
        dark: bool,
        running: bool,
        playhead: Option<u64>,
    ) -> Option<TrackListOutput> {
        let generation = self.doc_generation();
        let meter = self.meter_map().cloned()?;
        let doc = self.open.as_ref()?.doc.clone();
        if running
            && self.settings.follow_playhead
            && !self.follow_suspended
            && !self.track_list.is_dragging()
        {
            let style = if self.settings.smooth_follow {
                FollowStyle::Smooth
            } else {
                FollowStyle::Page
            };
            self.track_list.follow_playhead(
                self.transport.position(),
                doc.duration_ticks(),
                style,
                f64::from(self.settings.follow_position.min(100)) / 100.0,
            );
        }
        let loop_cfg = self.loop_config();
        let conductor = self.conductor_selected();
        let geometry = std::mem::take(&mut self.row_geometry);
        let selected_tracks = self.selected_tracks.clone();
        let input = TrackListInput {
            doc: &doc,
            generation,
            rows,
            geometry: &geometry,
            meter: &meter,
            palette: pal,
            dark,
            keyboard: self.dialog.is_none()
                && !ui.ctx().egui_wants_keyboard_input()
                && !self.focus_at_frame_start,
            playhead,
            cursor: self.roll.cursor_tick,
            anchor: self.transport.anchor(),
            loop_range: loop_cfg.range,
            loop_enabled: loop_cfg.enabled,
            range: self.time_selection,
            snap: self.roll.snap.unit_ticks(doc.division()),
            active: (!conductor).then(|| self.active_track()),
            editable: self.job.is_none() && self.dialog.is_none(),
            dense: Some(self.roll.dense_mut()),
            note_colors: self.settings.display_note_colors,
            clip_transparency: self.settings.clip_body_transparency,
            selected_tracks: &selected_tracks,
            merge_outline: match &self.dialog {
                Some(super::dialogs::Dialog::MergeClips(d)) => {
                    Some((d.plan.dest, d.plan.start, d.plan.end))
                }
                _ => None,
            },
            repeat_outline: match &self.dialog {
                Some(super::dialogs::Dialog::RepeatClips(d)) => d.outline.clone(),
                _ => None,
            },
            can_paste: self.clip_clipboard.as_ref().is_some_and(|c| !c.is_empty()),
            overlay_chips: super::progressor::overlay_controls(&self.overlay_views),
        };
        let out = self.track_list.show(ui, input);
        self.row_geometry = geometry;
        if self.track_list.user_panned && running && self.settings.follow_playhead {
            self.follow_suspended = true;
        }
        self.track_list_readouts(ui, pal);
        Some(out)
    }

    /// B1-0107: the Progressor and Selection Analytics over the Track List's
    /// clip area, as over the roll (same text, settings, chips; fading to
    /// 15 % under the pointer so mini notes show through, collapsible).
    fn track_list_readouts(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let area = self.track_list.areas.list;
        self.overlay_views = [None, None];
        if self.settings.hide_text_overlays || area.width() < 40.0 || area.height() < 20.0 {
            return;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if let Some((text, stale)) = self.progressor_text() {
            self.overlay_views[0] = super::progressor::paint_overlay(
                &ui.painter_at(area),
                area,
                &text,
                &self.settings.progressor,
                pal,
                stale,
                super::progressor::OverlayState {
                    advanced: None,
                    pointer,
                    collapsed: self.settings.progressor.collapsed,
                    chip: true,
                },
            );
        }
        self.overlay_views[1] =
            self.paint_selection_stats(&ui.painter_at(area), area, pal, pointer);
        self.sel_readout_last = self.overlay_views[1].map(|v| v.rect);
        self.overlay_chips(ui, area);
    }

    /// Applies a clip edit of the Track List through the history (one undo
    /// step); a failure is a sentence in the status bar. Duplicates become
    /// the selection.
    pub(super) fn apply_clip_intent(&mut self, intent: ClipIntent) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        if let ClipIntent::Open(id) = intent {
            self.open_clip(&doc, id);
            return;
        }
        if let ClipIntent::Merge(kind) = intent {
            self.start_clip_merge(kind);
            return;
        }
        if let ClipIntent::PasteRepeat = intent {
            self.start_paste_repeat();
            return;
        }
        // B1-0097: the clip clipboard and Slice.
        let intent = match intent {
            ClipIntent::Copy(ids) => {
                self.copy_clips(&doc, &ids);
                return;
            }
            ClipIntent::Cut(ids) => {
                let copied = self.copy_clips(&doc, &ids);
                // The document is shared until this clone goes.
                drop(doc);
                if copied {
                    self.apply_clip_intent(ClipIntent::Delete(ids));
                }
                return;
            }
            ClipIntent::Paste { track, at } => {
                drop(doc);
                self.paste_clips(track, at);
                return;
            }
            ClipIntent::Slice(ids) => {
                let ids = if ids.is_empty() {
                    // Nothing selected: the active track's clips.
                    let t = doc.track(self.active_track()).map(|t| t.id());
                    t.map_or_else(Vec::new, |t| {
                        doc.clips().placements_on(t).iter().map(|p| p.id).collect()
                    })
                } else {
                    ids
                };
                let at = self.roll.cursor_tick;
                let op = match plumspirit_engine::editor::clips::slice_clips(&doc, &ids, at) {
                    Ok(op) => op,
                    Err(e) => {
                        self.set_status(e.to_string(), crate::ui::status_bar::StatusKind::Error);
                        return;
                    }
                };
                let first_new = doc.clips().layout().next_clip;
                drop(doc);
                if self.apply_op(op).is_some() {
                    let added = self
                        .doc()
                        .map_or(first_new, |d| d.clips().layout().next_clip)
                        - first_new;
                    self.set_status(
                        format!(
                            "Sliced {} at the edit cursor: both parts show the same notes.",
                            if added == 1 {
                                "1 clip".to_string()
                            } else {
                                format!("{added} clips")
                            }
                        ),
                        crate::ui::status_bar::StatusKind::Info,
                    );
                }
                return;
            }
            other => other,
        };
        let first_new = doc.clips().layout().next_clip;
        let op = match plan(&doc, &intent) {
            Ok(Some(op)) => op,
            Ok(None) => return,
            Err(e) => {
                self.set_status(e.to_string(), crate::ui::status_bar::StatusKind::Error);
                return;
            }
        };
        // B1-0097: what a Delete removes, said after it (Undo restores it).
        let deleted = match &intent {
            ClipIntent::Delete(ids) => Some(deleted_notes(&doc, ids)),
            _ => None,
        };
        drop(doc);
        if self.apply_op(op).is_none() {
            return;
        }
        if let (Some(notes), ClipIntent::Delete(ids)) = (deleted, &intent) {
            let clips = if ids.len() == 1 {
                "1 clip".to_string()
            } else {
                format!("{} clips", ids.len())
            };
            let what = if notes == 0 {
                "their tracks keep their notes".to_string()
            } else {
                format!(
                    "{} notes of independent copies went with them",
                    plumspirit_engine::midi::group_digits(notes as u64)
                )
            };
            self.set_status(
                format!("Deleted {clips} ({what}); Undo brings them back."),
                crate::ui::status_bar::StatusKind::Info,
            );
        }
        match &intent {
            ClipIntent::Duplicate { copies, .. } => {
                self.track_list.selected = (0..copies.len() as u32)
                    .map(|k| ClipId::Placed(first_new + k))
                    .collect();
            }
            // The added clip is the selection and its track the active one
            // (Enter opens it).
            ClipIntent::Add { track, .. } => {
                self.track_list.select_only(ClipId::Placed(first_new));
                self.select_track(*track);
                self.set_status(
                    format!(
                        "Added a Piano Roll Clip to {}.",
                        self.track_display_name(*track)
                    ),
                    crate::ui::status_bar::StatusKind::Info,
                );
            }
            _ => {}
        }
    }

    /// B1-0097: Copy Clips into the app's clip clipboard (`false`: nothing
    /// was copied, the status bar says why).
    fn copy_clips(&mut self, doc: &PlumDocument, ids: &[ClipId]) -> bool {
        match plumspirit_engine::editor::clips::copy_clips(doc, ids) {
            Ok(c) => {
                let n = c.entries.len();
                self.clip_clipboard = Some(c);
                self.set_status(
                    format!(
                        "Copied {} (Ctrl+V pastes at the edit cursor on the active track).",
                        if n == 1 {
                            "1 clip".to_string()
                        } else {
                            format!("{n} clips")
                        }
                    ),
                    crate::ui::status_bar::StatusKind::Info,
                );
                true
            }
            Err(e) => {
                self.set_status(e.to_string(), crate::ui::status_bar::StatusKind::Error);
                false
            }
        }
    }

    /// B1-0097: Paste Clips — on `track` at `at`, or on the active track at
    /// the edit cursor; the pasted clips become the selection.
    fn paste_clips(&mut self, track: Option<usize>, at: Option<u64>) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let Some(clip) = self.clip_clipboard.clone() else {
            self.set_status(
                "Copy or cut clips first: there is nothing to paste.",
                crate::ui::status_bar::StatusKind::Warning,
            );
            return;
        };
        if track.is_none() && self.conductor_selected() {
            self.set_status(
                "Clips paste on a track: choose a track first (the Conductor holds no clips).",
                crate::ui::status_bar::StatusKind::Warning,
            );
            return;
        }
        let track = track.unwrap_or_else(|| self.active_track());
        let at = at.unwrap_or(self.roll.cursor_tick);
        let first_new = doc.clips().layout().next_clip;
        let op = match plumspirit_engine::editor::clips::paste_clips(&doc, &clip, track, at) {
            Ok(op) => op,
            Err(e) => {
                self.set_status(e.to_string(), crate::ui::status_bar::StatusKind::Error);
                return;
            }
        };
        drop(doc);
        if self.apply_op(op).is_some() {
            let n = clip.entries.len() as u32;
            self.track_list.selected = (0..n).map(|k| ClipId::Placed(first_new + k)).collect();
            self.set_status(
                format!(
                    "Pasted {} as independent copies.",
                    if n == 1 {
                        "1 clip".to_string()
                    } else {
                        format!("{n} clips")
                    }
                ),
                crate::ui::status_bar::StatusKind::Info,
            );
        }
    }

    /// Add Piano Roll Clip on track `track` at the edit cursor (the panel's
    /// track menu, Song menu and Insert): snapped down, one bar.
    pub(super) fn add_clip_at_cursor(&mut self, track: usize) {
        let Some(doc) = self.doc() else {
            return;
        };
        let division = doc.division();
        let snap = self.roll.snap.unit_ticks(division);
        let Some(meter) = self.meter_map().cloned() else {
            return;
        };
        let (start, length) = new_clip_window(&meter, division, snap, self.roll.cursor_tick);
        self.apply_clip_intent(ClipIntent::Add {
            track,
            start,
            length,
        });
    }

    /// Open in Piano Roll: the content's track, its window in view. An
    /// independent copy (owned content) gets its own editor with the clip
    /// Piano Roll (V09-S11).
    fn open_clip(&mut self, _doc: &PlumDocument, id: ClipId) {
        // V09-S11: any clip — a track's own content or an owned content —
        // opens in the Piano Roll as a session (`app/clip_session.rs`).
        self.open_clip_session(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::track_panel::{self, FolderPanel, RowLayout};
    use plumspirit_engine::editor::clips::{add_clip, duplicate_clips};
    use plumspirit_engine::history::{History, HistoryLimits};
    use plumspirit_engine::model::project::Bpm;
    use plumspirit_engine::model::{Division, NoteAttrs, NoteOffKind, NoteRecord};

    const SCALES: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

    /// B1-0056 / B1-0069: a clip header's title is readable (WCAG AA large
    /// text, ≥ 3 : 1; most ≥ 4.5) on every automatic track colour.
    #[test]
    fn clip_header_titles_are_readable_on_every_track_colour() {
        let mut worst = f64::MAX;
        for dark in [true, false] {
            for id in 0..96u32 {
                let fill = theme::auto_track_color(id, dark, 255);
                let t = text_on(fill);
                let c =
                    theme::contrast_ratio([t.r(), t.g(), t.b()], [fill.r(), fill.g(), fill.b()]);
                worst = worst.min(c);
                assert!(c >= 3.0, "track {id} ({dark}): {c}");
            }
        }
        assert!(worst >= 3.0);
    }

    fn note(s: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    /// `tracks` tracks of notes; every track also gets `extra` added clips
    /// (owned contents) after its own content, and track 0 a linked
    /// duplicate of itself.
    fn song(tracks: usize, extra: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        for _ in 1..tracks {
            d.add_track().unwrap();
        }
        for ti in 0..tracks {
            let recs: Vec<NoteRecord> = (0..8u64)
                .map(|k| note(k * 240, 200, 60 + (ti % 12) as u8))
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let mut h = History::new(HistoryLimits::default());
        for ti in 0..tracks {
            for k in 0..extra {
                let op = add_clip(&d, ti, 2400 + k * 1920, 960).unwrap();
                h.apply(&mut d, op).unwrap();
            }
        }
        let t0 = d.track(0).unwrap().id();
        let op = duplicate_clips(&d, &[(ClipId::Primary(t0), 0, 9600)], true).unwrap();
        h.apply(&mut d, op).unwrap();
        d
    }

    /// `song(tracks, 0)` plus `per` linked instances of every track's own
    /// content on that track (one clip operation).
    fn many_clips(tracks: usize, per: u64) -> PlumDocument {
        use plumspirit_engine::model::{ClipPlacement, ContentRef};
        let mut d = song(tracks, 0);
        let mut layout = d.clips().layout();
        for ti in 0..tracks {
            let id = d.track(ti).unwrap().id();
            for k in 0..per {
                let n = layout.next_clip;
                layout.next_clip += 1;
                layout.placements.push(ClipPlacement {
                    id: ClipId::Placed(n),
                    track: id,
                    content: ContentRef::Track(id),
                    start: 2400 + k * 1920,
                    offset: 0,
                    length: Some(960),
                    muted: false,
                    name: None,
                    color: None,
                    revision: 0,
                });
            }
        }
        layout.placements.sort_by_key(|p| p.id);
        let op = plumspirit_engine::history::Operation::SetClips {
            layout: Box::new(layout),
            contents: Vec::new(),
            label: "Add Clips",
        };
        History::new(HistoryLimits::default())
            .apply(&mut d, op)
            .unwrap();
        d
    }

    fn rows_of(d: &PlumDocument) -> Vec<TreeRowView> {
        (0..d.track_count())
            .map(|i| TreeRowView {
                index: i,
                depth: 0,
                has_children: false,
                collapsed: false,
                ordinal: i as u32 + 1,
                header: false,
                folder: 0,
            })
            .collect()
    }

    /// The panel and the Track List driven together frame by frame, laid out
    /// as the app lays them out (panel on the left, timeline in the centre).
    struct Rig {
        ctx: egui::Context,
        doc: PlumDocument,
        rows: Vec<TreeRowView>,
        tl: TrackList,
        geo: RowGeometry,
        scroll_to: Option<f32>,
        out: Vec<TrackListOutput>,
        meter: MeterMap,
        cursor: u64,
        size: Vec2,
        mods: egui::Modifiers,
        sent_mods: egui::Modifiers,
        history: History,
        /// Clip edits applied through `history` (as the app does).
        applied: Vec<ClipIntent>,
        /// Keys pressed in the last frame that the Track List left for the
        /// app's shortcuts (not consumed).
        unconsumed: Vec<Key>,
        /// The tile service (inline builds) and Display Note Colors.
        dense: DenseRenderer,
        note_colors: bool,
        /// The last frame's shapes.
        shapes: Vec<egui::epaint::ClippedShape>,
        /// Actions the panel returned (its menus).
        panel: Vec<track_panel::TrackAction>,
        /// The merge Preview's outline the app passes (V09-S12).
        merge_outline: Option<(usize, u64, u64)>,
        /// The Paste Repeat dialog's outlines (V09-S13).
        repeat_outline: Option<super::super::clip_repeat_ui::RepeatOutline>,
    }

    impl Rig {
        fn new(ppp: f32, doc: PlumDocument) -> Rig {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let rows = rows_of(&doc);
            let mut r = Rig {
                ctx,
                rows,
                meter: MeterMap::new(Division::ppqn(480), &[(0, 4, 2)]),
                doc,
                tl: TrackList::default(),
                geo: RowGeometry::default(),
                scroll_to: None,
                out: Vec::new(),
                cursor: 0,
                size: Vec2::new(1000.0, 600.0),
                mods: egui::Modifiers::NONE,
                sent_mods: egui::Modifiers::NONE,
                history: History::new(HistoryLimits::default()),
                applied: Vec::new(),
                unconsumed: Vec::new(),
                dense: DenseRenderer::new(true),
                note_colors: true,
                shapes: Vec::new(),
                panel: Vec::new(),
                merge_outline: None,
                repeat_outline: None,
            };
            r.frame(vec![]);
            r.frame(vec![]);
            assert_eq!(r.ctx.pixels_per_point(), ppp);
            r
        }

        fn frame(&mut self, mut events: Vec<egui::Event>) {
            if self.mods != self.sent_mods {
                events.insert(0, egui::Event::ModifiersChanged(self.mods));
                self.sent_mods = self.mods;
            }
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                events,
                ..Default::default()
            };
            let pal = &theme::DARK;
            let (doc, rows, tl, meter) = (&self.doc, &self.rows, &mut self.tl, &self.meter);
            let (dense, note_colors) = (&mut self.dense, self.note_colors);
            let mut geo = RowGeometry::default();
            let mut out = TrackListOutput::default();
            let scroll_to = self.scroll_to.take();
            let cursor = self.cursor;
            let merge_outline = self.merge_outline;
            let repeat_outline = self.repeat_outline.clone();
            let mut unconsumed = Vec::new();
            let mut panel_act = None;
            let mut o = self.ctx.run_ui(input, |ui| {
                egui::Panel::left("tracks")
                    .resizable(false)
                    .exact_size(track_panel::PANEL_WIDTH)
                    .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)))
                    .show(ui, |ui| {
                        let mut renaming = None;
                        let g;
                        (panel_act, g) = track_panel::track_panel(
                            ui,
                            Some(doc),
                            0,
                            false,
                            true,
                            pal,
                            &[],
                            true,
                            &BTreeSet::new(),
                            rows,
                            &doc.placed_note_counts(),
                            "120",
                            "",
                            None,
                            FolderPanel {
                                renaming: &mut renaming,
                            },
                            RowLayout {
                                row_h: tl.row_h,
                                scroll_to,
                            },
                        );
                        geo = g;
                    });
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    out = tl.show(
                        ui,
                        TrackListInput {
                            doc,
                            generation: 1,
                            rows,
                            geometry: &geo,
                            meter,
                            palette: pal,
                            dark: true,
                            keyboard: true,
                            playhead: None,
                            cursor,
                            anchor: None,
                            loop_range: None,
                            loop_enabled: false,
                            range: None,
                            snap: Some(120),
                            active: Some(0),
                            editable: true,
                            dense: Some(&mut *dense),
                            note_colors,
                            clip_transparency: crate::settings::DEFAULT_CLIP_BODY_TRANSPARENCY,
                            selected_tracks: &BTreeSet::new(),
                            merge_outline,
                            repeat_outline: repeat_outline.clone(),
                            can_paste: true,
                            overlay_chips: [None; 3],
                        },
                    );
                    unconsumed = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::Key {
                                    key, pressed: true, ..
                                } => Some(*key),
                                _ => None,
                            })
                            .collect()
                    });
                });
            });
            o.textures_delta.clear();
            self.shapes = std::mem::take(&mut o.shapes);
            self.panel.extend(panel_act);
            self.unconsumed = unconsumed;
            self.geo = geo;
            self.scroll_to = out.scroll_to;
            for e in &out.edits {
                if let Some(op) = plan(&self.doc, e).unwrap() {
                    self.history.apply(&mut self.doc, op).unwrap();
                    self.applied.push(e.clone());
                }
            }
            self.out.push(out);
        }

        /// The clip layout without per-placement revisions (compared across
        /// undo / redo).
        fn layout(&self) -> plumspirit_engine::model::ClipLayout {
            let mut l = self.doc.clips().layout();
            for p in &mut l.placements {
                p.revision = 0;
            }
            l
        }

        fn secondary_click(&mut self, p: Pos2) {
            let b = |pressed| egui::Event::PointerButton {
                pos: p,
                button: PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![Self::moved(p)]);
            self.frame(vec![b(true)]);
            self.frame(vec![b(false)]);
            self.frame(vec![]);
        }

        fn moved(p: Pos2) -> egui::Event {
            egui::Event::PointerMoved(p)
        }

        fn button(&mut self, p: Pos2, pressed: bool, m: egui::Modifiers) {
            self.mods = m;
            self.frame(vec![
                Self::moved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: m,
                },
            ]);
        }

        fn click(&mut self, p: Pos2, m: egui::Modifiers) {
            self.frame(vec![Self::moved(p)]);
            self.button(p, true, m);
            self.button(p, false, m);
            self.mods = egui::Modifiers::NONE;
            self.frame(vec![]);
        }

        fn drag(&mut self, a: Pos2, b: Pos2, m: egui::Modifiers) {
            self.frame(vec![Self::moved(a)]);
            self.button(a, true, m);
            for k in 1..=4 {
                self.frame(vec![Self::moved(a + (b - a) * (k as f32 / 4.0))]);
            }
            self.button(b, false, m);
            self.mods = egui::Modifiers::NONE;
            self.frame(vec![]);
        }

        fn wheel(&mut self, p: Pos2, delta: Vec2, m: egui::Modifiers) {
            self.frame(vec![Self::moved(p)]);
            self.frame(vec![
                Self::moved(p),
                egui::Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta,
                    modifiers: m,
                    phase: egui::TouchPhase::Move,
                },
            ]);
            self.frame(vec![]);
        }

        fn drawn(&self, id: ClipId) -> Rect {
            self.tl
                .drawn
                .iter()
                .find(|(c, _)| *c == id)
                .unwrap_or_else(|| panic!("{id:?} not drawn"))
                .1
        }

        fn roll_actions(&self) -> Vec<RollAction> {
            self.out.iter().flat_map(|o| o.roll.clone()).collect()
        }

        /// Where the last frame drew `label` (a menu item), if it did.
        fn label_pos(&self, label: &str) -> Option<Pos2> {
            fn find(shape: &egui::Shape, label: &str) -> Option<Pos2> {
                match shape {
                    egui::Shape::Text(t) if t.galley.text() == label => {
                        Some(t.pos + t.galley.rect.center().to_vec2())
                    }
                    egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                    _ => None,
                }
            }
            self.shapes.iter().find_map(|c| find(&c.shape, label))
        }

        /// Clicks the menu item `label` of the open menu.
        fn click_label(&mut self, label: &str) {
            let p = self
                .label_pos(label)
                .unwrap_or_else(|| panic!("menu item {label:?} not shown"));
            self.click(p, egui::Modifiers::NONE);
        }

        /// A point on the empty timeline of visible row `r` at `tick`.
        fn row_point(&self, r: usize, tick: u64) -> Pos2 {
            let t = self.tl.view.time;
            Pos2::new(
                self.tl.areas.list.left() + t.tick_to_x(tick) as f32,
                self.geo.y_of(r) + self.geo.row_h / 2.0,
            )
        }
    }

    #[test]
    fn rows_line_up_with_the_track_panel_at_every_display_scale() {
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(12, 2));
            for row_h in [MIN_ROW_H, DEFAULT_ROW_H, 57.5, MAX_ROW_H] {
                r.tl.row_h = row_h;
                r.frame(vec![]);
                r.frame(vec![]);
                let a = r.tl.areas;
                assert!(!r.geo.rows.is_empty(), "{ppp}: the panel drew no rows");
                assert_eq!(r.geo.row_h, row_h);
                // The timeline sits beside the panel's list and Conductor row.
                assert_eq!(a.list.y_range(), r.geo.list.y_range(), "{ppp}");
                assert_eq!(a.conductor.y_range(), r.geo.conductor.y_range(), "{ppp}");
                assert!(
                    a.ruler.bottom() <= a.conductor.top() && a.ruler.height() >= 16.0,
                    "{ppp}: {a:?}"
                );
                assert!(a.list.left() >= track_panel::PANEL_WIDTH - 0.5);
                // Every drawn row is where the panel drew it, rows are row_h
                // apart, and every clip sits inside its row.
                for w in r.geo.rows.windows(2) {
                    assert!(
                        (w[1].1 - w[0].1 - row_h).abs() <= 1.0 / ppp + 0.01,
                        "{ppp} {row_h}: {:?}",
                        r.geo.rows
                    );
                }
                for &(row, top) in &r.geo.rows {
                    assert!((r.geo.y_of(row) - top).abs() < 0.01, "{ppp} {row_h}");
                    let track = r.rows[row].index;
                    let model = TrackListModel::build(&r.doc);
                    for c in model.row(track) {
                        if let Some((_, rect)) = r.tl.drawn.iter().find(|(id, _)| *id == c.id) {
                            assert!(
                                (rect.top() - (top + CLIP_INSET)).abs() < 0.01,
                                "{ppp} {row_h}"
                            );
                            assert!((rect.bottom() - (top + row_h - CLIP_INSET)).abs() < 0.01);
                            let x = a.list.left() + r.tl.view.time.tick_to_x(c.start) as f32;
                            assert!((rect.left() - x).abs() < 0.01);
                        }
                    }
                }
                assert!(r.tl.stats.clips_drawn > 0);
            }
        }
    }

    #[test]
    fn the_first_frame_fits_the_song_and_draws_every_clip() {
        for ppp in SCALES {
            let r = Rig::new(ppp, song(3, 2));
            // 3 primaries + 6 added clips + 1 linked duplicate, all in view.
            assert_eq!(r.tl.drawn.len(), 10, "{ppp}");
            let end = r.doc.duration_ticks();
            let (t0, t1) = r.tl.view.time.visible_range(r.tl.width);
            assert!(t0 == 0 && t1 >= end, "{ppp}: [{t0}, {t1}) vs {end}");
        }
    }

    #[test]
    fn the_song_stays_fitted_while_the_window_settles_until_the_view_changes() {
        let mut r = Rig::new(1.0, song(3, 2));
        let end = r.doc.duration_ticks();
        let fitted = |r: &Rig| {
            let (t0, t1) = r.tl.view.time.visible_range(r.tl.width);
            t0 == 0 && t1 >= end && t1 < end + end / 5
        };
        assert!(fitted(&r));
        r.size = Vec2::new(1400.0, 700.0);
        r.frame(vec![]);
        r.frame(vec![]);
        assert!(fitted(&r), "a wider window still shows the whole song");
        // Zoomed by the user: a resize keeps the zoom.
        let p = r.tl.areas.list.center();
        r.wheel(p, Vec2::new(0.0, 100.0), egui::Modifiers::COMMAND);
        let ppt = r.tl.view.time.pixels_per_tick;
        r.size = Vec2::new(900.0, 700.0);
        r.frame(vec![]);
        r.frame(vec![]);
        assert_eq!(r.tl.view.time.pixels_per_tick, ppt);
    }

    #[test]
    fn clicks_select_clips_and_activate_their_track() {
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(3, 2));
            let model = TrackListModel::build(&r.doc);
            let row1: Vec<ClipId> = model.row(1).iter().map(|c| c.id).collect();
            assert_eq!(row1.len(), 3);
            let none = egui::Modifiers::NONE;
            // A click selects one clip and makes its track active.
            r.out.clear();
            r.click(r.drawn(row1[1]).center(), none);
            assert_eq!(
                r.tl.selected.iter().copied().collect::<Vec<_>>(),
                vec![row1[1]],
                "{ppp}"
            );
            assert!(r.out.iter().any(|o| o.activate == Some(1)));
            // Ctrl+click adds another; Ctrl+click again removes it.
            r.click(r.drawn(row1[2]).center(), egui::Modifiers::COMMAND);
            assert_eq!(r.tl.selected.len(), 2);
            r.click(r.drawn(row1[2]).center(), egui::Modifiers::COMMAND);
            assert_eq!(r.tl.selected.len(), 1);
            // Shift+click: the row's clips from the anchor to this one.
            r.click(r.drawn(row1[0]).center(), none);
            r.click(r.drawn(row1[2]).center(), egui::Modifiers::SHIFT);
            assert_eq!(
                r.tl.selected.iter().copied().collect::<BTreeSet<_>>(),
                row1.iter().copied().collect(),
                "{ppp}"
            );
            // Selected clips are outlined: the frame still draws them all.
            assert_eq!(r.tl.stats.clips_drawn, 10);
            // A click on empty timeline clears; Escape clears too.
            let empty = Pos2::new(r.tl.areas.list.right() - 3.0, r.drawn(row1[0]).center().y);
            r.click(empty, none);
            assert!(r.tl.selected.is_empty(), "{ppp}");
            r.click(r.drawn(row1[0]).center(), none);
            r.frame(vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: none,
            }]);
            assert!(r.tl.selected.is_empty(), "{ppp}: Escape");
        }
    }

    /// A click and a Ctrl+click (or a whole drag) that arrive in one frame
    /// act in order, each at its own position with its own modifiers.
    #[test]
    fn pointer_events_of_one_frame_act_in_order() {
        use egui::Event::{PointerButton as B, PointerMoved as M};
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(3, 2));
            let model = TrackListModel::build(&r.doc);
            let (c1, c2) = (model.row(1)[1].id, model.row(2)[2].id);
            let (p1, p2) = (r.drawn(c1).center(), r.drawn(c2).center());
            let b = |pos, pressed, modifiers| B {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers,
            };
            let (none, ctrl) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND);
            r.frame(vec![
                M(p1),
                b(p1, true, none),
                b(p1, false, none),
                M(p2),
                b(p2, true, ctrl),
                b(p2, false, ctrl),
            ]);
            assert_eq!(
                r.tl.selected.iter().copied().collect::<BTreeSet<_>>(),
                [c1, c2].into_iter().collect(),
                "{ppp}"
            );
            // A whole box drag in one frame, from empty space over c1's row.
            let gap = Pos2::new(r.tl.areas.list.right() - 2.0, p1.y);
            let over = Pos2::new(r.drawn(c1).left() + 2.0, p1.y + 1.0);
            r.frame(vec![
                M(gap),
                b(gap, true, none),
                M(over),
                b(over, false, none),
            ]);
            let row1_from_c1: BTreeSet<ClipId> = model.row(1)[1..].iter().map(|c| c.id).collect();
            assert_eq!(r.tl.selected, row1_from_c1, "{ppp}");
        }
    }

    #[test]
    fn a_track_without_content_shows_no_clip() {
        let mut d = song(2, 0);
        d.add_track().unwrap();
        let empty = d.track(2).unwrap().id();
        let mut r = Rig::new(1.0, d);
        r.rows = rows_of(&r.doc);
        r.frame(vec![]);
        r.frame(vec![]);
        assert!(
            r.tl.drawn
                .iter()
                .all(|(id, _)| *id != ClipId::Primary(empty))
        );
        let y = r.geo.y_of(2) + r.tl.row_h / 2.0;
        r.click(
            Pos2::new(r.tl.areas.list.left() + 1.0, y),
            egui::Modifiers::NONE,
        );
        assert!(r.tl.selected.is_empty());
    }

    #[test]
    fn a_box_selects_the_clips_it_touches() {
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(4, 2));
            let model = TrackListModel::build(&r.doc);
            // Box over rows 1-2, around their second added clips only.
            let a1 = model.row(1)[2].id;
            let a2 = model.row(2)[2].id;
            let (r1, r2) = (r.drawn(a1), r.drawn(a2));
            let to = Pos2::new(r2.left() + 4.0, r2.bottom() - 3.0);
            // From the empty gap before a1 down to a2.
            let gap = Pos2::new(r1.left() - 3.0, r1.top() + 3.0);
            r.drag(gap, to, egui::Modifiers::NONE);
            assert_eq!(
                r.tl.selected.iter().copied().collect::<BTreeSet<_>>(),
                [a1, a2].into_iter().collect(),
                "{ppp}"
            );
            // Ctrl+box adds; a plain box replaces.
            let p0 = model.row(0)[0].id;
            let r0 = r.drawn(p0);
            r.drag(
                Pos2::new(r0.right() + 2.0, r0.top() - 1.0),
                Pos2::new(r0.right() - 6.0, r0.center().y),
                egui::Modifiers::COMMAND,
            );
            assert_eq!(r.tl.selected.len(), 3, "{ppp}");
            r.drag(
                Pos2::new(r0.right() + 2.0, r0.top() - 1.0),
                Pos2::new(r0.right() - 6.0, r0.center().y),
                egui::Modifiers::NONE,
            );
            assert_eq!(
                r.tl.selected.iter().copied().collect::<Vec<_>>(),
                vec![p0],
                "{ppp}"
            );
        }
    }

    #[test]
    fn the_ruler_seeks_selects_time_and_sets_the_loop_like_the_roll() {
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(3, 2));
            let ruler = r.tl.areas.ruler;
            let x_of =
                |r: &Rig, t: u64| r.tl.areas.list.left() + r.tl.view.time.tick_to_x(t) as f32;
            let y = ruler.center().y;
            r.out.clear();
            r.click(Pos2::new(x_of(&r, 960) + 0.2, y), egui::Modifiers::NONE);
            assert_eq!(r.roll_actions(), vec![RollAction::Seek(960)], "{ppp}");
            r.out.clear();
            r.drag(
                Pos2::new(x_of(&r, 1920), y),
                Pos2::new(x_of(&r, 480), y),
                egui::Modifiers::NONE,
            );
            assert_eq!(
                r.roll_actions(),
                vec![
                    RollAction::SetTimeSelection(Some((480, 1920))),
                    RollAction::Seek(480)
                ],
                "{ppp}"
            );
            r.out.clear();
            r.drag(
                Pos2::new(x_of(&r, 480), y),
                Pos2::new(x_of(&r, 2400), y),
                egui::Modifiers::SHIFT,
            );
            assert_eq!(
                r.roll_actions(),
                vec![RollAction::SetLoop(480, 2400)],
                "{ppp}"
            );
            assert!(!r.tl.is_dragging());
        }
    }

    #[test]
    fn the_wheel_scrolls_the_shared_list_and_ctrl_wheel_zooms_around_the_pointer() {
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(60, 1));
            let list = r.tl.areas.list;
            let first = r.geo.rows[0].0;
            assert_eq!(first, 0);
            // Wheel down 3 rows: the panel scrolls next frame, the timeline
            // follows it (same rows at the same y).
            r.wheel(
                list.center(),
                Vec2::new(0.0, -3.0 * r.tl.row_h),
                egui::Modifiers::NONE,
            );
            r.frame(vec![]);
            assert!(
                (r.geo.scroll - 3.0 * r.tl.row_h).abs() < 0.5,
                "{ppp}: {}",
                r.geo.scroll
            );
            let t3 = r.doc.track(3).unwrap().id();
            let y3 = r.geo.y_of(3);
            assert!((r.drawn(ClipId::Primary(t3)).top() - (y3 + CLIP_INSET)).abs() < 0.01);
            assert!((y3 - list.top()).abs() < 0.6, "{ppp}: row 3 at the top");
            // Ctrl+wheel zooms in around the pointer: the tick under it stays.
            let p = Pos2::new(list.left() + 300.0, list.center().y);
            let before = r.tl.view.time.x_to_tick(300.0);
            let ppt = r.tl.view.time.pixels_per_tick;
            r.wheel(p, Vec2::new(0.0, 120.0), egui::Modifiers::COMMAND);
            assert!(r.tl.view.time.pixels_per_tick > ppt * 1.5, "{ppp}");
            let after = r.tl.view.time.x_to_tick(300.0);
            let tol = (2.0 / r.tl.view.time.pixels_per_tick) as u64 + 2;
            assert!(before.abs_diff(after) <= tol, "{ppp}: {before} vs {after}");
            // Shift+wheel scrolls time.
            let o = r.tl.view.time.origin_tick;
            r.wheel(p, Vec2::new(0.0, -200.0), egui::Modifiers::SHIFT);
            assert!(r.tl.view.time.origin_tick > o, "{ppp}");
        }
    }

    /// L-F11 (contract §008): Ctrl+Shift+wheel (a mouse wheel or a
    /// two-finger trackpad scroll) zooms the Track List's row height around
    /// the pointer — the row under it stays under it — never the time axis;
    /// fractional deltas zoom fractionally; Ctrl alone still zooms time.
    #[test]
    fn ctrl_shift_wheel_zooms_the_row_height_around_the_pointer() {
        let cs = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        for ppp in SCALES {
            let mut r = Rig::new(ppp, song(60, 1));
            let list = r.tl.areas.list;
            let rh = r.tl.row_h;
            let y = r.geo.y_of(6) + rh * 0.5;
            let p = Pos2::new(list.left() + 200.0, y);
            let (ppt, origin) = (r.tl.view.time.pixels_per_tick, r.tl.view.time.origin_tick);
            r.wheel(p, Vec2::new(0.0, 60.0), cs);
            r.frame(vec![]);
            assert!(r.tl.row_h > rh * 1.2, "{ppp}: {} vs {rh}", r.tl.row_h);
            assert_eq!(
                (r.tl.view.time.pixels_per_tick, r.tl.view.time.origin_tick),
                (ppt, origin),
                "{ppp}: the time axis is not touched"
            );
            let y6 = r.geo.y_of(6);
            assert!(
                y6 <= y + 1.0 && y < y6 + r.tl.row_h + 1.0,
                "{ppp}: row 6 stays under the pointer ({y6} … {})",
                y6 + r.tl.row_h
            );
            // A fractional trackpad delta zooms a little, not a whole step.
            let before = r.tl.row_h;
            r.wheel(p, Vec2::new(0.0, 0.5), cs);
            assert!(r.tl.row_h > before && r.tl.row_h < before * 1.01, "{ppp}");
            // Out again, down to the limit, never below it.
            for _ in 0..20 {
                r.wheel(p, Vec2::new(0.0, -400.0), cs);
            }
            assert_eq!(r.tl.row_h, MIN_ROW_H, "{ppp}");
            // Ctrl alone: time, not rows.
            let rows = r.tl.row_h;
            let ppt = r.tl.view.time.pixels_per_tick;
            r.wheel(p, Vec2::new(0.0, 120.0), egui::Modifiers::COMMAND);
            assert!(r.tl.view.time.pixels_per_tick > ppt);
            assert_eq!(r.tl.row_h, rows);
        }
    }

    /// 1,000 tracks × 10 clips: only the rows the panel shows are drawn, and
    /// the model is built once while nothing changes.
    #[test]
    fn only_the_visible_rows_and_clips_are_drawn_and_the_model_is_built_once() {
        let mut r = Rig::new(1.0, many_clips(1000, 9));
        let builds = r.tl.stats.model_builds;
        assert_eq!(builds, 1);
        for _ in 0..5 {
            r.frame(vec![]);
        }
        assert_eq!(r.tl.stats.model_builds, builds);
        let visible_rows = (r.geo.list.height() / r.tl.row_h).ceil() as usize + 2;
        assert!(
            r.tl.stats.rows_drawn <= visible_rows,
            "{} rows",
            r.tl.stats.rows_drawn
        );
        assert!(
            r.tl.stats.clips_drawn <= visible_rows * 11,
            "{}",
            r.tl.stats.clips_drawn
        );
        assert!(r.tl.stats.clips_drawn >= 10);
        // An edit rebuilds it once.
        let op = add_clip(&r.doc, 999, 0, 10).unwrap();
        History::new(HistoryLimits::default())
            .apply(&mut r.doc, op)
            .unwrap();
        r.frame(vec![]);
        r.frame(vec![]);
        assert_eq!(r.tl.stats.model_builds, builds + 1);
    }

    /// Frame cost of the panel + Track List with 10,000 clips over 1,000
    /// tracks and 1,000,000 notes (V09-S08 fixture), at three zooms and
    /// scrolled to the middle rows; and the model build after an edit.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn track_list_frame_cost() {
        use std::time::{Duration, Instant};
        let mut d = PlumDocument::new_project("f".into(), Bpm::DEFAULT, 960);
        for _ in 1..1000 {
            d.add_track().unwrap();
        }
        for ti in 0..1000usize {
            let recs: Vec<NoteRecord> = (0..1000u64)
                .map(|k| {
                    note(
                        k * 60 + (ti as u64 % 7) * 13,
                        50,
                        40 + ((k + ti as u64) % 60) as u8,
                    )
                })
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let d = {
            use plumspirit_engine::model::{ClipPlacement, ContentRef};
            let mut d = d;
            let mut layout = d.clips().layout();
            for ti in 0..1000usize {
                let id = d.track(ti).unwrap().id();
                for k in 0..9u64 {
                    let n = layout.next_clip;
                    layout.next_clip += 1;
                    layout.placements.push(ClipPlacement {
                        id: ClipId::Placed(n),
                        track: id,
                        content: ContentRef::Track(id),
                        start: 61_000 + k * 61_000,
                        offset: k * 960,
                        length: Some(30_000 + k * 960),
                        muted: k == 4,
                        name: None,
                        color: None,
                        revision: 0,
                    });
                }
            }
            layout.placements.sort_by_key(|p| p.id);
            let op = plumspirit_engine::history::Operation::SetClips {
                layout: Box::new(layout),
                contents: Vec::new(),
                label: "Add Clips",
            };
            History::new(HistoryLimits::default())
                .apply(&mut d, op)
                .unwrap();
            d
        };
        let t = Instant::now();
        let m = TrackListModel::build(&d);
        println!(
            "track list model (10,000 clips, 1,000 tracks, 1M notes): build {:.2} ms, {} clips",
            t.elapsed().as_secs_f64() * 1e3,
            m.clip_count()
        );
        assert_eq!(m.clip_count(), 10_000);
        // The song length (read every frame): first call, then the memo.
        let t = Instant::now();
        let end = d.content_end_tick();
        let cold = t.elapsed();
        let t = Instant::now();
        for _ in 0..100 {
            assert_eq!(std::hint::black_box(d.content_end_tick()), end);
        }
        println!(
            "track list song end (10,000 clips): first {:.2} ms, memo {:.4} ms",
            cold.as_secs_f64() * 1e3,
            t.elapsed().as_secs_f64() * 1e3 / 100.0
        );
        let mut r = Rig::new(1.0, d);
        r.size = Vec2::new(1280.0, 720.0);
        r.frame(vec![]);
        let report = |name: &str, r: &mut Rig| {
            let mut times: Vec<Duration> = (0..60)
                .map(|_| {
                    let t = Instant::now();
                    r.frame(vec![]);
                    t.elapsed()
                })
                .collect();
            let first = times[0];
            times.sort();
            println!(
                "track list (10,000 clips, 1,000 tracks, 1M notes), {name}: first {:.2} ms, median {:.2} ms, slowest {:.2} ms ({} frames, {} clips drawn; mini notes: {} exact, {} clips as tiles, {} waiting; {} tiles built)",
                first.as_secs_f64() * 1e3,
                times[times.len() / 2].as_secs_f64() * 1e3,
                times[times.len() - 1].as_secs_f64() * 1e3,
                times.len(),
                r.tl.stats.clips_drawn,
                r.tl.stats.mini_exact_notes,
                r.tl.stats.mini_raster_clips,
                r.tl.stats.mini_waiting_clips,
                r.dense.built
            );
        };
        report("whole song", &mut r);
        // The middle rows.
        r.scroll_to = Some(500.0 * r.tl.row_h);
        r.frame(vec![]);
        report("whole song, rows 500+", &mut r);
        r.tl.view.time = TimeAxis::new(300_000, 0.02);
        report("a few bars", &mut r);
        r.tl.view.time = TimeAxis::new(300_000, 0.2);
        report("close", &mut r);
        assert_eq!(r.tl.stats.model_builds, 1);
    }

    /// V09-S09 (§046, §272, §273): editing one content rebuilds only its
    /// clips' tiles — the other clips keep theirs (ready in the first frame
    /// after the edit) and the edited clip shows its previous tiles while the
    /// new ones are built (never blank).
    #[test]
    fn editing_one_content_rebuilds_only_its_clips_tiles() {
        let mut d = song(3, 0);
        for ti in 0..3usize {
            let recs: Vec<NoteRecord> = (0..6_000u64)
                .map(|k| note(2_000 + k * 7, 30, 30 + ((k + ti as u64) % 50) as u8))
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let mut r = zoomed(1.0, d);
        r.tl.view.time = TimeAxis::new(0, 0.02);
        r.frame(vec![]);
        r.frame(vec![]);
        // Track 0's primary, its linked copy, tracks 1 and 2: all tiles.
        assert_eq!(r.tl.stats.mini_raster_clips, 4);
        assert_eq!(r.tl.stats.mini_waiting_clips, 0);
        let before = r.dense.built;
        // Edit track 1's notes (its content revision changes).
        r.doc
            .track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(2_001, 30, 90)], false)
            .unwrap();
        r.frame(vec![]);
        assert_eq!(
            r.tl.stats.mini_waiting_clips, 1,
            "only the edited content's clip waits (its old tiles stand in)"
        );
        let st = r.dense.stats[dense::Role::Clip as usize];
        assert_eq!(st.coarse, 0, "{st:?}");
        r.frame(vec![]);
        assert_eq!(r.tl.stats.mini_waiting_clips, 0);
        let rebuilt = r.dense.built - before;
        assert!(
            rebuilt > 0 && rebuilt * 3 <= before + 3,
            "{rebuilt} of {before}"
        );
    }

    /// The S09 stress fixture (§046, §207, §270, §271): 100 tracks with
    /// nested subtracks (300 tracks), 64 subchannels per content (heavy note
    /// colours), 10 clips per track, and one 1M-note content shown in a
    /// 300-point clip; frame cost at three zooms (inline tile builds, so the
    /// first frames include them), tiles built and the cache within its
    /// budget.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn mini_notes_stress_cost() {
        use plumspirit_engine::history::Operation;
        use plumspirit_engine::model::{ClipPlacement, ContentRef, SubchannelId};
        use std::time::{Duration, Instant};
        let mut d = PlumDocument::new_project("s".into(), Bpm::DEFAULT, 960);
        let mut h = History::new(HistoryLimits::default());
        for _ in 1..100 {
            d.add_track().unwrap();
        }
        // Two subtracks under every track, a third level under every tenth.
        for parent in (0..100usize).rev() {
            h.apply(&mut d, Operation::AddSubtracks { parent, count: 2 })
                .unwrap();
            if parent % 10 == 0 {
                h.apply(
                    &mut d,
                    Operation::AddSubtracks {
                        parent: parent + 1,
                        count: 1,
                    },
                )
                .unwrap();
            }
        }
        let n_tracks = d.track_count();
        for ti in 0..n_tracks {
            let recs: Vec<NoteRecord> = (0..3_000u64)
                .map(|k| NoteRecord {
                    sub: SubchannelId(((k + ti as u64) % 64) as u32),
                    ..note(k * 40, 35, 24 + ((k * 7 + ti as u64) % 80) as u8)
                })
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        // One big content: 1,000,000 notes on the last track.
        let big = n_tracks - 1;
        let recs: Vec<NoteRecord> = (0..1_000_000u64)
            .map(|k| NoteRecord {
                sub: SubchannelId((k % 64) as u32),
                ..note(k / 8 * 15, 20, 20 + (k % 90) as u8)
            })
            .collect();
        d.track_mut(big)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut layout = d.clips().layout();
        for ti in 0..n_tracks {
            let id = d.track(ti).unwrap().id();
            for k in 1..10u64 {
                let n = layout.next_clip;
                layout.next_clip += 1;
                layout.placements.push(ClipPlacement {
                    id: ClipId::Placed(n),
                    track: id,
                    content: ContentRef::Track(id),
                    start: k * 130_000,
                    offset: (k % 3) * 960,
                    length: Some(100_000),
                    muted: k == 5,
                    name: None,
                    color: None,
                    revision: 0,
                });
            }
        }
        layout.placements.sort_by_key(|p| p.id);
        h.apply(
            &mut d,
            Operation::SetClips {
                layout: Box::new(layout),
                contents: Vec::new(),
                label: "Add Clips",
            },
        )
        .unwrap();
        let clips = TrackListModel::build(&d).clip_count();
        let mut r = Rig::new(1.0, d);
        r.size = Vec2::new(1280.0, 720.0);
        r.frame(vec![]);
        let report = |name: &str, r: &mut Rig| {
            let built0 = r.dense.built;
            let mut times: Vec<Duration> = (0..40)
                .map(|_| {
                    let t = Instant::now();
                    r.frame(vec![]);
                    t.elapsed()
                })
                .collect();
            let first = times[0];
            times.sort();
            let st = r.dense.stats[dense::Role::Clip as usize];
            println!(
                "mini notes ({clips} clips, {n_tracks} tracks incl. subtracks, 64 subchannels, a 1M-note content), {name}: first {:.2} ms, median {:.2} ms, slowest {:.2} ms; {} clips drawn, {} exact notes, {} clips as tiles, {} waiting; {} tiles built here, cache {} tiles / {:.1} MB (budget {:.0} MB)",
                first.as_secs_f64() * 1e3,
                times[times.len() / 2].as_secs_f64() * 1e3,
                times[times.len() - 1].as_secs_f64() * 1e3,
                r.tl.stats.clips_drawn,
                r.tl.stats.mini_exact_notes,
                r.tl.stats.mini_raster_clips,
                r.tl.stats.mini_waiting_clips,
                r.dense.built - built0,
                st.cached,
                st.cache_bytes as f64 / 1048576.0,
                dense::BUDGET_BYTES as f64 / 1048576.0,
            );
            assert!(st.cache_bytes <= dense::BUDGET_BYTES);
            assert_eq!(r.tl.stats.mini_waiting_clips, 0, "{name}: settled");
        };
        // Whole song: the 1M-note clip is about 300 points wide.
        let song_end = r.doc.content_end_tick() as f64;
        r.tl.view.time = TimeAxis::new(0, 1_100.0 / song_end);
        report("whole song", &mut r);
        r.scroll_to = Some(150.0 * r.tl.row_h);
        r.frame(vec![]);
        report("whole song, rows 150+", &mut r);
        r.scroll_to = Some((n_tracks as f32 - 12.0) * r.tl.row_h);
        r.frame(vec![]);
        report("whole song, the last rows (the 1M-note content)", &mut r);
        r.tl.view.time = TimeAxis::new(120_000, 0.01);
        report("a few bars", &mut r);
        r.tl.view.time = TimeAxis::new(120_000, 0.2);
        report("close", &mut r);
    }

    /// A Rig zoomed so that 120 ticks are 6 points (snap steps are easy to
    /// hit): 0.05 px / tick, origin 0.
    fn zoomed(ppp: f32, doc: PlumDocument) -> Rig {
        let mut r = Rig::new(ppp, doc);
        r.tl.view.time = TimeAxis::new(0, 0.05);
        r.frame(vec![]);
        r.frame(vec![]);
        r
    }

    #[test]
    fn dragging_clips_moves_the_selection_in_one_undo_step() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(4, 2));
            let model = TrackListModel::build(&r.doc);
            let (a, b) = (model.row(1)[1].clone(), model.row(1)[2].clone());
            let none = egui::Modifiers::NONE;
            r.click(r.drawn(a.id).center(), none);
            r.click(r.drawn(b.id).center(), egui::Modifiers::COMMAND);
            assert_eq!(r.tl.selected.len(), 2);
            let before = r.layout();
            // Drag a by +960 ticks (48 points, plus a little: snapped) and one
            // row down.
            let from = r.drawn(a.id).center();
            let to = from + Vec2::new(48.0 + 1.5, r.tl.row_h);
            r.drag(from, to, none);
            assert_eq!(r.applied.len(), 1, "{ppp}: {:?}", r.applied);
            let t2 = 2;
            assert_eq!(
                r.applied[0],
                ClipIntent::Move(vec![(a.id, t2, a.start + 960), (b.id, t2, b.start + 960)]),
                "{ppp}"
            );
            let p = r.doc.clips().placement(a.id).unwrap();
            assert_eq!(
                (p.track, p.start),
                (r.doc.track(2).unwrap().id(), a.start + 960)
            );
            assert_eq!(r.history.undo_len(), 1, "{ppp}: one step for the gesture");
            let after = r.layout();
            r.history.undo(&mut r.doc).unwrap();
            assert_eq!(r.layout(), before, "{ppp}: undo restores the layout");
            r.history.redo(&mut r.doc).unwrap();
            assert_eq!(r.layout(), after, "{ppp}: redo");
            // A click (no drag) on one of several selected clips selects it alone.
            r.frame(vec![]);
            r.frame(vec![]);
            r.click(r.drawn(a.id).center(), none);
            r.click(r.drawn(b.id).center(), egui::Modifiers::COMMAND);
            assert_eq!(r.tl.selected.len(), 2);
            r.click(r.drawn(a.id).center(), none);
            assert_eq!(
                r.tl.selected.iter().copied().collect::<Vec<_>>(),
                vec![a.id],
                "{ppp}"
            );
        }
    }

    #[test]
    fn alt_drag_duplicates_linked_and_ctrl_or_jitter_never_moves() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(3, 1));
            let model = TrackListModel::build(&r.doc);
            let a = model.row(0)[0].clone();
            let from = r.drawn(a.id).center();
            // Jitter under the slop: a click, nothing applied.
            r.drag(from, from + Vec2::new(1.0, 1.0), egui::Modifiers::NONE);
            assert!(r.applied.is_empty(), "{ppp}");
            // Ctrl+drag toggles the selection, never moves.
            r.drag(from, from + Vec2::new(60.0, 0.0), egui::Modifiers::COMMAND);
            assert!(r.applied.is_empty(), "{ppp}");
            // Alt+drag: a linked duplicate where it was dropped.
            r.click(from, egui::Modifiers::NONE);
            r.drag(
                from,
                from + Vec2::new(96.0 + 1.0, 0.0),
                egui::Modifiers::ALT,
            );
            assert_eq!(
                r.applied,
                vec![ClipIntent::Duplicate {
                    copies: vec![(a.id, 0, a.start + 1920)],
                    linked: true
                }],
                "{ppp}"
            );
            let m = TrackListModel::build(&r.doc);
            let copy = m.row(0).iter().find(|c| c.start == a.start + 1920).unwrap();
            assert!(copy.linked && !copy.owned);
            assert_eq!(
                r.doc.clips().placement(a.id).unwrap().start,
                a.start,
                "the original stays"
            );
        }
    }

    #[test]
    fn edges_resize_the_window_and_trim_keeps_the_notes_in_place() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(2, 1));
            let model = TrackListModel::build(&r.doc);
            // Track 1's added clip: 960 ticks at 2400 (48 points wide).
            let c = model.row(1)[1].clone();
            assert_eq!((c.start, c.end), (2400, 3360));
            let rect = r.drawn(c.id);
            // Right edge +240 ticks (12 points).
            let edge = Pos2::new(rect.right() - 2.0, rect.center().y);
            r.drag(
                edge,
                edge + Vec2::new(12.0 + 0.5, 0.0),
                egui::Modifiers::NONE,
            );
            assert_eq!(
                r.applied.last(),
                Some(&ClipIntent::Resize {
                    id: c.id,
                    start: 2400,
                    offset: 0,
                    length: Some(1200)
                }),
                "{ppp}"
            );
            // Left edge +480 ticks: start and offset move together (the notes
            // stay where they play), the end stays.
            r.frame(vec![]);
            r.frame(vec![]);
            let rect = r.drawn(c.id);
            let edge = Pos2::new(rect.left() + 2.0, rect.center().y);
            r.drag(
                edge,
                edge + Vec2::new(24.0 + 0.5, 0.0),
                egui::Modifiers::NONE,
            );
            let p = r.doc.clips().placement(c.id).unwrap();
            assert_eq!(
                (p.start, p.offset, p.length),
                (2880, 480, Some(720)),
                "{ppp}"
            );
            assert_eq!(p.start - p.offset, 2400, "the content stays where it plays");
            // Trimming never goes before the content's first tick.
            r.frame(vec![]);
            r.frame(vec![]);
            let rect = r.drawn(c.id);
            let edge = Pos2::new(rect.left() + 2.0, rect.center().y);
            r.drag(edge, edge - Vec2::new(200.0, 0.0), egui::Modifiers::NONE);
            let p = r.doc.clips().placement(c.id).unwrap();
            assert_eq!(
                (p.start, p.offset, p.length),
                (2400, 0, Some(1200)),
                "{ppp}"
            );
            // Three gestures, three undo steps.
            assert_eq!(r.applied.len(), 3);
        }
    }

    #[test]
    fn a_move_never_lands_on_a_folder_header() {
        let mut r = zoomed(1.0, song(3, 0));
        // Rows: track 0, a folder header, track 1, track 2.
        let mut rows = rows_of(&r.doc);
        rows.insert(
            1,
            TreeRowView {
                index: 1,
                depth: 0,
                has_children: false,
                collapsed: false,
                ordinal: 1,
                header: true,
                folder: 7,
            },
        );
        r.rows = rows;
        r.frame(vec![]);
        r.frame(vec![]);
        let t0 = ClipId::Primary(r.doc.track(0).unwrap().id());
        let from = r.drawn(t0).center();
        // One row down is the header: refused (no edit: nothing moved).
        r.drag(
            from,
            from + Vec2::new(0.0, r.tl.row_h),
            egui::Modifiers::NONE,
        );
        assert!(r.applied.is_empty(), "{:?}", r.applied);
        // Two rows down is track 1.
        r.drag(
            from,
            from + Vec2::new(0.0, 2.0 * r.tl.row_h),
            egui::Modifiers::NONE,
        );
        assert_eq!(r.applied, vec![ClipIntent::Move(vec![(t0, 1, 0)])]);
    }

    /// A folder header shows its members' clips as merged faint spans (S08
    /// GUI journey: the header row was empty), also while it is collapsed.
    #[test]
    fn a_folder_header_shows_where_its_members_clips_are() {
        for ppp in SCALES {
            // Every track: its notes at 0 .. 1,880 and a clip at 2,400 .. 3,360.
            let mut d = song(3, 1);
            let (t1, t2) = (d.track(1).unwrap().id(), d.track(2).unwrap().id());
            let f = d.project_mut().folders.create("Drums", Some(t1));
            d.project_mut().folders.set_member(t1, f);
            d.project_mut().folders.set_member(t2, f);
            let mut r = zoomed(ppp, d);
            let header = |collapsed| TreeRowView {
                index: 1,
                depth: 0,
                has_children: true,
                collapsed,
                ordinal: 2,
                header: true,
                folder: f,
            };
            let mut rows = rows_of(&r.doc);
            rows.insert(1, header(false));
            r.rows = rows;
            r.frame(vec![]);
            r.frame(vec![]);
            // Two spans (0 .. 1,880 and 2,400 .. 3,360 of tracks 1 and 2,
            // merged); the clips are drawn on their own rows as before.
            assert_eq!(r.tl.stats.folder_spans_drawn, 2, "{ppp}");
            // Collapsed: the member rows are hidden, the spans stay.
            r.rows = vec![rows_of(&r.doc)[0], header(true)];
            r.frame(vec![]);
            assert_eq!(r.tl.stats.folder_spans_drawn, 2, "{ppp}");
            // Nothing of the members in view: nothing drawn.
            r.tl.view.time = TimeAxis::new(100_000, 0.05);
            r.frame(vec![]);
            assert_eq!(r.tl.stats.folder_spans_drawn, 0, "{ppp}");
        }
    }

    /// Colours of the solid rectangles drawn inside `r` (mesh vertices).
    fn mesh_colors_in(shapes: &[egui::epaint::ClippedShape], r: Rect) -> HashSet<Color32> {
        let mut out = HashSet::new();
        for c in shapes {
            if let egui::Shape::Mesh(m) = &c.shape {
                if m.texture_id != egui::TextureId::default() {
                    continue;
                }
                for v in &m.vertices {
                    if r.contains(v.pos) {
                        out.insert(v.color);
                    }
                }
            }
        }
        out
    }

    /// Textured meshes (tiles) drawn inside `r`.
    fn tiles_in(shapes: &[egui::epaint::ClippedShape], r: Rect) -> usize {
        shapes
            .iter()
            .filter(|c| match &c.shape {
                egui::Shape::Mesh(m) => {
                    m.texture_id != egui::TextureId::default()
                        && m.vertices.iter().any(|v| r.contains(v.pos))
                }
                _ => false,
            })
            .count()
    }

    /// B1-0098: a clip is a square, opaque header bar over a body in the
    /// track color at 20 % opacity (80 % transparent); the notes area starts
    /// under the header at every height, and low clips get a thin header.
    #[test]
    fn clip_layout_puts_the_notes_under_an_opaque_header_with_square_corners() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(100.0, 30.0));
        let l = clip_layout(rect);
        assert!(l.label);
        assert_eq!(
            l.header,
            Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(110.0, 33.0))
        );
        assert_eq!(
            l.notes,
            Rect::from_min_max(Pos2::new(11.0, 33.0), Pos2::new(109.0, 49.0))
        );
        let low = clip_layout(Rect::from_min_size(
            Pos2::new(0.0, 0.0),
            Vec2::new(50.0, 18.0),
        ));
        assert!(!low.label);
        assert_eq!(low.header.height(), THIN_HEADER);
        assert_eq!(low.notes.top(), THIN_HEADER);
        assert_eq!(low.notes.bottom(), 17.0);
        for h in 1..=80 {
            let r = Rect::from_min_size(Pos2::new(0.0, 5.0), Vec2::new(40.0, h as f32 * 0.5));
            let l = clip_layout(r);
            assert!(l.notes.top() >= l.header.bottom(), "{h}");
            assert!(
                r.contains_rect(l.header) && l.notes.bottom() <= r.bottom(),
                "{h}"
            );
            assert!(l.notes.height() >= 0.0, "{h}");
        }
        let fill = Color32::from_rgb(200, 120, 40);
        assert_eq!(
            body_color(fill, 80).a(),
            51,
            "80 % transparent = 20 % opacity"
        );
        assert_eq!(body_color(fill, 0), fill);
        assert_eq!(body_color(fill, 100).a(), 0);
        assert_eq!(body_color(fill, 250).a(), 0);
        assert_eq!(over(Color32::BLACK, body_color(fill, 0)), fill);
        assert_eq!(
            over(Color32::from_rgb(9, 8, 7), body_color(fill, 100)),
            Color32::from_rgb(9, 8, 7)
        );
        // One-color mini notes stay readable on the body as it shows, for
        // every automatic track color in both themes.
        let mut worst = f64::MAX;
        for dark in [true, false] {
            let bg = if dark {
                Color32::from_rgb(24, 24, 28)
            } else {
                Color32::from_rgb(246, 246, 248)
            };
            for id in 0..96u32 {
                let fill = theme::auto_track_color(id, dark, 255);
                let behind = over(bg, body_color(fill, 80));
                let [r, g, b, a] = mono_note_color(fill, behind);
                assert_eq!(a, 255);
                let c = theme::contrast_ratio([r, g, b], [behind.r(), behind.g(), behind.b()]);
                worst = worst.min(c);
            }
        }
        assert!(worst >= 3.0, "worst mini note contrast {worst:.2}");
    }

    /// B1-0098: as drawn, at every UI scale — the body is the track color
    /// at alpha 51, the header the opaque track color with the name in it,
    /// no clip shape is rounded and no mini note lies under the header.
    #[test]
    fn drawn_clips_have_a_translucent_body_an_opaque_header_and_notes_below_it() {
        for ppp in SCALES {
            for note_colors in [true, false] {
                let mut r = zoomed(ppp, song(2, 0));
                r.note_colors = note_colors;
                r.frame(vec![]);
                let id = ClipId::Primary(r.doc.track(0).unwrap().id());
                let rect = r.drawn(id);
                let lay = clip_layout(rect);
                assert!(lay.label, "{ppp}: default rows get the full header");
                let rects: Vec<&egui::epaint::RectShape> = r
                    .shapes
                    .iter()
                    .filter_map(|c| match &c.shape {
                        egui::Shape::Rect(s) if rect.contains_rect(s.rect) => Some(s),
                        _ => None,
                    })
                    .collect();
                let body = rects
                    .iter()
                    .find(|s| s.rect == rect && s.fill.a() == 51)
                    .unwrap_or_else(|| panic!("{ppp}: no 20 % body"));
                let header = rects
                    .iter()
                    .find(|s| s.rect == lay.header && s.fill.a() == 255)
                    .unwrap_or_else(|| panic!("{ppp}: no opaque header"));
                assert_eq!(
                    body.fill,
                    body_color(header.fill, 80),
                    "{ppp}: body and header share the track color"
                );
                for s in &rects {
                    assert_eq!(s.corner_radius, egui::CornerRadius::ZERO, "{ppp}: square");
                }
                let mut below = 0;
                for c in &r.shapes {
                    if let egui::Shape::Mesh(m) = &c.shape
                        && m.texture_id == egui::TextureId::default()
                    {
                        for v in m.vertices.iter().filter(|v| rect.contains(v.pos)) {
                            assert!(
                                v.pos.y >= lay.header.bottom() - 0.01,
                                "{ppp}: note under the header"
                            );
                            below += 1;
                        }
                    }
                }
                assert!(below > 0, "{ppp}: mini notes drawn");
                let label = r.doc.track(0).unwrap().label_with(1);
                let text = r.shapes.iter().find_map(|c| match &c.shape {
                    egui::Shape::Text(t)
                        if rect.contains(t.pos + t.galley.rect.center().to_vec2()) =>
                    {
                        Some(t.pos + t.galley.rect.center().to_vec2())
                    }
                    _ => None,
                });
                let at = text.unwrap_or_else(|| panic!("{ppp}: no name ({label})"));
                assert!(
                    lay.header.y_range().contains(at.y),
                    "{ppp}: the name sits in the header"
                );
            }
        }
    }

    /// V09-S09 (§044, §045, §217): a sparse clip shows its notes exactly in
    /// the note colours (Display Note Colors) or in one colour (off); a
    /// dense one shows tiles, built once — moving the clip rebuilds nothing.
    #[test]
    fn clips_show_their_notes_exact_when_sparse_and_as_tiles_when_dense() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(2, 0));
            r.frame(vec![]);
            let t0 = ClipId::Primary(r.doc.track(0).unwrap().id());
            let rect = r.drawn(t0);
            // Two tracks of 8 notes and track 0's linked copy.
            assert_eq!(r.tl.stats.mini_exact_notes, 3 * 8, "{ppp}");
            assert_eq!(r.tl.stats.mini_raster_clips, 0);
            let coloured = mesh_colors_in(&r.shapes, clip_layout(rect).notes);
            // The note colour of subchannel 1 at velocity 100 (the roll's).
            assert!(!coloured.is_empty(), "{ppp}");
            // Off: one colour per clip.
            r.note_colors = false;
            r.frame(vec![]);
            let body = clip_layout(r.drawn(t0)).notes;
            let flat = mesh_colors_in(&r.shapes, body);
            assert_eq!(r.tl.stats.mini_exact_notes, 3 * 8);
            assert!(!flat.is_empty() && flat.is_disjoint(&coloured), "{ppp}");
        }
        // A dense content: 40,000 notes in one clip → tiles (inline builds
        // at the frame's end), drawn the next frame; a move rebuilds nothing.
        let mut d = song(2, 0);
        let recs: Vec<NoteRecord> = (0..40_000u64)
            .map(|k| note(2_000 + k * 3, 40, 40 + (k % 40) as u8))
            .collect();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut r = zoomed(1.0, d);
        r.tl.view.time = TimeAxis::new(0, 0.005);
        r.frame(vec![]);
        r.frame(vec![]);
        let t1 = ClipId::Primary(r.doc.track(1).unwrap().id());
        assert_eq!(r.tl.stats.mini_raster_clips, 1);
        assert_eq!(r.tl.stats.mini_waiting_clips, 0, "built inline last frame");
        assert!(tiles_in(&r.shapes, r.drawn(t1)) > 0);
        let built = r.dense.built;
        assert!(built > 0);
        let from = r.drawn(t1).center();
        r.drag(from, from + Vec2::new(40.0, 0.0), egui::Modifiers::NONE);
        r.frame(vec![]);
        assert_eq!(r.applied.len(), 1, "moved");
        assert_eq!(r.dense.built, built, "the same tiles at another place");
        assert_eq!(r.tl.stats.mini_waiting_clips, 0);
    }

    /// V09-S10 §041: a right-click on a track row's empty timeline opens the
    /// row menu; Add Piano Roll Clip Here adds one clip at the clicked tick
    /// snapped down (Rig snap: 120) and one bar long, as one undo step.
    #[test]
    fn a_right_click_on_an_empty_row_adds_a_piano_roll_clip_there() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(3, 2));
            let before = r.layout();
            let p = r.row_point(1, 8_050);
            r.secondary_click(p);
            match r.tl.context {
                Some(MenuTarget::Row {
                    row: 1,
                    track: 1,
                    tick,
                }) => {
                    assert!(tick.abs_diff(8_050) <= 40, "{ppp}: {tick}")
                }
                c => panic!("{ppp}: {c:?}"),
            }
            assert!(r.label_pos("Track “Track 2”").is_some(), "{ppp}");
            r.click_label("Add Piano Roll Clip Here");
            let added: Vec<_> = r
                .applied
                .iter()
                .filter(|e| matches!(e, ClipIntent::Add { .. }))
                .cloned()
                .collect();
            let [
                ClipIntent::Add {
                    track,
                    start,
                    length,
                },
            ] = added[..]
            else {
                panic!("{ppp}: {added:?}")
            };
            assert_eq!((track, length), (1, 1920), "{ppp}");
            assert_eq!(start % 120, 0, "{ppp}: snapped");
            assert!(start <= 8_050 && 8_050 - start < 160, "{ppp}: {start}");
            let new = r.doc.clips().layout();
            assert_eq!(new.placements.len(), before.placements.len() + 1);
            let p = new
                .placements
                .iter()
                .find(|p| matches!(p.content, ContentRef::Owned(_)) && p.start == start)
                .expect("the added clip");
            assert_eq!((p.length, p.offset, p.muted), (Some(1920), 0, false));
            assert_eq!(r.doc.tracks()[1].id(), p.track);
            assert_eq!(r.tl.context, None, "{ppp}: the menu closed");
            // One undo step brings the layout back.
            r.history.undo(&mut r.doc).unwrap();
            assert_eq!(r.layout(), before, "{ppp}");
        }
    }

    /// V09-S10 §043: the row menu carries the row's track menu (the panel's
    /// builder): its actions come out as track actions for that row.
    #[test]
    fn the_row_menu_offers_the_rows_track_actions() {
        let mut r = zoomed(1.0, song(3, 2));
        let p = r.row_point(2, 9_000);
        r.secondary_click(p);
        r.click_label("Track “Track 3”");
        assert!(r.label_pos("Add Track Below").is_some());
        assert!(r.label_pos("Track Properties…").is_some());
        // The clip item is the row's own (at the click), not repeated.
        assert!(r.label_pos("Add Piano Roll Clip").is_none());
        r.click_label("Add Track Below");
        let acts: Vec<_> = r.out.iter().filter_map(|o| o.track.clone()).collect();
        assert_eq!(acts, vec![crate::ui::track_panel::TrackAction::AddBelow(2)]);
        assert!(r.applied.is_empty(), "no clip edit");
    }

    /// A right-click on a clip still opens the clip menu, and the row menu
    /// does not appear there.
    #[test]
    fn a_right_click_on_a_clip_is_not_a_row_click() {
        let mut r = zoomed(1.0, song(3, 2));
        let c = TrackListModel::build(&r.doc).row(1)[1].clone();
        r.secondary_click(r.drawn(c.id).center());
        assert_eq!(r.tl.context, Some(MenuTarget::Clip(c.id)));
        assert!(r.label_pos("Open in Piano Roll").is_some());
        assert!(r.label_pos("Add Piano Roll Clip Here").is_none());
    }

    /// The track actions the Track List's menus returned.
    fn track_actions(r: &Rig) -> Vec<track_panel::TrackAction> {
        r.out.iter().filter_map(|o| o.track.clone()).collect()
    }

    /// V09-S10 (CONTEXT_MATRIX.md fixes 4–6): a folder row, the Conductor
    /// lane and the space below the rows open their own menus in the Track
    /// List; their actions go to the panel's dispatcher.
    #[test]
    fn folder_rows_the_conductor_lane_and_empty_space_open_their_menus() {
        use track_panel::{ConductorAdd, TrackAction};
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(3, 0));
            let mut rows = rows_of(&r.doc);
            rows.insert(
                1,
                TreeRowView {
                    index: 1,
                    depth: 0,
                    has_children: false,
                    collapsed: false,
                    ordinal: 2,
                    header: true,
                    folder: 7,
                },
            );
            r.rows = rows;
            r.frame(vec![]);
            r.frame(vec![]);
            // A folder row.
            let p = r.row_point(1, 9_000);
            r.secondary_click(p);
            assert_eq!(
                r.tl.context,
                Some(MenuTarget::Folder {
                    id: 7,
                    collapsed: false,
                    count: 2
                }),
                "{ppp}"
            );
            assert!(r.label_pos("Add Piano Roll Clip Here").is_none(), "{ppp}");
            r.click_label("Add Track to Folder");
            assert_eq!(
                track_actions(&r),
                vec![TrackAction::FolderAddTrack(7)],
                "{ppp}"
            );
            // The Conductor lane, at the clicked tick.
            let lane = r.tl.areas.conductor;
            let p = Pos2::new(lane.left() + 200.0, lane.center().y);
            let want = r.tl.view.time.x_to_tick(200.0);
            r.secondary_click(p);
            assert_eq!(
                r.tl.context,
                Some(MenuTarget::Conductor { tick: want }),
                "{ppp}"
            );
            r.click_label("Add Marker…");
            assert_eq!(
                track_actions(&r)[1],
                TrackAction::ConductorAdd(ConductorAdd::Marker, Some(want)),
                "{ppp}"
            );
            // Below the last row.
            let y = r.geo.y_of(r.rows.len()) + 20.0;
            assert!(y < r.tl.areas.list.bottom(), "{ppp}: room below the rows");
            r.secondary_click(Pos2::new(lane.left() + 100.0, y));
            assert_eq!(r.tl.context, Some(MenuTarget::Empty), "{ppp}");
            r.click_label("New Folder");
            assert_eq!(track_actions(&r)[2], TrackAction::FolderNew(None), "{ppp}");
            assert!(r.applied.is_empty(), "{ppp}: no clip edit");
        }
    }

    /// V09-S12 (§047): the clip menu offers both merges by name; the one
    /// that can run on the selection opens the Preview (an intent, no edit
    /// yet), the one that cannot is disabled and does nothing.
    #[test]
    fn the_clip_menu_offers_both_merges_and_only_the_eligible_one_runs() {
        use plumspirit_engine::editor::clip_merge::MergeKind;
        let mut r = zoomed(1.0, song(3, 2));
        let row = TrackListModel::build(&r.doc).row(1).to_vec();
        let (a, b) = (row[0].clone(), row[1].clone());
        r.click(r.drawn(a.id).center(), egui::Modifiers::NONE);
        r.click(r.drawn(b.id).center(), egui::Modifiers::COMMAND);
        assert_eq!(r.tl.selected.len(), 2);
        let merges = |r: &Rig| -> Vec<ClipIntent> {
            r.out
                .iter()
                .flat_map(|o| &o.edits)
                .filter(|e| matches!(e, ClipIntent::Merge(_)))
                .cloned()
                .collect()
        };
        r.secondary_click(r.drawn(a.id).center());
        assert!(r.label_pos("Merge Clips Stacked…").is_some());
        r.click_label("Merge Clips in Sequence…");
        assert_eq!(merges(&r), vec![ClipIntent::Merge(MergeKind::Sequence)]);
        assert!(r.applied.is_empty(), "the Preview comes first: no edit");
        // Both clips are on one row: Stacked is disabled.
        r.secondary_click(r.drawn(b.id).center());
        r.click_label("Merge Clips Stacked…");
        assert_eq!(merges(&r).len(), 1, "a disabled entry does nothing");
        assert!(r.applied.is_empty());
    }

    /// Every clip `clips` (track, start, end) at every offset, as the dialog
    /// plans them with no function on.
    fn outline_of(
        clips: &[(u32, u64, u64)],
        offsets: &[u64],
    ) -> super::super::clip_repeat_ui::RepeatOutline {
        use super::super::clip_repeat_ui::{OutlineClip, RepeatOutline};
        RepeatOutline::new(
            offsets
                .iter()
                .flat_map(|&o| {
                    clips.iter().map(move |&(track, s, e)| OutlineClip {
                        track,
                        start: s + o,
                        end: e + o,
                        muted: false,
                    })
                })
                .collect(),
        )
    }

    /// V09-S13 (§089): the clip menu's *Paste Repeat…* opens the dialog
    /// (an intent, no edit); while it is open the copies in view are drawn
    /// as outlines — found by binary search, capped per frame — and none
    /// once it closes.
    #[test]
    fn paste_repeat_outlines_show_the_copies_in_view_and_the_menu_opens_the_dialog() {
        let mut r = zoomed(1.0, song(3, 2));
        let row = TrackListModel::build(&r.doc).row(1).to_vec();
        let a = row[1].clone();
        r.click(r.drawn(a.id).center(), egui::Modifiers::NONE);
        r.secondary_click(r.drawn(a.id).center());
        r.click_label("Paste Repeat…");
        let intents: Vec<ClipIntent> = r.out.iter().flat_map(|o| o.edits.clone()).collect();
        assert!(intents.contains(&ClipIntent::PasteRepeat));
        assert!(r.applied.is_empty(), "the dialog comes first");
        // 0.05 px per tick, 1,000 px: ticks 0 – 20,000 are in view.
        let len = a.end - a.start;
        let offsets: Vec<u64> = (1..=10_000u64).map(|k| k * len).collect();
        r.repeat_outline = Some(outline_of(&[(1, a.start, a.end)], &offsets));
        r.frame(vec![]);
        let t1 = r.tl.view.time.origin_tick
            + (r.tl.width / r.tl.view.time.pixels_per_tick).ceil() as u64;
        let want = offsets.iter().filter(|&&o| a.start + o < t1).count();
        assert!(want > 3 && want < 10_000, "{want}");
        assert_eq!(r.tl.stats.repeat_outlines, want);
        // Thousands of clips × copies in view: capped.
        r.repeat_outline = Some(outline_of(
            &vec![(1, 0, 10); 5_000],
            &(1..=10u64).collect::<Vec<_>>(),
        ));
        r.frame(vec![]);
        assert_eq!(r.tl.stats.repeat_outlines, MAX_REPEAT_OUTLINES);
        r.repeat_outline = None;
        r.frame(vec![]);
        assert_eq!(r.tl.stats.repeat_outlines, 0);
    }

    /// V09-S13 (S13 GUI journey): the copies usually go past the song's
    /// end; while the dialog is open zooming out reaches the last one, so
    /// every outline can be seen before OK — and the view's limits are the
    /// song's again once it closes.
    #[test]
    fn zooming_out_reaches_the_last_paste_repeat_copy_past_the_song_end() {
        let mut r = Rig::new(1.0, song(3, 2));
        let row = TrackListModel::build(&r.doc).row(1).to_vec();
        let a = row[1].clone();
        let song_end = r.doc.duration_ticks();
        let len = a.end - a.start;
        // Ten copies: the last ends far beyond the song.
        let offsets: Vec<u64> = (1..=10u64).map(|k| k * song_end.max(len) * 2).collect();
        let outline = outline_of(&[(1, a.start, a.end)], &offsets);
        let last_end = outline.end();
        assert!(last_end > song_end * 10, "{last_end} vs {song_end}");
        let p = Pos2::new(r.tl.areas.list.left() + 300.0, r.tl.areas.list.center().y);
        let in_view = |r: &Rig| {
            r.tl.view.time.origin_tick + (r.tl.width / r.tl.view.time.pixels_per_tick) as u64
        };
        // Without the dialog zooming out stops at the song.
        for _ in 0..12 {
            r.wheel(p, Vec2::new(0.0, -400.0), egui::Modifiers::COMMAND);
        }
        assert!(in_view(&r) < last_end / 2, "{} vs {last_end}", in_view(&r));
        // With it, it reaches the last copy: every outline in view.
        r.repeat_outline = Some(outline);
        r.frame(vec![]);
        for _ in 0..12 {
            r.wheel(p, Vec2::new(0.0, -400.0), egui::Modifiers::COMMAND);
        }
        r.frame(vec![]);
        assert!(in_view(&r) >= last_end, "{} < {last_end}", in_view(&r));
        assert_eq!(r.tl.stats.repeat_outlines, 10, "every copy in view");
    }

    /// V09-S12 (§048): while the merge Preview is open the result is drawn
    /// as an outline on the destination row over the merged span — exactly
    /// where the merged clip will be — and nowhere once it closes.
    #[test]
    fn the_merge_preview_outline_covers_the_merged_span_on_its_row() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(3, 2));
            let row = TrackListModel::build(&r.doc).row(1).to_vec();
            let (a, b) = (row[1].clone(), row[2].clone());
            assert!(a.end <= b.start, "two clips with a gap");
            r.merge_outline = Some((1, a.start, b.end));
            r.frame(vec![]);
            let o = r.tl.merge_drawn.expect("drawn");
            let (ra, rb) = (r.drawn(a.id), r.drawn(b.id));
            assert!((o.left() - ra.left()).abs() < 0.01, "{ppp}");
            assert!((o.right() - rb.right()).abs() < 0.01, "{ppp}");
            assert_eq!((o.top(), o.bottom()), (ra.top(), ra.bottom()), "{ppp}");
            r.merge_outline = None;
            r.frame(vec![]);
            assert!(r.tl.merge_drawn.is_none());
        }
    }

    /// V09-S10 (§043): the clip menu carries the clip's track menu, so the
    /// track's actions (Add / Delete Track …) work from a clip too.
    #[test]
    fn the_clip_menu_carries_the_clips_track_menu() {
        let mut r = zoomed(1.0, song(3, 2));
        let c = TrackListModel::build(&r.doc).row(2)[1].clone();
        r.secondary_click(r.drawn(c.id).center());
        r.click_label("Track “Track 3”");
        r.click_label("Delete Track (with subtracks)…");
        assert_eq!(
            track_actions(&r),
            vec![track_panel::TrackAction::DeleteTracks(2)]
        );
        assert!(r.applied.is_empty());
    }

    /// V09-S10: the panel's Conductor row and its empty space below the rows
    /// have menus; the panel's track row offers Add Piano Roll Clip.
    #[test]
    fn the_panels_conductor_row_empty_space_and_rows_have_their_menus() {
        use track_panel::{ConductorAdd, TrackAction};
        let mut r = zoomed(1.0, song(3, 0));
        let c = r.geo.conductor;
        r.secondary_click(c.center());
        r.click_label("Add Tempo Change…");
        let y = r.geo.list.top() + r.geo.content_h + 20.0;
        r.secondary_click(Pos2::new(c.center().x, y));
        r.click_label("Add Track");
        r.secondary_click(Pos2::new(c.center().x, r.geo.y_of(2) + 5.0));
        r.click_label("Add Piano Roll Clip");
        assert_eq!(
            r.panel,
            vec![
                TrackAction::ConductorAdd(ConductorAdd::Tempo, None),
                TrackAction::Add,
                TrackAction::AddClip(2),
            ]
        );
    }

    #[test]
    fn a_new_clip_starts_snapped_down_and_lasts_the_bar_it_starts_in() {
        use plumspirit_engine::model::Division;
        let d = Division::ppqn(480);
        // 4/4 until bar 3 (tick 3840), then 3/4.
        let m = MeterMap::new(d, &[(0, 4, 2), (3840, 3, 2)]);
        assert_eq!(new_clip_window(&m, d, Some(120), 1_000), (960, 1920));
        assert_eq!(new_clip_window(&m, d, None, 1_000), (1_000, 1920));
        assert_eq!(new_clip_window(&m, d, Some(480), 4_000), (3840, 1440));
        assert_eq!(new_clip_window(&m, d, Some(1), 0), (0, 1920));
        // SMPTE (25 fps x 40): two seconds.
        let smpte = Division::from_raw(0xE728);
        let m = MeterMap::new(smpte, &[]);
        assert_eq!(new_clip_window(&m, smpte, None, 77), (77, 2000));
    }

    #[test]
    fn a_right_click_targets_the_clip_and_the_menu_acts_on_the_selection() {
        let mut r = zoomed(1.0, song(3, 2));
        let model = TrackListModel::build(&r.doc);
        let (a, b) = (model.row(2)[1].clone(), model.row(2)[2].clone());
        r.click(r.drawn(a.id).center(), egui::Modifiers::NONE);
        // Right-click an unselected clip: it becomes the selection.
        r.secondary_click(r.drawn(b.id).center());
        assert_eq!(r.tl.context, Some(MenuTarget::Clip(b.id)));
        assert_eq!(
            r.tl.selected.iter().copied().collect::<Vec<_>>(),
            vec![b.id]
        );
        // With both selected, the menu's commands take the selection.
        r.click(r.drawn(a.id).center(), egui::Modifiers::COMMAND);
        let mut out = TrackListOutput::default();
        let m = TrackListModel::build(&r.doc);
        for ch in [
            MenuChoice::Mute(true),
            MenuChoice::Duplicate(false),
            MenuChoice::Delete,
            MenuChoice::Open,
        ] {
            r.tl.menu_choice(ch, &b, &m, &r.doc, theme::default_note_palette(), &mut out);
        }
        assert_eq!(
            out.edits,
            vec![
                ClipIntent::Mute(vec![a.id, b.id], true),
                ClipIntent::Duplicate {
                    // One block right after the selection (no overlap).
                    copies: vec![
                        (a.id, 2, a.start + (b.end - a.start)),
                        (b.id, 2, b.start + (b.end - a.start))
                    ],
                    linked: false
                },
                ClipIntent::Delete(vec![a.id, b.id]),
                ClipIntent::Open(b.id),
            ]
        );
        // Each is one undo step (Open is not an edit).
        let labels: Vec<&str> = out
            .edits
            .iter()
            .filter_map(|e| plan(&r.doc, e).unwrap())
            .map(|op| op.label())
            .collect();
        assert_eq!(
            labels,
            vec!["Mute Clips", "Duplicate Clips", "Delete Clips"]
        );
        // Clip Color opens with the clip's colour (else its track's).
        r.tl.menu_choice(
            MenuChoice::Color,
            &b,
            &m,
            &r.doc,
            theme::default_note_palette(),
            &mut out,
        );
        let st = r.tl.color.clone().unwrap();
        assert!(st.track && st.ids == vec![a.id, b.id]);
    }

    #[test]
    fn rename_takes_typed_text_and_enter_only_after_it_opened() {
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(2, 1));
            let model = TrackListModel::build(&r.doc);
            let c = model.row(1)[1].clone();
            let mut out = TrackListOutput::default();
            r.tl.menu_choice(
                MenuChoice::Rename,
                &c,
                &model,
                &r.doc,
                theme::default_note_palette(),
                &mut out,
            );
            let key = |k: Key| egui::Event::Key {
                key: k,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            // An Enter in the window's first frame does not confirm it.
            r.frame(vec![key(Key::Enter)]);
            assert!(r.tl.rename.is_some() && r.applied.is_empty(), "{ppp}");
            r.frame(vec![]);
            r.frame(vec![egui::Event::Text("Intro".into())]);
            r.frame(vec![key(Key::Enter)]);
            // The confirming Enter is the window's: the app's Enter (Open
            // Clip) never sees it (found in the S08 GUI journey).
            assert!(!r.unconsumed.contains(&Key::Enter), "{ppp}");
            r.frame(vec![]);
            assert_eq!(
                r.applied,
                vec![ClipIntent::Rename(c.id, Some("Intro".into()))],
                "{ppp}"
            );
            assert!(r.tl.rename.is_none());
            assert_eq!(
                r.doc.clips().placement(c.id).unwrap().name.as_deref(),
                Some("Intro")
            );
            // Escape cancels.
            r.tl.menu_choice(
                MenuChoice::Rename,
                &c,
                &model,
                &r.doc,
                theme::default_note_palette(),
                &mut out,
            );
            r.frame(vec![]);
            r.frame(vec![key(Key::Escape)]);
            assert!(!r.unconsumed.contains(&Key::Escape), "{ppp}");
            assert!(r.tl.rename.is_none() && r.applied.len() == 1);
            // Clip Color: Enter and Escape stay in the window too.
            r.tl.menu_choice(
                MenuChoice::Color,
                &c,
                &model,
                &r.doc,
                theme::default_note_palette(),
                &mut out,
            );
            r.frame(vec![]);
            r.frame(vec![key(Key::Enter)]);
            assert!(r.tl.has_window() && !r.unconsumed.contains(&Key::Enter));
            r.frame(vec![key(Key::Escape)]);
            assert!(!r.tl.has_window() && !r.unconsumed.contains(&Key::Escape));
            assert_eq!(r.applied.len(), 1, "{ppp}");
        }
    }

    /// Duplicates land on the grid after the selection as a block (S08 GUI
    /// journey: a 4-bar loop ending 60 ticks before the bar was copied to
    /// 7,620 instead of bar 5; two clips in a row were copied onto each other).
    #[test]
    fn duplicates_follow_the_selection_as_a_block_on_the_snap_grid() {
        let rect = |n: u32, track: usize, start: u64, end: u64| ClipRect {
            id: ClipId::Placed(n),
            track,
            start,
            end,
            empty: false,
            name: None,
            color: None,
            muted: false,
            linked: false,
            owned: false,
            content_track: None,
            content: ContentRef::Owned(plumspirit_engine::model::ContentId(0)),
            offset: 0,
        };
        // One 4-bar track clip whose last note ends at 7,620 (480 PPQN).
        let loop4 = [rect(1, 0, 0, 7620)];
        assert_eq!(
            copies_after(&loop4, Some(120)),
            vec![(ClipId::Placed(1), 0, 7680)]
        );
        assert_eq!(
            copies_after(&loop4, Some(1920)),
            vec![(ClipId::Placed(1), 0, 7680)]
        );
        assert_eq!(
            copies_after(&loop4, None),
            vec![(ClipId::Placed(1), 0, 7620)]
        );
        // Two clips that follow each other on one row and one on another row:
        // every copy moves by the whole block (0 .. 3,840), none overlaps.
        let sel = [
            rect(1, 0, 0, 1900),
            rect(2, 0, 1920, 3830),
            rect(3, 2, 960, 2000),
        ];
        assert_eq!(
            copies_after(&sel, Some(120)),
            vec![
                (ClipId::Placed(1), 0, 3840),
                (ClipId::Placed(2), 0, 5760),
                (ClipId::Placed(3), 2, 4800)
            ]
        );
        // A clip off the grid keeps its distance to the grid; nothing
        // selected: no copies.
        assert_eq!(
            copies_after(&[rect(4, 1, 50, 1010)], Some(120)),
            vec![(ClipId::Placed(4), 1, 1010)]
        );
        assert!(copies_after(&[], Some(120)).is_empty());
    }

    #[test]
    fn a_double_click_opens_the_clip_and_keys_act_on_the_selection() {
        use crate::app::commands::Command;
        for ppp in SCALES {
            let mut r = zoomed(ppp, song(3, 2));
            let model = TrackListModel::build(&r.doc);
            let (a, b) = (model.row(1)[1].clone(), model.row(1)[2].clone());
            r.out.clear();
            let p = r.drawn(a.id).center();
            r.click(p, egui::Modifiers::NONE);
            r.click(p, egui::Modifiers::NONE);
            let opened: Vec<&ClipIntent> = r
                .out
                .iter()
                .flat_map(|o| &o.edits)
                .filter(|e| matches!(e, ClipIntent::Open(_)))
                .collect();
            assert_eq!(opened, vec![&ClipIntent::Open(a.id)], "{ppp}");
            assert!(r.applied.is_empty(), "opening is not an edit");
            // Enter opens the clicked clip; Delete / Ctrl+D act on the
            // selection (draw order); other chords keep their meaning.
            r.click(r.drawn(b.id).center(), egui::Modifiers::COMMAND);
            assert_eq!(r.tl.open_target(), Some(b.id));
            assert_eq!(
                r.tl.key_intent(Command::Delete),
                Some(ClipIntent::Delete(vec![a.id, b.id]))
            );
            // Ctrl+D: the two clips as one block, right after the block.
            let block = b.end - a.start;
            assert_eq!(
                r.tl.key_intent(Command::OpenTool("duplicate")),
                Some(ClipIntent::Duplicate {
                    copies: vec![(a.id, 1, a.start + block), (b.id, 1, b.start + block)],
                    linked: true
                })
            );
            // B1-0097: Copy acts on the selected clips too.
            assert_eq!(
                r.tl.key_intent(Command::Copy),
                Some(ClipIntent::Copy(vec![a.id, b.id]))
            );
            assert_eq!(r.tl.key_intent(Command::OpenTool("humanize")), None);
            r.tl.selected.clear();
            assert_eq!(r.tl.key_intent(Command::Delete), None, "nothing selected");
            assert_eq!(r.tl.open_target(), None);
        }
    }

    #[test]
    fn toolbar_zoom_fields_set_the_track_lists_zoom_and_row_height() {
        let mut r = zoomed(1.0, song(2, 1));
        r.tl.set_zoom_x_percent(100.0, &r.doc).unwrap();
        assert!((r.tl.zoom_x_percent(&r.doc) - 100.0).abs() < 1e-6);
        r.tl.set_zoom_x_percent(250.0, &r.doc).unwrap();
        assert!((r.tl.zoom_x_percent(&r.doc) - 250.0).abs() < 1e-6);
        assert!(r.tl.set_zoom_x_percent(0.0, &r.doc).is_err());
        r.tl.set_row_percent(200.0).unwrap();
        assert_eq!(r.tl.row_h, DEFAULT_ROW_H * 2.0);
        assert!((r.tl.row_percent() - 200.0).abs() < 1e-4);
        // B1-0090: beyond the row limits a request is clamped, not refused.
        assert!(r.tl.set_row_percent(0.0).is_err() && r.tl.set_row_percent(-1.0).is_err());
        r.tl.set_row_percent(40.0).unwrap();
        assert_eq!(r.tl.row_h, MIN_ROW_H);
        r.tl.set_row_percent(400.0).unwrap();
        assert_eq!(r.tl.row_h, DEFAULT_ROW_H * 4.0);
        r.tl.set_row_percent(1e9).unwrap();
        assert_eq!(r.tl.row_h, MAX_ROW_H);
        r.tl.zoom_command(crate::app::commands::Command::ResetZoom, &r.doc);
        assert_eq!(r.tl.row_h, DEFAULT_ROW_H);
        assert!((r.tl.zoom_x_percent(&r.doc) - 100.0).abs() < 1e-6);
        // The panel takes the new row height next frame.
        r.tl.row_h = MAX_ROW_H;
        r.frame(vec![]);
        assert_eq!(r.geo.row_h, MAX_ROW_H);
    }

    /// V09-0118: a project opens with the Track List view saved with it (no
    /// fit), and the view to save is the one shown.
    #[test]
    fn a_saved_view_is_restored_on_open_and_offered_for_saving() {
        use plumspirit_engine::model::project::TrackListView;
        let mut d = song(4, 1);
        let saved = TrackListView::new(1_920, 0.02, 48.0);
        d.project_mut().track_list = Some(saved);
        let mut r = Rig::new(1.25, d);
        assert_eq!(r.tl.view.time, TimeAxis::new(1_920, 0.02));
        assert_eq!((r.tl.row_h, r.geo.row_h), (48.0, 48.0));
        r.size = Vec2::new(1300.0, 700.0);
        r.frame(vec![]);
        r.frame(vec![]);
        assert_eq!(
            r.tl.view.time,
            TimeAxis::new(1_920, 0.02),
            "no fit on resize"
        );
        assert_eq!(r.tl.view_for(1), Some(saved));
        assert_eq!(r.tl.view_for(2), None, "not shown for another document");
        r.tl.row_h = 60.0;
        assert_eq!(
            r.tl.view_for(1),
            Some(TrackListView::new(1_920, 0.02, 60.0))
        );
        // An invalid saved view is ignored: default row height, fitted.
        let mut d = song(2, 0);
        d.project_mut().track_list = Some(TrackListView::new(0, 0.02, 2000.0));
        let r = Rig::new(1.0, d);
        assert_eq!(r.tl.row_h, DEFAULT_ROW_H);
        assert_eq!(r.tl.view.time.origin_tick, 0);
    }

    #[test]
    fn the_linked_mark_is_drawable_in_the_default_fonts() {
        let ctx = egui::Context::default();
        let mut o = ctx.run_ui(egui::RawInput::default(), |_| {});
        o.textures_delta.clear();
        let ok =
            ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(theme::size::SMALL), '\u{221E}'));
        assert!(ok, "∞ is missing from the default fonts");
    }

    /// B2-F29: the clip menu's Nudge / Zoom to Selection run as commands
    /// (the keys' path); nudges keep the clips' layout and never pass tick
    /// 0; Zoom to Selection frames the selection; Select All takes every
    /// drawn clip.
    #[test]
    fn the_clip_menu_runs_nudge_and_zoom_and_nudges_stop_at_the_song_start() {
        use crate::app::commands::Command;
        let mut r = zoomed(1.0, song(3, 2));
        let model = TrackListModel::build(&r.doc);
        let (first, a, b) = (
            model.row(2)[0].clone(),
            model.row(2)[1].clone(),
            model.row(2)[2].clone(),
        );
        assert_eq!(first.start, 0);
        r.click(r.drawn(a.id).center(), egui::Modifiers::NONE);
        r.click(r.drawn(b.id).center(), egui::Modifiers::COMMAND);
        let mut out = TrackListOutput::default();
        for cmd in [
            Command::NudgeClipsRight,
            Command::NudgeClipsLeft,
            Command::ZoomToSelection,
        ] {
            r.tl.menu_choice(
                MenuChoice::Command(cmd),
                &b,
                &model,
                &r.doc,
                theme::default_note_palette(),
                &mut out,
            );
            assert_eq!(out.command, Some(cmd));
        }
        assert!(out.edits.is_empty(), "the app runs them, as the keys do");
        assert_eq!(
            r.tl.nudge_moves(240, true),
            Some(vec![(a.id, 2, a.start + 240), (b.id, 2, b.start + 240)])
        );
        assert_eq!(
            r.tl.nudge_moves(240, false),
            Some(vec![(a.id, 2, a.start - 240), (b.id, 2, b.start - 240)])
        );
        // A step longer than the room: the earliest clip stops at 0, the
        // layout is kept.
        assert_eq!(
            r.tl.nudge_moves(a.start + 5_000, false),
            Some(vec![(a.id, 2, 0), (b.id, 2, b.start - a.start)])
        );
        // The track's own clip at 0: no room to the left.
        r.click(r.drawn(first.id).center(), egui::Modifiers::NONE);
        assert_eq!(
            r.tl.selected.iter().copied().collect::<Vec<_>>(),
            vec![first.id]
        );
        assert_eq!(r.tl.nudge_moves(240, false), None);
        assert!(r.tl.nudge_moves(240, true).is_some());
        let n = r.tl.select_all(&r.doc, 1);
        let drawn = (0..r.doc.track_count())
            .map(|t| model.row(t).iter().filter(|c| !c.empty).count())
            .sum::<usize>();
        assert_eq!(n, drawn);
        assert_eq!(r.tl.selected.len(), drawn);
        r.tl.zoom_to(a.start, b.end);
        let t = r.tl.view.time;
        let (x0, x1) = (t.tick_to_x(a.start), t.tick_to_x(b.end));
        assert!(x0 >= 0.0 && x1 <= r.tl.width(), "{x0} .. {x1}");
        assert!(x1 - x0 > r.tl.width() * 0.8);
    }
}

/// B1-S45 (B1-0097): the Track List's clip editing — Slice at the edit
/// cursor (Ctrl+K, the clip menu), Copy / Cut / Paste (Edit menu, Ctrl+C /
/// Ctrl+V), each one Undo step, the pasted clips selected.
#[cfg(test)]
mod parity_tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("parity".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        for (i, n) in [(0usize, 64u64), (1, 32)] {
            let recs: Vec<NoteRecord> = (0..n)
                .map(|k| {
                    NoteRecord::new(
                        k * 120,
                        100,
                        NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    fn played(h: &Harness) -> usize {
        let d = h.doc();
        (0..d.track_count())
            .map(|i| {
                d.placed_contents(i)
                    .iter()
                    .map(|c| c.note_count())
                    .sum::<usize>()
            })
            .sum()
    }

    fn steps(h: &Harness) -> usize {
        h.app.open.as_ref().unwrap().history.undo_len()
    }

    #[test]
    fn slice_copy_cut_and_paste_clips_in_the_track_list() {
        let mut h = Harness::new(song());
        let (t0, t1) = (h.id(0), h.id(1));
        assert_eq!(played(&h), 96);
        // Nothing selected: the clip commands are off; Copy does not jump
        // to the Piano Roll.
        assert!(!h.app.enabled(Command::SliceNotes));
        h.app.track_list.select_only(ClipId::Primary(t0));
        h.app.roll.cursor_tick = 960;
        let s0 = steps(&h);
        assert!(h.command(Command::SliceNotes));
        assert_eq!(steps(&h), s0 + 1, "one undo step");
        assert_eq!(h.doc().clips().placements_on(t0).len(), 2);
        assert_eq!(played(&h), 96, "every note still plays once");
        // Copy the two halves of track 0 and track 1's clip.
        h.app.track_list.selected = h
            .doc()
            .clips()
            .placements_on(t0)
            .iter()
            .map(|p| p.id)
            .chain([ClipId::Primary(t1)])
            .collect();
        assert!(h.command(Command::Copy));
        assert_eq!(steps(&h), s0 + 1, "Copy is not an edit");
        assert_eq!(
            h.app.main_view,
            MainView::TrackList,
            "stays in the Track List"
        );
        assert!(
            h.app
                .clip_clipboard
                .as_ref()
                .is_some_and(|c| c.entries.len() == 3)
        );
        // Paste at the edit cursor on the active track (track 0): three
        // independent copies, selected, one undo step.
        h.app.select_track(0);
        h.app.roll.cursor_tick = 19_200;
        assert!(h.command(Command::Paste));
        assert_eq!(steps(&h), s0 + 2);
        assert_eq!(h.app.track_list.selected.len(), 3);
        assert_eq!(played(&h), 192);
        assert!(
            h.app
                .track_list
                .selected
                .iter()
                .all(|id| matches!(id, ClipId::Placed(_)))
        );
        // Cut the pasted clips: copied, then deleted in one step. The copies
        // stored their whole contents (both halves of track 0 show all 64
        // of its notes through their windows): 64 + 64 + 32 stored notes go.
        assert!(h.command(Command::Cut));
        assert_eq!(steps(&h), s0 + 3);
        assert_eq!(played(&h), 96);
        assert!(
            h.status()
                .contains("Deleted 3 clips (160 notes of independent copies"),
            "{}",
            h.status()
        );
        // Paste on track 1 needs two rows: refused with a reason.
        h.app.select_track(1);
        h.app.track_list.selected.clear();
        assert!(h.command(Command::Paste));
        assert_eq!(steps(&h), s0 + 3, "refused");
        assert!(h.status().contains("track rows"), "{}", h.status());
        // Undo walks back one operation at a time.
        assert!(h.command(Command::Undo));
        assert_eq!(played(&h), 192);
    }
}
