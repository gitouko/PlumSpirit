//! Compact track list on the left. The Conductor (the song's tempo, time
//! signatures and markers) is pinned at the top; below it, the tracks — Standard
//! MIDI File tracks, not MIDI channels. The list is virtualised, so files with
//! thousands of tracks stay fast.
//!
//! Each row has **M** (Mute) and **S** (Solo) buttons. Click a row to make it
//! active, Ctrl+click to add it to the track selection (Select Range,
//! Selected tracks), double-click (or right-click, Track Properties…) to edit
//! its properties. A track that does not play is drawn dimmed with its name
//! struck through (muted) or in italics (silenced by another track's Solo).

use crate::ui::theme::{Palette, size, space};
use egui::text::{LayoutJob, TextWrapping};
use egui::{Button, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::PlumDocument;

pub const PANEL_WIDTH: f32 = 216.0;
/// Row height of the list next to the Piano Roll (and of the Conductor row).
pub const ROW_H: f32 = 26.0;

/// v0.9 (V09-S08): how the list lays out its rows. Next to the Track List
/// the rows take the Track List's row height, and a wheel over the timeline
/// scrolls this list (one vertical scroll for both).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowLayout {
    pub row_h: f32,
    /// Scroll the list to this offset (points) this frame.
    pub scroll_to: Option<f32>,
}

impl Default for RowLayout {
    fn default() -> Self {
        RowLayout {
            row_h: ROW_H,
            scroll_to: None,
        }
    }
}

/// v0.9 (V09-S08): where the panel drew its rows this frame (screen points),
/// so the Track List timeline draws every row at exactly the same y.
#[derive(Clone, Debug, PartialEq)]
pub struct RowGeometry {
    /// The pinned Conductor row.
    pub conductor: Rect,
    /// The scrolling list's viewport: rows show inside it only.
    pub list: Rect,
    pub row_h: f32,
    /// Vertical scroll offset of the list (points).
    pub scroll: f32,
    /// Height of all rows together (points).
    pub content_h: f32,
    /// The rows drawn this frame: (index into the visible-rows table, top y).
    pub rows: Vec<(usize, f32)>,
}

impl Default for RowGeometry {
    fn default() -> Self {
        RowGeometry {
            conductor: Rect::ZERO,
            list: Rect::ZERO,
            row_h: 0.0,
            scroll: 0.0,
            content_h: 0.0,
            rows: Vec::new(),
        }
    }
}

impl RowGeometry {
    /// Top y of visible-rows entry `r`: where the panel drew it this frame
    /// (egui places rows on the pixel grid), else where it would be.
    pub fn y_of(&self, r: usize) -> f32 {
        if let Some(&(first, _)) = self.rows.first()
            && r >= first
            && let Some(&(drawn, y)) = self.rows.get(r - first)
            && drawn == r
        {
            return y;
        }
        self.list.top() - self.scroll + r as f32 * self.row_h
    }

    /// The visible-rows entry under screen y `y` inside the list, if any of
    /// the `count` rows is there.
    pub fn row_at(&self, y: f32, count: usize) -> Option<usize> {
        if !(self.list.top()..self.list.bottom()).contains(&y) || self.row_h <= 0.0 {
            return None;
        }
        if let Some(&(r, _)) = self
            .rows
            .iter()
            .find(|&&(_, top)| y >= top && y < top + self.row_h)
        {
            return Some(r);
        }
        let r = ((y - self.list.top() + self.scroll) / self.row_h).floor();
        (r >= 0.0 && (r as usize) < count).then_some(r as usize)
    }

    /// The largest scroll offset (points).
    pub fn max_scroll(&self) -> f32 {
        (self.content_h - self.list.height()).max(0.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackAction {
    Select(usize),
    /// Ctrl+click: add to / remove from the track selection.
    ToggleSelected(usize),
    /// Shift+click: select the visible rows from the active track to this one.
    SelectRange(usize),
    /// v0.7: add an empty sibling track after this track's subtree.
    AddBelow(usize),
    /// B2-F03: Add Tracks… (a count, in a dialog) at this place; `None` =
    /// the active context (below the active track, at the end from the
    /// Conductor).
    AddTracks(Option<plumspirit_engine::editor::folders::AddTracksAt>),
    /// v0.9 V09-S10: the Conductor menu's Add Tempo Change / Time Signature /
    /// Marker, at a tick (the Track List's Conductor lane) or at the edit
    /// position (`None`).
    ConductorAdd(ConductorAdd, Option<u64>),
    /// v0.9 §041 (V09-0030): Add Piano Roll Clip on this track at the edit
    /// cursor (one bar, snapped; the Track List row menu adds at the
    /// clicked tick through its own clip intent).
    AddClip(usize),
    /// v0.7: delete this track (or the selection it belongs to), with subtracks.
    DeleteTracks(usize),
    /// v0.7: drag-and-drop of a track (or the selection it belongs to).
    Move {
        dragged: usize,
        target: usize,
        place: plumspirit_engine::history::TrackPlace,
    },
    /// v0.7: move a track (or the selection it belongs to) one sibling down
    /// (true) / up.
    Nudge(usize, bool),
    /// v0.7: indent (true) / outdent a track.
    Indent(usize, bool),
    SelectConductor,
    Add,
    Delete,
    Properties(usize),
    FitEnd(usize),
    ToggleMute(usize),
    /// Solo; `true` = keep other soloed tracks (Ctrl+click).
    ToggleSolo(usize, bool),
    PlayAll,
    /// L-F08: monitor exactly the selected tracks (or the clicked one) /
    /// back to the song's own Mute / Solo.
    PlayOnlySelected,
    RestoreAllTracks,
    /// v0.8 §204: Mute / Unmute every track of the selection the clicked
    /// track belongs to (context menu with several tracks selected).
    MuteSelection(usize, bool),
    /// v0.8 §204: Solo the selection the clicked track belongs to.
    SoloSelection(usize),
    // v0.5
    ToggleCollapse(plumspirit_engine::model::TrackId),
    AddSubtrack(usize),
    AddSubtracks(usize),
    MuteSubtree(usize, bool),
    SoloSubtree(usize),
    Sub(SubAction),
    // v0.8 Track Folders
    /// Collapse / expand a folder (view state, saved with the project).
    FolderToggle(u32),
    /// B1-0064: show (`true`) / hide these tracks as Onion Skin ghosts (a
    /// click on a track's colour box, or a drag over several).
    OnionSet(Vec<usize>, bool),
    /// L-G03: these tracks become the only ghosts (Alt+click on a colour box).
    OnionOnly(Vec<usize>),
    /// New Folder: with the clicked track (and the selection it belongs to),
    /// or empty and shown before it (`false`), or at the end (None).
    FolderNew(Option<(usize, bool)>),
    FolderRenameStart(u32),
    FolderRename(u32, String),
    FolderDelete(u32),
    /// v0.8 §203: an empty track added at the end of the folder (one step).
    FolderAddTrack(u32),
    /// v0.8 §203: the folder and all of its tracks deleted (confirmed).
    FolderDeleteWithTracks(u32),
    MoveToFolder(usize, u32),
    RemoveFromFolder(usize),
    /// Tracks dropped on a folder header (`before`: before the folder, outside).
    DropOnFolder {
        dragged: usize,
        folder: u32,
        before: bool,
    },
    /// A folder header dropped before / after a row.
    MoveFolder {
        folder: u32,
        target: usize,
        place: plumspirit_engine::history::TrackPlace,
    },
}

/// What the Conductor menu adds (v0.9 V09-S10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConductorAdd {
    Tempo,
    TimeSignature,
    Marker,
}

/// v0.8: folder state the track list edits in place (inline rename).
pub struct FolderPanel<'a> {
    pub renaming: &'a mut Option<(u32, String)>,
}

/// Subchannel section actions (active track).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubAction {
    Select(plumspirit_engine::model::SubchannelId),
    Add,
    AddMany,
    /// Go to a subchannel number (typed).
    GoTo(String),
    OpenWindow,
}

/// What the subchannel section shows for the active track.
pub struct SubchannelView<'a> {
    pub active: &'a plumspirit_engine::model::Subchannel,
    pub table: &'a plumspirit_engine::model::SubchannelTable,
    pub goto: &'a mut String,
}

/// Drag-and-drop payload: the dragged row's track index.
#[derive(Clone, Copy, Debug)]
struct DragTrack(usize);

/// Drag-and-drop payload: a folder header (v0.8).
#[derive(Clone, Copy, Debug)]
struct DragFolder(u32);

/// Subchannel quick-switch swatches: painted size and how many per row.
const SWATCH: f32 = 12.0;
const SWATCHES_PER_ROW: usize = 8;

/// Indentation per tree level, and the deepest level drawn indented further.
const INDENT: f32 = 12.0;
const MAX_INDENT_LEVELS: u32 = 8;

