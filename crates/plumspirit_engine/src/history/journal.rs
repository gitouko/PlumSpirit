//! Persistent undo / redo (v0.8 §084–§093): the history journal on disk.
//!
//! # Model
//!
//! Project checkpoint + append-only reversible operation journal + compaction:
//!
//! * The **checkpoint** is the saved `.lup` file itself. A `SAVED` record names
//!   the history state that was written and the file's [`identity`] (a hash of
//!   its musical content and structure), so a reopened file finds its place in
//!   the history — and a file changed elsewhere (stale) is never attached to a
//!   history that does not describe it.
//! * Every undo entry is appended once as an `ENTRY` record: the same compact
//!   reversible patch the in-memory history keeps (note positions as run-length
//!   ranks, notes as 6–9 byte compact records, removed / added tracks as a small
//!   `.lup` image) — never a snapshot of the project. Undo / redo append a tiny
//!   `CURSOR` record. Entries that do not fit the RAM budget stay on disk and are
//!   read back when undo reaches them (only a bounded window lives in RAM).
//! * A `BASE` record starts a lineage: history before it is not usable (cleared
//!   history, an edit that could not be recorded, a new project).
//! * **Compaction** rewrites the journal with only the live path (the undo and
//!   redo stacks), dropping discarded branches and old cursor / save records
//!   (temporary file + rename).
//!
//! # Branching
//!
//! Standard linear history: A → B → C, undo to B, new edit D — the redo path to C
//! is **discarded** (in memory at once; on disk it stays until compaction but is
//! never offered again: a restore follows the newest entry leaving each state).
//!
//! # Crash safety
//!
//! Records are `[length u32][kind u8, body][CRC-32 u32]`, appended after the
//! valid end. A record is written with a zero length, its body streamed, then
//! its real length patched in, so a crash leaves at worst one invalid record at
//! the end; opening stops at the first invalid record and truncates the
//! damaged tail (Repair). Bodies of large entries are checked again (CRC) when
//! they are read for undo. A journal that cannot be used (unknown version,
//! another project, stale) is never applied, and the project always opens.

use super::{Change, Entry, History, Inserted, NotesPatch};
use crate::codec::{Crc32, get_ivarint, get_uvarint, put_ivarint, put_uvarint};
use crate::editor::Shift;
use crate::model::clip::{ContentId, ContentRef};
use crate::model::conductor::{
    ConductorEdit, Interp, Marker, MasterVolumePoint, Origin, TempoPoint, TimeSignature,
};
use crate::model::events::{RawEvent, RawKind};
use crate::model::patch::{CompactRecords, ListPatch, RankSet};
use crate::model::project::KeyRange;
use crate::model::track::{
    GatePercent, OutputRoute, Subchannel, Track, TrackId, TrackKind, TrackProps,
};
use crate::model::{
    Bpm, FolderInfo, GroupInfo, GroupTable, PlumDocument, TempoValue, TrackFolders,
};
use std::collections::{HashMap, HashSet};

/// Where owned-content hosts start in the host encoding: a track id `t` is
/// written as `t` (as journals before V09-S11 wrote it), a content id `c` as
/// `2^32 + c` — a reader older than S11 refuses it as a track id, never
/// misreads it (`docs/CLIP_PIANO_ROLL.md`).
const HOST_CONTENT: u64 = 1 << 32;

fn put_host(o: &mut Vec<u8>, c: ContentRef) {
    put_uvarint(
        o,
        match c {
            ContentRef::Track(t) => u64::from(t.0),
            ContentRef::Owned(id) => HOST_CONTENT + u64::from(id.0),
        },
    );
}
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: [u8; 8] = *b"PLUMHIST";
pub const VERSION: u16 = 1;
const HEADER_LEN: u64 = 32;
/// Largest record accepted (a 4 GiB length field is never trusted blindly).
const MAX_RECORD: u64 = 1 << 32;

const K_BASE: u8 = 1;
const K_ENTRY: u8 = 2;
const K_CURSOR: u8 = 3;
const K_SAVED: u8 = 4;

/// Why a journal was not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalProblem {
    /// Written by a newer PlumSpirit.
    UnsupportedVersion(u16),
    /// Not a history journal, or its header is damaged.
    Corrupt(String),
    /// The journal belongs to another project file.
    WrongProject,
    /// The project file differs from every state the journal knows (changed
    /// outside PlumSpirit, or replaced).
    Stale,
}

impl std::fmt::Display for JournalProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalProblem::UnsupportedVersion(v) => write!(
                f,
                "the undo history was written by a newer PlumSpirit (history version {v})"
            ),
            JournalProblem::Corrupt(m) => write!(f, "the undo history file is damaged ({m})"),
            JournalProblem::WrongProject => {
                write!(f, "the undo history file belongs to another project")
            }
            JournalProblem::Stale => write!(
                f,
                "the project file was changed outside PlumSpirit since its undo history was written"
            ),
        }
    }
}

/// What restoring found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub undo_steps: usize,
    pub redo_steps: usize,
    /// Redo steps that were never saved (the last session ended after them).
    pub unsaved_steps: usize,
    /// Bytes of a damaged tail that were cut off (a crash during a write).
    pub repaired_bytes: u64,
}

/// Deterministic identity of a document as saved: musical content (the
/// document fingerprint) plus structure (project name, track ids, parents,
/// names, properties,
/// subchannels, note subchannels and groups, note groups, folders, 256-Key
/// Mode). Never 0.
pub fn identity(doc: &PlumDocument) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.u64(doc.fingerprint());
    h.bytes(doc.project().name.as_bytes());
    for t in doc.tracks() {
        h.u64(u64::from(t.id().0));
        h.u64(t.parent().map_or(u64::MAX, |p| u64::from(p.0)));
        h.bytes(t.name().unwrap_or("").as_bytes());
        let mut props = Vec::new();
        put_props(&mut props, &(t.props().clone(), t.end_of_track_tick));
        h.bytes(&props);
        for s in t.subchannels().entries() {
            h.u64(u64::from(s.id.0));
            h.u64(u64::from(s.midi_channel));
            h.bytes(s.name.as_deref().unwrap_or("").as_bytes());
        }
        for n in t.notes.iter() {
            if n.sub.0 != 0 || n.group != 0 {
                h.u64(u64::from(n.sub.0) << 32 | u64::from(n.group));
            }
        }
    }
    let mut g = Vec::new();
    put_groups(&mut g, &doc.project().groups);
    h.bytes(&g);
    let mut f = Vec::new();
    put_folders(&mut f, &doc.project().folders);
    h.bytes(&f);
    // v0.9: 256-Key Mode (an undoable project setting). Standard projects
    // keep their v0.8 identity, so existing journals still match.
    if doc.project().keys != KeyRange::Standard {
        h.bytes(b"keys256");
    }
    h.0 | 1
}

struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn u64(&mut self, v: u64) {
        for x in v.to_le_bytes() {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

// ------------------------------------------------------------------ encoding

fn put_bytes(o: &mut Vec<u8>, b: &[u8]) {
    put_uvarint(o, b.len() as u64);
    o.extend_from_slice(b);
}

fn put_opt_track(o: &mut Vec<u8>, t: Option<TrackId>) {
    put_uvarint(o, t.map_or(0, |t| u64::from(t.0) + 1));
}

fn put_ranks(o: &mut Vec<u8>, r: &RankSet) {
    put_bytes(o, r.raw());
}

fn put_records(o: &mut Vec<u8>, r: &CompactRecords) {
    let (data, count) = r.raw();
    put_uvarint(o, count as u64);
    put_bytes(o, data);
}

fn put_origin(o: &mut Vec<u8>, x: Option<Origin>) {
    match x {
        None => o.push(0),
        Some(x) => {
            o.push(1);
            put_uvarint(o, u64::from(x.track.0));
            put_uvarint(o, u64::from(x.anchor));
            put_uvarint(o, u64::from(x.rank));
            put_uvarint(o, u64::from(x.seq));
        }
    }
}

fn put_list<T: Clone + PartialEq>(
    o: &mut Vec<u8>,
    p: &ListPatch<T>,
    item: impl Fn(&mut Vec<u8>, &T) -> Result<(), String>,
) -> Result<(), String> {
    for side in [p.removed(), p.inserted()] {
        put_uvarint(o, side.len() as u64);
        for (i, x) in side {
            put_uvarint(o, *i as u64);
            item(o, x)?;
        }
    }
    Ok(())
}

fn put_tempo(o: &mut Vec<u8>, p: &TempoPoint) -> Result<(), String> {
    put_uvarint(o, u64::from(p.id));
    put_uvarint(o, p.tick);
    o.push(u8::from(p.interp == Interp::Linear) | u8::from(p.implicit) << 1);
    match p.value {
        TempoValue::Micros(us) => {
            o.push(0);
            put_uvarint(o, u64::from(us));
        }
        TempoValue::Bpm(b) => {
            o.push(1);
            put_uvarint(o, b.mantissa());
            put_ivarint(o, i64::from(b.exponent()));
        }
    }
    put_origin(o, p.origin);
    Ok(())
}

fn put_meter(o: &mut Vec<u8>, m: &TimeSignature) -> Result<(), String> {
    put_uvarint(o, u64::from(m.id));
    put_uvarint(o, m.tick);
    o.extend_from_slice(&[
        u8::from(m.implicit),
        m.numerator,
        m.denom_pow,
        m.clocks,
        m.thirty_seconds,
    ]);
    put_origin(o, m.origin);
    Ok(())
}

fn put_marker(o: &mut Vec<u8>, k: &Marker) -> Result<(), String> {
    put_uvarint(o, u64::from(k.id));
    put_uvarint(o, k.start);
    put_uvarint(o, k.end.map_or(0, |e| e.saturating_add(1)));
    o.push(u8::from(k.cue) | u8::from(k.raw_title.is_some()) << 1);
    put_bytes(o, k.title.as_bytes());
    if let Some(r) = &k.raw_title {
        put_bytes(o, r);
    }
    put_origin(o, k.origin);
    Ok(())
}

fn put_master(o: &mut Vec<u8>, p: &MasterVolumePoint) -> Result<(), String> {
    put_uvarint(o, u64::from(p.id));
    put_uvarint(o, p.tick);
    put_uvarint(o, u64::from(p.value));
    o.push(p.device);
    put_origin(o, p.origin);
    Ok(())
}

fn put_event(o: &mut Vec<u8>, e: &RawEvent) -> Result<(), String> {
    put_uvarint(o, e.tick);
    put_uvarint(o, u64::from(e.anchor));
    match e.kind {
        RawKind::Channel { status, data } => {
            o.extend_from_slice(&[0, status, data[0], data[1]]);
        }
        RawKind::System { status, data } => {
            o.extend_from_slice(&[1, status, data[0], data[1]]);
        }
        _ => return Err("events with data blocks".into()),
    }
    Ok(())
}

fn put_props(o: &mut Vec<u8>, p: &(TrackProps, Option<u64>)) {
    let (p, end) = p;
    o.push(p.default_channel);
    o.push(u8::from(p.kind == TrackKind::Rhythm));
    match &p.route {
        OutputRoute::Internal => o.push(0),
        OutputRoute::External(n) => {
            o.push(1);
            put_bytes(o, n.as_bytes());
        }
    }
    put_ivarint(o, p.tick_correction);
    put_uvarint(o, u64::from(p.correction_from_measure));
    put_ivarint(o, i64::from(p.key_correction));
    put_uvarint(o, u64::from(p.gate.hundredths()));
    o.extend_from_slice(&[p.key_range.0, p.key_range.1]);
    match p.onion_color {
        None => o.push(0),
        Some(c) => {
            o.push(1);
            o.extend_from_slice(&c);
        }
    }
    put_uvarint(o, end.map_or(0, |e| e.saturating_add(1)));
}

fn put_subs(o: &mut Vec<u8>, v: &[Subchannel]) {
    put_uvarint(o, v.len() as u64);
    for s in v {
        put_uvarint(o, u64::from(s.id.0));
        o.push(s.midi_channel);
        match &s.name {
            None => o.push(0),
            Some(n) => {
                o.push(1);
                put_bytes(o, n.as_bytes());
            }
        }
    }
}

fn put_groups(o: &mut Vec<u8>, t: &GroupTable) {
    put_uvarint(o, u64::from(t.next_id()));
    put_uvarint(o, t.entries().len() as u64);
    for g in t.entries() {
        put_uvarint(o, u64::from(g.id));
        put_uvarint(o, u64::from(g.parent));
        put_uvarint(o, u64::from(g.link));
        o.push(
            u8::from(g.locked)
                | u8::from(g.temporary) << 1
                | u8::from(g.name.is_some()) << 2
                | u8::from(g.color.is_some()) << 3,
        );
        if let Some(n) = &g.name {
            put_bytes(o, n.as_bytes());
        }
        if let Some(c) = g.color {
            o.extend_from_slice(&c);
        }
    }
}

fn put_folders(o: &mut Vec<u8>, f: &TrackFolders) {
    put_uvarint(o, u64::from(f.next_id()));
    put_uvarint(o, f.folders().len() as u64);
    for x in f.folders() {
        put_uvarint(o, u64::from(x.id));
        put_bytes(o, x.name.as_bytes());
        put_opt_track(o, x.anchor);
    }
    put_uvarint(o, f.members().len() as u64);
    for (t, id) in f.members() {
        put_uvarint(o, u64::from(t.0));
        put_uvarint(o, u64::from(*id));
    }
}

/// The tracks of a Track change as stored: their parents, then a `.lup` image
/// of the block (parents outside the block cleared there).
fn put_tracks(o: &mut Vec<u8>, doc: &PlumDocument, tracks: &[Track]) -> Result<(), String> {
    let img = tracks_image(o, doc, tracks);
    let bytes = crate::native::project_to_bytes(&img).map_err(|e| e.user_message())?;
    put_bytes(o, &bytes);
    Ok(())
}

/// [`put_tracks`] up to the image: writes the parents and returns the image
/// still to serialize (its tracks share the notes' chunks: cheap).
fn tracks_image(o: &mut Vec<u8>, doc: &PlumDocument, tracks: &[Track]) -> PlumDocument {
    let ids: HashSet<TrackId> = tracks.iter().map(Track::id).collect();
    put_uvarint(o, tracks.len() as u64);
    let mut image = Vec::with_capacity(tracks.len());
    for t in tracks {
        put_opt_track(o, t.parent());
        let mut c = t.clone();
        if c.parent().is_some_and(|p| !ids.contains(&p)) {
            c.set_parent(None);
        }
        image.push(c);
    }
    doc.image_of_tracks(image)
}

/// The start of a Track change's record, before its tracks.
fn put_track_head(
    o: &mut Vec<u8>,
    index: usize,
    count: usize,
    id: TrackId,
    present_after: bool,
    shifted: &[(u32, u32)],
) {
    o.push(8);
    put_uvarint(o, index as u64);
    put_uvarint(o, count as u64);
    put_uvarint(o, u64::from(id.0));
    o.push(u8::from(present_after));
    put_uvarint(o, shifted.len() as u64);
    for (a, b) in shifted {
        put_uvarint(o, u64::from(*a));
        put_uvarint(o, u64::from(*b));
    }
}

/// L-S48: a Track change whose block holds more notes than this is
/// serialized and written on a worker. Serializing costs ~24 ns a note on the
/// owner's PC: the Apply of a 224M-note Import Into held the window 5.4 s.
pub(super) const DEFER_NOTES: usize = 1 << 20;

/// Images of large track blocks left out of an encoded change: (where their
/// `bytes` field goes in the body, the image to serialize).
type Deferred = Vec<(usize, PlumDocument)>;

/// The body with every deferred image serialized into its place, as
/// [`encode_change`] would have written it, in parts (nothing copied).
fn assemble(body: &[u8], images: &[(usize, Vec<u8>)]) -> Vec<Vec<u8>> {
    let mut parts = Vec::with_capacity(images.len() * 3 + 1);
    let mut last = 0;
    for (at, bytes) in images {
        parts.push(body[last..*at].to_vec());
        let mut len = Vec::new();
        put_uvarint(&mut len, bytes.len() as u64);
        parts.push(len);
        last = *at;
    }
    parts.push(body[last..].to_vec());
    parts
}

/// Encodes `c` (taken right after it was applied, `doc` in its "after" state).
pub(super) fn encode_change(o: &mut Vec<u8>, doc: &PlumDocument, c: &Change) -> Result<(), String> {
    encode_change_deferring(o, doc, c, None)
}

/// [`encode_change`]; with `defer`, the image of a track block of more than
/// [`DEFER_NOTES`] notes (also inside a group) is not serialized here but
/// listed with its place (L-S48: the worker writes it).
fn encode_change_deferring(
    o: &mut Vec<u8>,
    doc: &PlumDocument,
    c: &Change,
    mut defer: Option<&mut Deferred>,
) -> Result<(), String> {
    match c {
        // B1-0103: a whole-timebase swap holds every note store; the
        // persistent history restarts after it (`persist_last` writes a BASE).
        Change::Timebase(_) => return Err("a PPQN change is not recorded on disk".into()),
        Change::Notes(p) => {
            o.push(0);
            put_host(o, p.track);
            put_ranks(o, &p.removed_ranks);
            put_records(o, &p.removed);
            put_ranks(o, &p.inserted_ranks);
            match &p.inserted {
                Inserted::Records(r) => {
                    o.push(0);
                    put_records(o, r);
                }
                Inserted::Shifted(s) => {
                    o.push(1);
                    let t = i64::try_from(s.ticks).map_err(|_| "shift out of range")?;
                    put_ivarint(o, t);
                    put_ivarint(o, i64::from(s.pitch));
                }
            }
        }
        Change::Pitch {
            track,
            ranks,
            old,
            table,
        } => {
            // Kind 1 (v0.8) holds the 128 standard keys; a table that moves
            // a key above 127 (256-Key Mode) is kind 18 with all 256.
            let wide = table[128..]
                .iter()
                .enumerate()
                .any(|(i, &p)| usize::from(p) != 128 + i)
                || table[..128].iter().any(|&p| p > 127);
            o.push(if wide { 18 } else { 1 });
            put_host(o, *track);
            put_ranks(o, ranks);
            put_bytes(o, old);
            o.extend_from_slice(&table[..if wide { 256 } else { 128 }]);
        }
        Change::Velocity {
            track,
            ranks,
            old,
            new,
        } => {
            o.push(2);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_bytes(o, old);
            o.push(*new);
        }
        Change::Duration {
            track,
            ranks,
            delta,
        } => {
            o.push(3);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_ivarint(o, *delta);
        }
        Change::Durations {
            track,
            ranks,
            old,
            new,
        } => {
            o.push(4);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_bytes(o, old);
            put_uvarint(o, *new);
        }
        Change::Conductor(e) => {
            o.push(5);
            put_list(o, &e.tempo, put_tempo)?;
            put_list(o, &e.meter, put_meter)?;
            put_list(o, &e.markers, put_marker)?;
            put_list(o, &e.master, put_master)?;
            put_uvarint(o, u64::from(e.next_id.0));
            put_uvarint(o, u64::from(e.next_id.1));
        }
        Change::Events { track, patch } => {
            o.push(6);
            put_host(o, *track);
            put_list(o, patch, put_event)?;
        }
        Change::Move {
            from,
            to,
            root,
            old_parent,
            new_parent,
        } => {
            o.push(7);
            put_uvarint(o, *from as u64);
            put_uvarint(o, *to as u64);
            put_uvarint(o, u64::from(root.0));
            put_opt_track(o, *old_parent);
            put_opt_track(o, *new_parent);
        }
        Change::Track {
            index,
            count,
            id,
            present_after,
            shifted,
            stash,
        } => {
            put_track_head(o, *index, *count, *id, *present_after, shifted);
            // The block as it is while present: added tracks are in the
            // document now, removed ones in the stash.
            let t: &[Track] = match stash {
                Some(t) => t,
                None => {
                    let end = index.checked_add(*count).ok_or("bad block")?;
                    let t = doc.tracks().get(*index..end).ok_or("the tracks moved")?;
                    if t.first().map(Track::id) != Some(*id) {
                        return Err("the tracks moved".into());
                    }
                    t
                }
            };
            match defer.as_deref_mut() {
                Some(d) if t.iter().map(|t| t.notes.len()).sum::<usize>() > DEFER_NOTES => {
                    let img = tracks_image(o, doc, t);
                    d.push((o.len(), img));
                }
                _ => put_tracks(o, doc, t)?,
            }
        }
        Change::Subs {
            track,
            added,
            removed,
        } => {
            o.push(9);
            put_uvarint(o, u64::from(track.0));
            put_subs(o, added);
            put_subs(o, removed);
        }
        Change::NoteSubs {
            track,
            ranks,
            old,
            new,
        } => {
            o.push(10);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_uvarint(o, old.len() as u64);
            for v in old {
                put_uvarint(o, u64::from(*v));
            }
            put_uvarint(o, u64::from(*new));
        }
        Change::Name { old, new } => {
            o.push(11);
            put_bytes(o, old.as_bytes());
            put_bytes(o, new.as_bytes());
        }
        Change::Keys { old, new } => {
            // v0.9: 256-Key Mode on / off (0 = standard, 1 = 256 keys).
            o.push(19);
            o.push(u8::from(*old == KeyRange::Extended256));
            o.push(u8::from(*new == KeyRange::Extended256));
        }
        Change::Props { track, old, new } => {
            o.push(12);
            put_uvarint(o, u64::from(track.0));
            put_props(o, old);
            put_props(o, new);
        }
        Change::Velocities {
            track,
            ranks,
            old,
            new,
        } => {
            o.push(13);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_bytes(o, old);
            put_bytes(o, new);
        }
        Change::Channel {
            track,
            ranks,
            old,
            new,
        } => {
            o.push(14);
            put_host(o, *track);
            put_ranks(o, ranks);
            put_bytes(o, old);
            o.push(*new);
        }
        Change::Group(v) => {
            o.push(15);
            put_uvarint(o, v.len() as u64);
            for c in v {
                encode_change_deferring(o, doc, c, defer.as_deref_mut())?;
            }
        }
        Change::Groups { old, new } => {
            o.push(16);
            put_groups(o, old);
            put_groups(o, new);
        }
        Change::Folders { old, new } => {
            o.push(17);
            put_folders(o, old);
            put_folders(o, new);
        }
        Change::Clips(c) => {
            o.push(20);
            put_layout(o, &c.old);
            put_layout(o, &c.new);
            // The owned contents only one side has, as they are while
            // present: in the document now (added) or in the stash (removed).
            let delta = layout_delta(&c.old, &c.new);
            put_uvarint(o, delta.len() as u64);
            let mut stores = Vec::with_capacity(delta.len());
            for (k, id) in delta.iter().enumerate() {
                let content = doc
                    .clips()
                    .content(*id)
                    .or_else(|| c.stash.iter().find(|x| x.id == *id))
                    .ok_or("a clip content is missing")?;
                put_uvarint(o, u64::from(id.0));
                put_label(o, content.name.as_deref(), content.color);
                stores.push(Track::from_parts_exact(
                    TrackId(k as u32),
                    content.notes.clone(),
                    content.events.clone(),
                    None,
                ));
            }
            if stores.is_empty() {
                put_bytes(o, &[]);
            } else {
                let img = doc.image_of_tracks(stores);
                let bytes = crate::native::project_to_bytes(&img).map_err(|e| e.user_message())?;
                put_bytes(o, &bytes);
            }
        }
    }
    Ok(())
}

/// Owned contents listed by exactly one of two layouts, sorted.
fn layout_delta(
    a: &crate::model::ClipLayout,
    b: &crate::model::ClipLayout,
) -> Vec<crate::model::ContentId> {
    let mut v: Vec<crate::model::ContentId> = a
        .contents
        .iter()
        .filter(|c| b.contents.binary_search(c).is_err())
        .chain(
            b.contents
                .iter()
                .filter(|c| a.contents.binary_search(c).is_err()),
        )
        .copied()
        .collect();
    v.sort_unstable();
    v
}

/// Name (flag bit 1) and colour (flag bit 2) of a clip or content.
fn put_label(o: &mut Vec<u8>, name: Option<&str>, color: Option<u32>) {
    o.push((u8::from(name.is_some()) << 1) | (u8::from(color.is_some()) << 2));
    if let Some(n) = name {
        put_bytes(o, n.as_bytes());
    }
    if let Some(c) = color {
        put_uvarint(o, u64::from(c));
    }
}

/// A clip layout: placements, removed primaries, content ids, next ids.
fn put_layout(o: &mut Vec<u8>, l: &crate::model::ClipLayout) {
    use crate::model::{ClipId, ContentRef};
    put_uvarint(o, u64::from(l.next_clip));
    put_uvarint(o, u64::from(l.next_content));
    put_uvarint(o, l.placements.len() as u64);
    for p in &l.placements {
        match p.id {
            ClipId::Primary(t) => {
                o.push(0);
                put_uvarint(o, u64::from(t.0));
            }
            ClipId::Placed(n) => {
                o.push(1);
                put_uvarint(o, u64::from(n));
            }
        }
        put_uvarint(o, u64::from(p.track.0));
        match p.content {
            ContentRef::Track(t) => {
                o.push(0);
                put_uvarint(o, u64::from(t.0));
            }
            ContentRef::Owned(c) => {
                o.push(1);
                put_uvarint(o, u64::from(c.0));
            }
        }
        put_uvarint(o, p.start);
        put_uvarint(o, p.offset);
        put_uvarint(o, p.length.map_or(0, |l| l.saturating_add(1)));
        o.push(u8::from(p.muted));
        put_label(o, p.name.as_deref(), p.color);
    }
    put_uvarint(o, l.removed_primaries.len() as u64);
    for t in &l.removed_primaries {
        put_uvarint(o, u64::from(t.0));
    }
    put_uvarint(o, l.contents.len() as u64);
    for c in &l.contents {
        put_uvarint(o, u64::from(c.0));
    }
}

// ------------------------------------------------------------------ decoding

struct Dec<'a> {
    b: &'a [u8],
    p: usize,
}

type R<T> = Result<T, String>;

impl<'a> Dec<'a> {
    fn uv(&mut self) -> R<u64> {
        get_uvarint(self.b, &mut self.p).ok_or_else(|| "truncated".into())
    }

    fn iv(&mut self) -> R<i64> {
        get_ivarint(self.b, &mut self.p).ok_or_else(|| "truncated".into())
    }

    fn u8(&mut self) -> R<u8> {
        let v = *self.b.get(self.p).ok_or("truncated")?;
        self.p += 1;
        Ok(v)
    }

    fn u32(&mut self) -> R<u32> {
        u32::try_from(self.uv()?).map_err(|_| "value out of range".into())
    }

    fn usize(&mut self) -> R<usize> {
        usize::try_from(self.uv()?).map_err(|_| "value out of range".into())
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or("truncated")?;
        let s = self.b.get(self.p..end).ok_or("truncated")?;
        self.p = end;
        Ok(s)
    }

    fn bytes(&mut self) -> R<&'a [u8]> {
        let n = self.usize()?;
        self.take(n)
    }

    fn string(&mut self) -> R<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| "text is not UTF-8".into())
    }

    /// A count of items that take at least `min` bytes each (bounds allocations).
    fn count(&mut self, min: usize) -> R<usize> {
        let n = self.usize()?;
        if n.saturating_mul(min.max(1)) > self.b.len() - self.p.min(self.b.len()) {
            return Err("count beyond the record".into());
        }
        Ok(n)
    }

    fn track(&mut self) -> R<TrackId> {
        Ok(TrackId(self.u32()?))
    }

    /// A note host (V09-S11): a track id as before, an owned content as
    /// 2^32 + its id (`put_host`).
    fn host(&mut self) -> R<ContentRef> {
        let v = self.uv()?;
        Ok(match v.checked_sub(HOST_CONTENT) {
            None => ContentRef::Track(TrackId(u32::try_from(v).map_err(|_| "bad track id")?)),
            Some(c) => {
                ContentRef::Owned(ContentId(u32::try_from(c).map_err(|_| "bad content id")?))
            }
        })
    }

    fn opt_track(&mut self) -> R<Option<TrackId>> {
        Ok(match self.uv()? {
            0 => None,
            v => Some(TrackId(u32::try_from(v - 1).map_err(|_| "bad track id")?)),
        })
    }

    fn ranks(&mut self) -> R<RankSet> {
        RankSet::from_raw(self.bytes()?.to_vec()).ok_or_else(|| "damaged positions".into())
    }

    fn records(&mut self) -> R<CompactRecords> {
        let n = self.usize()?;
        let data = self.bytes()?.to_vec();
        CompactRecords::from_raw(data, n).ok_or_else(|| "damaged notes".into())
    }

    fn origin(&mut self) -> R<Option<Origin>> {
        Ok(match self.u8()? {
            0 => None,
            1 => Some(Origin {
                track: self.track()?,
                anchor: self.u32()?,
                rank: self.u32()?,
                seq: self.u32()?,
            }),
            _ => return Err("bad origin".into()),
        })
    }

    fn list<T>(&mut self, mut item: impl FnMut(&mut Dec<'a>) -> R<T>) -> R<ListPatch<T>>
    where
        T: Clone + PartialEq,
    {
        let mut p = ListPatch::new();
        let n = self.count(2)?;
        for _ in 0..n {
            let i = self.usize()?;
            p.remove(i, item(self)?);
        }
        let n = self.count(2)?;
        for _ in 0..n {
            let i = self.usize()?;
            p.insert(i, item(self)?);
        }
        Ok(p)
    }
}

fn get_tempo(d: &mut Dec<'_>) -> R<TempoPoint> {
    let id = d.u32()?;
    let tick = d.uv()?;
    let flags = d.u8()?;
    let value = match d.u8()? {
        0 => TempoValue::Micros(d.u32()?),
        1 => {
            let m = d.uv()?;
            let e = i32::try_from(d.iv()?).map_err(|_| "bad tempo")?;
            TempoValue::Bpm(Bpm::new(m, e).map_err(|_| "bad tempo")?)
        }
        _ => return Err("bad tempo".into()),
    };
    Ok(TempoPoint {
        id,
        tick,
        value,
        interp: if flags & 1 != 0 {
            Interp::Linear
        } else {
            Interp::Step
        },
        implicit: flags & 2 != 0,
        origin: d.origin()?,
    })
}

fn get_meter(d: &mut Dec<'_>) -> R<TimeSignature> {
    let id = d.u32()?;
    let tick = d.uv()?;
    let b = d.take(5)?;
    Ok(TimeSignature {
        id,
        tick,
        implicit: b[0] != 0,
        numerator: b[1],
        denom_pow: b[2],
        clocks: b[3],
        thirty_seconds: b[4],
        origin: d.origin()?,
    })
}

fn get_marker(d: &mut Dec<'_>) -> R<Marker> {
    let id = d.u32()?;
    let start = d.uv()?;
    let end = match d.uv()? {
        0 => None,
        e => Some(e - 1),
    };
    let flags = d.u8()?;
    let title = d.string()?;
    let raw_title = if flags & 2 != 0 {
        Some(d.bytes()?.to_vec().into_boxed_slice())
    } else {
        None
    };
    Ok(Marker {
        id,
        start,
        end,
        title,
        raw_title,
        cue: flags & 1 != 0,
        origin: d.origin()?,
    })
}

fn get_master(d: &mut Dec<'_>) -> R<MasterVolumePoint> {
    Ok(MasterVolumePoint {
        id: d.u32()?,
        tick: d.uv()?,
        value: u16::try_from(d.uv()?).map_err(|_| "bad volume")?,
        device: d.u8()?,
        origin: d.origin()?,
    })
}

fn get_event(d: &mut Dec<'_>) -> R<RawEvent> {
    let tick = d.uv()?;
    let anchor = d.u32()?;
    let b = d.take(4)?;
    let kind = match b[0] {
        0 => RawKind::Channel {
            status: b[1],
            data: [b[2], b[3]],
        },
        1 => RawKind::System {
            status: b[1],
            data: [b[2], b[3]],
        },
        _ => return Err("bad event".into()),
    };
    Ok(RawEvent { tick, anchor, kind })
}

fn get_props(d: &mut Dec<'_>) -> R<(TrackProps, Option<u64>)> {
    let default_channel = d.u8()?;
    let kind = if d.u8()? != 0 {
        TrackKind::Rhythm
    } else {
        TrackKind::Melody
    };
    let route = match d.u8()? {
        0 => OutputRoute::Internal,
        1 => OutputRoute::External(d.string()?),
        _ => return Err("bad route".into()),
    };
    let tick_correction = d.iv()?;
    let correction_from_measure = d.u32()?;
    // ±255: 256-Key Mode (the history step re-validates against the project).
    let key_correction = i16::try_from(d.iv()?)
        .ok()
        .filter(|k| k.unsigned_abs() <= u16::from(crate::model::note::MAX_PITCH_256))
        .ok_or("bad key correction")?;
    let gate = GatePercent::from_hundredths(d.u32()?).ok_or("bad gate")?;
    let r = d.take(2)?;
    let key_range = (r[0], r[1]);
    let onion_color = match d.u8()? {
        0 => None,
        1 => {
            let c = d.take(3)?;
            Some([c[0], c[1], c[2]])
        }
        _ => return Err("bad color".into()),
    };
    let end = match d.uv()? {
        0 => None,
        e => Some(e - 1),
    };
    // Any key of a 256-key project; the step re-validates against the project.
    if default_channel > 15 || key_range.0 > key_range.1 {
        return Err("bad track properties".into());
    }
    Ok((
        TrackProps {
            default_channel,
            kind,
            route,
            tick_correction,
            correction_from_measure,
            key_correction,
            gate,
            key_range,
            onion_color,
        },
        end,
    ))
}

fn get_subs(d: &mut Dec<'_>) -> R<Vec<Subchannel>> {
    let n = d.count(3)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let id = crate::model::SubchannelId(d.u32()?);
        let midi_channel = d.u8()?;
        let name = match d.u8()? {
            0 => None,
            1 => Some(d.string()?),
            _ => return Err("bad subchannel".into()),
        };
        if midi_channel > 15 {
            return Err("bad subchannel".into());
        }
        v.push(Subchannel {
            id,
            midi_channel,
            name,
        });
    }
    Ok(v)
}

