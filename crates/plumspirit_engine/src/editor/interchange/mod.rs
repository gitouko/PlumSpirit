//! The musical clipboard shared by independent PlumSpirit projects.

#[cfg(test)]
mod tests;
mod wire;
pub use wire::{
    MAX_NOTES, MAX_WIRE_BYTES, decode, decode_controlled, encode, encode_controlled, wire_len,
};

use std::collections::{BTreeMap, BTreeSet};

use super::clipboard::Clipboard;
use super::clips::{self, ClipClipboard};
use super::controllers::EventClip;
use super::{NoteTarget, note_view};
use crate::history::Operation;
use crate::model::clip::ContentRef;
use crate::model::events::EventStore;
use crate::model::note_store::NoteStore;
use crate::model::project::KeyRange;
use crate::model::timing::{Division, TimingKind};
use crate::model::track::{Subchannel, SubchannelTable};
use crate::model::{ClipId, GroupInfo, GroupTable, NoteRecord, PlumDocument, SubchannelId};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub division: Division,
    pub keys: KeyRange,
    /// Song position of the first selected item, for provenance. The payload
    /// uses relative ticks, or content ticks inside a Clip.
    pub anchor: u64,
}

impl Origin {
    pub fn new(doc: &PlumDocument, anchor: u64) -> Self {
        Self {
            division: doc.division(),
            keys: doc.project().keys,
            anchor,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Music {
    Notes(Clipboard),
    Clips(ClipBlock),
    Controller { clip: EventClip, channel: u8 },
    Master(Vec<(u64, u16)>),
}

impl Music {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Notes(_) => "notes",
            Self::Clips(_) => "Clips",
            Self::Controller { .. } => "controller events",
            Self::Master(_) => "Master Volume points",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClipBlock {
    clips: ClipClipboard,
    subs: Vec<Vec<Subchannel>>,
    groups: Vec<GroupInfo>,
}

impl ClipBlock {
    pub fn capture(doc: &PlumDocument, ids: &[ClipId], clips: ClipClipboard) -> Result<Self> {
        if ids.len() != clips.entries.len() {
            return Err("The copied Clip selection changed.".into());
        }
        if clips
            .entries
            .iter()
            .try_fold(0usize, |n, entry| n.checked_add(entry.content.notes.len()))
            .is_none_or(|n| n > MAX_NOTES)
        {
            return Err(
                "The selection is too large for the clipboard. Copy fewer Clips at once.".into(),
            );
        }
        let mut subs = Vec::with_capacity(ids.len());
        let mut used_groups = BTreeSet::new();
        for (&id, entry) in ids.iter().zip(&clips.entries) {
            let placement = clips::placement(doc, id).map_err(|e| e.to_string())?;
            let source = match placement.content {
                ContentRef::Track(track) => track,
                ContentRef::Owned(_) => placement.track,
            };
            let track = doc
                .track_by_id(source)
                .ok_or("The source track no longer exists.")?;
            let mut used = BTreeMap::new();
            for n in entry.content.notes.iter() {
                used.entry(n.sub).or_insert(n.attrs.channel());
                if n.group != 0 {
                    used_groups.insert(n.group);
                }
            }
            subs.push(
                used.into_iter()
                    .map(|(id, channel)| {
                        track
                            .subchannels()
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| Subchannel::new(id, channel))
                    })
                    .collect(),
            );
        }
        let table = &doc.project().groups;
        let mut groups = BTreeMap::new();
        for mut id in used_groups {
            while id != 0 && !groups.contains_key(&id) {
                let group = table
                    .get(id)
                    .ok_or("A copied note refers to a missing group.")?;
                groups.insert(id, group.clone());
                id = group.parent;
            }
        }
        Ok(Self {
            clips,
            subs,
            groups: groups.into_values().collect(),
        })
    }

    pub fn len(&self) -> usize {
        self.clips.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// Prepares one undo step with all dependencies and owned Clip contents.
    pub fn prepare(
        mut self,
        doc: &PlumDocument,
        origin: Origin,
        track: usize,
        at: u64,
    ) -> Result<Operation> {
        let scale = TickScale::between(origin.division, doc.division())?;
        let rows = self.clips.rows();
        if rows > doc.track_count().saturating_sub(track) {
            return Err(format!(
                "The copied Clips need {rows} track rows from the active track down."
            ));
        }
        let (groups, group_ids) = import_groups(&doc.project().groups, &self.groups)?;
        let mut tables = BTreeMap::new();
        for (entry, source_subs) in self.clips.entries.iter_mut().zip(self.subs) {
            let row = track + entry.rows_below;
            let destination = doc
                .track(row)
                .ok_or("The destination track no longer exists.")?;
            let table = tables
                .entry(row)
                .or_insert_with(|| destination.subchannels().clone());
            let (ids, new_subs) = import_subs(&source_subs, table)?;
            table.insert_all(&new_subs)?;
            let offset = scale.tick(entry.offset)?;
            let length = entry
                .length
                .map(|len| {
                    let end = scale.tick(add_tick(entry.offset, len)?)?;
                    Ok::<_, String>(if len == 0 { 0 } else { (end - offset).max(1) })
                })
                .transpose()?;
            let mut records =
                super::try_records(entry.content.notes.len()).map_err(|e| e.user_message())?;
            for n in entry.content.notes.iter() {
                let mut note = n.record();
                scale.note(&mut note, doc.project().keys)?;
                let was_in_window = in_window(n.start_tick, entry.offset, entry.length);
                if was_in_window != in_window(note.start_tick, offset, length) {
                    return Err("The target PPQN would move a note across a Clip boundary.".into());
                }
                note.sub = *ids
                    .get(&note.sub)
                    .ok_or("A copied note refers to a missing Subchannel.")?;
                note.group = group_ids.get(&note.group).copied().unwrap_or(0);
                records.push(note);
            }
            let mut notes = NoteStore::new();
            notes
                .insert_sorted_batch(&records, false)
                .map_err(|e| e.to_string())?;
            let mut events = EventStore::new();
            for event in entry.content.events.events() {
                let tick = scale.tick(event.tick)?;
                if in_window(event.tick, entry.offset, entry.length)
                    != in_window(tick, offset, length)
                {
                    return Err(
                        "The target PPQN would move an event across a Clip boundary.".into(),
                    );
                }
                events
                    .push(crate::model::events::RawEvent { tick, ..*event })
                    .map_err(|e| e.to_string())?;
            }
            entry.after = scale.tick(entry.after)?;
            entry.offset = offset;
            entry.length = length;
            entry.content.notes = notes;
            entry.content.events = events;
        }
        let mut ops = Vec::new();
        if !self.groups.is_empty() {
            ops.push(Operation::SetGroups {
                table: Box::new(groups),
                label: "Paste Clips",
            });
        }
        for (track, table) in tables {
            let original = doc.track(track).unwrap().subchannels();
            let subs: Vec<_> = table
                .entries()
                .iter()
                .filter(|sub| !original.contains(sub.id))
                .cloned()
                .collect();
            if !subs.is_empty() {
                ops.push(Operation::AddSubchannels { track, subs });
            }
        }
        ops.push(clips::paste_clips(doc, &self.clips, track, at).map_err(|e| e.to_string())?);
        Ok(Operation::Group {
            ops,
            label: "Paste Clips",
        })
    }
}

fn in_window(tick: u64, offset: u64, length: Option<u64>) -> bool {
    tick.checked_sub(offset)
        .is_some_and(|rel| length.is_none_or(|len| rel < len))
}

/// Adapts clipboard note coordinates and subchannel identities to the target
/// before handing the batch to the editor's ordinary Paste preparation.
pub fn normalize_notes(
    clip: &mut Clipboard,
    doc: &PlumDocument,
    target: NoteTarget,
    origin: Origin,
) -> Result<()> {
    let scale = TickScale::between(origin.division, doc.division())?;
    let host = note_view(doc, target).map_err(|e| e.user_message())?;
    let (ids, new_subs) = import_subs(&clip.subs, host.subchannels())?;
    for note in &mut clip.records {
        scale.note(note, doc.project().keys)?;
        note.sub = *ids
            .get(&note.sub)
            .ok_or("A copied note refers to a missing Subchannel.")?;
    }
    clip.subs = new_subs;
    Ok(())
}

/// Keep one distinct destination ID per source identity. A compatible existing
/// descriptor can be reused on repeat paste. Destination properties stay intact.
fn import_subs(
    source: &[Subchannel],
    destination: &SubchannelTable,
) -> Result<(BTreeMap<SubchannelId, SubchannelId>, Vec<Subchannel>)> {
    let mut mapping = BTreeMap::new();
    let mut claimed = BTreeSet::new();
    let mut occupied: BTreeSet<_> = destination.entries().iter().map(|s| s.id).collect();
    occupied.extend(source.iter().map(|s| s.id));
    let mut added = Vec::new();
    for sub in source {
        let matches = |s: &Subchannel| s.midi_channel == sub.midi_channel && s.name == sub.name;
        let id = match destination.get(sub.id) {
            None => sub.id,
            Some(s) if matches(s) && !claimed.contains(&s.id) => s.id,
            Some(_) => {
                let compatible = destination.entries().iter().find(|s| {
                    matches(s)
                        && !claimed.contains(&s.id)
                        && source.binary_search_by_key(&s.id, |x| x.id).is_err()
                });
                if let Some(s) = compatible {
                    s.id
                } else {
                    let mut next = occupied.last().map_or(0, |id| id.0.wrapping_add(1));
                    while occupied.contains(&SubchannelId(next)) {
                        next = next.wrapping_add(1);
                    }
                    SubchannelId(next)
                }
            }
        };
        if !destination.contains(id) {
            added.push(Subchannel { id, ..sub.clone() });
        }
        claimed.insert(id);
        occupied.insert(id);
        mapping.insert(sub.id, id);
    }
    Ok((mapping, added))
}

pub(super) fn import_groups(
    destination: &GroupTable,
    source: &[GroupInfo],
) -> Result<(GroupTable, BTreeMap<u32, u32>)> {
    let mut table = destination.clone();
    let count = u32::try_from(source.len()).map_err(|_| "Too many copied groups.")?;
    if source.len().saturating_add(table.len()) > crate::model::group::MAX_GROUPS
        || table.next_id().checked_add(count).is_none()
    {
        return Err("There are too many note groups to paste.".into());
    }
    let first = table.reserve(count);
    let ids: BTreeMap<_, _> = source
        .iter()
        .enumerate()
        .map(|(i, g)| (g.id, first + i as u32))
        .collect();
    let mut links = BTreeMap::new();
    for g in source {
        let id = ids[&g.id];
        let link = if g.link == 0 {
            0
        } else {
            *links.entry(g.link).or_insert(id)
        };
        table.insert(GroupInfo {
            id,
            parent: ids.get(&g.parent).copied().unwrap_or(0),
            link,
            locked: false,
            temporary: false,
            ..g.clone()
        });
    }
    table.validate()?;
    Ok((table, ids))
}

pub fn normalize_points(
    points: &mut [(u64, u16)],
    source: Division,
    target: Division,
) -> Result<()> {
    let scale = TickScale::between(source, target)?;
    let mut last = None;
    for (tick, _) in points {
        *tick = scale.tick(*tick)?;
        if last == Some(*tick) {
            return Err(
                "The target PPQN would merge graph points. Increase it before pasting.".into(),
            );
        }
        last = Some(*tick);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct TickScale {
    from: u128,
    to: u128,
}

impl TickScale {
    fn between(from: Division, to: Division) -> Result<Self> {
        if from == to {
            return Ok(Self { from: 1, to: 1 });
        }
        let (Some(from), Some(to)) = (from.musical_ppqn(), to.musical_ppqn()) else {
            return Err("The clipboard and project use incompatible timebases. SMPTE requires the same frame rate and ticks per frame.".into());
        };
        Ok(Self {
            from: from.into(),
            to: to.into(),
        })
    }

    fn tick(self, tick: u64) -> Result<u64> {
        u64::try_from((u128::from(tick) * self.to + self.from / 2) / self.from)
            .map_err(|_| "The paste would exceed the largest tick.".into())
    }

    fn note(self, note: &mut NoteRecord, keys: KeyRange) -> Result<()> {
        if note.attrs.pitch() > keys.max_pitch() {
            return Err(
                "A copied note needs 256-Key Mode. Enable it in Project Properties before pasting."
                    .into(),
            );
        }
        let end = self.tick(add_tick(note.start_tick, note.duration_ticks)?)?;
        let start = self.tick(note.start_tick)?;
        let duration = (end - start).max(u64::from(note.duration_ticks != 0));
        add_tick(start, duration)?;
        note.start_tick = start;
        note.duration_ticks = duration;
        Ok(())
    }
}

fn add_tick(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| "The paste would exceed the largest tick.".into())
}

fn valid_division(d: Division) -> bool {
    match d.kind() {
        TimingKind::Ppqn(ppqn) => ppqn != 0,
        TimingKind::Smpte {
            frames_per_second,
            ticks_per_frame,
        } => matches!(frames_per_second, 24 | 25 | 29 | 30) && ticks_per_frame != 0,
    }
}