/// B1-0069: the M / S buttons of a track row at `rect` (painted box, hit
/// rectangle) — Mute left of Solo. The painted boxes stay compact; each hit
/// rectangle reaches half-way into the gap between them and nearly the full
/// row height, so both targets are at least [`size::MIN_TARGET`] square.
pub fn ms_rects(rect: Rect) -> [(Rect, Rect); 2] {
    // L-F10: larger boxes (19 → 22 px).
    const BOX: f32 = 22.0;
    const GAP: f32 = 4.0;
    let s = Rect::from_center_size(
        Pos2::new(rect.right() - 4.0 - BOX / 2.0, rect.center().y),
        Vec2::splat(BOX),
    );
    let m = s.translate(Vec2::new(-(BOX + GAP), 0.0));
    let hit_h = (rect.height() - 2.0).max(BOX);
    let hit = |r: Rect, left: f32, right: f32| {
        Rect::from_min_max(
            Pos2::new(left, r.center().y - hit_h / 2.0),
            Pos2::new(right, r.center().y + hit_h / 2.0),
        )
    };
    let mid = (m.right() + s.left()) / 2.0;
    [
        (m, hit(m, m.left() - GAP / 2.0, mid)),
        (s, hit(s, mid, s.right() + 2.0)),
    ]
}

/// A small square toggle (M / S) painted in the row; returns true when clicked.
#[allow(clippy::too_many_arguments)] // one helper, two call sites
fn ms_button(
    ui: &mut egui::Ui,
    (rect, hit): (Rect, Rect),
    id: egui::Id,
    label: &str,
    on: bool,
    on_color: egui::Color32,
    pal: &Palette,
    tip: &str,
) -> (bool, bool) {
    let resp = ui.interact(hit, id, Sense::click()).on_hover_text(tip);
    let p = ui.painter_at(rect.expand(2.0));
    let fill = if on {
        on_color
    } else if resp.hovered() {
        pal.roll_black_row
    } else {
        egui::Color32::TRANSPARENT
    };
    p.rect_filled(rect, 0.0, fill);
    p.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, if on { on_color } else { pal.panel_border }),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(size::SMALL),
        if on { pal.panel_bg } else { pal.text_weak },
    );
    if resp.has_focus() {
        // B1-0069: keyboard focus is always visible.
        p.rect_stroke(
            rect.expand(1.0),
            0.0,
            Stroke::new(2.0, pal.accent),
            egui::StrokeKind::Inside,
        );
    }
    (resp.clicked(), resp.ctx.input(|i| i.modifiers.command))
}

/// A small metronome drawn with shapes (the Conductor's icon).
fn conductor_icon(p: &egui::Painter, c: Pos2, color: egui::Color32) {
    let body = vec![
        c + Vec2::new(-4.5, 6.0),
        c + Vec2::new(4.5, 6.0),
        c + Vec2::new(2.0, -6.0),
        c + Vec2::new(-2.0, -6.0),
    ];
    p.add(egui::Shape::convex_polygon(
        body,
        egui::Color32::TRANSPARENT,
        Stroke::new(1.2, color),
    ));
    p.line_segment(
        [c + Vec2::new(0.0, 4.0), c + Vec2::new(3.5, -5.0)],
        Stroke::new(1.2, color),
    );
    p.line_segment(
        [c + Vec2::new(-4.5, 6.0), c + Vec2::new(4.5, 6.0)],
        Stroke::new(1.5, color),
    );
}

