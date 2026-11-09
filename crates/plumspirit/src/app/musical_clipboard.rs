//! Clipboard jobs use a read-only project snapshot. Only the UI thread
//! publishes a completed copy or commits a completed paste to history.

use std::sync::Arc;
use std::time::Instant;

use super::clip_session::AggregateHost;
use super::graphs::EventClipboard;
use super::jobs::{self, JobKind, JobOutput};
use super::track_list::{ClipIntent, MainView};
use super::{PlumSpiritApp, StatusKind};
use plumspirit_engine::editor::interchange::{
    self, ClipBlock, MAX_NOTES, Music, Origin, decode_controlled, encode, encode_controlled,
};
use plumspirit_engine::editor::{
    Clipboard, EditControl, EditError, NoteTarget, PreparedEdit, clips, note_view,
};
use plumspirit_engine::history::Operation;
use plumspirit_engine::model::{ClipId, GraphTarget, GroupTable, PlumDocument};

#[path = "musical_clipboard_packing.rs"]
mod packing;

#[cfg(windows)]
#[path = "musical_clipboard_win.rs"]
mod native;

#[cfg(not(windows))]
mod native {
    pub fn available() -> bool {
        false
    }
    pub fn sequence() -> u32 {
        0
    }
    pub fn read() -> Result<(u32, Vec<u8>), String> {
        Err("The musical clipboard is unavailable.".into())
    }
    pub fn write(_: &[u8]) -> Result<u32, String> {
        Err("The musical clipboard is unavailable.".into())
    }
}

/// Tests can share a memory clipboard between two independent app harnesses.
#[derive(Clone, Default)]
pub(super) struct Backend {
    #[cfg(test)]
    mock: Option<Arc<std::sync::Mutex<MockClipboard>>>,
}

#[cfg(test)]
#[derive(Default)]
struct MockClipboard {
    sequence: u32,
    bytes: Option<Vec<u8>>,
    busy: bool,
}

impl Backend {
    fn active(&self) -> bool {
        #[cfg(test)]
        if self.mock.is_some() {
            return true;
        }
        cfg!(all(windows, not(test)))
    }

    fn available(&self) -> bool {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            return mock.lock().unwrap().bytes.is_some();
        }
        native::available()
    }

    fn sequence(&self) -> u32 {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            return mock.lock().unwrap().sequence;
        }
        native::sequence()
    }

    fn read(&self) -> Result<(u32, Vec<u8>), String> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            let clipboard = mock.lock().unwrap();
            if clipboard.busy {
                return Err("Clipboard busy.".into());
            }
            return clipboard
                .bytes
                .clone()
                .map(|bytes| (clipboard.sequence, bytes))
                .ok_or_else(|| "No musical data.".into());
        }
        native::read()
    }

    fn write(&self, bytes: &[u8]) -> Result<u32, String> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            let mut clipboard = mock.lock().unwrap();
            if clipboard.busy {
                return Err("Clipboard busy.".into());
            }
            clipboard.sequence += 1;
            clipboard.bytes = Some(bytes.to_vec());
            return Ok(clipboard.sequence);
        }
        native::write(bytes)
    }
}

pub(super) enum Cut {
    None,
    Notes(Operation),
    Clips(Vec<ClipId>),
}

pub(super) enum Completion {
    Copy {
        bytes: Vec<u8>,
        message: String,
        cut: Cut,
    },
    Paste {
        sequence: u32,
        destination: Destination,
        prepared: Paste,
    },
}

pub(super) enum Paste {
    Notes(PreparedEdit, Option<GroupTable>),
    Clips {
        op: Operation,
        first: u32,
        count: u32,
    },
    Graph(EventClipboard),
}

#[derive(PartialEq, Eq)]
pub(super) struct Destination {
    view: MainView,
    track: usize,
    conductor: bool,
    cursor: u64,
    notes: (NoteTarget, u64),
    graph_focus: bool,
    graph_slot: usize,
    graph: GraphTarget,
    channel: u8,
}

