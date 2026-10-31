//! Restores PlumSpirit's track tree and subchannels from a MIDI file that
//! PlumSpirit exported with structure records (see `seqspec::TrackNodeRecord`).
//!
//! A Standard MIDI File has a flat list of tracks and 16 MIDI channels; it cannot
//! say that a track is a subtrack of another or that two groups of notes on the
//! same MIDI channel are different subchannels. When PlumSpirit exports such a
//! project it writes one record per `MTrk` chunk. On import:
//!
//! * no record at all → the ordinary mapping (every chunk a top-level track, one
//!   subchannel per MIDI channel used) stands;
//! * every chunk has exactly one well-formed record and together they describe a
//!   valid tree → the tree, the subchannel tables and every note's subchannel are
//!   restored, and extra "layer" chunks are merged back into their track;
//! * anything else (a record missing, damaged or contradictory — e.g. a file
//!   edited in another program) → the records are ignored and the ordinary mapping
//!   stands, with a warning. Nothing is invented.
//!
//! The records are always removed from the imported events (they are regenerated
//! on export), so they can never be duplicated.

use super::seqspec::{self, TYPE_TRACK_NODE, TrackNodeRecord};
use crate::model::document::PlumDocument;
use crate::model::events::RawKind;
use crate::model::note::SubchannelId;
use crate::model::track::{Subchannel, SubchannelTable, TrackId};
use std::collections::HashMap;

/// What [`restore_structure`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructureOutcome {
    /// The file has no PlumSpirit structure records.
    NotPresent,
    /// Tree and subchannels restored.
    Restored {
        subtracks: usize,
        subchannels: usize,
        layers_merged: usize,
    },
    /// Records were present but unusable; the ordinary mapping was kept.
    Ignored(String),
}

impl StructureOutcome {
    pub fn message(&self) -> Option<String> {
        match self {
            StructureOutcome::NotPresent => None,
            StructureOutcome::Restored {
                subtracks,
                subchannels,
                ..
            } => Some(format!(
                "PlumSpirit data in the file restored {subtracks} subtrack(s) and {subchannels} subchannel(s) exactly."
            )),
            StructureOutcome::Ignored(why) => Some(format!(
                "The file contains PlumSpirit structure data that does not match its tracks ({why}); it was ignored, and every MIDI track opens as a top-level track."
            )),
        }
    }
}

fn find_records(doc: &PlumDocument, i: usize) -> Vec<(usize, Option<TrackNodeRecord>)> {
    let t = &doc.tracks()[i];
    let mut out = Vec::new();
    for (k, e) in t.events.events().iter().enumerate() {
        if let RawKind::Meta {
            meta_type: seqspec::META_SEQUENCER_SPECIFIC,
            payload,
        } = e.kind
            && let Some((ty, body)) = seqspec::parse(t.events.payload(payload))
            && ty == TYPE_TRACK_NODE
        {
            out.push((k, TrackNodeRecord::decode(body)));
        }
    }
    out
}

/// Applies the structure records of a freshly imported document (see module
/// docs). O(notes) for tracks whose subchannels are restored.
pub fn restore_structure(doc: &mut PlumDocument) -> StructureOutcome {
    let n = doc.track_count();
    let found: Vec<Vec<(usize, Option<TrackNodeRecord>)>> =
        (0..n).map(|i| find_records(doc, i)).collect();
    if found.iter().all(Vec::is_empty) {
        return StructureOutcome::NotPresent;
    }
    // Remove every record from the events first; they are regenerated on export.
    let plan = plan_restore(doc, &found);
    for (i, recs) in found.iter().enumerate() {
        if recs.is_empty() {
            continue;
        }
        let idx: Vec<usize> = recs.iter().map(|r| r.0).collect();
        if let Some(t) = doc.track_mut(i) {
            t.events.remove_sorted_indices(&idx);
            t.bump_events_revision();
        }
    }
    match plan {
        Ok(p) => apply(doc, p),
        Err(why) => StructureOutcome::Ignored(why),
    }
}

struct Plan {
    /// Per chunk: node, parent, layer, channel → subchannel map.
    chunks: Vec<TrackNodeRecord>,
    /// Chunk index of each node's own (layer 0) chunk.
    primary: HashMap<u32, usize>,
}