#[allow(clippy::too_many_arguments)] // one call site; plain values
pub fn track_panel(
    ui: &mut egui::Ui,
    doc: Option<&PlumDocument>,
    active: usize,
    conductor_active: bool,
    editable: bool,
    pal: &Palette,
    ghosts: &[usize],
    dark: bool,
    selected: &std::collections::BTreeSet<plumspirit_engine::model::TrackId>,
    rows: &[crate::app::TreeRowView],
    // Notes each track plays through its clips (B1-0095), by track index.
    note_counts: &[usize],
    conductor_bpm: &str,
    conductor_bpm_tip: &str,
    sub: Option<SubchannelView<'_>>,
    folders: FolderPanel<'_>,
    layout: RowLayout,
) -> (Option<TrackAction>, RowGeometry) {
    let mut out = None;
    let row_h = layout.row_h.max(1.0);
    let mut geo = RowGeometry {
        row_h,
        ..RowGeometry::default()
    };
    ui.add_space(space::NORMAL);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Tracks").size(size::EMPHASIS).strong());
        if let Some(d) = doc {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(d.track_count().to_string())
                        .size(size::SMALL)
                        .color(pal.text_weak),
                );
                if d.tracks().iter().any(|t| t.mute || t.solo)
                    && ui
                        .small_button(RichText::new("Play All").size(size::SMALL))
                        .on_hover_text("Clear every Mute and Solo: all tracks play")
                        .clicked()
                {
                    out = Some(TrackAction::PlayAll);
                }
            });
        }
    });
    // L-F08 / L-F10 (contract §016): the current track's Mute / Solo as
    // larger buttons, and monitoring (Play Only Selected / Restore All).
    if let Some(d) = doc {
        ui.horizontal(|ui| {
            if let Some(t) = (!conductor_active).then(|| d.track(active)).flatten() {
                let big = |ui: &mut egui::Ui, text: &str, on: bool, color: egui::Color32, tip: &str| {
                    let rt = RichText::new(text).size(size::BODY).strong();
                    let b = egui::Button::new(if on { rt.color(pal.panel_bg) } else { rt })
                        .fill(if on { color } else { egui::Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0, if on { color } else { pal.panel_border }))
                        .min_size(Vec2::new(30.0, 24.0));
                    ui.add(b).on_hover_text(tip).clicked()
                };
                let muted_solo = t.mute && t.solo;
                if big(
                    ui,
                    "M",
                    t.mute,
                    pal.warning,
                    &format!(
                        "Mute the current track ({}){}",
                        t.display_name(active),
                        crate::app::keybinds::hint(crate::app::commands::Command::MuteCurrentTrack)
                    ),
                ) {
                    out = Some(TrackAction::ToggleMute(active));
                }
                if big(
                    ui,
                    "S",
                    t.solo && !t.mute,
                    pal.accent,
                    &if muted_solo {
                        "Soloed but muted: Mute wins over Solo, so this track does not play. Unmute it, or click to un-solo.".to_owned()
                    } else {
                        format!(
                            "Solo the current track ({}){}",
                            t.display_name(active),
                            crate::app::keybinds::hint(crate::app::commands::Command::SoloCurrentTrack)
                        )
                    },
                ) {
                    out = Some(TrackAction::ToggleSolo(active, false));
                }
                ui.label(RichText::new("current").size(size::SMALL).color(pal.text_weak));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &d.project().monitor {
                    Some(m) => {
                        if ui
                            .small_button(RichText::new("Restore All").size(size::SMALL).strong())
                            .on_hover_text(format!(
                                "Playing only {} selected track{}. Restore All Tracks: the song's own Mute / Solo apply again{}",
                                m.len(),
                                if m.len() == 1 { "" } else { "s" },
                                crate::app::keybinds::hint(crate::app::commands::Command::RestoreAllTracks)
                            ))
                            .clicked()
                        {
                            out = Some(TrackAction::RestoreAllTracks);
                        }
                    }
                    None => {
                        if ui
                            .small_button(RichText::new("Play Only Selected").size(size::SMALL))
                            .on_hover_text(format!(
                                "Hear exactly the selected tracks (or the current one) without changing any Mute / Solo; Restore All goes back{}",
                                crate::app::keybinds::hint(crate::app::commands::Command::PlayOnlySelected)
                            ))
                            .clicked()
                        {
                            out = Some(TrackAction::PlayOnlySelected);
                        }
                    }
                }
            });
        });
    }
    ui.add_space(space::MICRO);
    let Some(doc) = doc else {
        ui.add_space(space::SECTION);
        ui.label(RichText::new("No project open").color(pal.text_weak));
        return (None, geo);
    };
    // The Conductor row: always first, never scrolls away, cannot be deleted.
    {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        geo.conductor = rect;
        let p = ui.painter_at(rect);
        if conductor_active {
            p.rect_filled(rect, 0.0, pal.accent_weak);
            p.rect_filled(
                Rect::from_min_size(
                    rect.min + Vec2::new(0.0, 4.0),
                    Vec2::new(3.0, rect.height() - 8.0),
                ),
                0.0,
                pal.accent,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, 0.0, pal.roll_black_row);
        }
        conductor_icon(
            &p,
            Pos2::new(rect.left() + 14.0, rect.center().y),
            pal.accent,
        );
        p.text(
            Pos2::new(rect.left() + 36.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Conductor",
            FontId::proportional(size::BODY),
            pal.text,
        );
        let c = doc.conductor();
        // The BPM at the current position (follows tempo automation live).
        p.text(
            Pos2::new(rect.right() - 6.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("BPM {conductor_bpm}"),
            FontId::proportional(size::SMALL),
            pal.text_weak,
        );
        if resp.clicked() {
            out = Some(TrackAction::SelectConductor);
        }
        let resp = resp.on_hover_text(format!(
            "Conductor — the song's tempo ({} tempo point(s)), time signatures ({}) and markers ({}). It applies to every track and cannot be deleted.\n{conductor_bpm_tip}",
            c.tempo_points().len(),
            c.time_signatures().len(),
            c.markers().len()
        ));
        // v0.9 V09-S10 (§043): the Conductor's own menu (at the edit position).
        resp.context_menu(|ui| {
            if let Some(a) = conductor_menu(
                ui,
                editable,
                c.timing_editable(),
                c.markers_editable(),
                None,
            ) {
                out = Some(a);
            }
        });
        ui.add_space(2.0);
        let y = ui.cursor().top();
        ui.painter()
            .hline(rect.x_range(), y, Stroke::new(1.0, pal.panel_border));
        ui.add_space(3.0);
    }
    // The subchannel section (three rows) and the Add / Delete footer stay
    // visible below the list at every window height.
    let sub_h = if sub.is_some() { 92.0 } else { 0.0 };
    // B2-F03: the buttons wrap onto a second row (Add Track, Add Tracks… |
    // Delete Track).
    let button_row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    let footer_h = 44.0 + button_row + sub_h;
    let list_h = (ui.available_height() - footer_h).max(row_h);
    let eligible = plumspirit_engine::playback::eligibility(doc);
    let mut area = egui::ScrollArea::vertical()
        .max_height(list_h)
        .auto_shrink([false, false]);
    if let Some(y) = layout.scroll_to {
        area = area.vertical_scroll_offset(y.max(0.0));
    }
    // No spacing between rows (v0.9: it was counted by the virtualisation but
    // not drawn, so rows drifted while scrolling): row r is at r × row_h.
    let spacing_y = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
    // B1-0064: a drag that started on a colour box shows / hides every track
    // the pointer passes (the state the first track got), until release.
    let paint_id = ui.id().with("onion_paint");
    let primary_down = ui.input(|i| i.pointer.primary_down());
    let mut paint: Option<bool> = if primary_down {
        ui.data(|d| d.get_temp(paint_id))
    } else {
        ui.data_mut(|d| d.remove_temp::<bool>(paint_id));
        None
    };
    let pointer_y = ui.ctx().pointer_interact_pos().map(|p| p.y);
    let mut onion_set: Vec<usize> = Vec::new();
    let shown = area.show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in range {
                let Some(row) = rows.get(r) else {
                    continue;
                };
                geo.rows.push((r, ui.cursor().top()));
                if row.header {
                    folder_row(ui, doc, row, row_h, editable, pal, folders.renaming, &mut out);
                    continue;
                }
                let i = row.index;
                let Some(track) = doc.track(i) else {
                    continue;
                };
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_h),
                    if editable {
                        Sense::click_and_drag()
                    } else {
                        Sense::click()
                    },
                );
                if editable {
                    resp.dnd_set_drag_payload(DragTrack(i));
                }
                // Drop target: the upper third = before, the lower third =
                // after, the middle = into (as the last subtrack).
                let drop_place = |pos: Pos2| {
                    let f = (pos.y - rect.top()) / rect.height().max(1.0);
                    if f < 0.3 {
                        plumspirit_engine::history::TrackPlace::Before
                    } else if f > 0.7 {
                        plumspirit_engine::history::TrackPlace::After
                    } else {
                        plumspirit_engine::history::TrackPlace::Into
                    }
                };
                let pointer = ui.ctx().pointer_interact_pos();
                if let (Some(d), Some(pos)) = (resp.dnd_hover_payload::<DragTrack>(), pointer)
                    && d.0 != i
                {
                    let p = ui.painter();
                    let stroke = Stroke::new(2.0, pal.accent);
                    match drop_place(pos) {
                        plumspirit_engine::history::TrackPlace::Before => {
                            p.hline(rect.x_range(), rect.top() + 1.0, stroke);
                        }
                        plumspirit_engine::history::TrackPlace::After => {
                            p.hline(rect.x_range(), rect.bottom() - 1.0, stroke);
                        }
                        plumspirit_engine::history::TrackPlace::Into => {
                            p.rect_stroke(rect.shrink(1.0), 0.0, stroke, egui::StrokeKind::Inside);
                        }
                    }
                }
                if let (Some(d), Some(pos)) = (resp.dnd_release_payload::<DragTrack>(), pointer)
                    && d.0 != i
                {
                    out = Some(TrackAction::Move {
                        dragged: d.0,
                        target: i,
                        place: drop_place(pos),
                    });
                }
                // A folder dropped on a track: before / after it (or its folder).
                let half = |pos: Pos2| {
                    if pos.y < rect.center().y {
                        plumspirit_engine::history::TrackPlace::Before
                    } else {
                        plumspirit_engine::history::TrackPlace::After
                    }
                };
                if let (Some(_), Some(pos)) = (resp.dnd_hover_payload::<DragFolder>(), pointer) {
                    let y = match half(pos) {
                        plumspirit_engine::history::TrackPlace::Before => rect.top() + 1.0,
                        _ => rect.bottom() - 1.0,
                    };
                    ui.painter().hline(rect.x_range(), y, Stroke::new(2.0, pal.accent));
                }
                if let (Some(d), Some(pos)) = (resp.dnd_release_payload::<DragFolder>(), pointer) {
                    out = Some(TrackAction::MoveFolder {
                        folder: d.0,
                        target: i,
                        place: half(pos),
                    });
                }
                let is_active = i == active && !conductor_active;
                let in_selection = selected.contains(&track.id());
                let audible = eligible.get(i).copied().unwrap_or(true);
                {
                    let p = ui.painter_at(rect);
                    if is_active {
                        p.rect_filled(rect, 0.0, pal.accent_weak);
                        p.rect_filled(
                            Rect::from_min_size(
                                rect.min + Vec2::new(0.0, 4.0),
                                Vec2::new(3.0, rect.height() - 8.0),
                            ),
                            0.0,
                            pal.accent,
                        );
                    } else if in_selection {
                        p.rect_filled(rect, 0.0, pal.accent_weak.gamma_multiply(0.55));
                        p.rect_stroke(
                            rect.shrink(0.5),
                            0.0,
                            Stroke::new(1.0, pal.accent.gamma_multiply(0.6)),
                            egui::StrokeKind::Inside,
                        );
                    } else if resp.hovered() {
                        p.rect_filled(rect, 0.0, pal.roll_black_row);
                    }
                }
                let font = FontId::proportional(size::BODY);
                let small = FontId::proportional(size::SMALL);
                let p = ui.painter_at(rect);
                let indent = (row.depth + u32::from(row.folder != 0)).min(MAX_INDENT_LEVELS) as f32
                    * INDENT;
                // Disclosure arrow (▶ collapsed, ▼ expanded) for tracks with subtracks.
                let mut toggled = false;
                if row.has_children {
                    let arrow = Rect::from_center_size(
                        Pos2::new(rect.left() + 6.0 + indent, rect.center().y),
                        Vec2::new(12.0, 16.0),
                    );
                    let ar = ui
                        .interact(arrow, ui.id().with(("disclose", track.id().0)), Sense::click())
                        .on_hover_text(if row.collapsed {
                            "Show subtracks"
                        } else {
                            "Hide subtracks (view only: they still play and export)"
                        });
                    let c = arrow.center();
                    let col = if ar.hovered() { pal.text } else { pal.text_weak };
                    let pts = if row.collapsed {
                        vec![
                            c + Vec2::new(-2.5, -4.0),
                            c + Vec2::new(3.5, 0.0),
                            c + Vec2::new(-2.5, 4.0),
                        ]
                    } else {
                        vec![
                            c + Vec2::new(-4.0, -2.5),
                            c + Vec2::new(4.0, -2.5),
                            c + Vec2::new(0.0, 3.5),
                        ]
                    };
                    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
                    if ar.clicked() {
                        out = Some(TrackAction::ToggleCollapse(track.id()));
                        toggled = true;
                    }
                } else if row.depth > 0 {
                    // A small tick marks a subtrack.
                    p.hline(
                        (rect.left() + indent - 2.0)..=(rect.left() + indent + 4.0),
                        rect.center().y,
                        Stroke::new(1.0, pal.panel_border),
                    );
                }
                if row.depth == 0 {
                    p.text(
                        Pos2::new(rect.left() + 14.0 + indent, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        row.ordinal.to_string(),
                        small.clone(),
                        pal.text_weak,
                    );
                }
                // M / S buttons at the right.
                let [m_rects, s_rects] = ms_rects(rect);
                let m_rect = m_rects.0;
                let played = note_counts.get(i).copied().unwrap_or(track.notes.len());
                let count = group_digits(played as u64);
                let count_rect = p.text(
                    Pos2::new(m_rect.left() - 5.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    count,
                    small,
                    pal.text_weak,
                );
                {
                    // Track colour (the canonical resolver: ghosts and the
                    // live keyboard use the same colour). B1-0064: it is the
                    // track's Onion Skin switch — full colour with a ring
                    // while the track is drawn as ghosts in the Piano Roll,
                    // faded while it is not; a click shows / hides it, a
                    // drag over several tracks sets them all.
                    let sw = Rect::from_center_size(
                        Pos2::new(rect.left() + 33.0 + indent, rect.center().y),
                        Vec2::new(6.0, 12.0),
                    );
                    let included = ghosts.contains(&i);
                    let is_self = i == active;
                    let cell = Rect::from_center_size(
                        sw.center(),
                        Vec2::new(12.0, (row_h - 4.0).clamp(12.0, 22.0)),
                    );
                    let cr = ui
                        .interact(
                            cell,
                            ui.id().with(("onion_cell", track.id().0)),
                            if is_self { Sense::hover() } else { Sense::click_and_drag() },
                        )
                        .on_hover_text({
                            let keys = crate::app::commands::Command::OnionSelectedTracks.shortcut();
                            let keys = if keys.is_empty() {
                                String::new()
                            } else {
                                format!(" ({keys}: the selected tracks)")
                            };
                            if is_self {
                                "The active track's color (a track is never its own Onion Skin ghost).".to_owned()
                            } else if included {
                                format!("Shown as Onion Skin ghosts in the Piano Roll. Click to hide; drag over other tracks to hide them too{keys}. Alt+click: show only this track.")
                            } else {
                                format!("Click to show this track as Onion Skin ghosts in the Piano Roll (added to the others); drag over other tracks to show them too{keys}. Alt+click: show only this track.")
                            }
                        });
                    if !is_self && cr.clicked() {
                        out = Some(if ui.input(|inp| inp.modifiers.alt) {
                            TrackAction::OnionOnly(vec![i])
                        } else {
                            TrackAction::OnionSet(vec![i], !included)
                        });
                    }
                    if !is_self && cr.drag_started() {
                        // The box the drag started on (the pointer may
                        // already be over the next row).
                        paint = Some(!included);
                        ui.data_mut(|d| d.insert_temp(paint_id, !included));
                        onion_set.push(i);
                    } else if let (Some(target), Some(y)) = (paint, pointer_y)
                        && !is_self
                        && (rect.top()..rect.bottom()).contains(&y)
                        && included != target
                    {
                        onion_set.push(i);
                    }
                    let color = crate::ui::theme::note_palette(ui.ctx()).track_color(track.id().0, track.props().onion_color, dark, 255);
                    let hover = cr.hovered() && !is_self;
                    let fill = if included || is_self || hover {
                        color
                    } else {
                        color.gamma_multiply(0.35)
                    };
                    p.rect_filled(sw, 0.0, fill);
                    if included {
                        // Drawn as Onion Skin ghosts in the Piano Roll.
                        p.rect_stroke(
                            sw.expand(1.0),
                            0.0,
                            egui::Stroke::new(1.0, pal.text),
                            egui::StrokeKind::Inside,
                        );
                    } else if hover {
                        p.rect_stroke(
                            sw.expand(1.0),
                            0.0,
                            egui::Stroke::new(1.0, pal.text_weak),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                let name_left = rect.left() + 40.0 + indent;
                let max_w = (count_rect.left() - 6.0 - name_left).max(10.0);
                let name_color = if audible { pal.text } else { pal.text_weak };
                let mut job = LayoutJob::default();
                job.append(
                    &track.label_with(row.ordinal),
                    0.0,
                    egui::TextFormat {
                        font_id: font,
                        color: name_color,
                        strikethrough: if track.mute {
                            Stroke::new(1.0, pal.text_weak)
                        } else {
                            Stroke::NONE
                        },
                        italics: !audible && !track.mute,
                        ..Default::default()
                    },
                );
                job.wrap = TextWrapping::truncate_at_width(max_w);
                let galley = p.layout_job(job);
                let y = rect.center().y - galley.size().y / 2.0;
                p.galley(Pos2::new(name_left, y), galley, name_color);
                let (m_clicked, _) = ms_button(
                    ui,
                    m_rects,
                    ui.id().with(("mute", track.id().0)),
                    "M",
                    track.mute,
                    pal.warning,
                    pal,
                    if track.mute {
                        "Muted: this track does not play (it is still edited, saved and exported). Click to unmute."
                    } else {
                        "Mute this track (playback only; saved with the project)"
                    },
                );
                let (s_clicked, s_add) = ms_button(
                    ui,
                    s_rects,
                    ui.id().with(("solo", track.id().0)),
                    "S",
                    // L-F09: Mute wins over Solo — a muted track's Solo is
                    // not lit (it has no effect), the tip says why.
                    track.solo && !track.mute,
                    pal.accent,
                    pal,
                    if track.solo && track.mute {
                        "Soloed but muted: Mute wins over Solo, so this track does not play. Unmute it, or click to un-solo."
                    } else if track.solo {
                        "Soloed: only soloed tracks play. Click to un-solo; Ctrl+click to solo several."
                    } else {
                        "Solo: play only this track (Ctrl+click adds it to the soloed tracks)"
                    },
                );
                if toggled {
                    // The disclosure arrow handled the click.
                } else if m_clicked {
                    out = Some(TrackAction::ToggleMute(i));
                } else if s_clicked {
                    out = Some(TrackAction::ToggleSolo(i, s_add));
                } else if resp.double_clicked() {
                    out = Some(TrackAction::Properties(i));
                } else if resp.clicked() {
                    let m = ui.input(|x| x.modifiers);
                    out = Some(if m.shift {
                        TrackAction::SelectRange(i)
                    } else if m.command {
                        TrackAction::ToggleSelected(i)
                    } else {
                        TrackAction::Select(i)
                    });
                }
                let played = note_counts.get(i).copied().unwrap_or(track.notes.len());
                let mut tip = format!(
                    "{} — {} notes, {} other events, {} subchannel(s)",
                    track.label_with(row.ordinal),
                    group_digits(played as u64),
                    group_digits(track.events.len() as u64),
                    group_digits(track.subchannels().len() as u64)
                );
                if played != track.notes.len() {
                    tip.push_str(&format!(
                        "\nIts clips play {} notes; {} notes are stored in its own content.",
                        group_digits(played as u64),
                        group_digits(track.notes.len() as u64)
                    ));
                }
                if row.depth > 0 {
                    tip.push_str(&format!("\nSubtrack (level {})", row.depth));
                }
                if track.props().corrects() {
                    tip.push_str(&format!(
                        "\nCorrections: {}",
                        plumspirit_engine::model::correction::Correction::describe(track.props())
                    ));
                }
                if let plumspirit_engine::model::OutputRoute::External(n) = &track.props().route {
                    tip.push_str(&format!("\nOutput: {n}"));
                }
                tip.push_str("\nDouble-click: Track Properties · Ctrl+click: add to the selection · Shift+click: select a range · Drag: move (onto a track's middle: make it a subtrack)");
                let resp = resp.on_hover_text(tip);
                resp.context_menu(|ui| {
                    if let Some(a) = track_menu(
                        ui,
                        &TrackMenu {
                            doc,
                            index: i,
                            row,
                            editable,
                            selected,
                            active: (!conductor_active)
                                .then(|| doc.track(active).map(|t| t.id()))
                                .flatten(),
                            add_clip: true,
                        },
                    ) {
                        out = Some(a);
                    }
                });
            }
        });
    ui.spacing_mut().item_spacing.y = spacing_y;
    if let Some(on) = paint
        && !onion_set.is_empty()
        && out.is_none()
    {
        out = Some(TrackAction::OnionSet(onion_set, on));
    }
    geo.list = shown.inner_rect;
    geo.scroll = shown.state.offset.y;
    geo.content_h = shown.content_size.y;
    // v0.9 V09-S10: the list's empty space below the rows has its own menu
    // (Add Track, New Folder).
    let below_top = geo.list.top() + geo.content_h - geo.scroll;
    if below_top < geo.list.bottom() - 1.0 {
        let below = Rect::from_min_max(
            Pos2::new(geo.list.left(), below_top.max(geo.list.top())),
            geo.list.max,
        );
        ui.interact(below, ui.id().with("tracks_empty_space"), Sense::click())
            .context_menu(|ui| {
                if let Some(a) = empty_menu(ui, editable) {
                    out = Some(a);
                }
            });
    }
    ui.add_space(space::MICRO);
    ui.painter().hline(
        ui.max_rect().x_range(),
        ui.cursor().top(),
        Stroke::new(1.0, pal.panel_border),
    );
    if let Some(sv) = sub {
        subchannel_section(ui, sv, editable, pal, dark, &mut out);
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.cursor().top(),
            Stroke::new(1.0, pal.panel_border),
        );
    }
    ui.add_space(space::NORMAL);
    // v0.8 §204: the Delete button follows the multi-selection (it used to
    // delete only the active track while several were selected).
    let multi_delete = !conductor_active
        && doc
            .track(active)
            .is_some_and(|t| acts_on_several(selected, Some(t.id()), t.id()));
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(editable, Button::new("+ Add Track"))
            .on_hover_text("Add an empty track and make it active")
            .clicked()
        {
            out = Some(TrackAction::Add);
        }
        if ui
            .add_enabled(editable, Button::new("Add Tracks…"))
            .on_hover_text(format!(
                "Add several empty tracks below the active track (at the end from the Conductor), in one step{}",
                crate::app::keybinds::hint(crate::app::commands::Command::AddTracks)
            ))
            .clicked()
        {
            out = Some(TrackAction::AddTracks(None));
        }
        if ui
            .add_enabled(
                editable,
                Button::new(if multi_delete {
                    "Delete Tracks"
                } else {
                    "Delete Track"
                }),
            )
            .on_hover_text(if multi_delete {
                "Delete the selected tracks (with their subtracks) after a confirmation"
            } else {
                "Delete the active track and all of its events (the Conductor cannot be deleted)"
            })
            .clicked()
        {
            out = Some(TrackAction::Delete);
        }
    });
    (out, geo)
}