impl Destination {
    fn get(app: &PlumSpiritApp) -> Self {
        Self {
            view: app.main_view,
            track: app.active_track(),
            conductor: app.conductor_selected(),
            cursor: app.roll.cursor_tick,
            notes: app.destination_at(app.roll.cursor_tick),
            graph_focus: app.graph_focus,
            graph_slot: app.active_graph,
            graph: app.shown_target(app.active_graph),
            channel: app.graph_slot(app.active_graph).channel,
        }
    }
}

impl PlumSpiritApp {
    pub(super) fn uses_system_clipboard(&self) -> bool {
        self.clipboard_backend.active()
    }

    pub(super) fn has_musical_clipboard(&self) -> bool {
        self.uses_system_clipboard() && self.clipboard_backend.available()
    }

    fn queue_clipboard(
        &mut self,
        label: &'static str,
        defer: bool,
        work: impl FnOnce(&PlumDocument, EditControl<'_>) -> Result<Completion, String> + Send + 'static,
    ) {
        if self.job.is_some() {
            self.set_status(
                "Wait for the current task before using the clipboard.",
                StatusKind::Info,
            );
            return;
        }
        if !defer {
            let started = Instant::now();
            if let Some(result) = self.doc().map(|doc| work(doc, EditControl::default())) {
                self.finish_clipboard(result, started);
            }
            return;
        }
        let Some(doc) = self.open.as_ref().map(|o| Arc::clone(&o.doc)) else {
            return;
        };
        let id = self.next_id();
        let kind = JobKind::Clipboard {
            label,
            generation: self.doc_generation(),
        };
        match jobs::spawn(
            id,
            kind,
            0,
            &self.clipboard_ctx,
            "clipboard",
            move |cancel, _| {
                let result = work(
                    &doc,
                    EditControl {
                        cancel: Some(cancel),
                        progress: None,
                    },
                );
                drop(doc);
                JobOutput::Clipboard(result)
            },
        ) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.set_status(
                format!("Could not start the clipboard task ({e})."),
                StatusKind::Error,
            ),
        }
    }

    pub(super) fn request_musical_copy(&mut self, cut: bool) {
        if self.graph_focus {
            if let Some((anchor, clip)) = self.graph_selection_for_copy() {
                if self.copy_graph_selection(anchor, clip) && cut {
                    let before = self.open.as_ref().map(|o| o.history.current_state());
                    self.delete_graph_selection();
                    if before == self.open.as_ref().map(|o| o.history.current_state()) {
                        self.set_status(
                            "Copied the graph points. They were not cut.",
                            StatusKind::Warning,
                        );
                    }
                }
                return;
            }
            if self.graphs.views[self.active_graph]
                .has_point_selection(self.shown_target(self.active_graph))
            {
                self.set_status(
                    "This Event Graph selection cannot be copied.",
                    StatusKind::Warning,
                );
                return;
            }
        }
        self.request_note_copy(cut);
    }

    pub(super) fn request_note_copy(&mut self, cut: bool) {
        if !self.uses_system_clipboard() {
            if self.copy() && cut {
                self.delete_selected();
            }
            return;
        }
        let count = self.selected_count();
        if count > MAX_NOTES {
            self.set_status(
                format!("The clipboard accepts up to {MAX_NOTES} notes. Copy this selection in smaller parts."),
                StatusKind::Warning,
            );
            return;
        }
        let hosts = self.aggregate_hosts();
        let cut = if cut {
            Cut::Notes(self.routed("Cut Notes", |track| Operation::DeleteSelected { track }))
        } else {
            Cut::None
        };
        self.queue_clipboard("Copying notes", count >= 4096, move |doc, control| {
            let (anchor, clip) = snapshot_notes(doc, &hosts)
                .map_err(|e| e.user_message())?
                .ok_or("Select notes to copy first.")?;
            let message = format!("Copied {} note(s) to the Windows clipboard.", clip.len());
            let bytes = packing::pack(encode_controlled(
                Origin::new(doc, anchor),
                &Music::Notes(clip),
                control,
            )?);
            Ok(Completion::Copy {
                bytes,
                message,
                cut,
            })
        });
    }