fn get_groups(d: &mut Dec<'_>) -> R<GroupTable> {
    let next = d.u32()?;
    let n = d.count(4)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let id = d.u32()?;
        let parent = d.u32()?;
        let link = d.u32()?;
        let flags = d.u8()?;
        let name = if flags & 4 != 0 {
            Some(d.string()?)
        } else {
            None
        };
        let color = if flags & 8 != 0 {
            let c = d.take(3)?;
            Some([c[0], c[1], c[2]])
        } else {
            None
        };
        v.push(GroupInfo {
            id,
            parent,
            name,
            color,
            locked: flags & 1 != 0,
            temporary: flags & 2 != 0,
            link,
        });
    }
    GroupTable::from_entries(v, next)
}

fn get_folders(d: &mut Dec<'_>) -> R<TrackFolders> {
    let next = d.u32()?;
    let n = d.count(3)?;
    let mut folders = Vec::with_capacity(n);
    for _ in 0..n {
        folders.push(FolderInfo {
            id: d.u32()?,
            name: d.string()?,
            anchor: d.opt_track()?,
        });
    }
    let m = d.count(2)?;
    let mut members = Vec::with_capacity(m);
    for _ in 0..m {
        members.push((d.track()?, d.u32()?));
    }
    TrackFolders::from_parts(folders, members, next)
}

