//! The export snapshot (v0.9 V09-S16, `docs/EXPORT_PLAN.md` D3): one
//! immutable, shareable description of the MIDI file the project exports to
//! with given options — what the exporter writes, what the built-in MIDI
//! Player plays and draws and what the MIDI Inspector counts (contract §053,
//! §247, §248). Nobody writes a temporary `.mid` and reads it back.
//!
//! [`ExportSnapshot::take`] keeps a private copy of the document — its notes
//! share the document's chunks and its events the document's event lists
//! (copy-on-write), so the copy costs O(tracks + chunks + contents) and an
//! edit after it copies only what it changes; the application's own
//! document is not held, so editing is never blocked by a snapshot. With it
//! the [`ExportPlan`] for the options and the [`export_key`] it was taken
//! at: a consumer keeps the `Arc` and takes a new snapshot only when
//! [`ExportSnapshot::is_current`] says the project or the options changed.
//!
//! Everything a snapshot reports comes from the same [`PlannedStream`]s and
//! notes the writer uses ([`ExportSnapshot::write`]), so they cannot
//! disagree; `tests/v09_export_plan.rs` checks it against the written bytes
//! read back by the independent tokenizer of [`super::semantic`].
//!
//! [`export_key`]: PlumDocument::export_key

use super::export::{
    ExportControl, ExportError, OutEvent, PlannedStream, WriteStats, merge_by_track, output_notes,
    planned_eot_unmapped, write_document,
};
use super::overlap::{NoteWriteOrder, SameKeyOverlaps};
use super::plan::{ExportOptions, ExportPlan, OutputTrack, PlanKind, plan_export_with};
use super::semantic::{SemanticFile, SemanticTrack};
use crate::model::document::PlumDocument;
use crate::model::events::{RawKind, channel_data_len};
use crate::model::note::NoteOffKind;
use std::borrow::Cow;
use std::io::{Seek, Write};
use std::sync::Arc;

/// One note as the exported file holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SnapshotNote {
    /// Note On tick (file ticks).
    pub start: u64,
    /// Termination tick (file ticks; `start` for a zero-length note).
    pub end: u64,
    /// `MTrk` chunk index.
    pub chunk: u32,
    /// Document track the chunk belongs to.
    pub track: u32,
    pub channel: u8,
    /// 0–127; above 127 only in the experimental 256-key file.
    pub key: u8,
    pub velocity: u8,
    pub off_velocity: u8,
    pub off: NoteOffKind,
}

/// See the module docs.
#[derive(Debug)]
pub struct ExportSnapshot {
    doc: PlumDocument,
    plan: ExportPlan,
    options: ExportOptions,
    key: u64,
}

impl ExportSnapshot {
    /// Snapshot of `doc` exported with `options`. Costs the copy (see the
    /// module docs) plus [`plan_export_with`]: O(tracks, events, Conductor
    /// and placements), and O(notes) when the PPQN is rescaled, tracks have
    /// corrections or keys above 127 are possible.
    pub fn take(doc: &PlumDocument, options: ExportOptions) -> Arc<ExportSnapshot> {
        let doc = doc.clone();
        let plan = plan_export_with(&doc, options);
        let key = doc.export_key();
        Arc::new(ExportSnapshot {
            doc,
            plan,
            options,
            key,
        })
    }

    /// True while `doc` exported with `options` would give this snapshot.
    pub fn is_current(&self, doc: &PlumDocument, options: ExportOptions) -> bool {
        self.options == options && self.key == doc.export_key()
    }

    /// The document's [`PlumDocument::export_key`] when it was taken.
    pub fn key(&self) -> u64 {
        self.key
    }

    pub fn options(&self) -> ExportOptions {
        self.options
    }

    /// The document as it was when the snapshot was taken.
    pub fn document(&self) -> &PlumDocument {
        &self.doc
    }

    /// The decisions (disclosures, approval, blockers, reports).
    pub fn plan(&self) -> &ExportPlan {
        &self.plan
    }

    /// Why nothing can be written, if anything prevents it.
    pub fn blocker(&self) -> Option<&str> {
        self.plan.blockers.first().map(String::as_str)
    }