    pub(super) fn request_clip_copy(&mut self, ids: Vec<ClipId>, cut: bool) {
        let defer = ids.len() > 16
            || self.doc().is_some_and(|doc| {
                let (notes, events) = ids.iter().filter_map(|&id| doc.clips().placement(id)).fold(
                    (0usize, 0usize),
                    |(notes, events), p| {
                        (
                            notes.saturating_add(doc.notes_of(p.content).map_or(0, |s| s.len())),
                            events.saturating_add(
                                doc.events_of(p.content).map_or(0, |s| s.events().len()),
                            ),
                        )
                    },
                );
                notes.saturating_add(events) >= 4096
            });
        self.queue_clipboard("Copying Clips", defer, move |doc, control| {
            let anchor = ids
                .iter()
                .filter_map(|&id| doc.clips().placement(id))
                .map(|p| p.start)
                .min()
                .unwrap_or(0);
            let count = ids.len();
            let clip = clips::copy_clips(doc, &ids).map_err(|e| e.to_string())?;
            let block = ClipBlock::capture(doc, &ids, clip)?;
            let bytes = packing::pack(encode_controlled(
                Origin::new(doc, anchor),
                &Music::Clips(block),
                control,
            )?);
            let cut = if cut { Cut::Clips(ids) } else { Cut::None };
            Ok(Completion::Copy {
                bytes,
                message: format!("Copied {count} Clip(s) to the Windows clipboard."),
                cut,
            })
        });
    }

    pub(super) fn publish_music(&mut self, origin: Origin, music: &Music) -> bool {
        match encode(origin, music)
            .and_then(|bytes| self.clipboard_backend.write(&packing::pack(bytes)))
        {
            Ok(sequence) => {
                self.clipboard_origin = Some((sequence, self.doc_generation()));
                true
            }
            Err(e) => {
                self.set_status(e, StatusKind::Error);
                false
            }
        }
    }

    pub(super) fn paste_music(&mut self) {
        self.request_clipboard_paste(None);
    }

    pub(super) fn request_clipboard_paste(&mut self, clips: Option<(usize, u64)>) {
        let destination = Destination::get(self);
        let own_sequence = self
            .clipboard_origin
            .filter(|(_, generation)| *generation == self.doc_generation())
            .map(|(seq, _)| seq);
        let backend = self.clipboard_backend.clone();
        self.queue_clipboard("Preparing paste", true, move |doc, control| {
            let (sequence, bytes) = backend.read()?;
            let (origin, music) = decode_controlled(&packing::unpack(bytes)?, control)?;
            let same_project = own_sequence == Some(sequence);
            let prepared = match (music, clips) {
                (Music::Clips(block), Some((track, at))) => Paste::Clips {
                    count: block.len() as u32,
                    op: block.prepare(doc, origin, track, at)?,
                    first: doc.clips().layout().next_clip,
                },
                (Music::Notes(mut clip), None)
                    if !destination.graph_focus && !destination.conductor =>
                {
                    let (target, at) = destination.notes;
                    if !same_project || origin.division != doc.division() {
                        interchange::normalize_notes(&mut clip, doc, target, origin)?;
                    }
                    if clip.has_groups() {
                        let (edit, groups) = clip
                            .prepare_paste_grouped(doc, target, at)
                            .map_err(|e| e.user_message())?;
                        Paste::Notes(edit, Some(groups))
                    } else {
                        let edit = clip
                            .prepare_paste(doc, target, at)
                            .map_err(|e| e.user_message())?;
                        Paste::Notes(edit, None)
                    }
                }
                (Music::Controller { mut clip, channel }, None) if !destination.conductor => {
                    if !same_project && channel != destination.channel {
                        return Err(format!(
                            "Choose MIDI channel {} in the Event Graph to paste these events.",
                            channel + 1
                        ));
                    }
                    interchange::normalize_points(
                        &mut clip.points,
                        origin.division,
                        doc.division(),
                    )?;
                    Paste::Graph(EventClipboard::Controller(clip))
                }
                (Music::Master(mut points), None) => {
                    interchange::normalize_points(&mut points, origin.division, doc.division())?;
                    Paste::Graph(EventClipboard::Master(points))
                }
                (other, _) => return Err(wrong_target(&other)),
            };
            Ok(Completion::Paste {
                sequence,
                destination,
                prepared,
            })
        });
    }