fn get_tracks(d: &mut Dec<'_>) -> R<Vec<Track>> {
    let n = d.count(1)?;
    let mut parents = Vec::with_capacity(n);
    for _ in 0..n {
        parents.push(d.opt_track()?);
    }
    let bytes = d.bytes()?;
    let (img, _) = crate::native::project_from_bytes(bytes).map_err(|e| e.user_message())?;
    let mut tracks = img.into_tracks();
    if tracks.len() != n {
        return Err("the stored tracks are damaged".into());
    }
    for (t, p) in tracks.iter_mut().zip(parents) {
        t.set_parent(p);
    }
    Ok(tracks)
}

/// Decodes a change. `after`: the document is in the entry's "after" state
/// (an undo entry), otherwise in its "before" state (a redo entry) — it
/// decides which removed / added tracks the change holds.
pub(super) fn decode_change(b: &[u8], after: bool) -> R<Change> {
    let mut d = Dec { b, p: 0 };
    let c = get_change(&mut d, after, 0)?;
    if d.p != b.len() {
        return Err("trailing data".into());
    }
    Ok(c)
}

fn get_change(d: &mut Dec<'_>, after: bool, depth: u32) -> R<Change> {
    if depth > 8 {
        return Err("nested too deeply".into());
    }
    Ok(match d.u8()? {
        0 => {
            let track = d.host()?;
            let removed_ranks = d.ranks()?;
            let removed = d.records()?;
            let inserted_ranks = d.ranks()?;
            let inserted = match d.u8()? {
                0 => Inserted::Records(d.records()?),
                1 => Inserted::Shifted(Shift {
                    ticks: i128::from(d.iv()?),
                    pitch: i32::try_from(d.iv()?).map_err(|_| "bad shift")?,
                }),
                _ => return Err("bad notes".into()),
            };
            Change::Notes(NotesPatch {
                track,
                removed_ranks,
                removed,
                inserted_ranks,
                inserted,
            })
        }
        k @ (1 | 18) => {
            let track = d.host()?;
            let ranks = d.ranks()?;
            let old = d.bytes()?.to_vec();
            let n = if k == 18 { 256 } else { 128 };
            let t = d.take(n)?;
            let mut table = crate::editor::identity_pitch_table();
            table[..n].copy_from_slice(t);
            Change::Pitch {
                track,
                ranks,
                old,
                table,
            }
        }
        2 => Change::Velocity {
            track: d.host()?,
            ranks: d.ranks()?,
            old: d.bytes()?.to_vec(),
            new: d.u8()?,
        },
        3 => Change::Duration {
            track: d.host()?,
            ranks: d.ranks()?,
            delta: d.iv()?,
        },
        4 => Change::Durations {
            track: d.host()?,
            ranks: d.ranks()?,
            old: d.bytes()?.to_vec(),
            new: d.uv()?,
        },
        5 => Change::Conductor(ConductorEdit {
            tempo: d.list(get_tempo)?,
            meter: d.list(get_meter)?,
            markers: d.list(get_marker)?,
            master: d.list(get_master)?,
            next_id: (d.u32()?, d.u32()?),
            // A restored edit is applied by patch verification, not by
            // revision (the revision counter starts over each session).
            base_revision: u64::MAX,
        }),
        6 => Change::Events {
            track: d.host()?,
            patch: d.list(get_event)?,
        },
        7 => Change::Move {
            from: d.usize()?,
            to: d.usize()?,
            root: d.track()?,
            old_parent: d.opt_track()?,
            new_parent: d.opt_track()?,
        },
        8 => {
            let index = d.usize()?;
            let count = d.usize()?;
            let id = d.track()?;
            let present_after = d.u8()? != 0;
            let n = d.count(2)?;
            let mut shifted = Vec::with_capacity(n);
            for _ in 0..n {
                shifted.push((d.u32()?, d.u32()?));
            }
            let tracks = get_tracks(d)?;
            if tracks.len() != count || tracks.first().map(Track::id) != Some(id) {
                return Err("the stored tracks are damaged".into());
            }
            // Present in the document in this state: the history does not
            // hold them; absent: it does.
            let present_now = present_after == after;
            Change::Track {
                index,
                count,
                id,
                present_after,
                shifted,
                stash: (!present_now).then_some(tracks),
            }
        }
        9 => Change::Subs {
            track: d.track()?,
            added: get_subs(d)?,
            removed: get_subs(d)?,
        },
        10 => {
            let track = d.host()?;
            let ranks = d.ranks()?;
            let n = d.count(1)?;
            let mut old = Vec::with_capacity(n);
            for _ in 0..n {
                old.push(d.u32()?);
            }
            Change::NoteSubs {
                track,
                ranks,
                old,
                new: d.u32()?,
            }
        }
        11 => Change::Name {
            old: d.string()?,
            new: d.string()?,
        },
        19 => Change::Keys {
            old: get_keys(d)?,
            new: get_keys(d)?,
        },
        12 => Change::Props {
            track: d.track()?,
            old: Box::new(get_props(d)?),
            new: Box::new(get_props(d)?),
        },
        13 => Change::Velocities {
            track: d.host()?,
            ranks: d.ranks()?,
            old: d.bytes()?.to_vec(),
            new: d.bytes()?.to_vec(),
        },
        14 => Change::Channel {
            track: d.host()?,
            ranks: d.ranks()?,
            old: d.bytes()?.to_vec(),
            new: d.u8()?,
        },
        15 => {
            let n = d.count(1)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(get_change(d, after, depth + 1)?);
            }
            Change::Group(v)
        }
        16 => Change::Groups {
            old: Box::new(get_groups(d)?),
            new: Box::new(get_groups(d)?),
        },
        17 => Change::Folders {
            old: Box::new(get_folders(d)?),
            new: Box::new(get_folders(d)?),
        },
        20 => {
            let old = get_layout(d)?;
            let new = get_layout(d)?;
            let delta = layout_delta(&old, &new);
            if d.usize()? != delta.len() {
                return Err("the stored clip contents are damaged".into());
            }
            let mut heads = Vec::with_capacity(delta.len());
            for id in &delta {
                if d.u32()? != id.0 {
                    return Err("the stored clip contents are damaged".into());
                }
                heads.push((*id, get_label(d)?));
            }
            let image = d.bytes()?;
            let stores = if delta.is_empty() && image.is_empty() {
                Vec::new()
            } else {
                crate::native::project_from_bytes(image)
                    .map_err(|e| e.user_message())?
                    .0
                    .into_tracks()
            };
            if stores.len() != delta.len() {
                return Err("the stored clip contents are damaged".into());
            }
            // Keep only the contents the document lacks in its current
            // state: after the change, those the change removed; before it,
            // those it adds.
            let lacking = if after { &old } else { &new };
            let mut stash = Vec::new();
            for ((id, (name, color)), t) in heads.into_iter().zip(stores) {
                if lacking.contents.binary_search(&id).is_ok() {
                    let mut c = crate::model::OwnedContent::new(id);
                    c.notes = t.notes;
                    c.events = t.events;
                    c.name = name;
                    c.color = color;
                    stash.push(c);
                }
            }
            Change::Clips(Box::new(super::ClipsChange { old, new, stash }))
        }
        _ => return Err("unknown change".into()),
    })
}

