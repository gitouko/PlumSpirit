//! Internal note clipboard.
//!
//! The clipboard owns compact copies of the copied notes (relative start, duration,
//! packed pitch/velocity/channel/off data). It never refers to document memory, so
//! deleting the source notes or the source track does not affect it. The
//! musical clipboard encodes these records without document references.

use super::{EditError, PreparedEdit, try_records};
use crate::model::document::PlumDocument;
use crate::model::note::NoteRecord;
use crate::model::track::Subchannel;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clipboard {
    /// Notes with `start_tick` relative to the earliest copied note, in stored order.
    pub(super) records: Vec<NoteRecord>,
    /// The subchannels the copied notes use (so a paste into another track can
    /// recreate the ones it lacks).
    pub(super) subs: Vec<Subchannel>,
    /// The groups the copied notes use, with their ancestors (v0.7): a paste
    /// creates fresh groups with the same names, colours and nesting.
    pub(super) groups: Vec<crate::model::GroupInfo>,
}

impl Clipboard {
    /// Copies the selected notes of `track`. Returns `Ok(None)` if nothing is
    /// selected. Fails gracefully if the copy cannot be allocated.
    pub fn copy_selected<'a>(
        track: impl Into<super::HostView<'a>>,
    ) -> Result<Option<Clipboard>, EditError> {
        let track = track.into();
        let Some(b) = track.notes.selection_bounds() else {
            return Ok(None);
        };
        let mut records = try_records(b.count)?;
        let mut used = BTreeMap::new();
        track.notes.for_each_selected(|v| {
            used.entry(v.sub).or_insert(v.attrs.channel());
            records.push(NoteRecord {
                start_tick: v.start_tick - b.min_start,
                duration_ticks: v.duration_ticks,
                attrs: v.attrs,
                sub: v.sub,
                group: v.group,
            });
        });
        let subs = used
            .into_iter()
            .map(|(id, channel)| {
                track
                    .subchannels()
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| Subchannel::new(id, channel))
            })
            .collect();
        Ok(Some(Clipboard {
            records,
            subs,
            groups: Vec::new(),
        }))
    }

    /// v1.0-beta2 B2-F18: one clipboard from the copies of several note
    /// hosts (the clips of a Track Aggregate), each moved `offset` ticks
    /// after the earliest copied note of all; notes ordered by start
    /// (stable), subchannels and groups each once (the first wins).
    pub fn combine(parts: Vec<(Clipboard, u64)>) -> Clipboard {
        let mut out = Clipboard::default();
        for (c, offset) in parts {
            out.records.extend(c.records.into_iter().map(|mut r| {
                r.start_tick = r.start_tick.saturating_add(offset);
                r
            }));
            for s in c.subs {
                if !out.subs.iter().any(|x| x.id == s.id) {
                    out.subs.push(s);
                }
            }
            for g in c.groups {
                if !out.groups.iter().any(|x| x.id == g.id) {
                    out.groups.push(g);
                }
            }
        }
        out.records.sort_by_key(|r| r.start_tick);
        out.subs.sort_by_key(|s| s.id);
        out.groups.sort_by_key(|g| g.id);
        out
    }

    /// Like [`Clipboard::copy_selected`], also remembering the copied notes'
    /// groups (and their ancestors) from `table`.
    pub fn copy_selected_with_groups<'a>(
        track: impl Into<super::HostView<'a>>,
        table: &crate::model::GroupTable,
    ) -> Result<Option<Clipboard>, EditError> {
        let Some(mut c) = Self::copy_selected(track)? else {
            return Ok(None);
        };
        let mut ids: Vec<u32> = c
            .records
            .iter()
            .map(|r| r.group)
            .filter(|&g| g != 0)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let mut i = 0;
        while i < ids.len() {
            if let Some(g) = table.get(ids[i])
                && g.parent != 0
                && !ids.contains(&g.parent)
            {
                ids.push(g.parent);
            }
            i += 1;
        }
        ids.sort_unstable();
        c.groups = ids
            .iter()
            .filter_map(|&id| table.get(id).cloned())
            .collect();
        Ok(Some(c))
    }

    /// Whether the copied notes belong to groups.
    pub fn has_groups(&self) -> bool {
        self.records.iter().any(|r| r.group != 0)
    }

    /// [`Clipboard::prepare_paste`] for notes in groups: the pasted notes go to
    /// fresh groups (same names, colours, nesting and links; unlocked), returned
    /// as the new group table to commit in the same undo step.
    pub fn prepare_paste_grouped(
        &self,
        doc: &PlumDocument,
        track_index: impl Into<super::NoteTarget>,
        at_tick: u64,
    ) -> Result<(PreparedEdit, crate::model::GroupTable), EditError> {
        let edit = self.prepare_paste(doc, track_index, at_tick)?;
        let (table, ids) = super::interchange::import_groups(&doc.project().groups, &self.groups)
            .map_err(EditError::Invalid)?;
        let edit = match edit {
            PreparedEdit::InsertSelected {
                host,
                base_revision,
                mut records,
                subs_track,
                new_subs,
            } => {
                for r in &mut records {
                    r.group = ids.get(&r.group).copied().unwrap_or(0);
                }
                PreparedEdit::InsertSelected {
                    host,
                    base_revision,
                    records,
                    subs_track,
                    new_subs,
                }
            }
            other => other,
        };
        Ok((edit, table))
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[NoteRecord] {
        &self.records
    }

    pub fn heap_bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<NoteRecord>()
    }

    /// Prepares pasting into track `track_index` so the earliest copied note starts
    /// at `at_tick`. Relative timing, pitch, duration, channel and velocities are
    /// preserved; the pasted notes become the selection.
    ///
    /// Subchannels: every note keeps its subchannel number. When the destination
    /// has no subchannel with that number, the paste creates it (same name and
    /// MIDI channel as in the source) as part of the same undo step; pasting again
    /// reuses it, so repeated pastes never multiply subchannels.
    pub fn prepare_paste(
        &self,
        doc: &PlumDocument,
        track_index: impl Into<super::NoteTarget>,
        at_tick: u64,
    ) -> Result<PreparedEdit, EditError> {
        if self.records.is_empty() {
            return Err(EditError::ClipboardEmpty);
        }
        let t = super::note_view(doc, track_index)?;
        // Notes copied in a 256-key project paste only where their keys exist.
        let top = doc.project().keys.max_pitch();
        let (lo, hi) = self.records.iter().fold((u8::MAX, 0u8), |(lo, hi), r| {
            (lo.min(r.attrs.pitch()), hi.max(r.attrs.pitch()))
        });
        if hi > top {
            return Err(EditError::PitchOutOfRange {
                operation: "Pasting here",
                lowest: i32::from(lo),
                highest: i32::from(hi),
                top,
            });
        }
        let mut out = try_records(self.records.len())?;
        let overflow = EditError::TickOverflow {
            operation: "Pasting here",
        };
        for r in &self.records {
            let start = at_tick
                .checked_add(r.start_tick)
                .ok_or_else(|| overflow.clone())?;
            start
                .checked_add(r.duration_ticks)
                .ok_or_else(|| overflow.clone())?;
            out.push(NoteRecord {
                start_tick: start,
                duration_ticks: r.duration_ticks,
                attrs: r.attrs,
                sub: r.sub,
                group: r.group,
            });
        }
        let mut new_subs = Vec::new();
        for sc in &self.subs {
            if !t.subchannels().contains(sc.id) {
                new_subs.push(sc.clone());
            }
        }
        // Notes copied from a subchannel that no longer existed at copy time (or
        // a clipboard from before subchannels) fall back to the main one.
        for r in &mut out {
            if !t.subchannels().contains(r.sub) && !new_subs.iter().any(|s| s.id == r.sub) {
                r.sub = t.subchannels().first().id;
            }
        }
        Ok(PreparedEdit::InsertSelected {
            host: t.host(),
            base_revision: t.notes.revision(),
            records: out,
            subs_track: t.track.id(),
            new_subs,
        })
    }
}

#[cfg(test)]
mod combine_tests {
    use super::*;

    /// B2-F18: copies of two clips become one clipboard in song order.
    #[test]
    fn combined_copies_keep_their_distance() {
        use crate::model::note::{NoteAttrs, NoteOffKind};
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let one = |t: u64| Clipboard {
            records: vec![NoteRecord::new(t, 10, a)],
            ..Clipboard::default()
        };
        let c = Clipboard::combine(vec![(one(0), 500), (one(5), 0)]);
        let starts: Vec<u64> = c.records().iter().map(|r| r.start_tick).collect();
        assert_eq!(starts, vec![5, 500]);
    }
}