    pub(super) fn finish_clipboard(
        &mut self,
        result: Result<Completion, String>,
        started: Instant,
    ) {
        match result {
            Err(e) => self.set_status(e, StatusKind::Warning),
            Ok(Completion::Copy {
                bytes,
                message,
                cut,
            }) => {
                let sequence = match self.clipboard_backend.write(&bytes) {
                    Ok(sequence) => sequence,
                    Err(e) => {
                        self.set_status(e, StatusKind::Error);
                        return;
                    }
                };
                self.clipboard_origin = Some((sequence, self.doc_generation()));
                let before = self.open.as_ref().map(|o| o.history.current_state());
                let cut_name = match cut {
                    Cut::None => {
                        self.set_status(message, StatusKind::Info);
                        return;
                    }
                    Cut::Notes(op) => {
                        self.apply_op(op);
                        "notes"
                    }
                    Cut::Clips(ids) => {
                        self.apply_clip_intent(ClipIntent::Delete(ids));
                        "Clips"
                    }
                };
                let after = self.open.as_ref().map(|o| o.history.current_state());
                if before == after {
                    self.set_status(
                        format!("Copied the {cut_name}. They were not cut."),
                        StatusKind::Warning,
                    );
                } else {
                    self.set_status(
                        format!("Cut {cut_name} to the Windows clipboard."),
                        StatusKind::Info,
                    );
                }
            }
            Ok(Completion::Paste {
                sequence,
                destination,
                prepared,
            }) => {
                if sequence != self.clipboard_backend.sequence()
                    || destination != Destination::get(self)
                {
                    self.set_status(
                        "The clipboard or paste destination changed. Paste again.",
                        StatusKind::Warning,
                    );
                    return;
                }
                match prepared {
                    Paste::Notes(edit, Some(groups)) => {
                        self.apply_grouped_paste(edit, groups, destination.cursor)
                    }
                    Paste::Notes(edit, None) => self.commit_prepared("Paste", edit, started),
                    Paste::Clips { op, first, count } => self.finish_clip_paste(op, first, count),
                    Paste::Graph(clip) => {
                        self.paste_graph_clip(clip);
                    }
                }
            }
        }
    }
}

fn wrong_target(music: &Music) -> String {
    let target = match music {
        Music::Notes(_) => "the Piano Roll note grid",
        Music::Clips(_) => "the Track List",
        Music::Controller { .. } => "the matching controller in the Event Graph",
        Music::Master(_) => "the Master Volume Event Graph",
    };
    format!(
        "The clipboard contains {}. Paste them in {target}.",
        music.label()
    )
}

pub(super) fn snapshot_notes(
    doc: &PlumDocument,
    hosts: &[AggregateHost],
) -> Result<Option<(u64, Clipboard)>, EditError> {
    let mut parts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for host in hosts {
        // Linked instances share their notes and are copied once.
        if !seen.insert(host.target) {
            continue;
        }
        let view = note_view(doc, host.target)?;
        let Some(bounds) = view.notes.selection_bounds() else {
            continue;
        };
        let anchor = super::clip_session::shifted(bounds.min_start, host.shift);
        if let Some(clip) = Clipboard::copy_selected_with_groups(view, &doc.project().groups)? {
            parts.push((clip, anchor));
        }
    }
    if parts.len() == 1 {
        return Ok(parts.pop().map(|(clip, anchor)| (anchor, clip)));
    }
    Ok(parts.iter().map(|p| p.1).min().map(|first| {
        (
            first,
            Clipboard::combine(
                parts
                    .into_iter()
                    .map(|(clip, at)| (clip, at - first))
                    .collect(),
            ),
        )
    }))
}

#[cfg(test)]
#[path = "musical_clipboard_tests.rs"]
mod tests;