fn plan_restore(
    doc: &PlumDocument,
    found: &[Vec<(usize, Option<TrackNodeRecord>)>],
) -> Result<Plan, String> {
    let mut chunks = Vec::with_capacity(found.len());
    for (i, recs) in found.iter().enumerate() {
        match recs.as_slice() {
            [(_, Some(r))] => chunks.push(r.clone()),
            [] => return Err(format!("MIDI track {} has none", i + 1)),
            [(_, None)] => return Err(format!("MIDI track {} has damaged data", i + 1)),
            _ => return Err(format!("MIDI track {} has more than one record", i + 1)),
        }
    }
    let mut primary: HashMap<u32, usize> = HashMap::new();
    let mut last_layer: HashMap<u32, u32> = HashMap::new();
    for (i, r) in chunks.iter().enumerate() {
        if r.layer == 0 {
            if primary.insert(r.node, i).is_some() {
                return Err("two tracks claim the same identity".into());
            }
            if r.subs.is_empty() {
                return Err(format!("MIDI track {} lists no subchannels", i + 1));
            }
            last_layer.insert(r.node, 0);
        } else {
            // Layers follow their track directly, in order.
            let prev = i
                .checked_sub(1)
                .map(|p| &chunks[p])
                .ok_or("a layer comes first")?;
            if prev.node != r.node || prev.layer + 1 != r.layer || !r.subs.is_empty() {
                return Err(format!("MIDI track {} is a misplaced layer", i + 1));
            }
            last_layer.insert(r.node, r.layer);
        }
        if let Some(p) = r.parent
            && p == r.node
        {
            return Err("a track is its own parent".into());
        }
    }
    // Every channel a chunk's notes use must be mapped, to a listed subchannel.
    for (i, r) in chunks.iter().enumerate() {
        let own = &chunks[*primary.get(&r.node).ok_or("a layer has no track")?];
        let used = doc.tracks()[i].notes.channel_sub_pairs();
        for (ch, _) in used {
            let Ok(k) = r.map.binary_search_by_key(&ch, |m| m.0) else {
                return Err(format!("MIDI track {} uses an unmapped channel", i + 1));
            };
            let sub = r.map[k].1;
            if own.subs.binary_search_by_key(&sub, |s| s.0).is_err() {
                return Err(format!(
                    "MIDI track {} maps to an unknown subchannel",
                    i + 1
                ));
            }
        }
        if let Some(p) = r.parent
            && !primary.contains_key(&p)
        {
            return Err(format!("MIDI track {} names a missing parent", i + 1));
        }
    }
    Ok(Plan { chunks, primary })
}

fn apply(doc: &mut PlumDocument, plan: Plan) -> StructureOutcome {
    let n = plan.chunks.len();
    let id_of = |chunk: usize, doc: &PlumDocument| doc.tracks()[chunk].id();
    let mut layers_merged = 0usize;
    // Subchannel tables and note subchannels, per chunk.
    for (i, r) in plan.chunks.iter().enumerate() {
        let mut map = [0u32; 16];
        for &(ch, sub) in &r.map {
            map[usize::from(ch)] = sub;
        }
        let t = doc.track_mut(i).expect("chunk exists");
        t.notes.assign_subs_by_channel(&map);
        if r.layer == 0 {
            let entries: Vec<Subchannel> = r
                .subs
                .iter()
                .map(|(id, ch, name)| Subchannel {
                    id: SubchannelId(*id),
                    midi_channel: *ch,
                    name: name
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .filter(|s| !s.trim().is_empty()),
                })
                .collect();
            match SubchannelTable::from_entries(entries) {
                Ok(table) => t.set_subchannels(table),
                Err(e) => return undo_partial(doc, e),
            }
        }
    }
    // Parents (by node id → the node's track id in this document).
    let parents: Vec<Option<TrackId>> = plan
        .chunks
        .iter()
        .map(|r| r.parent.map(|p| id_of(plan.primary[&p], doc)))
        .collect();
    for (i, p) in parents.into_iter().enumerate() {
        if plan.chunks[i].layer == 0 {
            doc.track_mut(i).expect("chunk exists").set_parent(p);
        }
    }
    // Merge each track's layers into it: one k-way merge per track (O(notes),
    // however many layers), then drop the layer chunks (back to front so
    // indices stay valid).
    let mut remove = Vec::new();
    let mut i = 0;
    while i < n {
        let mut end = i + 1;
        while end < n && plan.chunks[end].layer > 0 {
            end += 1;
        }
        if end > i + 1 {
            let merged = {
                let parts: Vec<&crate::model::NoteStore> =
                    (i..end).map(|k| &doc.tracks()[k].notes).collect();
                crate::model::NoteStore::merge_sorted(&parts)
            };
            match merged {
                Ok(m) => doc.track_mut(i).expect("chunk exists").notes = m,
                Err(e) => return undo_partial(doc, e.to_string()),
            }
            remove.extend(i + 1..end);
            layers_merged += end - i - 1;
        }
        i = end;
    }
    for k in remove.into_iter().rev() {
        doc.remove_imported_track(k);
    }
    if let Err(e) = doc.validate_hierarchy() {
        return undo_partial(doc, e);
    }
    let rows = doc.tree_rows();
    StructureOutcome::Restored {
        subtracks: rows.iter().filter(|r| r.0 > 0).count(),
        subchannels: doc.tracks().iter().map(|t| t.subchannels().len()).sum(),
        layers_merged,
    }
}

/// A restore step failed after validation (should not happen): flatten the
/// tree again so the document is at least a valid flat import.
fn undo_partial(doc: &mut PlumDocument, why: String) -> StructureOutcome {
    for i in 0..doc.track_count() {
        if let Some(t) = doc.track_mut(i) {
            t.set_parent(None);
            t.infer_subchannels_from_channels();
        }
    }
    StructureOutcome::Ignored(why)
}