fn get_label(d: &mut Dec<'_>) -> R<(Option<String>, Option<u32>)> {
    let flags = d.u8()?;
    if flags & !0x06 != 0 {
        return Err("bad clip label".into());
    }
    let name = if flags & 2 != 0 {
        Some(d.string()?)
    } else {
        None
    };
    let color = if flags & 4 != 0 { Some(d.u32()?) } else { None };
    Ok((name, color))
}

fn get_layout(d: &mut Dec<'_>) -> R<crate::model::ClipLayout> {
    use crate::model::{ClipId, ClipPlacement, ContentId, ContentRef};
    let next_clip = d.u32()?;
    let next_content = d.u32()?;
    let n = d.count(8)?;
    let mut placements = Vec::with_capacity(n);
    for _ in 0..n {
        let id = match d.u8()? {
            0 => ClipId::Primary(d.track()?),
            1 => ClipId::Placed(d.u32()?),
            _ => return Err("bad clip id".into()),
        };
        let track = d.track()?;
        let content = match d.u8()? {
            0 => ContentRef::Track(d.track()?),
            1 => ContentRef::Owned(ContentId(d.u32()?)),
            _ => return Err("bad clip content".into()),
        };
        let start = d.uv()?;
        let offset = d.uv()?;
        let length = d.uv()?.checked_sub(1);
        let muted = match d.u8()? {
            0 => false,
            1 => true,
            _ => return Err("bad clip flags".into()),
        };
        let (name, color) = get_label(d)?;
        placements.push(ClipPlacement {
            id,
            track,
            content,
            start,
            offset,
            length,
            muted,
            name,
            color,
            revision: 0,
        });
    }
    let n = d.count(1)?;
    let mut removed_primaries = Vec::with_capacity(n);
    for _ in 0..n {
        removed_primaries.push(d.track()?);
    }
    let n = d.count(1)?;
    let mut contents = Vec::with_capacity(n);
    for _ in 0..n {
        contents.push(ContentId(d.u32()?));
    }
    let l = crate::model::ClipLayout {
        placements,
        removed_primaries,
        contents,
        next_clip,
        next_content,
    };
    if l.placements.windows(2).any(|w| w[0].id >= w[1].id)
        || l.removed_primaries.windows(2).any(|w| w[0] >= w[1])
        || l.contents.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("the stored clip layout is not in order".into());
    }
    Ok(l)
}

fn get_keys(d: &mut Dec<'_>) -> R<KeyRange> {
    Ok(match d.u8()? {
        0 => KeyRange::Standard,
        1 => KeyRange::Extended256,
        _ => return Err("bad key range".into()),
    })
}

// ------------------------------------------------------------------ the file

/// Where an entry's record is in the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DiskRef {
    pub offset: u64,
    pub len: u32,
}

/// A journal file opened for appending.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
    /// End of the valid records (where the next one goes).
    end: u64,
    path_hash: u64,
    /// v0.8 §216: writes that failed (disk full, permission, device gone):
    /// the in-memory history goes on; the app tells the user once per new
    /// failure that recent steps will not survive a restart.
    failures: u64,
    last_failure: Option<String>,
    /// L-S48: a large entry being serialized and written on a worker.
    pending: Option<Box<Pending>>,
}

/// L-S48: a large entry written on a worker through its own file handle from
/// `at` on; records appended meanwhile wait in `queued` (kind, body, the
/// entry's state for an ENTRY) and follow it in order.
#[derive(Debug)]
struct Pending {
    at: u64,
    after: super::StateId,
    worker: std::thread::JoinHandle<io::Result<u64>>,
    queued: Vec<(u8, Vec<u8>, Option<super::StateId>)>,
}

/// Writes one record at `at` of `file`: `[length u32][kind, body][CRC-32]`,
/// written with a zero length and its body streamed, the real length patched
/// in last (a crash leaves at worst one invalid record). Returns the length
/// (kind and body).
fn write_record(file: &mut File, at: u64, kind: u8, body: &[&[u8]]) -> io::Result<u64> {
    let total: u64 = 1 + body.iter().map(|b| b.len() as u64).sum::<u64>();
    if total >= MAX_RECORD {
        return Err(io::Error::other("history record too large"));
    }
    file.seek(SeekFrom::Start(at))?;
    let mut crc = Crc32::default();
    {
        let mut w = BufWriter::with_capacity(1 << 16, &mut *file);
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&[kind])?;
        crc.update(&[kind]);
        for b in body {
            w.write_all(b)?;
            crc.update(b);
        }
        w.write_all(&crc.finish().to_le_bytes())?;
        w.flush()?;
    }
    // Commit: the real length makes the record valid.
    file.seek(SeekFrom::Start(at))?;
    file.write_all(&(total as u32).to_le_bytes())?;
    Ok(total)
}

/// One record found when opening.
#[derive(Clone, Debug)]
struct Found {
    kind: u8,
    at: DiskRef,
    /// BASE / SAVED: (state, identity); CURSOR: (state, 0); ENTRY: (before, after).
    a: u64,
    b: u64,
    label: String,
}

/// What [`Journal::open`] read: the records' index (not the bodies).
#[derive(Debug, Default)]
pub struct Scan {
    found: Vec<Found>,
    repaired_bytes: u64,
}

fn header(path_hash: u64) -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[..8].copy_from_slice(&MAGIC);
    h[8..10].copy_from_slice(&VERSION.to_le_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    h[12..20].copy_from_slice(&now.to_le_bytes());
    h[20..28].copy_from_slice(&path_hash.to_le_bytes());
    h
}

/// FNV-1a of a project path (case-insensitive on Windows): the journal's key.
pub fn path_key(path: &Path) -> u64 {
    let s = path.to_string_lossy();
    let s = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.bytes(s.as_bytes());
    h.0
}