/// What the track menu needs. One builder serves the panel's row, the Track
/// List's row (inside "Track “name” ▸") and the clip menu's Track ▸ (v0.9
/// §043 / §199, `docs/v0_9/CONTEXT_MATRIX.md`): the same actions from every
/// place a track is the context.
pub struct TrackMenu<'a> {
    pub doc: &'a PlumDocument,
    pub index: usize,
    pub row: &'a crate::app::TreeRowView,
    pub editable: bool,
    pub selected: &'a std::collections::BTreeSet<plumspirit_engine::model::TrackId>,
    /// The active track (`None`: the Conductor is selected).
    pub active: Option<plumspirit_engine::model::TrackId>,
    /// Offer Add Piano Roll Clip (at the edit cursor).
    pub add_clip: bool,
}

/// The track menu, categorised (§199): Clip | Track | Play | Add | Organise
/// | Delete. Returns the chosen action (the menu closes).
pub fn track_menu(ui: &mut egui::Ui, m: &TrackMenu<'_>) -> Option<TrackAction> {
    let (doc, i, row, editable) = (m.doc, m.index, m.row, m.editable);
    let track = doc.track(i)?;
    let mut out = None;
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, tip: &str, act: TrackAction| {
        let mut r = ui.add_enabled(enabled, Button::new(label));
        if !tip.is_empty() {
            r = r.on_hover_text(tip);
        }
        if r.clicked() {
            out = Some(act);
            ui.close();
        }
    };
    if m.add_clip {
        item(
            ui,
            editable,
            "Add Piano Roll Clip",
            "A new empty clip on this track at the edit cursor (one bar)",
            TrackAction::AddClip(i),
        );
        ui.separator();
    }
    item(
        ui,
        editable,
        "Track Properties…",
        "",
        TrackAction::Properties(i),
    );
    if ui
        .add_enabled(
            editable && track.end_of_track_tick.is_some(),
            Button::new("Fit Track End to Content"),
        )
        .on_disabled_hover_text("The track end already fits its content.")
        .clicked()
    {
        out = Some(TrackAction::FitEnd(i));
        ui.close();
    }
    ui.separator();
    // v0.8 §204: with several tracks selected, Mute / Solo act on the whole
    // selection (the M / S buttons stay per row).
    let multi = acts_on_several(m.selected, m.active, track.id());
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, tip: &str, act: TrackAction| {
        let mut r = ui.add_enabled(enabled, Button::new(label));
        if !tip.is_empty() {
            r = r.on_hover_text(tip);
        }
        if r.clicked() {
            out = Some(act);
            ui.close();
        }
    };
    let mute_label = match (multi, track.mute) {
        (true, false) => "Mute Selected Tracks",
        (true, true) => "Unmute Selected Tracks",
        (false, false) => "Mute",
        (false, true) => "Unmute",
    };
    item(
        ui,
        true,
        mute_label,
        "",
        if multi {
            TrackAction::MuteSelection(i, !track.mute)
        } else {
            TrackAction::ToggleMute(i)
        },
    );
    let solo_label = match (multi, track.solo) {
        (true, _) => "Solo Selected Tracks",
        (false, false) => "Solo",
        (false, true) => "Unsolo",
    };
    item(
        ui,
        true,
        solo_label,
        "",
        if multi {
            TrackAction::SoloSelection(i)
        } else {
            TrackAction::ToggleSolo(i, false)
        },
    );
    item(
        ui,
        true,
        "Play Only Selected Tracks",
        "",
        TrackAction::PlayOnlySelected,
    );
    item(
        ui,
        true,
        "Restore All Tracks",
        "",
        TrackAction::RestoreAllTracks,
    );
    item(
        ui,
        true,
        "Play All (clear Mute / Solo)",
        "",
        TrackAction::PlayAll,
    );
    if row.has_children {
        item(
            ui,
            true,
            "Mute Track + Subtracks",
            "",
            TrackAction::MuteSubtree(i, true),
        );
        item(
            ui,
            true,
            "Unmute Track + Subtracks",
            "",
            TrackAction::MuteSubtree(i, false),
        );
        item(
            ui,
            true,
            "Solo Track + Subtracks",
            "",
            TrackAction::SoloSubtree(i),
        );
    }
    ui.separator();
    item(
        ui,
        editable,
        "Add Subtrack",
        "",
        TrackAction::AddSubtrack(i),
    );
    item(
        ui,
        editable,
        "Add Subtracks…",
        "",
        TrackAction::AddSubtracks(i),
    );
    item(
        ui,
        editable,
        "Add Track Below",
        "An empty track after this one (and its subtracks), at the same level",
        TrackAction::AddBelow(i),
    );
    item(
        ui,
        editable,
        "Add Tracks Below…",
        "Several empty tracks after this one (and its subtracks), at the same level, in one step",
        TrackAction::AddTracks(Some(
            plumspirit_engine::editor::folders::AddTracksAt::Below(i),
        )),
    );
    ui.separator();
    ui.menu_button("Move", |ui| {
        use crate::app::commands::Command;
        use crate::app::keybinds::hint;
        for (label, act, tip, cmd) in [
            (
                "Up",
                TrackAction::Nudge(i, false),
                "Before the previous track of the same level",
                Command::MoveTrackUp,
            ),
            (
                "Down",
                TrackAction::Nudge(i, true),
                "After the next track of the same level",
                Command::MoveTrackDown,
            ),
            (
                "Indent (make subtrack)",
                TrackAction::Indent(i, true),
                "Becomes the last subtrack of the track above",
                Command::IndentTrack,
            ),
            (
                "Outdent",
                TrackAction::Indent(i, false),
                "Becomes the next track after its parent",
                Command::OutdentTrack,
            ),
        ] {
            item(ui, editable, label, &format!("{tip}{}", hint(cmd)), act);
        }
    });
    ui.menu_button("Folder", |ui| {
        let list = doc.project().folders.folders();
        for f in list {
            item(
                ui,
                editable && row.folder != f.id,
                f.name.as_str(),
                "Move this track (with its subtracks) into this folder",
                TrackAction::MoveToFolder(i, f.id),
            );
        }
        if !list.is_empty() {
            ui.separator();
        }
        item(
            ui,
            editable,
            if multi {
                "New Folder with Selected Tracks"
            } else {
                "New Folder with This Track"
            },
            "",
            TrackAction::FolderNew(Some((i, true))),
        );
        item(
            ui,
            editable,
            "New Empty Folder Here",
            "",
            TrackAction::FolderNew(Some((i, false))),
        );
        item(
            ui,
            editable && row.folder != 0,
            "Remove from Folder",
            "The track leaves its folder (placed right after it); nothing else changes",
            TrackAction::RemoveFromFolder(i),
        );
    });
    if row.has_children {
        item(
            ui,
            true,
            if row.collapsed { "Expand" } else { "Collapse" },
            "",
            TrackAction::ToggleCollapse(track.id()),
        );
    }
    ui.separator();
    item(
        ui,
        editable,
        if multi {
            "Delete Selected Tracks (with subtracks)…"
        } else {
            "Delete Track (with subtracks)…"
        },
        "",
        TrackAction::DeleteTracks(i),
    );
    out
}