    /// The `MTrk` chunks, in file order.
    pub fn chunks(&self) -> Cow<'_, [OutputTrack]> {
        self.plan.layout(&self.doc)
    }

    /// The header's timing division word.
    pub fn division_word(&self) -> u16 {
        self.plan.division_word
    }

    /// The header's format word.
    pub fn format_word(&self) -> u16 {
        self.plan.format(&self.doc).word()
    }

    /// The events of chunk `chunk` in the order they are written: `(file
    /// tick, event)`, gap fillers and the structure record included, End of
    /// Track excluded (it follows at [`PlannedStream::end_of_track`]).
    pub fn stream(&self, chunk: usize) -> Option<PlannedStream<'_>> {
        let out = match &self.plan.layout(&self.doc) {
            Cow::Borrowed(v) => v.get(chunk)?,
            Cow::Owned(_) => {
                let t = self.doc.track(chunk)?;
                return Some(PlannedStream::new(t, chunk, &self.plan));
            }
        };
        let t = self.doc.track(out.track)?;
        Some(PlannedStream::for_output(t, out, &self.plan))
    }

    /// Calls `f` with every note the file holds, chunk by chunk, each chunk's
    /// notes in Note On order. O(notes); nothing is collected.
    pub fn for_each_note(&self, mut f: impl FnMut(SnapshotNote)) {
        let layout = self.plan.layout(&self.doc);
        for (ci, out) in layout.iter().enumerate() {
            let Some(t) = self.doc.track(out.track) else {
                continue;
            };
            for n in NoteWriteOrder::new(output_notes(t, out, &self.plan)) {
                let a = n.attrs;
                let start = self.plan.map_tick(n.start_tick);
                f(SnapshotNote {
                    start,
                    end: if n.duration_ticks == 0 {
                        start
                    } else {
                        self.plan.map_tick(n.end_tick())
                    },
                    chunk: ci as u32,
                    track: out.track as u32,
                    channel: a.channel(),
                    key: a.pitch(),
                    velocity: a.velocity(),
                    off_velocity: a.off_velocity(),
                    off: a.off_kind(),
                });
            }
        }
    }

    /// Document tracks whose chunks hold notes that start inside a note of
    /// the same channel and key, with their counts — exactly what the writer
    /// reports in [`WriteStats::same_key`] (V09-0102). O(notes); nothing is
    /// collected.
    pub fn same_key_overlaps(&self) -> Vec<(usize, SameKeyOverlaps)> {
        let layout = self.plan.layout(&self.doc);
        let mut v = Vec::new();
        for out in layout.iter() {
            let Some(t) = self.doc.track(out.track) else {
                continue;
            };
            let mut order = NoteWriteOrder::new(output_notes(t, out, &self.plan)).counting();
            for _ in order.by_ref() {}
            if order.overlaps().any() {
                v.push((out.track, order.overlaps()));
            }
        }
        merge_by_track(v)
    }

    /// The file's length in document ticks: its latest End of Track — every
    /// chunk's End of Track as the writer computes it (the explicit end or
    /// the corrected content end, clips' placements included; the first
    /// track's also the Conductor's last event) — and never before the
    /// last Conductor event. 0 for an empty project. What the MIDI Player
    /// shows as the length (V09-S17 P10) and the Inspector as the duration.
    /// O(chunks), never the notes.
    pub fn end_tick(&self) -> u64 {
        let layout = self.plan.layout(&self.doc);
        layout
            .iter()
            .filter_map(|out| {
                let t = self.doc.track(out.track)?;
                Some(planned_eot_unmapped(t, out.track, &self.plan))
            })
            .max()
            .unwrap_or(0)
            .max(self.plan.last_tick())
    }

    /// Notes the file holds (its Note On events). O(notes).
    pub fn note_count(&self) -> u64 {
        let mut n = 0u64;
        self.for_each_note(|_| n += 1);
        n
    }

    /// The tempo changes the file holds, `(file tick, microseconds per
    /// quarter note)` sorted by tick (a tie keeps chunk order): the
    /// Conductor's (tempo ramps as the steps written) and any tempo event
    /// preserved in a track. What a player of the exported file follows.
    /// O(events + Conductor), never the notes.
    pub fn tempo_changes(&self) -> Vec<(u64, u32)> {
        let mut v: Vec<(u64, u32)> = self
            .plan
            .events
            .iter()
            .filter_map(|e| match e.kind {
                PlanKind::Tempo(us) => Some((self.plan.map_tick(e.tick), us)),
                _ => None,
            })
            .collect();
        for (i, t) in self.doc.tracks().iter().enumerate() {
            for (tick, e) in self.plan.events_of(t, i) {
                if let RawKind::Meta {
                    meta_type: crate::model::events::meta::TEMPO,
                    payload,
                } = e.kind
                {
                    let p = t.events.payload(payload);
                    if p.len() >= 3 {
                        let us = u32::from_be_bytes([0, p[0], p[1], p[2]]);
                        v.push((self.plan.map_tick(tick), us));
                    }
                }
            }
        }
        v.sort_by_key(|&(t, _)| t);
        v
    }

    /// The file in the normalised form of [`super::semantic`], built from the
    /// streams (no bytes): header, every chunk's other events in order, its
    /// Note On and termination multisets and End of Track, and the preserved
    /// extra chunks. O(events + notes).
    pub fn semantic(&self) -> SemanticFile {
        let layout = self.plan.layout(&self.doc);
        let mut file = SemanticFile {
            format: self.format_word(),
            division: self.division_word(),
            tracks: Vec::with_capacity(layout.len()),
            extra_chunks: Vec::new(),
        };
        let count = self.doc.track_count();
        let extra = |pos: usize, out: &mut Vec<([u8; 4], Vec<u8>)>| {
            for c in self.doc.extra_chunks() {
                if c.after_track_chunks.min(count) == pos {
                    out.push((c.chunk_type, c.data.clone()));
                }
            }
        };
        for (ci, out) in layout.iter().enumerate() {
            if out.layer == 0 {
                extra(out.track, &mut file.extra_chunks);
            }
            let Some(t) = self.doc.track(out.track) else {
                continue;
            };
            let mut st = SemanticTrack::default();
            let mut stream = self.stream(ci).expect("a chunk of the layout");
            let mut last = 0;
            for (tick, ev) in stream.by_ref() {
                last = tick;
                if let Some(status) = ev.channel_status() {
                    let data = ev.channel_data();
                    let (kind, ch) = (status & 0xF0, status & 0x0F);
                    if kind == 0x90 && data[1] > 0 {
                        st.note_ons.push((tick, ch, data[0], data[1]));
                    } else if kind == 0x90 {
                        st.note_offs.push((tick, ch, data[0], 0, true));
                    } else if kind == 0x80 {
                        st.note_offs.push((tick, ch, data[0], data[1], false));
                    } else {
                        let mut v = vec![status];
                        v.extend_from_slice(&data[..channel_data_len(status)]);
                        st.other.push((tick, v));
                    }
                    continue;
                }
                let bytes = match ev {
                    OutEvent::Tempo(us) => {
                        let b = us.to_be_bytes();
                        meta_bytes(0x51, &[b[1], b[2], b[3]])
                    }
                    OutEvent::Meta { meta_type, payload } => meta_bytes(meta_type, payload),
                    OutEvent::SysEx { payload } => [&[0xF0][..], payload].concat(),
                    OutEvent::Raw(kind) => match *kind {
                        RawKind::Meta { meta_type, payload } => {
                            meta_bytes(meta_type, t.events.payload(payload))
                        }
                        RawKind::SysEx { payload } => {
                            [&[0xF0][..], t.events.payload(payload)].concat()
                        }
                        RawKind::SysExEscape { payload } => {
                            [&[0xF7][..], t.events.payload(payload)].concat()
                        }
                        RawKind::System { status, data } => vec![status, data[0], data[1]],
                        RawKind::Channel { .. } => unreachable!("channel events have a status"),
                    },
                    OutEvent::NoteOn { .. } | OutEvent::NoteOff { .. } => {
                        unreachable!("note events have a status")
                    }
                };
                st.other.push((tick, bytes));
            }
            st.end_of_track = Some(stream.end_of_track().max(last));
            st.note_ons.sort_unstable();
            st.note_offs.sort_unstable();
            file.tracks.push(st);
        }
        extra(count, &mut file.extra_chunks);
        file
    }

    /// Writes the file into `w` (see [`write_document`]).
    pub fn write<W: Write + Seek>(
        &self,
        w: &mut W,
        control: ExportControl<'_>,
    ) -> Result<WriteStats, ExportError> {
        write_document(&self.doc, &self.plan, w, control)
    }

    /// The file's bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ExportError> {
        let mut cur = std::io::Cursor::new(Vec::new());
        self.write(&mut cur, ExportControl::default())?;
        Ok(cur.into_inner())
    }
}

/// A meta event in the normalised form of [`super::semantic`].
fn meta_bytes(meta_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, meta_type];
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}