impl Journal {
    /// Creates (or replaces) a journal for the project at key `path_hash`.
    pub fn create(path: &Path, path_hash: u64) -> io::Result<Journal> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&header(path_hash))?;
        file.sync_all()?;
        Ok(Journal {
            path: path.to_path_buf(),
            file,
            end: HEADER_LEN,
            path_hash,
            failures: 0,
            last_failure: None,
            pending: None,
        })
    }

    /// Opens an existing journal and indexes its records; a damaged tail is
    /// cut off (Repair). `Err`: the file is not usable (nothing was changed).
    pub fn open(
        path: &Path,
        path_hash: u64,
    ) -> io::Result<Result<(Journal, Scan), JournalProblem>> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len();
        let mut h = [0u8; HEADER_LEN as usize];
        if len < HEADER_LEN || file.read_exact(&mut h).is_err() || h[..8] != MAGIC {
            return Ok(Err(JournalProblem::Corrupt("no history header".into())));
        }
        let version = u16::from_le_bytes([h[8], h[9]]);
        if version > VERSION {
            return Ok(Err(JournalProblem::UnsupportedVersion(version)));
        }
        if version == 0 {
            return Ok(Err(JournalProblem::Corrupt(
                "unknown history version".into(),
            )));
        }
        let key = u64::from_le_bytes(h[20..28].try_into().unwrap_or([0; 8]));
        if key != path_hash {
            return Ok(Err(JournalProblem::WrongProject));
        }
        let mut scan = Scan::default();
        let mut pos = HEADER_LEN;
        let mut r = io::BufReader::with_capacity(1 << 16, &mut file);
        r.seek(SeekFrom::Start(pos))?;
        while let Some(f) = read_index(&mut r, pos, len)? {
            pos = f.at.offset + 4 + u64::from(f.at.len) + 4;
            scan.found.push(f);
        }
        drop(r);
        if pos < len {
            scan.repaired_bytes = len - pos;
            // Repair keeps a copy of the damaged file next to it (nothing lost).
            let _ = std::fs::copy(path, path.with_extension("plh.damaged"));
            file.set_len(pos)?;
            file.sync_all()?;
        }
        Ok(Ok((
            Journal {
                path: path.to_path_buf(),
                file,
                end: pos,
                path_hash,
                failures: 0,
                last_failure: None,
                pending: None,
            },
            scan,
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Moves the journal to `path` for the project key `path_hash` (first
    /// save of a new project, Save As): the history follows the project.
    pub fn rekey(&mut self, path: &Path, path_hash: u64) -> io::Result<()> {
        self.file.sync_all()?;
        if path != self.path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::rename(&self.path, path)?;
            self.path = path.to_path_buf();
        }
        self.file.seek(SeekFrom::Start(20))?;
        self.file.write_all(&path_hash.to_le_bytes())?;
        self.file.sync_all()?;
        self.path_hash = path_hash;
        Ok(())
    }

    /// Bytes on disk.
    pub fn disk_bytes(&self) -> u64 {
        self.end
    }

    /// Appends one record whose body `body` streams into the writer (the
    /// length is patched in afterwards). Returns where it is.
    fn append(&mut self, kind: u8, body: &[&[u8]]) -> io::Result<DiskRef> {
        debug_assert!(
            self.pending.is_none(),
            "a record over an entry being written"
        );
        let at = self.end;
        let total = write_record(&mut self.file, at, kind, body)?;
        self.end = at + 4 + total + 4;
        Ok(DiskRef {
            offset: at,
            len: total as u32,
        })
    }

    /// L-S48: whether a large entry is still being written in the background.
    pub fn writing(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the entry leading to `after` waits to be written (L-S48).
    fn awaits(&self, after: super::StateId) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.after == after || p.queued.iter().any(|q| q.2 == Some(after)))
    }

    /// Appends a record now, or after the entry being written (`None` then).
    fn append_or_queue(
        &mut self,
        kind: u8,
        body: &[&[u8]],
        entry: Option<super::StateId>,
    ) -> io::Result<Option<DiskRef>> {
        match self.pending.as_mut() {
            Some(p) => {
                p.queued.push((kind, body.concat(), entry));
                Ok(None)
            }
            None => self.append(kind, body).map(Some),
        }
    }

    /// Starts writing the ENTRY `head` + `body` with the `images` serialized
    /// into their places on a worker with its own handle (nothing may be
    /// writing).
    fn spawn_entry(
        &mut self,
        after: super::StateId,
        head: Vec<u8>,
        body: Vec<u8>,
        images: Deferred,
    ) -> io::Result<()> {
        debug_assert!(self.pending.is_none());
        let at = self.end;
        let path = self.path.clone();
        let worker = std::thread::Builder::new()
            .name("plumspirit-history".into())
            .spawn(move || {
                let mut done = Vec::with_capacity(images.len());
                for (place, img) in images {
                    let bytes = crate::native::project_to_bytes(&img)
                        .map_err(|e| io::Error::other(e.user_message()))?;
                    done.push((place, bytes));
                }
                let mut parts = assemble(&body, &done);
                let mut all: Vec<&[u8]> = vec![&head];
                let tail = parts.pop().unwrap_or_default();
                for (k, p) in parts.iter().enumerate() {
                    all.push(p);
                    if k % 2 == 1 {
                        all.push(&done[k / 2].1);
                    }
                }
                all.push(&tail);
                let mut f = OpenOptions::new().write(true).open(&path)?;
                write_record(&mut f, at, K_ENTRY, &all)
            })?;
        self.pending = Some(Box::new(Pending {
            at,
            after,
            worker,
            queued: Vec::new(),
        }));
        Ok(())
    }

    /// Finishes the entry being written (waits for it when `wait`, else only
    /// once it is done) and appends the records queued behind it. Returns
    /// where each entry went (`None`: not written; the failure is counted).
    fn settle(&mut self, wait: bool) -> Vec<(super::StateId, Option<DiskRef>)> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| wait || p.worker.is_finished())
        {
            return Vec::new();
        }
        let p = *self.pending.take().expect("checked");
        let mut out = Vec::with_capacity(1 + p.queued.len());
        let done = p
            .worker
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the undo history writer stopped")));
        match done {
            Ok(total) => {
                self.end = p.at + 4 + total + 4;
                out.push((
                    p.after,
                    Some(DiskRef {
                        offset: p.at,
                        len: total as u32,
                    }),
                ));
            }
            Err(e) => {
                self.failed(&e);
                out.push((p.after, None));
            }
        }
        for (kind, body, entry) in p.queued {
            let at = match self.append(kind, &[&body]) {
                Ok(at) => Some(at),
                Err(e) => {
                    self.failed(&e);
                    None
                }
            };
            if let Some(a) = entry {
                out.push((a, at));
            }
        }
        out
    }

    /// Flushes to the disk (save points, closing).
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// The body of the entry at `at` (checked again).
    fn read_entry(&mut self, at: DiskRef) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; 4 + at.len as usize + 4];
        self.file
            .seek(SeekFrom::Start(at.offset))
            .and_then(|_| self.file.read_exact(&mut buf))
            .map_err(|e| format!("could not read the undo history ({e})"))?;
        let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let payload = &buf[4..4 + at.len as usize];
        let crc = u32::from_le_bytes(buf[4 + at.len as usize..].try_into().unwrap_or([0; 4]));
        if n != at.len || Crc32::of(payload) != crc || payload.first() != Some(&K_ENTRY) {
            return Err("the undo history file is damaged at this step".into());
        }
        // Skip the entry header (before, after, label).
        let mut p = 1;
        get_uvarint(payload, &mut p).ok_or("damaged")?;
        get_uvarint(payload, &mut p).ok_or("damaged")?;
        let l = get_uvarint(payload, &mut p).ok_or("damaged")? as usize;
        p = p.checked_add(l).ok_or("damaged")?;
        payload
            .get(p..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "damaged".into())
    }

    /// The raw record at `at` (compaction copies records unchanged).
    fn read_raw(&mut self, at: DiskRef) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; 4 + at.len as usize + 4];
        self.file.seek(SeekFrom::Start(at.offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Reads the next record's index at `pos` (CRC-checked for small records and
/// the bounds of every record; a large entry body is checked when it is
/// read). `None`: no valid record here (end or damaged tail).
fn read_index<Rd: Read + Seek>(r: &mut Rd, pos: u64, file_len: u64) -> io::Result<Option<Found>> {
    if pos + 9 > file_len {
        return Ok(None);
    }
    let mut lb = [0u8; 4];
    if r.read_exact(&mut lb).is_err() {
        return Ok(None);
    }
    let len = u64::from(u32::from_le_bytes(lb));
    if len == 0 || pos + 4 + len + 4 > file_len {
        return Ok(None);
    }
    // Small records are read whole and checked; entries only their header.
    let head = len.min(4096) as usize;
    let mut body = vec![0u8; head];
    if r.read_exact(&mut body).is_err() {
        return Ok(None);
    }
    let kind = body[0];
    let mut crc_ok = true;
    if len as usize == head {
        let mut cb = [0u8; 4];
        if r.read_exact(&mut cb).is_err() {
            return Ok(None);
        }
        crc_ok = Crc32::of(&body) == u32::from_le_bytes(cb);
    } else {
        // Check the whole entry when it is the last record (a crash can only
        // damage the end); otherwise skip its body.
        let rest = len - head as u64;
        if pos + 4 + len + 4 == file_len {
            let mut crc = Crc32::default();
            crc.update(&body);
            let mut left = rest;
            let mut chunk = vec![0u8; 1 << 16];
            while left > 0 {
                let n = left.min(chunk.len() as u64) as usize;
                if r.read_exact(&mut chunk[..n]).is_err() {
                    return Ok(None);
                }
                crc.update(&chunk[..n]);
                left -= n as u64;
            }
            let mut cb = [0u8; 4];
            if r.read_exact(&mut cb).is_err() {
                return Ok(None);
            }
            crc_ok = crc.finish() == u32::from_le_bytes(cb);
        } else {
            r.seek(SeekFrom::Current(rest as i64 + 4))?;
        }
    }
    if !crc_ok {
        return Ok(None);
    }
    let mut p = 1usize;
    let mut a = || get_uvarint(&body, &mut p);
    let (x, y, label) = match kind {
        K_BASE | K_SAVED => {
            let s = a();
            let id = a();
            match (s, id) {
                (Some(s), Some(id)) => (s, id, String::new()),
                _ => return Ok(None),
            }
        }
        K_CURSOR => match a() {
            Some(s) => (s, 0, String::new()),
            None => return Ok(None),
        },
        K_ENTRY => {
            let (Some(before), Some(after), Some(l)) = (a(), a(), a()) else {
                return Ok(None);
            };
            let l = l as usize;
            let Some(t) = body.get(p..p + l) else {
                return Ok(None);
            };
            (before, after, String::from_utf8_lossy(t).into_owned())
        }
        _ => return Ok(None),
    };
    Ok(Some(Found {
        kind,
        at: DiskRef {
            offset: pos,
            len: len as u32,
        },
        a: x,
        b: y,
        label,
    }))
}

/// A `&'static str` for a label read from disk (labels are a small fixed set;
/// each distinct text is kept once).
fn intern(label: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static SET: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = SET.get_or_init(|| Mutex::new(HashSet::new()));
    let mut g = match set.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(s) = g.get(label) {
        return s;
    }
    if g.len() > 4096 {
        return "Edit";
    }
    let s: &'static str = Box::leak(label.to_owned().into_boxed_str());
    g.insert(s);
    s
}

// ------------------------------------------------------- history integration

impl History {
    /// Starts persisting into a new journal from the current state (`identity`:
    /// of the document as saved in its file, when the document is saved).
    pub fn attach_new_journal(&mut self, mut j: Journal, identity: Option<u64>) -> io::Result<()> {
        self.settle_journal(true);
        let mut b = Vec::new();
        put_uvarint(&mut b, self.current);
        put_uvarint(&mut b, identity.unwrap_or(0));
        j.append(K_BASE, &[&b])?;
        if let (Some(id), Some(s)) = (identity, self.saved) {
            let mut b = Vec::new();
            put_uvarint(&mut b, s);
            put_uvarint(&mut b, id);
            j.append(K_SAVED, &[&b])?;
        }
        j.sync()?;
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            e.disk = None;
        }
        // Entries already in memory stay usable this session but are not
        // persisted (they precede the journal's base).
        self.journal_floor = self.undo.len();
        self.journal = Some(j);
        Ok(())
    }

    /// Restores the undo / redo stacks of `doc` (just opened from its file,
    /// identity `identity`) from journal `j`. On a problem the journal is
    /// handed back untouched with the reason.
    pub fn restore_journal(
        &mut self,
        mut j: Journal,
        scan: Scan,
        identity: u64,
    ) -> Result<RestoreReport, (Journal, JournalProblem)> {
        self.settle_journal(true);
        let found = scan.found;
        let Some(base) = found.iter().rposition(|f| f.kind == K_BASE) else {
            return Err((j, JournalProblem::Corrupt("no starting point".into())));
        };
        let live = &found[base..];
        // The state of the opened file: the newest save with its identity.
        let saved = live
            .iter()
            .rev()
            .find(|f| (f.kind == K_SAVED || f.kind == K_BASE) && f.b == identity)
            .map(|f| f.a);
        let Some(s) = saved else {
            return Err((j, JournalProblem::Stale));
        };
        let mut by_after: HashMap<u64, usize> = HashMap::new();
        let mut by_before: HashMap<u64, usize> = HashMap::new();
        let mut max_state = live[0].a;
        for (i, f) in live.iter().enumerate() {
            match f.kind {
                K_ENTRY => {
                    by_after.insert(f.b, i);
                    by_before.insert(f.a, i);
                    max_state = max_state.max(f.a).max(f.b);
                }
                _ => max_state = max_state.max(f.a),
            }
        }
        let cursor = live
            .iter()
            .rev()
            .find(|f| f.kind == K_CURSOR || f.kind == K_ENTRY)
            .map(|f| if f.kind == K_ENTRY { f.b } else { f.a });
        // Undo chain: back from the saved state.
        let mut undo: Vec<Entry> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::from([s]);
        let mut st = s;
        while let Some(&i) = by_after.get(&st) {
            let f = &live[i];
            if !seen.insert(f.a) {
                break;
            }
            undo.push(Entry::on_disk(intern(&f.label), f.a, f.b, f.at));
            st = f.a;
        }
        undo.reverse();
        // Redo chain: forward along the newest branch.
        let mut redo: Vec<Entry> = Vec::new();
        let mut st = s;
        // Steps up to where the last session stood (never saved).
        let mut unsaved = 0usize;
        while let Some(&i) = by_before.get(&st) {
            let f = &live[i];
            if !seen.insert(f.b) {
                break;
            }
            redo.push(Entry::on_disk(intern(&f.label), f.a, f.b, f.at));
            st = f.b;
            if Some(st) == cursor {
                unsaved = redo.len();
            }
        }
        redo.reverse();
        let report = RestoreReport {
            undo_steps: undo.len(),
            redo_steps: redo.len(),
            unsaved_steps: unsaved.min(redo.len()),
            repaired_bytes: scan.repaired_bytes,
        };
        self.undo = undo.into();
        self.redo = redo;
        self.bytes = (self.undo.len() + self.redo.len()) * std::mem::size_of::<Entry>();
        self.next_state = max_state + 1;
        self.current = s;
        self.saved = Some(s);
        self.journal_floor = 0;
        let mut b = Vec::new();
        put_uvarint(&mut b, s);
        if let Err(e) = j.append(K_CURSOR, &[&b]) {
            j.failed(&e);
        }
        self.journal = Some(j);
        Ok(report)
    }

    /// Stops persisting (the journal file stays; an entry still being written
    /// is finished first).
    pub fn detach_journal(&mut self) -> Option<Journal> {
        self.settle_journal(true);
        if let Some(j) = self.journal.as_mut() {
            let _ = j.sync();
        }
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            e.disk = None;
        }
        self.journal.take()
    }

    pub fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    pub fn journal_mut(&mut self) -> Option<&mut Journal> {
        self.journal.as_mut()
    }

    /// L-S48: finishes an entry written in the background once it is done
    /// (never waits) and records where it and the entries queued behind it
    /// went. Cheap; the app calls it every frame, every step calls it too.
    pub fn poll_journal(&mut self) {
        self.settle_journal(false);
    }

    /// [`History::poll_journal`], waiting for the writer when `wait`.
    pub(super) fn settle_journal(&mut self, wait: bool) {
        let Some(j) = self.journal.as_mut() else {
            return;
        };
        for (after, at) in j.settle(wait) {
            if let Some(at) = at
                && let Some(e) = self
                    .undo
                    .iter_mut()
                    .chain(self.redo.iter_mut())
                    .find(|e| e.after == after && e.disk.is_none())
            {
                e.disk = Some(at);
            }
        }
    }

    /// Whether the entry leading to `after` waits for the journal (L-S48):
    /// it is not evicted meanwhile.
    pub(super) fn awaits_journal(&self, after: super::StateId) -> bool {
        self.journal.as_ref().is_some_and(|j| j.awaits(after))
    }

    /// Appends the newest entry (called right after [`History::record`]).
    /// L-S48: a Track change of more than [`DEFER_NOTES`] notes is serialized
    /// and written on a worker; records meanwhile queue behind it.
    pub(super) fn persist_last(&mut self, doc: &PlumDocument) {
        self.settle_journal(false);
        if self.journal.is_none() {
            return;
        }
        let Some(e) = self.undo.back() else {
            return;
        };
        let Some(change) = e.change.as_ref() else {
            return;
        };
        let after = e.after;
        let mut head = Vec::new();
        put_uvarint(&mut head, e.before);
        put_uvarint(&mut head, e.after);
        put_bytes(&mut head, e.label.as_bytes());
        let mut body = Vec::new();
        let mut images = Deferred::new();
        let encoded = encode_change_deferring(&mut body, doc, change, Some(&mut images));
        if encoded.is_ok() && !images.is_empty() {
            // One large entry at a time: a second waits for the first.
            self.settle_journal(true);
            let j = self.journal.as_mut().expect("checked");
            if j.spawn_entry(after, head.clone(), body.clone(), images)
                .is_ok()
            {
                return;
            }
            // No worker thread: written here, as before.
            body.clear();
        }
        let Some(e) = self.undo.back_mut() else {
            return;
        };
        let Some(change) = e.change.as_ref() else {
            return;
        };
        let j = self.journal.as_mut().expect("checked");
        let encoded = if body.is_empty() || encoded.is_err() {
            body.clear();
            encode_change(&mut body, doc, change)
        } else {
            encoded
        };
        match encoded {
            Ok(()) => match j.append_or_queue(K_ENTRY, &[&head, &body], Some(after)) {
                Ok(Some(at)) => e.disk = Some(at),
                Ok(None) => {}
                Err(err) => j.failed(&err),
            },
            Err(_) => {
                // Not recordable on disk: the persistent history restarts here.
                let mut b = Vec::new();
                put_uvarint(&mut b, after);
                put_uvarint(&mut b, 0);
                if let Err(err) = j.append_or_queue(K_BASE, &[&b], None) {
                    j.failed(&err);
                }
                for x in self.undo.iter_mut() {
                    x.disk = None;
                }
                self.journal_floor = self.undo.len();
            }
        }
    }

    /// Records the current position (after undo / redo).
    pub(super) fn persist_cursor(&mut self) {
        if let Some(j) = self.journal.as_mut() {
            let mut b = Vec::new();
            put_uvarint(&mut b, self.current);
            if let Err(e) = j.append_or_queue(K_CURSOR, &[&b], None) {
                j.failed(&e);
            }
        }
    }

    /// Records that `state` was saved with file identity `identity`.
    pub fn persist_saved(&mut self, state: super::StateId, identity: u64) {
        // A save point is synced: everything before it is written first.
        self.settle_journal(true);
        if let Some(j) = self.journal.as_mut() {
            let mut b = Vec::new();
            put_uvarint(&mut b, state);
            put_uvarint(&mut b, identity);
            if let Err(e) = j.append(K_SAVED, &[&b]).and_then(|_| j.sync()) {
                j.failed(&e);
            }
        }
    }

    /// Restarts the persistent history at the current state (Clear History,
    /// or an edit the history does not cover).
    pub(super) fn persist_base(&mut self, identity: Option<u64>) {
        self.settle_journal(true);
        if let Some(j) = self.journal.as_mut() {
            let mut b = Vec::new();
            put_uvarint(&mut b, self.current);
            put_uvarint(&mut b, identity.unwrap_or(0));
            if let Err(e) = j.append(K_BASE, &[&b]) {
                j.failed(&e);
            }
        }
        for x in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            x.disk = None;
        }
        self.journal_floor = self.undo.len();
    }

    /// Loads an on-disk entry's change. `after`: see [`decode_change`].
    pub(super) fn load_change(&mut self, at: DiskRef, after: bool) -> Result<Change, String> {
        let j = self
            .journal
            .as_mut()
            .ok_or("the undo history file is closed")?;
        let body = j.read_entry(at)?;
        decode_change(&body, after)
    }

    /// Clear History: drops every step (memory and disk) and starts a fresh
    /// journal at the current state.
    pub fn clear_persistent(&mut self, identity: Option<u64>) -> io::Result<()> {
        // The file is recreated: nothing may still write into it.
        self.settle_journal(true);
        self.clear();
        if let Some(j) = self.journal.take() {
            let path = j.path.clone();
            let key = j.path_hash;
            drop(j);
            let j = Journal::create(&path, key)?;
            self.attach_new_journal(j, identity)?;
        }
        Ok(())
    }

    /// Compact History: rewrites the journal with only the live undo / redo
    /// path (temporary file + rename). Returns (bytes before, bytes after).
    /// Synchronous form of [`History::plan_compaction`] → [`CompactJob::run`]
    /// → [`History::finish_compaction`] (the app runs the middle step on a
    /// worker, §209).
    pub fn compact_journal(&mut self, identity_of_saved: Option<u64>) -> io::Result<(u64, u64)> {
        let Some(job) = self.plan_compaction(identity_of_saved) else {
            return Ok((0, 0));
        };
        let done = job.run(&std::sync::atomic::AtomicBool::new(false))?;
        self.finish_compaction(done)
    }

    fn compact_stamp(&self) -> CompactStamp {
        CompactStamp {
            end: self.journal.as_ref().map_or(0, |j| j.end),
            undo: self.undo.len(),
            redo: self.redo.len(),
            current: self.current,
            floor: self.journal_floor,
            evicted: self.evicted,
        }
    }

    /// Plans Compact History: what to copy (record positions only, no
    /// bodies). `None` without a journal.
    pub fn plan_compaction(&self, identity_of_saved: Option<u64>) -> Option<CompactJob> {
        let j = self.journal.as_ref()?;
        let root = self
            .undo
            .get(self.journal_floor)
            .map_or(self.current, |e| e.before);
        let mut refs = Vec::new();
        for (i, e) in self.undo.iter().enumerate().skip(self.journal_floor) {
            if let Some(at) = e.disk {
                refs.push((true, i, at));
            }
        }
        for (i, e) in self.redo.iter().enumerate().rev() {
            if let Some(at) = e.disk {
                refs.push((false, i, at));
            }
        }
        Some(CompactJob {
            src: j.path.clone(),
            tmp: j.path.with_extension("plh.tmp"),
            key: j.path_hash,
            before: j.disk_bytes(),
            root,
            refs,
            saved: self.saved.zip(identity_of_saved),
            cursor: self.current,
            stamp: self.compact_stamp(),
        })
    }

    /// Installs a finished compaction: the compacted file replaces the
    /// journal and the entries point into it. Refused (temporary file
    /// removed, nothing changed) when the history changed meanwhile. A failed
    /// rename keeps the original journal attached and valid.
    pub fn finish_compaction(&mut self, done: CompactDone) -> io::Result<(u64, u64)> {
        // An entry written meanwhile moves the end: the result is refused.
        self.settle_journal(true);
        if done.stamp != self.compact_stamp()
            || self.journal.as_ref().is_none_or(|j| j.path != done.src)
        {
            let _ = std::fs::remove_file(&done.tmp);
            return Err(io::Error::other(
                "the history changed while it was being compacted; nothing was changed",
            ));
        }
        let j = self.journal.take().expect("checked");
        let (path, key) = (j.path.clone(), j.path_hash);
        drop(j);
        if let Err(e) = std::fs::rename(&done.tmp, &path) {
            let _ = std::fs::remove_file(&done.tmp);
            // The original file is untouched: reattach it.
            if let Ok(Ok((oj, _))) = Journal::open(&path, key) {
                self.journal = Some(oj);
            }
            return Err(e);
        }
        for &(undo, i, at) in &done.refs {
            if undo {
                self.undo[i].disk = Some(at);
            } else {
                self.redo[i].disk = Some(at);
            }
        }
        let (nj, _) = match Journal::open(&path, key)? {
            Ok(x) => x,
            Err(p) => return Err(io::Error::other(p.to_string())),
        };
        let after = nj.disk_bytes();
        self.journal = Some(nj);
        Ok((done.before, after))
    }
}