/// The Conductor's menu (the panel's Conductor row at the edit position,
/// the Track List's Conductor lane at the clicked tick): Add Tempo Change /
/// Time Signature / Marker | Add Track, New Folder | Play All.
pub fn conductor_menu(
    ui: &mut egui::Ui,
    editable: bool,
    timing: bool,
    markers: bool,
    at: Option<u64>,
) -> Option<TrackAction> {
    use crate::app::commands::Command;
    let mut out = None;
    let fixed = "The Conductor's timing cannot be edited in this project (SMPTE timing or a MIDI Format 2 file).";
    for (enabled, cmd, what) in [
        (timing, Command::AddTempoChange, ConductorAdd::Tempo),
        (
            timing,
            Command::AddTimeSignature,
            ConductorAdd::TimeSignature,
        ),
        (markers, Command::AddMarker, ConductorAdd::Marker),
    ] {
        if ui
            .add_enabled(editable && enabled, Button::new(cmd.label()))
            .on_disabled_hover_text(fixed)
            .clicked()
        {
            out = Some(TrackAction::ConductorAdd(what, at));
            ui.close();
        }
    }
    ui.separator();
    if let Some(a) = empty_menu(ui, editable) {
        out = Some(a);
    }
    ui.separator();
    if ui
        .button(Command::PlayAll.label())
        .on_hover_text(format!(
            "Clears every Mute and Solo so the whole song plays{}",
            crate::app::keybinds::hint(Command::PlayAll)
        ))
        .clicked()
    {
        out = Some(TrackAction::PlayAll);
        ui.close();
    }
    out
}