/// What identifies the history a compaction was planned for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompactStamp {
    end: u64,
    undo: usize,
    redo: usize,
    current: super::StateId,
    floor: usize,
    evicted: u64,
}

/// Compact History planned by [`History::plan_compaction`]; [`run`] copies
/// the live records into a temporary journal without touching the history
/// (safe on a worker thread; the live journal may keep growing meanwhile —
/// [`History::finish_compaction`] then refuses the result).
///
/// [`run`]: CompactJob::run
#[derive(Debug)]
pub struct CompactJob {
    src: PathBuf,
    tmp: PathBuf,
    key: u64,
    before: u64,
    root: super::StateId,
    /// (undo?, entry index, record) in copy order.
    refs: Vec<(bool, usize, DiskRef)>,
    saved: Option<(super::StateId, u64)>,
    cursor: super::StateId,
    stamp: CompactStamp,
}

/// A compacted temporary journal waiting to be installed.
#[derive(Debug)]
pub struct CompactDone {
    src: PathBuf,
    tmp: PathBuf,
    before: u64,
    refs: Vec<(bool, usize, DiskRef)>,
    stamp: CompactStamp,
}

impl CompactDone {
    /// Drops a result that will not be installed (another project is open
    /// now): removes its temporary file.
    pub fn discard(self) {
        let _ = std::fs::remove_file(&self.tmp);
    }
}