/// The menu of empty space (below the rows, in the panel and the Track
/// List): Add Track, New Folder.
pub fn empty_menu(ui: &mut egui::Ui, editable: bool) -> Option<TrackAction> {
    let mut out = None;
    if ui
        .add_enabled(editable, Button::new("Add Track"))
        .on_hover_text(format!(
            "An empty track at the end, made active{}",
            crate::app::keybinds::hint(crate::app::commands::Command::AddTrack)
        ))
        .clicked()
    {
        out = Some(TrackAction::Add);
        ui.close();
    }
    if ui
        .add_enabled(editable, Button::new("Add Tracks…"))
        .on_hover_text("Several empty tracks at the end, in one step")
        .clicked()
    {
        out = Some(TrackAction::AddTracks(Some(
            plumspirit_engine::editor::folders::AddTracksAt::End,
        )));
        ui.close();
    }
    if ui
        .add_enabled(editable, Button::new("New Folder"))
        .on_hover_text("An empty folder at the end of the track list")
        .clicked()
    {
        out = Some(TrackAction::FolderNew(None));
        ui.close();
    }
    out
}

/// The folder header's menu (the panel's header and the Track List's folder
/// row): Rename / Expand | Add Track to Folder | New / Delete.
pub fn folder_menu(
    ui: &mut egui::Ui,
    id: u32,
    collapsed: bool,
    count: u32,
    editable: bool,
) -> Option<TrackAction> {
    let mut out = None;
    if ui.add_enabled(editable, Button::new("Rename…")).clicked() {
        out = Some(TrackAction::FolderRenameStart(id));
        ui.close();
    }
    if ui
        .button(if collapsed { "Expand" } else { "Collapse" })
        .clicked()
    {
        out = Some(TrackAction::FolderToggle(id));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(editable, Button::new("Add Track to Folder"))
        .on_hover_text("An empty track at the end of this folder")
        .clicked()
    {
        out = Some(TrackAction::FolderAddTrack(id));
        ui.close();
    }
    if ui
        .add_enabled(editable, Button::new("Add Tracks to Folder…"))
        .on_hover_text("Several empty tracks at the end of this folder, in one step")
        .clicked()
    {
        out = Some(TrackAction::AddTracks(Some(
            plumspirit_engine::editor::folders::AddTracksAt::Folder(id),
        )));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(editable, Button::new("New Folder"))
        .clicked()
    {
        out = Some(TrackAction::FolderNew(None));
        ui.close();
    }
    if ui
        .add_enabled(editable, Button::new("Delete Folder (tracks are kept)"))
        .on_hover_text("Removes the folder only: its tracks stay where they are, outside folders. Undo brings it back.")
        .clicked()
    {
        out = Some(TrackAction::FolderDelete(id));
        ui.close();
    }
    if ui
        .add_enabled(editable && count > 0, Button::new("Delete Folder and Its Tracks…"))
        .on_hover_text("Deletes the folder and every track in it (with subtracks), after a confirmation. One undo step.")
        .clicked()
    {
        out = Some(TrackAction::FolderDeleteWithTracks(id));
        ui.close();
    }
    out
}

/// Whether an action on `clicked` acts on several tracks: the same rule as
/// `app::track_ops::action_set` (the Ctrl-clicked tracks plus the active one,
/// when `clicked` is one of them), so labels say what the action does.
fn acts_on_several(
    selected: &std::collections::BTreeSet<plumspirit_engine::model::TrackId>,
    active: Option<plumspirit_engine::model::TrackId>,
    clicked: plumspirit_engine::model::TrackId,
) -> bool {
    let in_sel = selected.contains(&clicked) || active == Some(clicked);
    if !in_sel || selected.is_empty() {
        return false;
    }
    selected.len() + usize::from(active.is_some_and(|a| !selected.contains(&a))) > 1
}

/// A folder header row (v0.8): disclosure arrow, folder glyph, name (inline
/// rename), member count; drop target for tracks and folders, draggable.
#[allow(clippy::too_many_arguments)] // one call site; plain values
fn folder_row(
    ui: &mut egui::Ui,
    doc: &PlumDocument,
    row: &crate::app::TreeRowView,
    row_h: f32,
    editable: bool,
    pal: &Palette,
    renaming: &mut Option<(u32, String)>,
    out: &mut Option<TrackAction>,
) {
    use plumspirit_engine::history::TrackPlace;
    let id = row.folder;
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), row_h),
        if editable {
            Sense::click_and_drag()
        } else {
            Sense::click()
        },
    );
    let Some(info) = doc.project().folders.get(id) else {
        return;
    };
    if editable {
        resp.dnd_set_drag_payload(DragFolder(id));
    }
    let pointer = ui.ctx().pointer_interact_pos();
    let before_edge = |pos: Pos2| (pos.y - rect.top()) / rect.height().max(1.0) < 0.3;
    let stroke = Stroke::new(2.0, pal.accent);
    if let (Some(_), Some(pos)) = (resp.dnd_hover_payload::<DragTrack>(), pointer) {
        if before_edge(pos) {
            ui.painter().hline(rect.x_range(), rect.top() + 1.0, stroke);
        } else {
            ui.painter()
                .rect_stroke(rect.shrink(1.0), 0.0, stroke, egui::StrokeKind::Inside);
        }
    }
    if let (Some(d), Some(pos)) = (resp.dnd_release_payload::<DragTrack>(), pointer) {
        *out = Some(TrackAction::DropOnFolder {
            dragged: d.0,
            folder: id,
            before: before_edge(pos),
        });
    }
    let above = |pos: Pos2| pos.y < rect.center().y;
    if let (Some(d), Some(pos)) = (resp.dnd_hover_payload::<DragFolder>(), pointer)
        && d.0 != id
    {
        let y = if above(pos) {
            rect.top() + 1.0
        } else {
            rect.bottom() - 1.0
        };
        ui.painter().hline(rect.x_range(), y, stroke);
    }
    if let (Some(d), Some(pos)) = (resp.dnd_release_payload::<DragFolder>(), pointer)
        && d.0 != id
    {
        *out = Some(TrackAction::MoveFolder {
            folder: d.0,
            target: row.index,
            place: if above(pos) {
                TrackPlace::Before
            } else {
                TrackPlace::After
            },
        });
    }
    let p = ui.painter_at(rect);
    if resp.hovered() {
        p.rect_filled(rect, 0.0, pal.roll_black_row);
    }
    // Disclosure arrow.
    let c = Pos2::new(rect.left() + 6.0, rect.center().y);
    let col = pal.text_weak;
    let pts = if row.collapsed {
        vec![
            c + Vec2::new(-2.5, -4.0),
            c + Vec2::new(3.5, 0.0),
            c + Vec2::new(-2.5, 4.0),
        ]
    } else {
        vec![
            c + Vec2::new(-4.0, -2.5),
            c + Vec2::new(4.0, -2.5),
            c + Vec2::new(0.0, 3.5),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    // Folder glyph: a tab and a body.
    let g = Rect::from_min_size(
        Pos2::new(rect.left() + 15.0, rect.center().y - 5.0),
        Vec2::new(15.0, 11.0),
    );
    let tab = Rect::from_min_size(g.min - Vec2::new(0.0, 2.0), Vec2::new(6.0, 3.0));
    p.rect_filled(tab, 0.0, pal.text_weak);
    p.rect_filled(g, 0.0, pal.text_weak);
    let count = row.ordinal;
    let count_text = if count == 1 {
        "1 track".to_owned()
    } else {
        format!("{} tracks", group_digits(u64::from(count)))
    };
    let count_rect = p.text(
        Pos2::new(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        count_text,
        FontId::proportional(size::SMALL),
        pal.text_weak,
    );
    let name_left = rect.left() + 38.0;
    let editing = renaming.as_ref().is_some_and(|(r, _)| *r == id);
    if editing {
        let r = Rect::from_min_max(
            Pos2::new(name_left - 2.0, rect.top() + 3.0),
            Pos2::new(count_rect.left() - 6.0, rect.bottom() - 3.0),
        );
        let mut done = None;
        if let Some((_, text)) = renaming.as_mut() {
            let te = ui.put(
                r,
                egui::TextEdit::singleline(text)
                    .char_limit(plumspirit_engine::model::folder::MAX_FOLDER_NAME_CHARS)
                    .font(FontId::proportional(size::BODY)),
            );
            if !te.has_focus() && !te.lost_focus() {
                // First frame: focus with the whole name selected, so typing
                // replaces it.
                te.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(text.chars().count()),
                    );
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), te.id);
                }
            }
            if te.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(None);
                } else {
                    done = Some(Some(text.clone()));
                }
            }
        }
        match done {
            Some(Some(t)) => {
                *renaming = None;
                *out = Some(TrackAction::FolderRename(id, t));
            }
            Some(None) => *renaming = None,
            None => {}
        }
    } else {
        let mut job = LayoutJob::default();
        job.append(
            &info.name,
            0.0,
            egui::TextFormat {
                font_id: FontId::proportional(size::BODY),
                color: pal.text,
                ..Default::default()
            },
        );
        job.wrap = TextWrapping::truncate_at_width((count_rect.left() - 6.0 - name_left).max(10.0));
        let galley = p.layout_job(job);
        let y = rect.center().y - galley.size().y / 2.0;
        p.galley(Pos2::new(name_left, y), galley, pal.text);
    }
    if !editing {
        if resp.double_clicked() && editable {
            *out = Some(TrackAction::FolderRenameStart(id));
        } else if resp.clicked() {
            *out = Some(TrackAction::FolderToggle(id));
        }
    }
    let resp = resp.on_hover_text(format!(
        "Folder “{}” — {} (with their subtracks). A folder only organizes the track list: it has no notes or channel, does not play and is never exported.\nClick: collapse / expand · Double-click: rename · Drag: move the folder with its tracks · Drop tracks on it to put them inside (on its upper edge: before the folder)",
        info.name,
        if count == 1 { "1 track".to_owned() } else { format!("{count} tracks") }
    ));
    resp.context_menu(|ui| {
        if let Some(a) = folder_menu(ui, id, row.collapsed, count, editable) {
            *out = Some(a);
        }
    });
}

/// The active track's subchannel: colour, number/name, MIDI channel, quick
/// switching among the first 16, direct number entry, add and the full list.
/// Never one widget per subchannel beyond the first 16 swatches.
fn subchannel_section(
    ui: &mut egui::Ui,
    sv: SubchannelView<'_>,
    editable: bool,
    pal: &Palette,
    dark: bool,
    out: &mut Option<TrackAction>,
) {
    use crate::ui::theme::subchannel_color;
    let palette = crate::ui::theme::note_palette(ui.ctx());
    ui.add_space(space::MICRO);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Subchannel").size(size::SMALL).color(pal.text_weak));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("MIDI Ch {}", u16::from(sv.active.midi_channel) + 1))
                    .size(size::SMALL)
                    .color(pal.text_weak),
            )
            .on_hover_text("The real MIDI channel (1–16) new notes in this subchannel get. Subchannel numbers are PlumSpirit's own and are not limited to 16.");
        });
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (sw, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
        ui.painter()
            .rect_filled(sw, 0.0, subchannel_color(&palette, sv.active.id.0, dark));
        let label = match &sv.active.name {
            Some(n) => format!("{} · {n}", sv.active.id.number()),
            None => format!("Subchannel {}", sv.active.id.number()),
        };
        let b = ui
            .add_sized(
                [100.0, 20.0],
                Button::new(RichText::new(label).size(size::SMALL)).truncate(),
            )
            .on_hover_text(format!(
                "New notes go to this subchannel. {} subchannel(s) in this track — click for the full list.",
                group_digits(sv.table.len() as u64)
            ));
        if b.clicked() {
            *out = Some(TrackAction::Sub(SubAction::OpenWindow));
        }
        let add = ui
            .add_enabled(editable, Button::new(RichText::new("+").size(size::SMALL)))
            .on_hover_text("Add a subchannel (right-click: add several)");
        if add.clicked() {
            *out = Some(TrackAction::Sub(SubAction::Add));
        }
        if add.secondary_clicked() {
            *out = Some(TrackAction::Sub(SubAction::AddMany));
        }
        let r = ui
            .add(
                egui::TextEdit::singleline(sv.goto)
                    .desired_width(38.0)
                    .hint_text("#")
                    .font(FontId::proportional(size::SMALL)),
            )
            .on_hover_text("Type a subchannel number and press Enter to switch to it.");
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            *out = Some(TrackAction::Sub(SubAction::GoTo(sv.goto.clone())));
        }
    });
    // Quick switching: the first 16 subchannels as colour swatches, eight
    // per row; each a `size::MIN_TARGET` square target around a 12 pt mark
    // (B1-0069; they were 9 pt targets).
    let entries: Vec<_> = sv.table.entries().iter().take(16).collect();
    for (row, chunk) in entries.chunks(SWATCHES_PER_ROW).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for e in chunk {
                let (cell, resp) =
                    ui.allocate_exact_size(Vec2::splat(size::MIN_TARGET), Sense::click());
                let r = Rect::from_center_size(cell.center(), Vec2::splat(SWATCH));
                let c = subchannel_color(&palette, e.id.0, dark);
                ui.painter().rect_filled(r, 0.0, c);
                if e.id == sv.active.id {
                    ui.painter().rect_stroke(
                        r.expand(1.5),
                        0.0,
                        Stroke::new(1.0, pal.text),
                        egui::StrokeKind::Outside,
                    );
                } else if resp.hovered() {
                    ui.painter().rect_stroke(
                        r.expand(1.5),
                        0.0,
                        Stroke::new(1.0, pal.text_weak),
                        egui::StrokeKind::Outside,
                    );
                }
                if resp.has_focus() {
                    ui.painter().rect_stroke(
                        cell,
                        0.0,
                        Stroke::new(size::FOCUS_STROKE, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                let resp = resp.on_hover_text(format!(
                    "{} (MIDI Ch {})",
                    e.display_name(),
                    u16::from(e.midi_channel) + 1
                ));
                if resp.clicked() {
                    *out = Some(TrackAction::Sub(SubAction::Select(e.id)));
                }
            }
            if row == 1 && sv.table.len() > 16 {
                ui.label(
                    RichText::new(format!("+{}", sv.table.len() - 16))
                        .size(size::SMALL)
                        .color(pal.text_weak),
                );
            }
        });
    }
    ui.add_space(space::MICRO);
}

#[cfg(test)]
mod label_tests {
    use super::acts_on_several;
    use plumspirit_engine::model::TrackId;
    use std::collections::BTreeSet;

    /// Menu labels ("Delete Selected Tracks", "Mute Selected Tracks", the
    /// "Delete Tracks" button) follow the same rule as the actions.
    #[test]
    fn labels_say_several_exactly_when_the_action_takes_several_tracks() {
        let none = BTreeSet::new();
        let one: BTreeSet<TrackId> = [TrackId(3)].into();
        let two: BTreeSet<TrackId> = [TrackId(3), TrackId(4)].into();
        // No Ctrl-click: one track.
        assert!(!acts_on_several(&none, Some(TrackId(2)), TrackId(2)));
        // Active 2 + Ctrl-clicked 3: both, from either row.
        assert!(acts_on_several(&one, Some(TrackId(2)), TrackId(2)));
        assert!(acts_on_several(&one, Some(TrackId(2)), TrackId(3)));
        // A row outside the selection: that row alone.
        assert!(!acts_on_several(&one, Some(TrackId(2)), TrackId(5)));
        // The active track is the only selected one.
        assert!(!acts_on_several(
            &[TrackId(2)].into(),
            Some(TrackId(2)),
            TrackId(2)
        ));
        // Conductor selected (no active track): Ctrl-clicked tracks only.
        assert!(acts_on_several(&two, None, TrackId(4)));
        assert!(!acts_on_several(&one, None, TrackId(3)));
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    use crate::app::TreeRowView;
    use plumspirit_engine::model::{Bpm, TrackId};
    use std::collections::BTreeSet;

    fn run(
        build: &dyn Fn(&mut egui::Ui) -> Option<TrackAction>,
        label: Option<&str>,
    ) -> (Option<TrackAction>, Vec<String>) {
        crate::ui::testkit::run_menu(build, label)
    }

    fn doc(n: usize) -> PlumDocument {
        let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 480);
        for _ in 1..n {
            d.add_track().unwrap();
        }
        d
    }

    fn row(i: usize) -> TreeRowView {
        TreeRowView {
            index: i,
            depth: 0,
            has_children: false,
            collapsed: false,
            ordinal: i as u32 + 1,
            header: false,
            folder: 0,
        }
    }