impl CompactJob {
    /// Records to copy.
    pub fn records(&self) -> usize {
        self.refs.len()
    }

    /// Copies the live records (`cancel` stops between records; the
    /// temporary file is removed on any failure).
    pub fn run(self, cancel: &std::sync::atomic::AtomicBool) -> io::Result<CompactDone> {
        let tmp = self.tmp.clone();
        let r = self.copy(cancel);
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    fn copy(self, cancel: &std::sync::atomic::AtomicBool) -> io::Result<CompactDone> {
        let mut src = Journal {
            path: self.src.clone(),
            file: File::open(&self.src)?,
            end: 0,
            path_hash: self.key,
            failures: 0,
            last_failure: None,
            pending: None,
        };
        let mut n = Journal::create(&self.tmp, self.key)?;
        let mut b = Vec::new();
        put_uvarint(&mut b, self.root);
        put_uvarint(&mut b, 0);
        n.append(K_BASE, &[&b])?;
        let mut refs = Vec::with_capacity(self.refs.len());
        for &(undo, i, at) in &self.refs {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let raw = src.read_raw(at)?;
            refs.push((undo, i, n.append_raw(&raw)?));
        }
        if let Some((s, id)) = self.saved {
            let mut b = Vec::new();
            put_uvarint(&mut b, s);
            put_uvarint(&mut b, id);
            n.append(K_SAVED, &[&b])?;
        }
        let mut b = Vec::new();
        put_uvarint(&mut b, self.cursor);
        n.append(K_CURSOR, &[&b])?;
        n.sync()?;
        Ok(CompactDone {
            src: self.src,
            tmp: self.tmp,
            before: self.before,
            refs,
            stamp: self.stamp,
        })
    }
}

impl Journal {
    /// Appends a record copied unchanged from another journal.
    fn append_raw(&mut self, raw: &[u8]) -> io::Result<DiskRef> {
        let at = self.end;
        self.file.seek(SeekFrom::Start(at))?;
        self.file.write_all(raw)?;
        self.end = at + raw.len() as u64;
        Ok(DiskRef {
            offset: at,
            len: (raw.len() - 8) as u32,
        })
    }
}

/// L-S48: a journal closed while an entry is still being written finishes
/// it and the records queued behind it (the file stays complete).
impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.settle(true);
    }
}

impl Journal {
    /// A write failed: counted (the app reports it); the in-memory history
    /// keeps working.
    fn failed(&mut self, e: &io::Error) {
        self.failures += 1;
        self.last_failure = Some(e.to_string());
    }

    /// Failed writes so far and the last error (§216).
    pub fn failures(&self) -> (u64, Option<&str>) {
        (self.failures, self.last_failure.as_deref())
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::model::patch::RankSetBuilder;

    fn velocity_change(host: ContentRef) -> Change {
        let mut b = RankSetBuilder::default();
        b.push(0);
        b.push(2);
        Change::Velocity {
            track: host,
            ranks: b.finish(),
            old: vec![90, 91],
            new: 64,
        }
    }

    fn encoded(c: &Change) -> Vec<u8> {
        let doc = PlumDocument::new_empty(crate::model::Division::from_raw(960));
        let mut o = Vec::new();
        encode_change(&mut o, &doc, c).unwrap();
        o
    }

    /// V09-S11 (`docs/CLIP_PIANO_ROLL.md`): a note change of a track's own
    /// notes is encoded exactly as before (the track id as a uvarint), so
    /// journals written before S11 read the same; an owned content's is
    /// `2^32 + id` and comes back as that content.
    #[test]
    fn note_hosts_round_trip_and_track_hosts_keep_their_encoding() {
        let track = encoded(&velocity_change(ContentRef::Track(TrackId(300))));
        let mut old = vec![track[0]];
        put_uvarint(&mut old, 300);
        assert_eq!(&track[..old.len()], &old[..], "kind + the plain track id");
        for host in [
            ContentRef::Track(TrackId(0)),
            ContentRef::Track(TrackId(u32::MAX)),
            ContentRef::Owned(ContentId(0)),
            ContentRef::Owned(ContentId(7)),
            ContentRef::Owned(ContentId(u32::MAX)),
        ] {
            let o = encoded(&velocity_change(host));
            let Change::Velocity {
                track, old, new, ..
            } = decode_change(&o, false).unwrap()
            else {
                panic!("a velocity change");
            };
            assert_eq!((track, old, new), (host, vec![90, 91], 64), "{host:?}");
        }
    }

    /// A host past the largest content id is refused, never wrapped.
    #[test]
    fn a_host_beyond_every_content_id_is_refused() {
        // [kind, host = 5 (one byte), the rest of a valid record].
        let valid = encoded(&velocity_change(ContentRef::Track(TrackId(5))));
        assert_eq!(valid[1], 5);
        let with_host = |host: u64| {
            let mut o = vec![valid[0]];
            put_uvarint(&mut o, host);
            o.extend_from_slice(&valid[2..]);
            o
        };
        let last = HOST_CONTENT + u64::from(u32::MAX);
        assert!(
            decode_change(&with_host(last), false).is_ok(),
            "the largest content"
        );
        assert!(decode_change(&with_host(last + 1), false).is_err());
    }
}

#[cfg(test)]
mod pitch_table_tests {
    use super::*;
    use crate::model::patch::RankSetBuilder;

    fn round_trip(table: Box<crate::editor::PitchTable>) -> (u8, Box<crate::editor::PitchTable>) {
        let doc = PlumDocument::new_empty(crate::model::Division::from_raw(960));
        let mut b = RankSetBuilder::default();
        b.push(0);
        b.push(3);
        let c = Change::Pitch {
            track: ContentRef::Track(TrackId(1)),
            ranks: b.finish(),
            old: vec![200, 60],
            table,
        };
        let mut o = Vec::new();
        encode_change(&mut o, &doc, &c).unwrap();
        let Change::Pitch { table, old, .. } = decode_change(&o, false).unwrap() else {
            panic!("a pitch change");
        };
        assert_eq!(old, vec![200, 60]);
        (o[0], table)
    }

    /// v0.9: a pitch table of the 128 standard keys keeps the v0.8 record
    /// (kind 1, 128 bytes); one that reaches above 127 (256-Key Mode) is
    /// kind 18 with all 256 entries; both decode to the same table.
    #[test]
    fn pitch_tables_keep_the_v08_record_unless_they_use_256_keys() {
        let mut t = crate::editor::identity_pitch_table();
        t[60] = 64;
        t[64] = 60;
        let (kind, back) = round_trip(t.clone());
        assert_eq!((kind, back), (1, t));
        let mut t = crate::editor::identity_pitch_table();
        t[200] = 255;
        t[100] = 140;
        let (kind, back) = round_trip(t.clone());
        assert_eq!((kind, back), (18, t));
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::history::{History, Operation};
    use crate::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    /// §216 / §274: a journal that cannot be written (here a read-only
    /// handle: disk full, permission or a vanished device behave alike) never
    /// breaks editing — the steps stay undoable in memory and every failed
    /// write is counted with its error for the app to report.
    #[test]
    fn a_journal_that_cannot_be_written_counts_failures_and_editing_goes_on() {
        let dir = std::env::temp_dir().join(format!("plumspirit-jfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("p.plh");
        let mut doc = PlumDocument::new_project("j".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.attach_new_journal(Journal::create(&path, 7).unwrap(), None)
            .unwrap();
        // From now on every write fails.
        h.journal_mut().unwrap().file = File::open(&path).unwrap();
        let before = h.journal().unwrap().failures().0;
        assert_eq!(before, 0);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: 0.into(),
                record: NoteRecord::new(0, 10, a),
            },
        )
        .unwrap();
        let (n, last) = h.journal().unwrap().failures();
        assert!(n > before, "the failed entry write is counted");
        assert!(last.is_some_and(|m| !m.is_empty()));
        assert_eq!(doc.note_count(), 1);
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.note_count(), 0, "undo works from memory");
        h.redo(&mut doc).unwrap();
        assert_eq!(doc.note_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod clip_record_tests {
    use super::*;
    use crate::history::History;
    use crate::model::{Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord};

    /// A recorded clip change (an independent copy: its layout and the copied
    /// content) encodes, decodes back to the same layouts and content, and any
    /// damaged byte is refused or decodes to something the history validates —
    /// never a panic (V09-S07).
    #[test]
    fn clip_records_round_trip_and_damage_never_panics() {
        let mut doc = PlumDocument::new_project("j".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..50u64)
            .map(|k| {
                NoteRecord::new(
                    k * 60,
                    50,
                    NoteAttrs::new(40 + (k % 40) as u8, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let t0 = doc.track(0).unwrap().id();
        let op =
            crate::editor::clips::duplicate_clips(&doc, &[(ClipId::Primary(t0), 0, 9600)], false)
                .unwrap();
        let mut h = History::default();
        h.apply(&mut doc, op).unwrap();
        let entry = h.undo.back().unwrap();
        let change = entry.change.as_ref().unwrap();
        let mut o = Vec::new();
        encode_change(&mut o, &doc, change).unwrap();
        assert_eq!(o[0], 20);
        // Decoded as an undo entry (document after the change): the copied
        // content is in the document, so the record keeps nothing.
        let Change::Clips(after) = decode_change(&o, true).unwrap() else {
            panic!("a clip change");
        };
        assert!(after.stash.is_empty());
        // As a redo entry (document before it): the content comes back.
        let Change::Clips(before) = decode_change(&o, false).unwrap() else {
            panic!("a clip change");
        };
        assert_eq!(before.stash.len(), 1);
        assert_eq!(before.stash[0].notes.len(), 50);
        assert_eq!(before.new, doc.clips().layout().clone().into_unrevised());
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut b = o.clone();
            let at = (seed % b.len() as u64) as usize;
            b[at] ^= 1 << ((seed >> 32) % 8);
            if let Ok(Change::Clips(mut c)) = decode_change(&b, false) {
                // Whatever decodes is applied only if the table stays valid.
                let mut d = doc.clone();
                let _ = crate::history::move_clips_for_test(&mut d, &mut c);
                d.validate_clips().unwrap();
            }
        }
    }
}