    /// V09-S10: the panel's track menu offers Add Piano Roll Clip (at the
    /// edit cursor) for the clicked row, first, and its categories in order.
    #[test]
    fn the_track_menu_adds_a_clip_on_its_track_and_keeps_its_categories() {
        let d = doc(3);
        let r = row(1);
        let none = BTreeSet::new();
        let m = TrackMenu {
            doc: &d,
            index: 1,
            row: &r,
            editable: true,
            selected: &none,
            active: Some(d.tracks()[1].id()),
            add_clip: true,
        };
        let (a, labels) = run(&|ui| track_menu(ui, &m), Some("Add Piano Roll Clip"));
        assert_eq!(a, Some(TrackAction::AddClip(1)));
        let pos = |l: &str| labels.iter().position(|x| x == l).unwrap();
        assert!(pos("Add Piano Roll Clip") < pos("Track Properties…"));
        assert!(pos("Track Properties…") < pos("Mute"));
        assert!(pos("Mute") < pos("Add Subtrack"));
        assert!(pos("Add Subtrack") < pos("Move"));
        assert!(pos("Move") < pos("Delete Track (with subtracks)…"));
        // Without the clip item (the Track List row and the clip menu).
        let m = TrackMenu {
            add_clip: false,
            ..m
        };
        let (_, labels) = run(&|ui| track_menu(ui, &m), None);
        assert!(!labels.iter().any(|l| l == "Add Piano Roll Clip"));
        // Not editable: nothing that edits can be chosen.
        let m = TrackMenu {
            editable: false,
            add_clip: true,
            ..m
        };
        let (a, _) = run(&|ui| track_menu(ui, &m), Some("Add Piano Roll Clip"));
        assert_eq!(a, None);
    }

    /// With several tracks selected the labels and actions take the
    /// selection (Mute / Solo / Delete), as the panel did before.
    #[test]
    fn the_track_menu_acts_on_the_selection_it_belongs_to() {
        let d = doc(4);
        let r = row(2);
        let sel: BTreeSet<TrackId> = [d.tracks()[2].id(), d.tracks()[3].id()].into();
        let m = TrackMenu {
            doc: &d,
            index: 2,
            row: &r,
            editable: true,
            selected: &sel,
            active: Some(d.tracks()[2].id()),
            add_clip: true,
        };
        let (a, labels) = run(&|ui| track_menu(ui, &m), Some("Mute Selected Tracks"));
        assert_eq!(a, Some(TrackAction::MuteSelection(2, true)));
        assert!(
            labels
                .iter()
                .any(|l| l == "Delete Selected Tracks (with subtracks)…")
        );
    }

    #[test]
    fn the_folder_menu_offers_every_folder_action() {
        let (a, labels) = run(
            &|ui| folder_menu(ui, 7, false, 2, true),
            Some("Add Track to Folder"),
        );
        assert_eq!(a, Some(TrackAction::FolderAddTrack(7)));
        for l in [
            "Rename…",
            "Collapse",
            "New Folder",
            "Delete Folder (tracks are kept)",
            "Delete Folder and Its Tracks…",
        ] {
            assert!(labels.iter().any(|x| x == l), "{l}");
        }
    }
}

#[cfg(test)]
mod onion_cell_tests {
    use super::*;
    use crate::app::TreeRowView;
    use egui::{Pos2, Rect};
    use plumspirit_engine::model::Bpm;
    use std::collections::BTreeSet;

    struct Rig {
        ctx: egui::Context,
        doc: PlumDocument,
        rows: Vec<TreeRowView>,
        ghosts: Vec<usize>,
        geo: RowGeometry,
        left: f32,
    }

    impl Rig {
        fn new(n: usize) -> Rig {
            let mut doc = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 480);
            for _ in 1..n {
                doc.add_track().unwrap();
            }
            let rows = (0..n)
                .map(|i| TreeRowView {
                    index: i,
                    depth: 0,
                    has_children: false,
                    collapsed: false,
                    ordinal: i as u32 + 1,
                    header: false,
                    folder: 0,
                })
                .collect();
            let mut r = Rig {
                ctx: egui::Context::default(),
                doc,
                rows,
                ghosts: vec![],
                geo: RowGeometry::default(),
                left: 0.0,
            };
            r.frame(vec![]);
            r
        }

        /// One frame; returns the panel's action.
        fn frame(&mut self, events: Vec<egui::Event>) -> Option<TrackAction> {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 600.0))),
                events,
                ..Default::default()
            };
            let mut act = None;
            let (doc, rows, ghosts) = (&self.doc, &self.rows, &self.ghosts);
            let mut geo = RowGeometry::default();
            let mut left = 0.0;
            let mut o = self.ctx.run_ui(input, |ui| {
                egui::Panel::left("tracks")
                    .resizable(false)
                    .exact_size(PANEL_WIDTH)
                    .show(ui, |ui| {
                        left = ui.max_rect().left();
                        let mut renaming = None;
                        let counts = vec![0; doc.track_count()];
                        let (a, g) = track_panel(
                            ui,
                            Some(doc),
                            0,
                            false,
                            true,
                            &crate::ui::theme::DARK,
                            ghosts,
                            true,
                            &BTreeSet::new(),
                            rows,
                            &counts,
                            "120",
                            "",
                            None,
                            FolderPanel {
                                renaming: &mut renaming,
                            },
                            RowLayout {
                                row_h: 26.0,
                                scroll_to: None,
                            },
                        );
                        act = a;
                        geo = g;
                    });
            });
            o.textures_delta.clear();
            self.geo = geo;
            self.left = left;
            act
        }

        /// The centre of track `i`'s colour box.
        fn cell(&self, i: usize) -> Pos2 {
            let top = self.geo.rows.iter().find(|r| r.0 == i).unwrap().1;
            Pos2::new(self.left + 33.0, top + self.geo.row_h / 2.0)
        }
    }

    fn press(p: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// B1-0064: a click on a track's colour box shows it as a ghost, a click
    /// on a ghost hides it; the active track's box does nothing.
    #[test]
    fn a_click_on_the_colour_box_shows_or_hides_the_track() {
        let mut r = Rig::new(5);
        let c = r.cell(3);
        r.frame(vec![egui::Event::PointerMoved(c)]);
        let a = r.frame(vec![press(c, true), press(c, false)]);
        assert!(
            matches!(a, Some(TrackAction::OnionSet(ref t, true)) if t == &vec![3]),
            "{a:?}"
        );
        r.ghosts = vec![3];
        r.frame(vec![]);
        let a = r.frame(vec![press(c, true), press(c, false)]);
        assert!(
            matches!(a, Some(TrackAction::OnionSet(ref t, false)) if t == &vec![3]),
            "{a:?}"
        );
        let own = r.cell(0);
        r.frame(vec![egui::Event::PointerMoved(own)]);
        let a = r.frame(vec![press(own, true), press(own, false)]);
        assert!(!matches!(a, Some(TrackAction::OnionSet(..))), "{a:?}");
    }

    /// A drag from one colour box over the next rows sets them all to the
    /// first one's new state (already-matching rows are left alone).
    #[test]
    fn a_drag_over_colour_boxes_sets_every_track_passed() {
        let mut r = Rig::new(6);
        r.ghosts = vec![3];
        let (a, b, c, d) = (r.cell(1), r.cell(2), r.cell(3), r.cell(4));
        r.frame(vec![egui::Event::PointerMoved(a)]);
        r.frame(vec![press(a, true)]);
        let mut got: Vec<usize> = Vec::new();
        let mut on = None;
        for p in [Pos2::new(a.x, a.y + 3.0), b, c, d] {
            if let Some(TrackAction::OnionSet(t, v)) = r.frame(vec![egui::Event::PointerMoved(p)]) {
                got.extend(t);
                on = Some(v);
            }
        }
        r.frame(vec![press(d, false)]);
        got.sort_unstable();
        got.dedup();
        assert_eq!(on, Some(true));
        assert_eq!(got, vec![1, 2, 4], "3 was already shown");
        // After the release, moving over rows changes nothing.
        let e = r.cell(5);
        assert!(r.frame(vec![egui::Event::PointerMoved(e)]).is_none());
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    /// B1-0069: the M / S hit targets are at least `size::MIN_TARGET` in
    /// both directions at every row height the panel uses, never overlap,
    /// and contain their painted boxes.
    #[test]
    fn mute_and_solo_targets_are_big_enough_and_apart() {
        for row_h in [ROW_H, 22.0, 30.0] {
            let row = Rect::from_min_size(Pos2::new(0.0, 100.0), Vec2::new(PANEL_WIDTH, row_h));
            let [(m, mh), (s, sh)] = ms_rects(row);
            for (paint, hit) in [(m, mh), (s, sh)] {
                assert!(hit.width() >= size::MIN_TARGET, "{row_h}: {hit:?}");
                assert!(hit.height() >= size::MIN_TARGET, "{row_h}: {hit:?}");
                assert!(hit.contains_rect(paint));
                assert!(paint.width() >= size::MIN_TARGET);
                assert!(row.expand(2.0).contains_rect(hit));
            }
            assert!(mh.right() <= sh.left(), "targets overlap");
            assert!(m.right() < s.left());
        }
    }
}
