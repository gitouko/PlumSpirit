//! The `.lup` native project format (version 3; version 2 files from PlumSpirit
//! v0.3 and version 1 files from v0.2 are read and converted in memory). See
//! `docs/LUP_FORMAT.md` for the byte-level specification; this module is its
//! reference implementation.
//!
//! * A 32-byte header (magic, version, section count, CRC-32) followed by
//!   self-describing sections: `tag[4]`, `flags u32`, `length u64`, payload,
//!   `CRC-32 u32`. All integers are little-endian; inside payloads, counts and
//!   ticks are LEB128 varints.
//! * Notes are stored compactly (start delta, duration, packed attributes —
//!   typically 6–9 bytes per note), never as text.
//! * Writing streams (section lengths are patched by seeking back); reading
//!   streams through a bounded buffer. No allocation is sized from file metadata
//!   without first checking it against the bytes that actually remain.
//! * The same parser drives both loading ([`read_project`]) and post-save
//!   verification, which compares the written file with the document note by note
//!   without building a second copy of it.

use crate::codec::{Crc32, put_ivarint, put_uvarint};
use crate::midi::group_digits;
use crate::model::conductor::{
    Conductor, ConductorScope, Interp, MASTER_VOLUME_MAX, MAX_DENOM_POW, Marker, MasterVolumePoint,
    Origin, TempoPoint, TimeSignature,
};
use crate::model::document::{ExtraChunk, PlumDocument, SmfFormat};
use crate::model::events::{
    ArenaRange, EventStore, RawEvent, RawKind, channel_data_len, system_data_len,
};
use crate::model::note::{NoteAttrs, NoteRecord, SubchannelId};
use crate::model::note_store::{NoteStore, NoteView};
use crate::model::patch::{GROUP_FOLLOWS, SUB_FOLLOWS};
use crate::model::project::{
    Bpm, DensityBasis, GraphConfig, GraphSlot, GraphTarget, KeyRange, LoopConfig,
    MAX_ONION_OPACITY, MIN_ONION_OPACITY, OnionConfig, OnionMode, OnionOrder, OnionTrackEntry,
    ProjectInfo, SoundFontRef, UnknownSection,
};
use crate::model::tempo::TempoValue;
use crate::model::timing::Division;
use crate::model::track::{
    GatePercent, MAX_TICK_CORRECTION, OutputRoute, Subchannel, SubchannelTable, Track, TrackId,
    TrackKind, TrackProps,
};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// File signature: `\x89LUP\r\n\x1a\n` (binary-safe like PNG's: detects text-mode
/// transfers and truncation to 7 bits).
pub const LUP_MAGIC: [u8; 8] = [0x89, b'L', b'U', b'P', 0x0D, 0x0A, 0x1A, 0x0A];
/// Native schema version of a standard project (0–127 keys) written by this
/// release. Version 1 (PlumSpirit v0.2) had no Conductor section; version 2
/// (v0.3) had no Master Volume, track properties or transport section. Both
/// are still read.
pub const LUP_VERSION: u16 = 5;
/// Version of a 256-Key Mode project (v0.9): version 5 plus the key range in
/// the project information and pitch bit 27 in note words. A standard project
/// keeps version 5 (byte for byte what v0.8 writes and opens); v0.8 refuses a
/// version 6 file as "saved by a newer PlumSpirit" instead of misreading it.
pub const LUP_VERSION_256: u16 = 6;
/// Format version written when the project has clips (v0.9 `CLIP` section): the
/// version 6 layout (key range in `META`) plus the mandatory clip table.
pub const LUP_VERSION_CLIPS: u16 = 7;
/// The newest format version this PlumSpirit reads.
const LUP_VERSION_MAX: u16 = LUP_VERSION_CLIPS;
/// File name extension (without the dot).
pub const LUP_EXTENSION: &str = "lup";

const HEADER_SIZE: u16 = 32;
const FLAG_MANDATORY: u32 = 1;

const TAG_META: [u8; 4] = *b"META";
const TAG_TRACK: [u8; 4] = *b"TRAK";
const TAG_EXTRA: [u8; 4] = *b"XCHK";
const TAG_ONION: [u8; 4] = *b"ONIO";
const TAG_SOUNDFONT: [u8; 4] = *b"SFNT";
const TAG_CONDUCTOR: [u8; 4] = *b"COND";
const TAG_TRANSPORT: [u8; 4] = *b"XPRT";
/// Note groups (v5).
const TAG_GROUPS: [u8; 4] = *b"GRPS";
/// Named Piano Roll views (v0.7, optional).
const TAG_ZOOM: [u8; 4] = *b"ZOOM";
/// Reference audio (v0.8; optional — older versions keep it unread).
const TAG_REFERENCE: [u8; 4] = *b"REFA";
/// Track Folders (v0.8; optional — older versions keep it unread).
const TAG_FOLDERS: [u8; 4] = *b"FOLD";
/// v0.9 clips (mandatory when present, version 7).
const TAG_CLIPS: [u8; 4] = *b"CLIP";
/// v0.9 Track List view (optional; display settings, older versions keep it
/// unread).
const TAG_TRACK_LIST: [u8; 4] = *b"TLST";
/// v1.0-beta1 source provenance for strict export (optional; older versions
/// keep it unread).
const TAG_SOURCE: [u8; 4] = *b"PROV";
/// v1.0-beta1 (B1-0064) per-track Onion Skin settings (optional; written only
/// when used; older versions keep it unread).
const TAG_ONION_TRACKS: [u8; 4] = *b"ONPT";
/// v1.0-beta1 (B1-0035): per-project Lua plugin data (optional; older
/// versions keep it unread).
const TAG_METADATA: [u8; 4] = *b"XMDA";
const TAG_PLUGIN_DATA: [u8; 4] = *b"PLUG";
/// Most plugins with project data, and the largest data of one.
pub const MAX_PLUGIN_DATA_ENTRIES: usize = 4096;
pub const MAX_PLUGIN_DATA_BYTES: usize = 1024 * 1024;
/// Smallest encoded placement / content in `CLIP` (count checks).
const MIN_PLACEMENT_BYTES: u64 = 8;
const MIN_CONTENT_BYTES: u64 = 4;

/// The folders as saved: members and anchors only for existing top-level
/// tracks, collapse state only for existing folders. `None`: nothing to save.
fn saved_folders(
    doc: &PlumDocument,
) -> Option<(crate::model::TrackFolders, std::collections::BTreeSet<u32>)> {
    let p = doc.project();
    if p.folders.is_empty() {
        return None;
    }
    let roots: std::collections::HashSet<crate::model::TrackId> = doc
        .tracks()
        .iter()
        .filter(|t| t.parent().is_none())
        .map(|t| t.id())
        .collect();
    let f = p.folders.pruned(|t| roots.contains(&t));
    let collapsed = p
        .folders_collapsed
        .iter()
        .copied()
        .filter(|&id| f.get(id).is_some())
        .collect();
    Some((f, collapsed))
}

/// Unknown optional sections larger than this are dropped (with a warning)
/// instead of being kept in memory.
const MAX_PRESERVED_SECTION: u64 = 16 * 1024 * 1024;
const MAX_PRESERVED_TOTAL: u64 = 64 * 1024 * 1024;
/// Smallest encoded note: 1-byte delta, 1-byte duration, 4-byte attributes.
const MIN_NOTE_BYTES: u64 = 6;
/// Smallest encoded event: delta, anchor, kind, 3 data bytes (channel) or
/// meta type + length.
const MIN_EVENT_BYTES: u64 = 4;
/// Tracks whose subchannel ids are all below this validate note subchannels
/// through a dense table (at most 1 MiB).
const DENSE_SUB_IDS: u32 = 1 << 20;
const NOTE_BATCH: usize = 64 * 1024;
const READ_BUFFER: usize = 256 * 1024;

// ------------------------------------------------------------------- errors

/// Where a save failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStage {
    CreateTemp,
    Write,
    Flush,
    Verify,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LupError {
    /// The file does not start with the `.lup` signature.
    NotAProject,
    /// Written by a newer PlumSpirit.
    NewerVersion(u16),
    /// A section this version must understand but does not.
    UnsupportedSection(String),
    /// A section's checksum does not match (damaged file).
    Corrupt {
        section: String,
        offset: u64,
    },
    /// The file ends early.
    Truncated {
        offset: u64,
    },
    /// Structurally invalid content.
    Malformed {
        offset: u64,
        detail: String,
    },
    Io {
        stage: Option<SaveStage>,
        kind: io::ErrorKind,
        message: String,
    },
    OutOfMemory,
    /// The written file did not read back as the document.
    Verification {
        detail: String,
    },
    Cancelled,
}

impl LupError {
    fn io(stage: Option<SaveStage>, e: io::Error) -> Self {
        LupError::Io {
            stage,
            kind: e.kind(),
            message: e.to_string(),
        }
    }

    fn malformed(offset: u64, detail: impl Into<String>) -> Self {
        LupError::Malformed {
            offset,
            detail: detail.into(),
        }
    }

    /// Concise, actionable message for the user.
    pub fn user_message(&self) -> String {
        match self {
            LupError::NotAProject => {
                "This file is not a PlumSpirit project (.lup). To open a MIDI file, choose a .mid file.".into()
            }
            LupError::NewerVersion(v) => format!(
                "Unsupported .lup version: this project was saved by a newer PlumSpirit (format version {v}; this version reads version {LUP_VERSION_MAX}). Update PlumSpirit to open it."
            ),
            LupError::UnsupportedSection(tag) => format!(
                "This project contains data this version of PlumSpirit does not understand (section \"{tag}\"). It was probably saved by a newer version; update PlumSpirit to open it."
            ),
            LupError::Corrupt { section, offset } => format!(
                "The project file is damaged: the {section} data at byte {} failed its checksum.",
                group_digits(*offset)
            ),
            LupError::Truncated { offset } => format!(
                "The project file is incomplete: it ends unexpectedly at byte {}.",
                group_digits(*offset)
            ),
            LupError::Malformed { offset, detail } => format!(
                "The project file is damaged ({detail}, at byte {}).",
                group_digits(*offset)
            ),
            LupError::Io { stage, message, .. } => match stage {
                None => format!("The project could not be read: {message}."),
                Some(SaveStage::CreateTemp) => format!(
                    "PlumSpirit could not create a temporary file next to the project: {message}. Nothing was saved and the previous file is unchanged."
                ),
                Some(SaveStage::Write | SaveStage::Flush) => format!(
                    "Writing the project failed: {message}. The previous file is unchanged."
                ),
                Some(SaveStage::Verify) => format!(
                    "The project was written but could not be read back: {message}. The previous file is unchanged."
                ),
                Some(SaveStage::Replace) => format!(
                    "The project was written and verified, but the destination could not be replaced safely: {message}. The previous file is unchanged."
                ),
            },
            LupError::OutOfMemory => "Not enough memory to open this project.".into(),
            LupError::Verification { detail } => format!(
                "The saved file did not match the project when read back ({detail}). The previous file is unchanged."
            ),
            LupError::Cancelled => "Canceled.".into(),
        }
    }
}

impl fmt::Display for LupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for LupError {}

/// Progress and cancellation for background loads/saves.
#[derive(Default, Clone, Copy)]
pub struct LupControl<'a> {
    pub cancel: Option<&'a AtomicBool>,
    pub progress_notes: Option<&'a AtomicU64>,
}

impl LupControl<'_> {
    fn tick(&self, notes: u64) -> Result<(), LupError> {
        if let Some(p) = self.progress_notes {
            p.store(notes, Ordering::Relaxed);
        }
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(LupError::Cancelled);
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ writing

/// Collects a section payload, streaming it to the file in blocks while
/// computing its CRC and length.
struct SectionWriter<'w, W: Write + Seek> {
    w: &'w mut W,
    buf: Vec<u8>,
    crc: Crc32,
    len: u64,
    len_pos: u64,
    tag: [u8; 4],
    flags: u32,
}

/// The stored section checksum: CRC-32 of `tag ‖ flags ‖ length ‖ CRC-32(payload)`
/// (all little-endian), so the section header is protected as well as the payload
/// while the payload can still be streamed.
fn section_checksum(tag: [u8; 4], flags: u32, len: u64, payload_crc: u32) -> u32 {
    let mut c = Crc32::new();
    c.update(&tag);
    c.update(&flags.to_le_bytes());
    c.update(&len.to_le_bytes());
    c.update(&payload_crc.to_le_bytes());
    c.finish()
}

impl<'w, W: Write + Seek> SectionWriter<'w, W> {
    fn begin(w: &'w mut W, tag: [u8; 4], flags: u32) -> io::Result<Self> {
        w.write_all(&tag)?;
        w.write_all(&flags.to_le_bytes())?;
        let len_pos = w.stream_position()?;
        w.write_all(&0u64.to_le_bytes())?;
        Ok(SectionWriter {
            w,
            buf: Vec::with_capacity(64 * 1024),
            crc: Crc32::new(),
            len: 0,
            len_pos,
            tag,
            flags,
        })
    }

    #[inline]
    fn maybe_flush(&mut self) -> io::Result<()> {
        if self.buf.len() >= 60 * 1024 {
            self.flush_buf()?;
        }
        Ok(())
    }

    fn flush_buf(&mut self) -> io::Result<()> {
        self.crc.update(&self.buf);
        self.w.write_all(&self.buf)?;
        self.len += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    fn uv(&mut self, v: u64) {
        put_uvarint(&mut self.buf, v);
    }

    fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        if b.len() > 32 * 1024 {
            self.flush_buf()?;
            self.crc.update(b);
            self.w.write_all(b)?;
            self.len += b.len() as u64;
            Ok(())
        } else {
            self.buf.extend_from_slice(b);
            self.maybe_flush()
        }
    }

    fn string(&mut self, s: &[u8]) -> io::Result<()> {
        self.uv(s.len() as u64);
        self.bytes(s)
    }

    fn finish(mut self) -> io::Result<u64> {
        self.flush_buf()?;
        let end = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(self.len_pos))?;
        self.w.write_all(&self.len.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        let sum = section_checksum(self.tag, self.flags, self.len, self.crc.finish());
        self.w.write_all(&sum.to_le_bytes())?;
        Ok(16 + self.len + 4)
    }
}

/// Statistics of a written project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LupWriteStats {
    pub bytes: u64,
    pub tracks: usize,
    pub notes: u64,
    pub events: u64,
}

fn encode_kind<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    events: &EventStore,
    kind: &RawKind,
) -> io::Result<()> {
    match *kind {
        RawKind::Channel { status, data } => {
            s.buf.extend_from_slice(&[0, status, data[0], data[1]]);
        }
        RawKind::Meta { meta_type, payload } => {
            s.buf.extend_from_slice(&[1, meta_type]);
            s.string(events.payload(payload))?;
        }
        RawKind::SysEx { payload } => {
            s.buf.push(2);
            s.string(events.payload(payload))?;
        }
        RawKind::SysExEscape { payload } => {
            s.buf.push(3);
            s.string(events.payload(payload))?;
        }
        RawKind::System { status, data } => {
            s.buf.extend_from_slice(&[4, status, data[0], data[1]]);
        }
    }
    Ok(())
}

fn write_origin<W: Write + Seek>(s: &mut SectionWriter<'_, W>, o: Option<Origin>) {
    if let Some(o) = o {
        s.uv(u64::from(o.track.0));
        s.uv(u64::from(o.anchor));
        s.uv(u64::from(o.rank));
        s.uv(u64::from(o.seq));
    }
}

fn write_conductor<W: Write + Seek>(s: &mut SectionWriter<'_, W>, c: &Conductor) -> io::Result<()> {
    s.uv(0); // conductor identity (the single Conductor of the project)
    s.buf.push(c.scope().code());
    s.uv(u64::from(c.next_id()));
    s.uv(c.tempo_points().len() as u64);
    let mut prev = 0u64;
    for p in c.tempo_points() {
        s.uv(u64::from(p.id));
        s.uv(p.tick - prev);
        prev = p.tick;
        let flags = u8::from(p.interp == Interp::Linear)
            | u8::from(p.implicit) << 1
            | u8::from(p.origin.is_some()) << 2;
        s.buf.push(flags);
        match p.value {
            TempoValue::Micros(us) => {
                s.buf.push(0);
                s.uv(u64::from(us));
            }
            TempoValue::Bpm(b) => {
                s.buf.push(1);
                s.uv(b.mantissa());
                put_ivarint(&mut s.buf, i64::from(b.exponent()));
            }
        }
        write_origin(s, p.origin);
        s.maybe_flush()?;
    }
    s.uv(c.time_signatures().len() as u64);
    let mut prev = 0u64;
    for m in c.time_signatures() {
        s.uv(u64::from(m.id));
        s.uv(m.tick - prev);
        prev = m.tick;
        s.buf
            .push(u8::from(m.implicit) << 1 | u8::from(m.origin.is_some()) << 2);
        s.buf
            .extend_from_slice(&[m.numerator, m.denom_pow, m.clocks, m.thirty_seconds]);
        write_origin(s, m.origin);
        s.maybe_flush()?;
    }
    s.uv(c.markers().len() as u64);
    let mut prev = 0u64;
    for k in c.markers() {
        s.uv(u64::from(k.id));
        s.uv(k.start - prev);
        prev = k.start;
        let flags = u8::from(k.end.is_some())
            | u8::from(k.cue) << 1
            | u8::from(k.origin.is_some()) << 2
            | u8::from(k.raw_title.is_some()) << 3;
        s.buf.push(flags);
        if let Some(e) = k.end {
            s.uv(e - k.start);
        }
        s.string(k.title.as_bytes())?;
        if let Some(r) = &k.raw_title {
            s.string(r)?;
        }
        write_origin(s, k.origin);
        s.maybe_flush()?;
    }
    // Version 3: Master Volume points.
    s.uv(c.master_volume().len() as u64);
    let mut prev = 0u64;
    for p in c.master_volume() {
        s.uv(u64::from(p.id));
        s.uv(p.tick - prev);
        prev = p.tick;
        s.buf.push(u8::from(p.origin.is_some()) << 2);
        s.uv(u64::from(p.value));
        s.buf.push(p.device);
        write_origin(s, p.origin);
        s.maybe_flush()?;
    }
    Ok(())
}

/// Track properties (version 3), after the End-of-Track entry of `TRAK`.
fn write_subchannels<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    subs: &SubchannelTable,
) -> io::Result<()> {
    s.uv(subs.len() as u64);
    for e in subs.entries() {
        s.uv(u64::from(e.id.0));
        s.buf.push(e.midi_channel);
        match &e.name {
            Some(n) => {
                s.buf.push(1);
                s.string(n.as_bytes())?;
            }
            None => s.buf.push(0),
        }
        s.maybe_flush()?;
    }
    Ok(())
}

fn write_track_props<W: Write + Seek>(s: &mut SectionWriter<'_, W>, t: &Track) -> io::Result<()> {
    let p = t.props();
    let external = matches!(p.route, OutputRoute::External(_));
    let flags = u8::from(t.mute)
        | u8::from(t.solo) << 1
        | u8::from(p.kind == TrackKind::Rhythm) << 2
        | u8::from(p.onion_color.is_some()) << 3
        | u8::from(external) << 4;
    s.buf.push(flags);
    s.buf.push(p.default_channel);
    if let OutputRoute::External(name) = &p.route {
        s.string(name.as_bytes())?;
    }
    put_ivarint(&mut s.buf, p.tick_correction);
    s.uv(u64::from(p.correction_from_measure));
    put_ivarint(&mut s.buf, i64::from(p.key_correction));
    s.uv(u64::from(p.gate.hundredths()));
    s.buf.extend_from_slice(&[p.key_range.0, p.key_range.1]);
    if let Some(c) = p.onion_color {
        s.buf.extend_from_slice(&c);
    }
    Ok(())
}

fn write_groups<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    t: &crate::model::GroupTable,
) -> io::Result<()> {
    let saved: Vec<&crate::model::GroupInfo> =
        t.entries().iter().filter(|g| !g.temporary).collect();
    s.uv(u64::from(t.next_id()));
    s.uv(saved.len() as u64);
    for g in saved {
        s.uv(u64::from(g.id));
        // A temporary parent is not saved: the group becomes top level.
        s.uv(u64::from(if t.saved(g.parent) { g.parent } else { 0 }));
        let flags =
            u8::from(g.locked) | u8::from(g.name.is_some()) << 1 | u8::from(g.color.is_some()) << 2;
        s.buf.push(flags);
        s.uv(u64::from(g.link));
        if let Some(n) = &g.name {
            s.string(n.as_bytes())?;
        }
        if let Some(c) = g.color {
            s.buf.extend_from_slice(&c);
        }
        s.maybe_flush()?;
    }
    Ok(())
}

fn write_transport<W: Write + Seek>(s: &mut SectionWriter<'_, W>, p: &ProjectInfo) {
    let l = &p.looping;
    s.buf.push(u8::from(l.enabled));
    match l.range {
        Some((a, b)) => {
            s.buf.push(1);
            s.uv(a);
            s.uv(b);
        }
        None => s.buf.push(0),
    }
    s.buf.push(p.graphs.slots.len() as u8);
    for g in &p.graphs.slots {
        let (kind, n) = g.target.code();
        s.buf
            .extend_from_slice(&[u8::from(g.visible), kind, n, g.channel]);
    }
}

/// Writes `doc` as a `.lup` project to a seekable writer.
/// Notes in the `TRAK` note encoding (count, then delta / duration / attribute
/// word / subchannel / group), shared by tracks and clip contents.
fn write_note_block<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    notes: &NoteStore,
    groups: &crate::model::GroupTable,
    control: &LupControl<'_>,
    notes_done: &mut u64,
) -> Result<(), LupError> {
    let wr = |e: io::Error| LupError::io(Some(SaveStage::Write), e);
    s.uv(notes.len() as u64);
    let mut prev = 0u64;
    for n in notes.iter() {
        s.uv(n.start_tick - prev);
        s.uv(n.duration_ticks);
        let group = if n.group != 0 && groups.saved(n.group) {
            n.group
        } else {
            0
        };
        let mut word = n.attrs.raw();
        if n.sub.0 != 0 {
            word |= SUB_FOLLOWS;
        }
        if group != 0 {
            word |= GROUP_FOLLOWS;
        }
        s.buf.extend_from_slice(&word.to_le_bytes());
        if n.sub.0 != 0 {
            s.uv(u64::from(n.sub.0));
        }
        if group != 0 {
            s.uv(u64::from(group));
        }
        prev = n.start_tick;
        s.maybe_flush().map_err(wr)?;
        *notes_done += 1;
        if notes_done.is_multiple_of(65_536) {
            control.tick(*notes_done)?;
        }
    }
    Ok(())
}

/// Events in the `TRAK` event encoding (count, then tick delta / anchor /
/// kind / data), shared by tracks and clip contents.
fn write_event_block<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    events: &EventStore,
) -> io::Result<()> {
    s.uv(events.len() as u64);
    let mut prev = 0u64;
    for e in events.events() {
        s.uv(e.tick - prev);
        s.uv(u64::from(e.anchor));
        encode_kind(s, events, &e.kind)?;
        prev = e.tick;
        s.maybe_flush()?;
    }
    Ok(())
}

/// Name (flag bit 1) and colour (flag bit 2) of a clip or content.
fn label_flags(name: Option<&str>, color: Option<u32>) -> u8 {
    (u8::from(name.is_some()) << 1) | (u8::from(color.is_some()) << 2)
}

fn write_label<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    name: Option<&str>,
    color: Option<u32>,
) -> io::Result<()> {
    if let Some(n) = name {
        s.string(n.as_bytes())?;
    }
    if let Some(c) = color {
        s.buf
            .extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
    }
    Ok(())
}

/// The `CLIP` payload (docs/LUP_FORMAT.md): layout, next ids, explicit
/// placements, removed primaries, owned contents with their notes and events.
fn write_clips<W: Write + Seek>(
    s: &mut SectionWriter<'_, W>,
    clips: &crate::model::ClipTable,
    groups: &crate::model::GroupTable,
    control: &LupControl<'_>,
    notes_done: &mut u64,
) -> Result<(), LupError> {
    use crate::model::{ClipId, ContentRef};
    let wr = |e: io::Error| LupError::io(Some(SaveStage::Write), e);
    let (next_clip, next_content) = clips.next_ids();
    s.uv(1); // layout version of the section
    s.uv(u64::from(next_clip));
    s.uv(u64::from(next_content));
    s.uv(clips.explicit_placements().len() as u64);
    for p in clips.explicit_placements() {
        match p.id {
            ClipId::Primary(t) => {
                s.buf.push(0);
                s.uv(u64::from(t.0));
            }
            ClipId::Placed(n) => {
                s.buf.push(1);
                s.uv(u64::from(n));
            }
        }
        s.uv(u64::from(p.track.0));
        match p.content {
            ContentRef::Track(t) => {
                s.buf.push(0);
                s.uv(u64::from(t.0));
            }
            ContentRef::Owned(c) => {
                s.buf.push(1);
                s.uv(u64::from(c.0));
            }
        }
        s.uv(p.start);
        s.uv(p.offset);
        s.buf.push(
            u8::from(p.muted)
                | label_flags(p.name.as_deref(), p.color)
                | (u8::from(p.length.is_some()) << 3),
        );
        if let Some(l) = p.length {
            s.uv(l);
        }
        write_label(s, p.name.as_deref(), p.color).map_err(wr)?;
        s.maybe_flush().map_err(wr)?;
    }
    s.uv(clips.removed_primaries().len() as u64);
    for t in clips.removed_primaries() {
        s.uv(u64::from(t.0));
    }
    s.uv(clips.contents().len() as u64);
    for c in clips.contents() {
        s.uv(u64::from(c.id.0));
        s.buf.push(label_flags(c.name.as_deref(), c.color));
        write_label(s, c.name.as_deref(), c.color).map_err(wr)?;
        write_note_block(s, &c.notes, groups, control, notes_done)?;
        write_event_block(s, &c.events).map_err(wr)?;
    }
    Ok(())
}

/// B1-0064: whether the ONPT section is written (Per-Track Settings on, or
/// entries kept from an earlier use); a project that never used it is saved
/// byte for byte as before.
fn onion_tracks_used(o: &OnionConfig) -> bool {
    o.per_track || !o.track_entries.is_empty()
}

/// One `TRAK` section: tag, flags, length, payload, checksum. Returns its size.
fn write_track_section<W: Write + Seek>(
    w: &mut W,
    t: &Track,
    groups: &crate::model::GroupTable,
    control: &LupControl<'_>,
    notes_done: &mut u64,
) -> Result<u64, LupError> {
    let wr = |e: io::Error| LupError::io(Some(SaveStage::Write), e);
    let mut s = SectionWriter::begin(w, TAG_TRACK, FLAG_MANDATORY).map_err(wr)?;
    s.uv(u64::from(t.id().0));
    s.uv(t.parent().map_or(0, |p| u64::from(p.0) + 1));
    write_subchannels(&mut s, &t.subchannels_for_saving()).map_err(wr)?;
    write_note_block(&mut s, &t.notes, groups, control, notes_done)?;
    write_event_block(&mut s, &t.events).map_err(wr)?;
    match t.end_of_track_tick {
        Some(x) => {
            s.buf.push(1);
            s.uv(x);
        }
        None => s.buf.push(0),
    }
    write_track_props(&mut s, t).map_err(wr)?;
    s.finish().map_err(wr)
}

/// The `TRAK` sections of every track, in order. With `threads` workers, a
/// track whose estimated section fits the `window` is encoded into memory on
/// a worker and written here in order; a larger one is streamed in place.
#[allow(clippy::too_many_arguments)] // one caller; plain parts
fn write_tracks<W: Write + Seek>(
    doc: &PlumDocument,
    w: &mut W,
    groups: &crate::model::GroupTable,
    control: &LupControl<'_>,
    notes_done: &mut u64,
    stats: &mut LupWriteStats,
    threads: usize,
    window: u64,
) -> Result<(), LupError> {
    let wr = |e: io::Error| LupError::io(Some(SaveStage::Write), e);
    let tracks = doc.tracks();
    let count = |stats: &mut LupWriteStats, t: &Track| {
        stats.notes += t.notes.len() as u64;
        stats.events += t.events.len() as u64;
    };
    if threads == 0 {
        for t in tracks {
            stats.bytes += write_track_section(w, t, groups, control, notes_done)?;
            count(stats, t);
        }
        return Ok(());
    }
    let cancel = control.cancel;
    let encode = |i: usize| -> Result<Vec<u8>, LupError> {
        let mut cur = io::Cursor::new(Vec::new());
        let quiet = LupControl {
            cancel,
            progress_notes: None,
        };
        let mut done = 0u64;
        write_track_section(&mut cur, &tracks[i], groups, &quiet, &mut done)?;
        Ok(cur.into_inner())
    };
    crate::midi::workers::run(threads, window, encode, |pipe| {
        let write_oldest =
            |pipe: &mut crate::midi::workers::Pipeline<usize, Result<Vec<u8>, LupError>>,
             w: &mut W,
             stats: &mut LupWriteStats,
             notes_done: &mut u64|
             -> Result<(), LupError> {
                let Some(i) = pipe.oldest() else {
                    return Ok(());
                };
                let bytes = pipe.take(i, cancel).ok_or(LupError::Cancelled)??;
                w.write_all(&bytes).map_err(wr)?;
                stats.bytes += bytes.len() as u64;
                count(stats, &tracks[i]);
                *notes_done += tracks[i].notes.len() as u64;
                control.tick(*notes_done)
            };
        for (i, t) in tracks.iter().enumerate() {
            let est =
                t.notes.len() as u64 * 16 + t.events.len() as u64 * 12 + t.heap_bytes() as u64 / 4;
            if est <= pipe.window() {
                while !pipe.has_room(est) {
                    write_oldest(pipe, w, stats, notes_done)?;
                }
                pipe.submit(i, i, est);
            } else {
                while pipe.oldest().is_some() {
                    write_oldest(pipe, w, stats, notes_done)?;
                }
                stats.bytes += write_track_section(w, t, groups, control, notes_done)?;
                count(stats, t);
            }
        }
        while pipe.oldest().is_some() {
            write_oldest(pipe, w, stats, notes_done)?;
        }
        Ok(())
    })
}

pub fn write_project<W: Write + Seek>(
    doc: &PlumDocument,
    w: &mut W,
    control: LupControl<'_>,
) -> Result<LupWriteStats, LupError> {
    let threads = crate::midi::workers::threads_for(Some(doc.note_count() as u64 * 8));
    write_project_threads(doc, w, control, threads, crate::midi::workers::WINDOW)
}

/// [`write_project`] with `threads` workers for the track sections (0 = in
/// place) and a chunk window (B1-0163): the same bytes.
pub(crate) fn write_project_threads<W: Write + Seek>(
    doc: &PlumDocument,
    w: &mut W,
    control: LupControl<'_>,
    threads: usize,
    window: u64,
) -> Result<LupWriteStats, LupError> {
    let wr = |e: io::Error| LupError::io(Some(SaveStage::Write), e);
    let project = doc.project();
    project
        .metadata
        .validate_targets(doc)
        .map_err(|m| LupError::malformed(0, m))?;
    let has_extra = !doc.extra_chunks().is_empty();
    let has_groups = project.groups.entries().iter().any(|g| !g.temporary);
    let folders = saved_folders(doc);
    let clips = doc.clips();
    let has_clips = !clips.is_empty();
    let section_count = 2
        + doc.track_count()
        + usize::from(has_clips)
        + usize::from(has_extra)
        + 3
        + usize::from(has_groups)
        + usize::from(!project.zoom_regions.is_empty())
        + usize::from(project.track_list.is_some())
        + usize::from(project.source.is_some())
        + usize::from(project.reference.is_some())
        + usize::from(!project.plugin_data.is_empty())
        + usize::from(!project.metadata.is_empty())
        + usize::from(folders.is_some())
        + usize::from(onion_tracks_used(&project.onion))
        + project.unknown_sections.len();
    let mut header = [0u8; HEADER_SIZE as usize];
    header[..8].copy_from_slice(&LUP_MAGIC);
    // Version 7 only with clips (older PlumSpirit refuses such a file instead of
    // dropping them); otherwise exactly what v0.9 wrote before clips.
    let version = match project.keys {
        _ if has_clips => LUP_VERSION_CLIPS,
        KeyRange::Standard => LUP_VERSION,
        KeyRange::Extended256 => LUP_VERSION_256,
    };
    header[8..10].copy_from_slice(&version.to_le_bytes());
    header[10..12].copy_from_slice(&HEADER_SIZE.to_le_bytes());
    // flags (12..16) and reserved (20..28) are zero.
    let count =
        u32::try_from(section_count).map_err(|_| LupError::malformed(0, "too many sections"))?;
    header[16..20].copy_from_slice(&count.to_le_bytes());
    let crc = Crc32::of(&header[..28]);
    header[28..32].copy_from_slice(&crc.to_le_bytes());
    w.write_all(&header).map_err(wr)?;
    let mut stats = LupWriteStats {
        bytes: u64::from(HEADER_SIZE),
        tracks: doc.track_count(),
        ..Default::default()
    };

    // META
    let mut s = SectionWriter::begin(w, TAG_META, FLAG_MANDATORY).map_err(wr)?;
    s.string(project.name.as_bytes()).map_err(wr)?;
    s.string(format!("PlumSpirit {}", crate::VERSION).as_bytes())
        .map_err(wr)?;
    s.uv(u64::from(doc.source_format().word()));
    s.uv(u64::from(doc.division().encoded()));
    // Version 2: the tempo lives in the Conductor section.
    s.buf.push(0);
    s.uv(u64::from(doc.next_track_id()));
    s.uv(doc.track_count() as u64);
    s.uv(doc.extra_chunks().len() as u64);
    // Version 6: the key range (1 = 256 keys).
    if version >= LUP_VERSION_256 {
        s.uv(u64::from(project.keys == KeyRange::Extended256));
    }
    stats.bytes += s.finish().map_err(wr)?;

    // COND
    let mut s = SectionWriter::begin(w, TAG_CONDUCTOR, FLAG_MANDATORY).map_err(wr)?;
    write_conductor(&mut s, doc.conductor()).map_err(wr)?;
    stats.bytes += s.finish().map_err(wr)?;

    // TRAK × n — encoded on worker threads for a large project and written in
    // order (B1-0163): the same bytes.
    let groups = &project.groups;
    let mut notes_done = 0u64;
    write_tracks(
        doc,
        w,
        groups,
        &control,
        &mut notes_done,
        &mut stats,
        threads,
        window,
    )?;

    // CLIP (v0.9 clips; mandatory: an older PlumSpirit must not drop them)
    if has_clips {
        let mut s = SectionWriter::begin(w, TAG_CLIPS, FLAG_MANDATORY).map_err(wr)?;
        write_clips(&mut s, clips, groups, &control, &mut notes_done)?;
        for c in clips.contents() {
            stats.notes += c.notes.len() as u64;
            stats.events += c.events.len() as u64;
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // XCHK
    if has_extra {
        let mut s = SectionWriter::begin(w, TAG_EXTRA, FLAG_MANDATORY).map_err(wr)?;
        for c in doc.extra_chunks() {
            s.bytes(&c.chunk_type).map_err(wr)?;
            s.uv(c.after_track_chunks as u64);
            s.string(&c.data).map_err(wr)?;
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // ONIO
    let o = &project.onion;
    let mut s = SectionWriter::begin(w, TAG_ONION, 0).map_err(wr)?;
    s.buf.push(o.mode.code());
    s.buf.push(o.opacity);
    s.uv(o.specific.len() as u64);
    for id in &o.specific {
        s.uv(u64::from(id.0));
    }
    s.buf.push(o.order.code());
    s.buf.push(o.density.code());
    stats.bytes += s.finish().map_err(wr)?;

    // ONPT (B1-0064): only when Per-Track Settings are or were used.
    if onion_tracks_used(o) {
        let mut s = SectionWriter::begin(w, TAG_ONION_TRACKS, 0).map_err(wr)?;
        s.buf.push(u8::from(o.per_track));
        s.uv(o.track_entries.len() as u64);
        for e in &o.track_entries {
            s.uv(u64::from(e.track.0));
            s.buf.push(e.mode.code());
            s.uv(e.specific.len() as u64);
            for id in &e.specific {
                s.uv(u64::from(id.0));
            }
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // SFNT
    let mut s = SectionWriter::begin(w, TAG_SOUNDFONT, 0).map_err(wr)?;
    match &project.soundfont {
        Some(sf) => {
            s.buf.push(1);
            s.string(sf.path.as_bytes()).map_err(wr)?;
        }
        None => s.buf.push(0),
    }
    stats.bytes += s.finish().map_err(wr)?;

    // XPRT (loop and graph slots)
    let mut s = SectionWriter::begin(w, TAG_TRANSPORT, 0).map_err(wr)?;
    write_transport(&mut s, project);
    stats.bytes += s.finish().map_err(wr)?;

    // GRPS (note groups; mandatory: notes refer to them)
    if has_groups {
        let mut s = SectionWriter::begin(w, TAG_GROUPS, FLAG_MANDATORY).map_err(wr)?;
        write_groups(&mut s, &project.groups).map_err(wr)?;
        stats.bytes += s.finish().map_err(wr)?;
    }

    // ZOOM (named views; optional)
    if !project.zoom_regions.is_empty() {
        let mut s = SectionWriter::begin(w, TAG_ZOOM, 0).map_err(wr)?;
        s.uv(project.zoom_regions.len() as u64);
        for z in &project.zoom_regions {
            s.string(z.name.as_bytes()).map_err(wr)?;
            s.uv(z.origin_tick);
            s.uv(z.ppt_bits);
            s.uv(z.scroll_bits);
            s.uv(z.row_bits);
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // TLST (the Track List's view; optional)
    if let Some(v) = &project.track_list {
        let mut s = SectionWriter::begin(w, TAG_TRACK_LIST, 0).map_err(wr)?;
        s.uv(1); // layout version of the section
        s.uv(v.origin_tick);
        s.uv(v.ppt_bits);
        s.uv(u64::from(v.row_bits));
        stats.bytes += s.finish().map_err(wr)?;
    }

    // PROV (the imported MIDI file, for strict export; optional)
    if let Some(p) = &project.source {
        let mut s = SectionWriter::begin(w, TAG_SOURCE, 0).map_err(wr)?;
        s.uv(1); // layout version of the section
        s.string(p.path.as_bytes()).map_err(wr)?;
        s.uv(p.size);
        s.uv(p.hash);
        s.uv(p.fingerprint);
        stats.bytes += s.finish().map_err(wr)?;
    }

    // XMDA: API02 typed transaction metadata, optional and MIDI-independent.
    if !project.metadata.is_empty() {
        let bytes = project
            .metadata
            .encode()
            .map_err(|m| LupError::malformed(0, m))?;
        let mut s = SectionWriter::begin(w, TAG_METADATA, 0).map_err(wr)?;
        s.uv(1);
        s.string(&bytes).map_err(wr)?;
        stats.bytes += s.finish().map_err(wr)?;
    }

    // PLUG (Lua plugins' project data; optional)
    if !project.plugin_data.is_empty() {
        let mut s = SectionWriter::begin(w, TAG_PLUGIN_DATA, 0).map_err(wr)?;
        s.uv(1); // layout version of the section
        s.uv(project.plugin_data.len() as u64);
        for (id, data) in &project.plugin_data {
            s.string(id.as_bytes()).map_err(wr)?;
            s.string(data.as_bytes()).map_err(wr)?;
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // REFA (reference audio; optional)
    if let Some(r) = &project.reference {
        let mut s = SectionWriter::begin(w, TAG_REFERENCE, 0).map_err(wr)?;
        s.uv(1); // layout version of the section
        s.string(r.path.as_bytes()).map_err(wr)?;
        s.uv(((r.offset_us << 1) ^ (r.offset_us >> 63)) as u64);
        s.uv(u64::from(r.volume_pct));
        s.uv(u64::from(r.mute));
        stats.bytes += s.finish().map_err(wr)?;
    }

    // FOLD (Track Folders; optional)
    if let Some((f, collapsed)) = &folders {
        let mut s = SectionWriter::begin(w, TAG_FOLDERS, 0).map_err(wr)?;
        s.uv(1); // layout version of the section
        s.uv(u64::from(f.next_id()));
        s.uv(f.folders().len() as u64);
        for x in f.folders() {
            s.uv(u64::from(x.id));
            s.string(x.name.as_bytes()).map_err(wr)?;
            s.uv(x.anchor.map_or(0, |a| u64::from(a.0) + 1));
        }
        s.uv(f.members().len() as u64);
        for (t, id) in f.members() {
            s.uv(u64::from(t.0));
            s.uv(u64::from(*id));
        }
        s.uv(collapsed.len() as u64);
        for id in collapsed {
            s.uv(u64::from(*id));
        }
        stats.bytes += s.finish().map_err(wr)?;
    }

    // Preserved unknown optional sections, unchanged.
    for u in &project.unknown_sections {
        let mut s = SectionWriter::begin(w, u.tag, u.flags).map_err(wr)?;
        s.bytes(&u.data).map_err(wr)?;
        stats.bytes += s.finish().map_err(wr)?;
    }
    control.tick(notes_done)?;
    Ok(stats)
}

/// Serializes into memory (tests).
pub fn project_to_bytes(doc: &PlumDocument) -> Result<Vec<u8>, LupError> {
    let mut cur = io::Cursor::new(Vec::new());
    write_project(doc, &mut cur, LupControl::default())?;
    Ok(cur.into_inner())
}

// ------------------------------------------------------------------ reading

/// Reads one section payload through a bounded buffer. It can never read past
/// the section, and it computes the CRC of everything it consumed.
struct SectionReader<'a, R: Read> {
    src: &'a mut R,
    buf: &'a mut Vec<u8>,
    pos: usize,
    /// Payload bytes not yet loaded into `buf`.
    remaining: u64,
    crc: Crc32,
    /// File offset of `buf[0]`.
    base: u64,
}

impl<'a, R: Read> SectionReader<'a, R> {
    fn new(src: &'a mut R, buf: &'a mut Vec<u8>, len: u64, offset: u64) -> Self {
        buf.clear();
        SectionReader {
            src,
            buf,
            pos: 0,
            remaining: len,
            crc: Crc32::new(),
            base: offset,
        }
    }

    fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    /// Bytes of this section not yet consumed.
    fn left(&self) -> u64 {
        self.remaining + (self.buf.len() - self.pos) as u64
    }

    /// Consumes the rest of the payload in buffer-sized steps (checksummed
    /// like any read): an optional section of a newer layout.
    fn skip_rest(&mut self) -> Result<(), LupError> {
        self.pos = self.buf.len();
        while self.fill()? {
            self.pos = self.buf.len();
        }
        Ok(())
    }

    fn fill(&mut self) -> Result<bool, LupError> {
        if self.remaining == 0 {
            return Ok(false);
        }
        self.base += self.buf.len() as u64;
        let n = (READ_BUFFER as u64).min(self.remaining) as usize;
        self.buf.resize(n, 0);
        self.src.read_exact(&mut self.buf[..n]).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                LupError::Truncated { offset: self.base }
            } else {
                LupError::io(None, e)
            }
        })?;
        self.crc.update(&self.buf[..n]);
        self.remaining -= n as u64;
        self.pos = 0;
        Ok(true)
    }

    #[inline]
    fn u8(&mut self) -> Result<u8, LupError> {
        if self.pos == self.buf.len() && !self.fill()? {
            return Err(LupError::malformed(
                self.offset(),
                "a section ends too early",
            ));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn uv(&mut self) -> Result<u64, LupError> {
        if self.buf.len() - self.pos >= 10 {
            let mut p = self.pos;
            let v = crate::codec::get_uvarint(self.buf, &mut p)
                .ok_or_else(|| LupError::malformed(self.offset(), "invalid number"))?;
            self.pos = p;
            return Ok(v);
        }
        let mut v: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            let low = u64::from(b & 0x7F);
            if shift == 63 && low > 1 {
                return Err(LupError::malformed(self.offset(), "number too large"));
            }
            v |= low << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(LupError::malformed(self.offset(), "number too long"));
            }
        }
    }

    fn iv(&mut self) -> Result<i64, LupError> {
        let u = self.uv()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }

    fn u32(&mut self) -> Result<u32, LupError> {
        let mut b = [0u8; 4];
        for x in &mut b {
            *x = self.u8()?;
        }
        Ok(u32::from_le_bytes(b))
    }

    /// A count of items that each need at least `min_bytes`; rejected when the
    /// section cannot possibly hold that many (no allocation is sized from it
    /// otherwise).
    fn count(&mut self, min_bytes: u64, what: &str) -> Result<u64, LupError> {
        let at = self.offset();
        let n = self.uv()?;
        if n.saturating_mul(min_bytes) > self.left() {
            return Err(LupError::malformed(
                at,
                format!("{what} count exceeds the section"),
            ));
        }
        Ok(n)
    }

    /// Reads `len` bytes into `out` (cleared first); `len` is checked against the
    /// section before allocating.
    fn bytes_into(&mut self, len: u64, out: &mut Vec<u8>) -> Result<(), LupError> {
        out.clear();
        if len > self.left() {
            return Err(LupError::malformed(
                self.offset(),
                "a length exceeds the section",
            ));
        }
        let len = usize::try_from(len).map_err(|_| LupError::OutOfMemory)?;
        out.try_reserve_exact(len)
            .map_err(|_| LupError::OutOfMemory)?;
        while out.len() < len {
            if self.pos == self.buf.len() && !self.fill()? {
                return Err(LupError::malformed(
                    self.offset(),
                    "a section ends too early",
                ));
            }
            let take = (len - out.len()).min(self.buf.len() - self.pos);
            out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
        }
        Ok(())
    }

    fn string(&mut self, out: &mut Vec<u8>) -> Result<(), LupError> {
        let len = self.uv()?;
        self.bytes_into(len, out)
    }

    /// Requires the whole payload to be consumed, then checks the stored checksum
    /// (see [`section_checksum`]).
    fn finish(self, tag: [u8; 4], flags: u32, len: u64, crc_offset: u64) -> Result<(), LupError> {
        if self.left() != 0 {
            return Err(LupError::malformed(
                self.offset(),
                format!("unexpected extra data in section {}", tag_name(tag)),
            ));
        }
        let mut b = [0u8; 4];
        read_exact_at(self.src, &mut b, crc_offset)?;
        if u32::from_le_bytes(b) != section_checksum(tag, flags, len, self.crc.finish()) {
            return Err(LupError::Corrupt {
                section: section_label(tag),
                offset: crc_offset,
            });
        }
        Ok(())
    }
}

fn read_exact_at<R: Read>(r: &mut R, b: &mut [u8], offset: u64) -> Result<(), LupError> {
    r.read_exact(b).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            LupError::Truncated { offset }
        } else {
            LupError::io(None, e)
        }
    })
}

fn tag_name(tag: [u8; 4]) -> String {
    tag.iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

fn section_label(tag: [u8; 4]) -> String {
    match tag {
        TAG_META => "project information".into(),
        TAG_TRACK => "track".into(),
        TAG_EXTRA => "extra MIDI chunk".into(),
        TAG_ONION => "Onion Skin".into(),
        TAG_SOUNDFONT => "SoundFont".into(),
        TAG_CONDUCTOR => "Conductor".into(),
        TAG_TRANSPORT => "loop and graph settings".into(),
        TAG_ZOOM => "named zoom regions".into(),
        TAG_REFERENCE => "reference audio".into(),
        TAG_FOLDERS => "track folders".into(),
        TAG_CLIPS => "clip".into(),
        TAG_TRACK_LIST => "Track List view".into(),
        TAG_SOURCE => "MIDI source".into(),
        TAG_PLUGIN_DATA => "plugin data".into(),
        TAG_METADATA => "typed metadata".into(),
        TAG_ONION_TRACKS => "per-track Onion Skin settings".into(),
        other => format!("\"{}\"", tag_name(other)),
    }
}

/// Project-level fields from the META section.
#[derive(Clone, Debug)]
struct Meta {
    name: String,
    /// The project's key range (version 6; standard before).
    keys: KeyRange,
    format: SmfFormat,
    division: Division,
    tempo: Option<Bpm>,
    next_track_id: u32,
    track_count: usize,
    extra_count: usize,
}

/// Receives parsed content. Implemented by the loader (builds a document) and by
/// the post-save verifier (compares with the document in memory).
trait Sink {
    fn meta(&mut self, m: &Meta, at: u64) -> Result<(), LupError>;
    /// `node`: parent and subchannels (`None` for files before version 4).
    fn begin_track(
        &mut self,
        index: usize,
        id: TrackId,
        node: Option<&ParsedNode>,
        notes: u64,
        at: u64,
    ) -> Result<(), LupError>;
    fn notes(
        &mut self,
        batch: &[NoteRecord],
        remaining_after: u64,
        at: u64,
    ) -> Result<(), LupError>;
    fn begin_events(&mut self, count: u64) -> Result<(), LupError>;
    fn event(&mut self, ev: ParsedEvent<'_>, at: u64) -> Result<(), LupError>;
    /// `props`: `None` for files before version 3.
    fn end_track(
        &mut self,
        eot: Option<u64>,
        props: Option<ParsedProps>,
        at: u64,
    ) -> Result<(), LupError>;
    fn extra_chunk(&mut self, chunk: ExtraChunk, at: u64) -> Result<(), LupError>;
    fn onion(&mut self, o: OnionConfig, at: u64) -> Result<(), LupError>;
    /// B1-0064: the ONPT section (after ONIO).
    fn onion_tracks(
        &mut self,
        per_track: bool,
        entries: Vec<OnionTrackEntry>,
        at: u64,
    ) -> Result<(), LupError>;
    fn soundfont(&mut self, sf: Option<SoundFontRef>, at: u64) -> Result<(), LupError>;
    fn conductor(&mut self, c: Conductor, at: u64) -> Result<(), LupError>;
    fn transport(&mut self, l: LoopConfig, g: GraphConfig, at: u64) -> Result<(), LupError>;
    fn groups(&mut self, t: crate::model::GroupTable, at: u64) -> Result<(), LupError>;
    fn zoom(&mut self, v: Vec<crate::model::project::ZoomRegion>, at: u64) -> Result<(), LupError>;
    fn track_list(
        &mut self,
        v: crate::model::project::TrackListView,
        at: u64,
    ) -> Result<(), LupError>;
    fn reference(
        &mut self,
        r: crate::model::project::ReferenceAudio,
        at: u64,
    ) -> Result<(), LupError>;
    fn source(
        &mut self,
        p: crate::model::project::SourceProvenance,
        at: u64,
    ) -> Result<(), LupError>;
    fn folders(
        &mut self,
        f: crate::model::TrackFolders,
        collapsed: std::collections::BTreeSet<u32>,
        at: u64,
    ) -> Result<(), LupError>;
    fn unknown(&mut self, u: UnknownSection, at: u64) -> Result<(), LupError>;
    /// v1.0-beta1 `PLUG`: (plugin id, data) sorted by id.
    fn plugin_data(&mut self, v: Vec<(String, String)>, at: u64) -> Result<(), LupError>;
    fn metadata(&mut self, v: crate::model::metadata::Metadata, at: u64) -> Result<(), LupError>;
    /// v0.9 `CLIP`: the placement records; the owned contents follow, each
    /// as `begin_content`, `notes` / `begin_events` / `event`, `end_content`.
    fn clips(&mut self, head: ParsedClips, at: u64) -> Result<(), LupError>;
    fn begin_content(&mut self, c: ParsedContent, notes: u64, at: u64) -> Result<(), LupError>;
    fn end_content(&mut self, at: u64) -> Result<(), LupError>;
    fn end_clips(&mut self, contents: u64, at: u64) -> Result<(), LupError>;
}

/// The records of a `CLIP` section before its contents.
#[derive(Clone, Debug)]
struct ParsedClips {
    next_clip: u32,
    next_content: u32,
    placements: Vec<crate::model::ClipPlacement>,
    removed: Vec<TrackId>,
}

/// An owned clip content's header in a `CLIP` section.
#[derive(Clone, Debug)]
struct ParsedContent {
    id: crate::model::ContentId,
    name: Option<String>,
    color: Option<u32>,
}

/// Placements compared without their in-memory revision counters.
fn same_placements(a: &[crate::model::ClipPlacement], b: &[crate::model::ClipPlacement]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            crate::model::ClipPlacement {
                revision: 0,
                ..x.clone()
            } == crate::model::ClipPlacement {
                revision: 0,
                ..y.clone()
            }
        })
}

/// Hierarchy and subchannels read from a version 4 `TRAK` section.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedNode {
    parent: Option<TrackId>,
    subs: SubchannelTable,
}

/// Track properties read from a version 3 `TRAK` section.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedProps {
    props: TrackProps,
    mute: bool,
    solo: bool,
}

#[derive(Clone, Copy, Debug)]
struct ParsedEvent<'p> {
    tick: u64,
    anchor: u32,
    kind: u8,
    a: u8,
    data: [u8; 2],
    payload: &'p [u8],
}

/// What loading found besides the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LupLoadReport {
    pub bytes: u64,
    pub notes: u64,
    pub events: u64,
    pub version: u16,
    /// `Some(version)` when an older format was converted in memory.
    pub migrated_from: Option<u16>,
    /// Human-readable notes (e.g. unknown optional sections kept or dropped).
    pub warnings: Vec<String>,
    pub duration: Duration,
}

/// v1.0-beta1 (B1-0163): a sink whose `TRAK` sections can be parsed on worker
/// threads. A worker parses one section into a fresh [`SplitSink::Part`]; the
/// result is handed back in file order to [`SplitSink::adopt`].
trait SplitSink: Sink {
    type Part: Sink + Send;
    type Out: Send;
    /// A fresh sink for track `index`.
    fn part(&self, index: usize) -> Self::Part;
    /// The id and offset the part's track began with (`begin_track` reached).
    fn begun(part: &Self::Part) -> Option<(TrackId, u64)>;
    fn finish_part(part: Self::Part) -> Self::Out;
    /// The checks of `begin_track` that need the tracks before it, made when
    /// the track is taken back (in file order).
    fn check_begun(&mut self, id: TrackId, at: u64) -> Result<(), LupError>;
    fn adopt(&mut self, out: Self::Out) -> Result<(), LupError>;
}

impl SplitSink for Builder {
    type Part = Builder;
    type Out = Track;

    fn part(&self, _index: usize) -> Builder {
        Builder {
            meta: self.meta.clone(),
            tracks: Vec::with_capacity(1),
            begun: None,
            extra: Vec::new(),
            project: ProjectInfo {
                keys: self.project.keys,
                ..ProjectInfo::default()
            },
            conductor: None,
            cur_id: TrackId(0),
            cur_node: None,
            cur_notes: NoteStore::new(),
            cur_events: EventStore::new(),
            clips: None,
            contents: Vec::new(),
            cur_content: None,
        }
    }

    fn begun(part: &Builder) -> Option<(TrackId, u64)> {
        part.begun
    }

    fn finish_part(mut part: Builder) -> Track {
        part.tracks.pop().expect("a parsed track")
    }

    fn check_begun(&mut self, id: TrackId, at: u64) -> Result<(), LupError> {
        if self.tracks.iter().any(|t| t.id() == id) {
            return Err(LupError::malformed(at, "two tracks share an id"));
        }
        Ok(())
    }

    fn adopt(&mut self, t: Track) -> Result<(), LupError> {
        self.tracks
            .try_reserve(1)
            .map_err(|_| LupError::OutOfMemory)?;
        self.tracks.push(t);
        Ok(())
    }
}

impl<'d> SplitSink for Verifier<'d> {
    type Part = Verifier<'d>;
    type Out = ();

    fn part(&self, _index: usize) -> Verifier<'d> {
        Verifier::new(self.doc)
    }

    fn begun(_part: &Verifier<'d>) -> Option<(TrackId, u64)> {
        None
    }

    fn finish_part(_part: Verifier<'d>) {}

    fn check_begun(&mut self, _id: TrackId, _at: u64) -> Result<(), LupError> {
        Ok(())
    }

    fn adopt(&mut self, (): ()) -> Result<(), LupError> {
        Ok(())
    }
}

/// One `TRAK` section handed to a worker: its payload and checksum bytes.
struct TrackJob<P> {
    index: usize,
    payload_at: u64,
    len: u64,
    flags: u32,
    version: u16,
    keys: KeyRange,
    data: Vec<u8>,
    part: P,
}

/// What a worker's parse of one section adds to the parser's counters.
struct TrackCounts {
    notes: u64,
    events: u64,
    groups: std::collections::HashSet<u32>,
}

/// A worker's parse of one section: the result with its counters, or the
/// error; either way the id the track began with (for the in-order checks).
type TrackResult<O> = (Result<(O, TrackCounts), LupError>, Option<(TrackId, u64)>);

/// Parses one deferred `TRAK` section on a worker, checksum included, exactly
/// as [`Parser::run`] parses it in place.
fn parse_track_job<S: SplitSink>(
    job: TrackJob<S::Part>,
    file_len: Option<u64>,
    cancel: Option<&AtomicBool>,
) -> TrackResult<S::Out> {
    let mut parser = Parser {
        control: LupControl {
            cancel,
            progress_notes: None,
        },
        keys: job.keys,
        file_len,
        notes_seen: 0,
        report: LupLoadReport {
            version: job.version,
            ..LupLoadReport::default()
        },
        preserved_total: 0,
        note_groups: Default::default(),
        taken_back_failed: false,
    };
    let mut part = job.part;
    let mut src: &[u8] = &job.data;
    let mut buf = Vec::new();
    let mut scratch = Vec::new();
    let mut sr = SectionReader::new(&mut src, &mut buf, job.len, job.payload_at);
    let parsed = parser
        .parse_track(&mut sr, &mut part, job.index, &mut scratch)
        .and_then(|()| sr.finish(TAG_TRACK, job.flags, job.len, job.payload_at + job.len));
    let begun = S::begun(&part);
    let r = parsed.map(|()| {
        (
            S::finish_part(part),
            TrackCounts {
                notes: parser.report.notes,
                events: parser.report.events,
                groups: parser.note_groups,
            },
        )
    });
    (r, begun)
}

type TrackPipe<S> = crate::midi::workers::Pipeline<
    TrackJob<<S as SplitSink>::Part>,
    TrackResult<<S as SplitSink>::Out>,
>;

struct Parser<'c> {
    control: LupControl<'c>,
    /// The project's key range (from the project information).
    keys: KeyRange,
    file_len: Option<u64>,
    notes_seen: u64,
    report: LupLoadReport,
    preserved_total: u64,
    /// Distinct group ids the notes use (checked against `GRPS` at the end).
    note_groups: std::collections::HashSet<u32>,
    /// A track taken back from a worker failed (B1-0163: tracks still with
    /// the workers come after it, so its error stands).
    taken_back_failed: bool,
}

fn valid_channel_status(s: u8) -> bool {
    (0x80..=0xEF).contains(&s)
}

impl Parser<'_> {
    /// Reads every section into `sink`. With `threads` workers (and a known
    /// file length) `TRAK` sections whose payload fits the `window` are parsed
    /// on the workers and handed to the sink in file order (B1-0163); the
    /// result and the first error are the same as parsing in place.
    fn run<R: Read, S: SplitSink>(
        &mut self,
        r: &mut R,
        sink: &mut S,
        threads: usize,
        window: u64,
    ) -> Result<(), LupError> {
        if threads == 0 || self.file_len.is_none() {
            return self.run_inner(r, sink, None);
        }
        let file_len = self.file_len;
        let cancel = self.control.cancel;
        let work = move |job: TrackJob<S::Part>| parse_track_job::<S>(job, file_len, cancel);
        crate::midi::workers::run(threads, window, work, |pipe| {
            match self.run_inner(r, sink, Some(pipe)) {
                Ok(()) => Ok(()),
                Err(e) if self.taken_back_failed => Err(e),
                // An earlier track still with the workers fails first.
                Err(e) => {
                    // (Only pending tracks remain: every earlier one was taken.)
                    while let Some(i) = pipe.oldest() {
                        if let Some((r, begun)) = pipe.take(i, None) {
                            if let Some((id, at)) = begun {
                                sink.check_begun(id, at)?;
                            }
                            if let Err(early) = r {
                                return Err(early);
                            }
                            if let Ok((out, _)) = r {
                                sink.adopt(out)?;
                            }
                        }
                    }
                    Err(e)
                }
            }
        })
    }

    /// Takes back every track still with the workers, in order.
    fn drain<S: SplitSink>(
        &mut self,
        sink: &mut S,
        pipe: &mut Option<&mut TrackPipe<S>>,
    ) -> Result<(), LupError> {
        let Some(p) = pipe.as_mut() else {
            return Ok(());
        };
        while let Some(i) = p.oldest() {
            let r = p.take(i, self.control.cancel).ok_or(LupError::Cancelled)?;
            self.take_back(sink, r)?;
        }
        Ok(())
    }

    /// Hands a track parsed on a worker to the sink (in file order).
    fn take_back<S: SplitSink>(
        &mut self,
        sink: &mut S,
        (r, begun): TrackResult<S::Out>,
    ) -> Result<(), LupError> {
        let checked = (|| {
            // The id check comes before anything the track's notes could fail.
            if let Some((id, at)) = begun {
                sink.check_begun(id, at)?;
            }
            let (out, counts) = r?;
            sink.adopt(out)?;
            Ok(counts)
        })();
        let counts = checked.inspect_err(|_| self.taken_back_failed = true)?;
        self.report.notes += counts.notes;
        self.report.events += counts.events;
        self.note_groups.extend(counts.groups);
        self.notes_seen += counts.notes;
        self.control.tick(self.notes_seen)
    }

    fn run_inner<R: Read, S: SplitSink>(
        &mut self,
        r: &mut R,
        sink: &mut S,
        mut pipe: Option<&mut TrackPipe<S>>,
    ) -> Result<(), LupError> {
        let mut header = [0u8; HEADER_SIZE as usize];
        // Magic first, so non-projects get a clear message even when short.
        let mut magic = [0u8; 8];
        let mut got = 0;
        while got < 8 {
            match r.read(&mut magic[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(LupError::io(None, e)),
            }
        }
        if got < 8 || magic != LUP_MAGIC {
            return Err(LupError::NotAProject);
        }
        header[..8].copy_from_slice(&magic);
        read_exact_at(r, &mut header[8..], 8)?;
        let version = u16::from_le_bytes([header[8], header[9]]);
        // Migration boundary: version 1 (v0.2, no Conductor section) is converted
        // in memory; version 2 must contain the Conductor.
        match version {
            1..=LUP_VERSION_MAX => {}
            v if v > LUP_VERSION_MAX => return Err(LupError::NewerVersion(v)),
            _ => return Err(LupError::malformed(8, "invalid format version 0")),
        }
        self.report.version = version;
        let stored_crc = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
        if Crc32::of(&header[..28]) != stored_crc {
            return Err(LupError::Corrupt {
                section: "header".into(),
                offset: 28,
            });
        }
        let header_size = u16::from_le_bytes([header[10], header[11]]);
        let flags = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
        if header_size != HEADER_SIZE || flags != 0 {
            return Err(LupError::malformed(10, "unsupported header layout"));
        }
        let sections = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
        let mut offset = u64::from(HEADER_SIZE);
        let mut buf: Vec<u8> = Vec::new();
        let mut scratch: Vec<u8> = Vec::new();
        let mut meta: Option<Meta> = None;
        let mut tracks_read = 0usize;
        let (mut saw_extra, mut saw_onion, mut saw_sf) = (false, false, false);
        let mut saw_onion_tracks = false;
        let mut saw_conductor = false;
        let mut saw_transport = false;
        let mut saw_groups = false;
        let mut saw_zoom = false;
        let mut saw_track_list = false;
        let mut saw_source = false;
        let mut saw_reference = false;
        let mut saw_plugin_data = false;
        let mut saw_metadata = false;
        let mut saw_folders = false;
        let mut saw_clips = false;
        for _ in 0..sections {
            let mut sh = [0u8; 16];
            read_exact_at(r, &mut sh, offset)?;
            let tag = [sh[0], sh[1], sh[2], sh[3]];
            let sflags = u32::from_le_bytes([sh[4], sh[5], sh[6], sh[7]]);
            let len =
                u64::from_le_bytes([sh[8], sh[9], sh[10], sh[11], sh[12], sh[13], sh[14], sh[15]]);
            let payload_at = offset + 16;
            if sflags & !FLAG_MANDATORY != 0 {
                return Err(LupError::malformed(offset + 4, "unknown section flags"));
            }
            let end = payload_at
                .checked_add(len)
                .and_then(|e| e.checked_add(4))
                .ok_or_else(|| LupError::malformed(offset + 8, "section length overflows"))?;
            if let Some(fl) = self.file_len
                && end > fl
            {
                return Err(LupError::Truncated { offset: fl });
            }
            let deferrable = tag == TAG_TRACK
                && meta.as_ref().is_some_and(|m| tracks_read < m.track_count)
                && pipe
                    .as_ref()
                    .is_some_and(|p| len.saturating_add(4) <= p.window());
            if deferrable {
                // The payload and its checksum go to a worker; the sink gets
                // the track back in order.
                let p = pipe.as_mut().expect("deferrable");
                let cost = len + 4;
                while !p.has_room(cost) {
                    let i = p.oldest().expect("room is made by taking jobs back");
                    let r = p.take(i, self.control.cancel).ok_or(LupError::Cancelled)?;
                    self.take_back(sink, r)?;
                }
                let mut data = Vec::new();
                data.try_reserve_exact(cost as usize)
                    .map_err(|_| LupError::OutOfMemory)?;
                data.resize(cost as usize, 0);
                read_exact_at(r, &mut data, payload_at)?;
                let part = sink.part(tracks_read);
                p.submit(
                    tracks_read,
                    TrackJob {
                        index: tracks_read,
                        payload_at,
                        len,
                        flags: sflags,
                        version,
                        keys: self.keys,
                        data,
                        part,
                    },
                    cost,
                );
                tracks_read += 1;
                offset = end;
                continue;
            }
            // Everything else sees the tracks before it complete.
            self.drain(sink, &mut pipe)?;
            let mut sr = SectionReader::new(r, &mut buf, len, payload_at);
            let first = meta.is_none();
            match tag {
                TAG_META => {
                    if !first {
                        return Err(LupError::malformed(
                            offset,
                            "project information appears twice",
                        ));
                    }
                    let m = parse_meta(&mut sr, &mut scratch, version)?;
                    self.keys = m.keys;
                    if version >= 2 && m.tempo.is_some() {
                        return Err(LupError::malformed(
                            payload_at,
                            "a version 2 project keeps its tempo in the Conductor",
                        ));
                    }
                    sink.meta(&m, payload_at)?;
                    meta = Some(m);
                }
                _ if first => {
                    return Err(LupError::malformed(
                        offset,
                        "project information must come first",
                    ));
                }
                TAG_TRACK => {
                    let m = meta.as_ref().expect("checked");
                    if tracks_read >= m.track_count {
                        return Err(LupError::malformed(offset, "more tracks than declared"));
                    }
                    self.parse_track(&mut sr, sink, tracks_read, &mut scratch)?;
                    tracks_read += 1;
                }
                TAG_CLIPS if version >= LUP_VERSION_CLIPS => {
                    let m = meta.as_ref().expect("checked");
                    if saw_clips {
                        return Err(LupError::malformed(offset, "the clips appear twice"));
                    }
                    if tracks_read != m.track_count {
                        return Err(LupError::malformed(
                            offset,
                            "the clips must follow the tracks",
                        ));
                    }
                    saw_clips = true;
                    self.parse_clips(&mut sr, sink, &mut scratch)?;
                }
                TAG_EXTRA => {
                    let m = meta.as_ref().expect("checked");
                    if saw_extra {
                        return Err(LupError::malformed(offset, "extra chunks appear twice"));
                    }
                    saw_extra = true;
                    for _ in 0..m.extra_count {
                        let at = sr.offset();
                        let mut ty = [0u8; 4];
                        for b in &mut ty {
                            *b = sr.u8()?;
                        }
                        let after = usize::try_from(sr.uv()?)
                            .map_err(|_| LupError::malformed(at, "invalid chunk position"))?;
                        let mut data = Vec::new();
                        sr.string(&mut data)?;
                        sink.extra_chunk(
                            ExtraChunk {
                                chunk_type: ty,
                                data,
                                after_track_chunks: after.min(m.track_count),
                            },
                            at,
                        )?;
                    }
                }
                TAG_ONION => {
                    if saw_onion {
                        return Err(LupError::malformed(
                            offset,
                            "Onion Skin settings appear twice",
                        ));
                    }
                    saw_onion = true;
                    let at = sr.offset();
                    let mode = OnionMode::from_code(sr.u8()?)
                        .ok_or_else(|| LupError::malformed(at, "unknown Onion Skin mode"))?;
                    let opacity = sr.u8()?;
                    if !(MIN_ONION_OPACITY..=MAX_ONION_OPACITY).contains(&opacity) {
                        return Err(LupError::malformed(
                            at + 1,
                            "Onion Skin opacity out of range",
                        ));
                    }
                    let n = sr.count(1, "Onion Skin track")?;
                    let mut specific = Vec::new();
                    for _ in 0..n {
                        let id = u32::try_from(sr.uv()?)
                            .map_err(|_| LupError::malformed(sr.offset(), "invalid track id"))?;
                        specific.push(TrackId(id));
                    }
                    let (order, density) = if version >= 4 {
                        let oat = sr.offset();
                        let order = OnionOrder::from_code(sr.u8()?).ok_or_else(|| {
                            LupError::malformed(oat, "unknown Onion Skin layer order")
                        })?;
                        let density = DensityBasis::from_code(sr.u8()?).ok_or_else(|| {
                            LupError::malformed(oat + 1, "unknown Onion Skin density measure")
                        })?;
                        (order, density)
                    } else {
                        (OnionOrder::Project, DensityBasis::VisibleRange)
                    };
                    sink.onion(
                        OnionConfig {
                            mode,
                            order,
                            density,
                            specific,
                            opacity,
                            per_track: false,
                            track_entries: Vec::new(),
                        },
                        at,
                    )?;
                }
                TAG_ONION_TRACKS => {
                    if saw_onion_tracks {
                        return Err(LupError::malformed(
                            offset,
                            "per-track Onion Skin settings appear twice",
                        ));
                    }
                    saw_onion_tracks = true;
                    let at = sr.offset();
                    let per_track = match sr.u8()? {
                        0 => false,
                        1 => true,
                        _ => return Err(LupError::malformed(at, "invalid per-track Onion flag")),
                    };
                    let n = sr.count(3, "per-track Onion entry")?;
                    let mut entries: Vec<OnionTrackEntry> = Vec::new();
                    for _ in 0..n {
                        let eat = sr.offset();
                        let track = TrackId(
                            u32::try_from(sr.uv()?)
                                .map_err(|_| LupError::malformed(eat, "invalid track id"))?,
                        );
                        let mat = sr.offset();
                        let mode = OnionMode::from_code(sr.u8()?)
                            .ok_or_else(|| LupError::malformed(mat, "unknown Onion Skin mode"))?;
                        let k = sr.count(1, "Onion Skin track")?;
                        let mut specific = Vec::new();
                        for _ in 0..k {
                            let id = u32::try_from(sr.uv()?).map_err(|_| {
                                LupError::malformed(sr.offset(), "invalid track id")
                            })?;
                            specific.push(TrackId(id));
                        }
                        if entries.last().is_some_and(|l| l.track >= track) {
                            return Err(LupError::malformed(
                                eat,
                                "per-track Onion entries out of order",
                            ));
                        }
                        entries.push(OnionTrackEntry {
                            track,
                            mode,
                            specific,
                        });
                    }
                    sink.onion_tracks(per_track, entries, at)?;
                }
                TAG_CONDUCTOR if version >= 2 => {
                    if saw_conductor {
                        return Err(LupError::malformed(offset, "the Conductor appears twice"));
                    }
                    saw_conductor = true;
                    let at = sr.offset();
                    let c = parse_conductor(&mut sr, &mut scratch, version)?;
                    sink.conductor(c, at)?;
                }
                TAG_TRANSPORT if version >= 3 => {
                    if saw_transport {
                        return Err(LupError::malformed(
                            offset,
                            "transport settings appear twice",
                        ));
                    }
                    saw_transport = true;
                    let at = sr.offset();
                    let (l, g) = parse_transport(&mut sr)?;
                    sink.transport(l, g, at)?;
                }
                TAG_GROUPS if version >= 5 => {
                    if saw_groups {
                        return Err(LupError::malformed(offset, "note groups appear twice"));
                    }
                    saw_groups = true;
                    let at = sr.offset();
                    let t = parse_groups(&mut sr, &mut scratch)?;
                    sink.groups(t, at)?;
                }
                TAG_SOURCE if version >= 5 => {
                    let at = sr.offset();
                    if saw_source {
                        return Err(LupError::malformed(at, "duplicate MIDI source section"));
                    }
                    saw_source = true;
                    if sr.uv()? == 1 {
                        sr.string(&mut scratch)?;
                        let path = String::from_utf8(scratch.clone()).map_err(|_| {
                            LupError::malformed(at, "the MIDI source path is not text")
                        })?;
                        let p = crate::model::project::SourceProvenance {
                            path,
                            size: sr.uv()?,
                            hash: sr.uv()?,
                            fingerprint: sr.uv()?,
                        };
                        sink.source(p, at)?;
                    } else {
                        // A newer layout: this version offers no strict export.
                        sr.skip_rest()?;
                    }
                }
                TAG_METADATA => {
                    let at = sr.offset();
                    if saw_metadata {
                        return Err(LupError::malformed(at, "duplicate typed metadata"));
                    }
                    saw_metadata = true;
                    if sr.uv()? != 1 {
                        return Err(LupError::malformed(at, "unsupported typed metadata layout"));
                    }
                    let n = sr.uv()?;
                    if n > crate::model::metadata::MAX_BYTES as u64 {
                        return Err(LupError::malformed(at, "typed metadata exceeds1MiB"));
                    }
                    sr.bytes_into(n, &mut scratch)?;
                    let v = crate::model::metadata::Metadata::decode(&scratch)
                        .map_err(|m| LupError::malformed(at, m))?;
                    if v.is_empty() {
                        return Err(LupError::malformed(at, "empty typed metadata section"));
                    }
                    sink.metadata(v, at)?;
                }
                TAG_PLUGIN_DATA if version >= 5 => {
                    let at = sr.offset();
                    if saw_plugin_data {
                        return Err(LupError::malformed(at, "duplicate plugin data section"));
                    }
                    saw_plugin_data = true;
                    if sr.uv()? != 1 {
                        // A newer layout: skipped (the data is not used).
                        sr.skip_rest()?;
                    } else {
                        let n = sr.uv()?;
                        if n == 0 || n > MAX_PLUGIN_DATA_ENTRIES as u64 {
                            return Err(LupError::malformed(at, "invalid plugin data count"));
                        }
                        let mut v: Vec<(String, String)> = Vec::with_capacity(n as usize);
                        for _ in 0..n {
                            sr.string(&mut scratch)?;
                            let id = String::from_utf8(scratch.clone())
                                .map_err(|_| LupError::malformed(at, "a plugin id is not text"))?;
                            let ok_id = (3..=64).contains(&id.len())
                                && id.bytes().all(|b| {
                                    b.is_ascii_lowercase()
                                        || b.is_ascii_digit()
                                        || b == b'.'
                                        || b == b'_'
                                        || b == b'-'
                                });
                            sr.string(&mut scratch)?;
                            if !ok_id || scratch.len() > MAX_PLUGIN_DATA_BYTES {
                                return Err(LupError::malformed(at, "invalid plugin data"));
                            }
                            let data = String::from_utf8(scratch.clone())
                                .map_err(|_| LupError::malformed(at, "plugin data is not text"))?;
                            if v.last().is_some_and(|(last, _)| *last >= id) {
                                return Err(LupError::malformed(at, "plugin data is not sorted"));
                            }
                            v.push((id, data));
                        }
                        sink.plugin_data(v, at)?;
                    }
                }
                TAG_REFERENCE if version >= 5 => {
                    let at = sr.offset();
                    if saw_reference {
                        return Err(LupError::malformed(at, "duplicate reference audio section"));
                    }
                    saw_reference = true;
                    if sr.uv()? != 1 {
                        return Err(LupError::malformed(at, "unknown reference audio layout"));
                    }
                    sr.string(&mut scratch)?;
                    let path = String::from_utf8(scratch.clone()).map_err(|_| {
                        LupError::malformed(at, "the reference audio path is not text")
                    })?;
                    let offset_us = sr.iv()?;
                    let volume = sr.uv()?;
                    let mute = sr.uv()?;
                    let r = crate::model::project::ReferenceAudio {
                        path,
                        offset_us,
                        volume_pct: u16::try_from(volume).unwrap_or(u16::MAX),
                        mute: mute == 1,
                    };
                    if !r.is_valid() || mute > 1 {
                        return Err(LupError::malformed(at, "invalid reference audio settings"));
                    }
                    sink.reference(r, at)?;
                }
                TAG_FOLDERS if version >= 5 => {
                    let at = sr.offset();
                    if saw_folders {
                        return Err(LupError::malformed(at, "duplicate track folders section"));
                    }
                    saw_folders = true;
                    let bad =
                        |m: &str| LupError::malformed(at, format!("invalid track folders ({m})"));
                    if sr.uv()? != 1 {
                        return Err(bad("unknown layout"));
                    }
                    let next = u32::try_from(sr.uv()?).map_err(|_| bad("next id"))?;
                    let n = sr.uv()?;
                    if n > crate::model::folder::MAX_FOLDERS as u64 {
                        return Err(bad("too many folders"));
                    }
                    let mut folders = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        let id = u32::try_from(sr.uv()?).map_err(|_| bad("id"))?;
                        sr.string(&mut scratch)?;
                        let name = String::from_utf8(scratch.clone()).map_err(|_| bad("name"))?;
                        let anchor = match sr.uv()? {
                            0 => None,
                            a => Some(crate::model::TrackId(
                                u32::try_from(a - 1).map_err(|_| bad("anchor"))?,
                            )),
                        };
                        folders.push(crate::model::FolderInfo { id, name, anchor });
                    }
                    let m = sr.uv()?;
                    if m > sr.left() / 2 {
                        return Err(bad("too many members"));
                    }
                    let mut members = Vec::with_capacity(m as usize);
                    for _ in 0..m {
                        let t = u32::try_from(sr.uv()?).map_err(|_| bad("track id"))?;
                        let id = u32::try_from(sr.uv()?).map_err(|_| bad("folder id"))?;
                        members.push((crate::model::TrackId(t), id));
                    }
                    let c = sr.uv()?;
                    if c > n {
                        return Err(bad("collapse state"));
                    }
                    let mut collapsed = std::collections::BTreeSet::new();
                    for _ in 0..c {
                        collapsed.insert(u32::try_from(sr.uv()?).map_err(|_| bad("collapsed id"))?);
                    }
                    let f = crate::model::TrackFolders::from_parts(folders, members, next)
                        .map_err(|m| bad(&m))?;
                    if collapsed.iter().any(|&id| f.get(id).is_none()) {
                        return Err(bad("collapse state"));
                    }
                    sink.folders(f, collapsed, at)?;
                }
                TAG_ZOOM if version >= 5 => {
                    let at = sr.offset();
                    if saw_zoom {
                        return Err(LupError::malformed(at, "duplicate zoom regions section"));
                    }
                    saw_zoom = true;
                    let n = sr.uv()?;
                    if n > crate::model::project::MAX_ZOOM_REGIONS as u64 {
                        return Err(LupError::malformed(at, "too many zoom regions"));
                    }
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        sr.string(&mut scratch)?;
                        let name = String::from_utf8(scratch.clone()).map_err(|_| {
                            LupError::malformed(at, "a zoom region name is not text")
                        })?;
                        let z = crate::model::project::ZoomRegion {
                            name,
                            origin_tick: sr.uv()?,
                            ppt_bits: sr.uv()?,
                            scroll_bits: sr.uv()?,
                            row_bits: sr.uv()?,
                        };
                        if !z.is_valid()
                            || z.name.chars().count() > crate::model::project::MAX_ZOOM_NAME_CHARS
                        {
                            return Err(LupError::malformed(at, "invalid zoom region"));
                        }
                        v.push(z);
                    }
                    sink.zoom(v, at)?;
                }
                TAG_TRACK_LIST if version >= 5 => {
                    let at = sr.offset();
                    if saw_track_list {
                        return Err(LupError::malformed(at, "duplicate Track List view section"));
                    }
                    saw_track_list = true;
                    if sr.uv()? == 1 {
                        let origin_tick = sr.uv()?;
                        let ppt_bits = sr.uv()?;
                        let row_bits = u32::try_from(sr.uv()?)
                            .map_err(|_| LupError::malformed(at, "invalid Track List view"))?;
                        let v = crate::model::project::TrackListView {
                            origin_tick,
                            ppt_bits,
                            row_bits,
                        };
                        if !v.is_valid() {
                            return Err(LupError::malformed(at, "invalid Track List view"));
                        }
                        sink.track_list(v, at)?;
                    } else {
                        // A newer layout: this version shows its default view.
                        sr.skip_rest()?;
                    }
                }
                TAG_SOUNDFONT => {
                    if saw_sf {
                        return Err(LupError::malformed(
                            offset,
                            "SoundFont settings appear twice",
                        ));
                    }
                    saw_sf = true;
                    let at = sr.offset();
                    let sf = match sr.u8()? {
                        0 => None,
                        1 => {
                            sr.string(&mut scratch)?;
                            let path = String::from_utf8(scratch.clone()).map_err(|_| {
                                LupError::malformed(at, "SoundFont path is not text")
                            })?;
                            Some(SoundFontRef { path })
                        }
                        _ => return Err(LupError::malformed(at, "invalid SoundFont entry")),
                    };
                    sink.soundfont(sf, at)?;
                }
                other => {
                    if sflags & FLAG_MANDATORY != 0 {
                        return Err(LupError::UnsupportedSection(tag_name(other)));
                    }
                    if len <= MAX_PRESERVED_SECTION
                        && self.preserved_total + len <= MAX_PRESERVED_TOTAL
                    {
                        let mut data = Vec::new();
                        sr.bytes_into(len, &mut data)?;
                        self.preserved_total += len;
                        self.report.warnings.push(format!(
                            "Kept section \"{}\" from a newer PlumSpirit unchanged.",
                            tag_name(other)
                        ));
                        sink.unknown(
                            UnknownSection {
                                tag: other,
                                flags: sflags,
                                data,
                            },
                            payload_at,
                        )?;
                    } else {
                        // Too large to keep in memory: skip it (still checksummed).
                        while sr.left() > 0 {
                            let skip = sr.left().min(READ_BUFFER as u64);
                            sr.bytes_into(skip, &mut scratch)?;
                        }
                        self.report.warnings.push(format!(
                            "Section \"{}\" from a newer PlumSpirit ({} bytes) was too large to keep and will not be saved again.",
                            tag_name(other),
                            group_digits(len)
                        ));
                    }
                }
            }
            sr.finish(tag, sflags, len, payload_at + len)?;
            offset = end;
        }
        self.drain(sink, &mut pipe)?;
        let Some(m) = meta else {
            return Err(LupError::malformed(
                offset,
                "project information is missing",
            ));
        };
        if tracks_read != m.track_count {
            return Err(LupError::malformed(
                offset,
                format!("{} of {} tracks present", tracks_read, m.track_count),
            ));
        }
        if m.extra_count > 0 && !saw_extra {
            return Err(LupError::malformed(offset, "extra MIDI chunks are missing"));
        }
        if version >= 2 && !saw_conductor {
            return Err(LupError::malformed(
                offset,
                "the Conductor section is missing",
            ));
        }
        // Nothing may follow the last section.
        let mut probe = [0u8; 1];
        loop {
            match r.read(&mut probe) {
                Ok(0) => break,
                Ok(_) => {
                    return Err(LupError::malformed(
                        offset,
                        "unexpected data after the last section",
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(LupError::io(None, e)),
            }
        }
        self.report.bytes = offset;
        Ok(())
    }

    /// The `CLIP` section (docs/LUP_FORMAT.md). Records are checked here for
    /// encoding; the table as a whole is checked against the tracks after
    /// reading (`PlumDocument::validate_clips`).
    fn parse_clips<R: Read, S: Sink>(
        &mut self,
        sr: &mut SectionReader<'_, R>,
        sink: &mut S,
        scratch: &mut Vec<u8>,
    ) -> Result<(), LupError> {
        use crate::model::clip::MAX_CLIP_RECORDS;
        use crate::model::{ClipId, ClipPlacement, ContentId, ContentRef};
        let bad = |o: u64, m: &str| LupError::malformed(o, format!("invalid clips: {m}"));
        let id32 = |sr: &mut SectionReader<'_, R>, what: &str| -> Result<u32, LupError> {
            let o = sr.offset();
            u32::try_from(sr.uv()?).map_err(|_| bad(o, what))
        };
        let at = sr.offset();
        if sr.uv()? != 1 {
            return Err(bad(at, "unknown layout"));
        }
        let next_clip = id32(sr, "next clip id")?;
        let next_content = id32(sr, "next content id")?;
        let n = sr.count(MIN_PLACEMENT_BYTES, "clip")?;
        if n > MAX_CLIP_RECORDS as u64 {
            return Err(bad(at, "too many clips"));
        }
        let mut placements = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let pat = sr.offset();
            let id = match sr.u8()? {
                0 => ClipId::Primary(TrackId(id32(sr, "track id")?)),
                1 => ClipId::Placed(id32(sr, "clip id")?),
                _ => return Err(bad(pat, "unknown clip kind")),
            };
            let track = TrackId(id32(sr, "track id")?);
            let cat = sr.offset();
            let content = match sr.u8()? {
                0 => ContentRef::Track(TrackId(id32(sr, "track id")?)),
                1 => ContentRef::Owned(ContentId(id32(sr, "content id")?)),
                _ => return Err(bad(cat, "unknown content kind")),
            };
            let start = sr.uv()?;
            let offset = sr.uv()?;
            let fat = sr.offset();
            let flags = sr.u8()?;
            if flags & !0x0F != 0 {
                return Err(bad(fat, "unknown clip flags"));
            }
            let length = if flags & 8 != 0 { Some(sr.uv()?) } else { None };
            let (name, color) = parse_label(sr, scratch, flags)?;
            placements.push(ClipPlacement {
                id,
                track,
                content,
                start,
                offset,
                length,
                muted: flags & 1 != 0,
                name,
                color,
                revision: 0,
            });
        }
        let r = sr.count(1, "removed clip")?;
        if r > MAX_CLIP_RECORDS as u64 {
            return Err(bad(sr.offset(), "too many removed clips"));
        }
        let mut removed = Vec::with_capacity(r as usize);
        for _ in 0..r {
            removed.push(TrackId(id32(sr, "track id")?));
        }
        sink.clips(
            ParsedClips {
                next_clip,
                next_content,
                placements,
                removed,
            },
            at,
        )?;
        let cn = sr.count(MIN_CONTENT_BYTES, "clip content")?;
        if cn > MAX_CLIP_RECORDS as u64 {
            return Err(bad(sr.offset(), "too many clip contents"));
        }
        for _ in 0..cn {
            let cat = sr.offset();
            let id = ContentId(id32(sr, "content id")?);
            let fat = sr.offset();
            let flags = sr.u8()?;
            if flags & !0x06 != 0 {
                return Err(bad(fat, "unknown content flags"));
            }
            let (name, color) = parse_label(sr, scratch, flags)?;
            let notes = sr.count(MIN_NOTE_BYTES, "note")?;
            sink.begin_content(ParsedContent { id, name, color }, notes, cat)?;
            self.parse_note_block(sr, sink, notes, None, true)?;
            self.parse_event_block(sr, sink, scratch)?;
            sink.end_content(cat)?;
        }
        sink.end_clips(cn, at)
    }

    /// `n` notes in the `TRAK` note encoding, delivered to `sink` in batches.
    /// `node`: the track's subchannel table (`None` before version 4);
    /// `open_subs`: a clip content, whose subchannel ids are not checked
    /// against a table.
    fn parse_note_block<R: Read, S: Sink>(
        &mut self,
        sr: &mut SectionReader<'_, R>,
        sink: &mut S,
        n: u64,
        node: Option<&ParsedNode>,
        open_subs: bool,
    ) -> Result<(), LupError> {
        let mut batch: Vec<NoteRecord> = Vec::new();
        batch
            .try_reserve_exact((n as usize).min(NOTE_BATCH))
            .map_err(|_| LupError::OutOfMemory)?;
        let mut start = 0u64;
        let mut left = n;
        // Validity of subchannel ids, checked once per run of equal ids (not per
        // note): the main subchannel's presence, and the last id seen.
        let has_main = node.is_none_or(|nd| nd.subs.contains(SubchannelId::MAIN));
        // Ids the track has, as a dense table when they are small (the common
        // case: a lookup per note instead of a search); otherwise searched.
        let dense: Vec<bool> = match node {
            Some(nd) if nd.subs.max_id().0 < DENSE_SUB_IDS => {
                let mut v = vec![false; nd.subs.max_id().0 as usize + 1];
                for e in nd.subs.entries() {
                    v[e.id.0 as usize] = true;
                }
                v
            }
            _ => Vec::new(),
        };
        let mut last_ok: Option<u32> = None;
        // Version 5: per-note groups and the mute bit.
        let grouped = self.report.version >= 5;
        let mut last_group = 0u32;
        while left > 0 {
            batch.clear();
            let take = left.min(NOTE_BATCH as u64);
            let batch_at = sr.offset();
            for _ in 0..take {
                let nat = sr.offset();
                let delta = sr.uv()?;
                let dur = sr.uv()?;
                let raw = sr.u32()?;
                start = start
                    .checked_add(delta)
                    .ok_or_else(|| LupError::malformed(nat, "note position out of range"))?;
                start
                    .checked_add(dur)
                    .ok_or_else(|| LupError::malformed(nat, "note end out of range"))?;
                let sub = match node {
                    // A clip content: its subchannel ids are read by the
                    // hosting track's table (docs/CLIP_ARCHITECTURE.md).
                    None if open_subs && raw & SUB_FOLLOWS != 0 => SubchannelId(
                        u32::try_from(sr.uv()?)
                            .ok()
                            .filter(|&v| v != 0)
                            .ok_or_else(|| LupError::malformed(nat, "invalid note subchannel"))?,
                    ),
                    Some(nd) if raw & SUB_FOLLOWS != 0 => {
                        let v =
                            SubchannelId(u32::try_from(sr.uv()?).map_err(|_| {
                                LupError::malformed(nat, "invalid note subchannel")
                            })?);
                        if last_ok != Some(v.0) {
                            let known = if dense.is_empty() {
                                nd.subs.contains(v)
                            } else {
                                dense.get(v.0 as usize).copied().unwrap_or(false)
                            };
                            if v.0 == 0 || !known {
                                return Err(LupError::malformed(
                                    nat,
                                    "a note refers to a subchannel the track does not have",
                                ));
                            }
                            last_ok = Some(v.0);
                        }
                        v
                    }
                    Some(_) => {
                        if !has_main {
                            return Err(LupError::malformed(
                                nat,
                                "a note refers to a subchannel the track does not have",
                            ));
                        }
                        SubchannelId::MAIN
                    }
                    None => SubchannelId::MAIN,
                };
                let group = if raw & GROUP_FOLLOWS != 0 && grouped {
                    let g = u32::try_from(sr.uv()?)
                        .ok()
                        .filter(|&g| g != 0)
                        .ok_or_else(|| LupError::malformed(nat, "invalid note group"))?;
                    if g != last_group {
                        last_group = g;
                        self.note_groups.insert(g);
                    }
                    g
                } else {
                    0
                };
                let strip = if grouped {
                    SUB_FOLLOWS | GROUP_FOLLOWS
                } else {
                    SUB_FOLLOWS
                };
                let attrs = NoteAttrs::from_raw(raw & !strip)
                    .filter(|_| node.is_some() || open_subs || raw & SUB_FOLLOWS == 0)
                    .filter(|a| grouped || !a.muted())
                    // 256-key pitches only in a 256-key project (version 6);
                    // anywhere else they are damage, as in v0.8.
                    .filter(|a| !a.is_extended() || self.keys == KeyRange::Extended256)
                    .ok_or_else(|| {
                        LupError::malformed(nat, "invalid note (pitch/velocity/channel)")
                    })?;
                batch.push(NoteRecord {
                    start_tick: start,
                    duration_ticks: dur,
                    attrs,
                    sub,
                    group,
                });
            }
            left -= take;
            sink.notes(&batch, left, batch_at)?;
            self.notes_seen += take;
            self.control.tick(self.notes_seen)?;
        }
        self.report.notes += n;
        Ok(())
    }

    /// Events in the `TRAK` event encoding, delivered to `sink`.
    fn parse_event_block<R: Read, S: Sink>(
        &mut self,
        sr: &mut SectionReader<'_, R>,
        sink: &mut S,
        scratch: &mut Vec<u8>,
    ) -> Result<(), LupError> {
        let ne = sr.count(MIN_EVENT_BYTES, "event")?;
        sink.begin_events(ne)?;
        let mut tick = 0u64;
        for _ in 0..ne {
            let eat = sr.offset();
            tick = tick
                .checked_add(sr.uv()?)
                .ok_or_else(|| LupError::malformed(eat, "event position out of range"))?;
            let anchor = u32::try_from(sr.uv()?)
                .map_err(|_| LupError::malformed(eat, "invalid event order"))?;
            let kind = sr.u8()?;
            let mut ev = ParsedEvent {
                tick,
                anchor,
                kind,
                a: 0,
                data: [0, 0],
                payload: &[],
            };
            match kind {
                0 | 4 => {
                    ev.a = sr.u8()?;
                    ev.data = [sr.u8()?, sr.u8()?];
                    let ok = if kind == 0 {
                        // A 256-key project keeps unmatched Note On / Off
                        // messages with keys above 127 (B1-0163: they were
                        // refused, so such a project could not be saved).
                        let wide_key = self.keys == KeyRange::Extended256 && ev.a & 0xE0 == 0x80;
                        valid_channel_status(ev.a)
                            && ev.data[..channel_data_len(ev.a)]
                                .iter()
                                .enumerate()
                                .all(|(i, b)| *b < 0x80 || (i == 0 && wide_key))
                    } else {
                        ev.a > 0xF0
                            && ev.a != 0xF7
                            && ev.a != 0xFF
                            && ev.data[..system_data_len(ev.a).unwrap_or(0)]
                                .iter()
                                .all(|b| *b < 0x80)
                    };
                    if !ok {
                        return Err(LupError::malformed(eat, "invalid MIDI event"));
                    }
                    sink.event(ev, eat)?;
                }
                1 => {
                    // Any type byte: MIDI import keeps whatever a file has
                    // (B1-0163: types from 0x80 were refused, so a project
                    // imported from such a file could not be saved).
                    ev.a = sr.u8()?;
                    sr.string(scratch)?;
                    ev.payload = scratch;
                    sink.event(ev, eat)?;
                }
                2 | 3 => {
                    sr.string(scratch)?;
                    ev.payload = scratch;
                    sink.event(ev, eat)?;
                }
                _ => return Err(LupError::malformed(eat, "unknown event kind")),
            }
        }
        self.report.events += ne;
        Ok(())
    }

    fn parse_track<R: Read, S: Sink>(
        &mut self,
        sr: &mut SectionReader<'_, R>,
        sink: &mut S,
        index: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<(), LupError> {
        let at = sr.offset();
        let id =
            u32::try_from(sr.uv()?).map_err(|_| LupError::malformed(at, "invalid track id"))?;
        let node = if self.report.version >= 4 {
            Some(parse_node(sr, scratch, id)?)
        } else {
            None
        };
        let n = sr.count(MIN_NOTE_BYTES, "note")?;
        sink.begin_track(index, TrackId(id), node.as_ref(), n, at)?;
        self.parse_note_block(sr, sink, n, node.as_ref(), false)?;
        self.parse_event_block(sr, sink, scratch)?;
        let eat = sr.offset();
        let eot = match sr.u8()? {
            0 => None,
            1 => Some(sr.uv()?),
            _ => return Err(LupError::malformed(eat, "invalid end-of-track entry")),
        };
        let props = if self.report.version >= 3 {
            Some(parse_track_props(sr, scratch, self.keys)?)
        } else {
            None
        };
        sink.end_track(eot, props, eat)
    }
}

/// A clip / content name (flag bit 1: text) and colour (flag bit 2: RGB).
fn parse_label<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
    flags: u8,
) -> Result<(Option<String>, Option<u32>), LupError> {
    let at = sr.offset();
    let name = if flags & 2 != 0 {
        sr.string(scratch)?;
        Some(
            String::from_utf8(scratch.clone())
                .map_err(|_| LupError::malformed(at, "invalid clips: a clip name is not text"))?,
        )
    } else {
        None
    };
    let color = if flags & 4 != 0 {
        let (r, g, b) = (sr.u8()?, sr.u8()?, sr.u8()?);
        Some(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))
    } else {
        None
    };
    Ok((name, color))
}

fn parse_node<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
    own_id: u32,
) -> Result<ParsedNode, LupError> {
    let at = sr.offset();
    let bad = |what: &str| LupError::malformed(at, format!("invalid track hierarchy ({what})"));
    let parent = match sr.uv()? {
        0 => None,
        v => Some(TrackId(u32::try_from(v - 1).map_err(|_| bad("parent id"))?)),
    };
    if parent == Some(TrackId(own_id)) {
        return Err(bad("a track cannot be its own parent"));
    }
    let n = sr.count(3, "subchannel")?;
    if n == 0 {
        return Err(bad("a track needs at least one subchannel"));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(usize::try_from(n).map_err(|_| LupError::OutOfMemory)?)
        .map_err(|_| LupError::OutOfMemory)?;
    for _ in 0..n {
        let id = SubchannelId(u32::try_from(sr.uv()?).map_err(|_| bad("subchannel id"))?);
        let midi_channel = sr.u8()?;
        let name = match sr.u8()? {
            0 => None,
            1 => {
                sr.string(scratch)?;
                Some(String::from_utf8(scratch.clone()).map_err(|_| bad("subchannel name"))?)
            }
            _ => return Err(bad("subchannel flags")),
        };
        entries.push(Subchannel {
            id,
            midi_channel,
            name,
        });
    }
    let subs = SubchannelTable::from_entries(entries).map_err(|e| bad(&e))?;
    Ok(ParsedNode { parent, subs })
}

fn parse_track_props<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
    keys: KeyRange,
) -> Result<ParsedProps, LupError> {
    let at = sr.offset();
    let bad = |what: &str| LupError::malformed(at, format!("invalid track properties ({what})"));
    let flags = sr.u8()?;
    if flags & !0b1_1111 != 0 {
        return Err(bad("flags"));
    }
    let default_channel = sr.u8()?;
    let route = if flags & 16 != 0 {
        sr.string(scratch)?;
        let name = String::from_utf8(scratch.clone()).map_err(|_| bad("output device name"))?;
        OutputRoute::External(name)
    } else {
        OutputRoute::Internal
    };
    let tick_correction = sr.iv()?;
    if tick_correction.unsigned_abs() > MAX_TICK_CORRECTION.unsigned_abs() {
        return Err(bad("tick correction"));
    }
    let correction_from_measure =
        u32::try_from(sr.uv()?).map_err(|_| bad("correction start measure"))?;
    let key_correction = i16::try_from(sr.iv()?).map_err(|_| bad("key correction"))?;
    let gate = u32::try_from(sr.uv()?)
        .ok()
        .and_then(GatePercent::from_hundredths)
        .ok_or_else(|| bad("gate correction"))?;
    let key_range = (sr.u8()?, sr.u8()?);
    let onion_color = if flags & 8 != 0 {
        Some([sr.u8()?, sr.u8()?, sr.u8()?])
    } else {
        None
    };
    let props = TrackProps {
        default_channel,
        kind: if flags & 4 != 0 {
            TrackKind::Rhythm
        } else {
            TrackKind::Melody
        },
        route,
        tick_correction,
        correction_from_measure,
        key_correction,
        gate,
        key_range,
        onion_color,
    };
    props.validate(keys).map_err(|e| bad(&e))?;
    Ok(ParsedProps {
        props,
        mute: flags & 1 != 0,
        solo: flags & 2 != 0,
    })
}

/// The part of a group table that is saved (temporary groups dropped).
fn saved_groups(t: &crate::model::GroupTable) -> crate::model::GroupTable {
    if t.entries().iter().all(|g| !g.temporary) {
        return t.clone();
    }
    let entries = t
        .entries()
        .iter()
        .filter(|g| !g.temporary)
        .map(|g| crate::model::GroupInfo {
            parent: if t.saved(g.parent) { g.parent } else { 0 },
            ..g.clone()
        })
        .collect();
    crate::model::GroupTable::from_entries(entries, t.next_id()).unwrap_or_default()
}

fn parse_groups<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
) -> Result<crate::model::GroupTable, LupError> {
    let at = sr.offset();
    let bad = |what: &str| LupError::malformed(at, format!("invalid note groups ({what})"));
    let next = u32::try_from(sr.uv()?).map_err(|_| bad("next id"))?;
    let n = sr.count(4, "group")?;
    if n > crate::model::group::MAX_GROUPS as u64 {
        return Err(bad("too many groups"));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(n as usize)
        .map_err(|_| LupError::OutOfMemory)?;
    for _ in 0..n {
        let id = u32::try_from(sr.uv()?).map_err(|_| bad("id"))?;
        let parent = u32::try_from(sr.uv()?).map_err(|_| bad("parent"))?;
        let flags = sr.u8()?;
        if flags & !0b111 != 0 {
            return Err(bad("flags"));
        }
        let link = u32::try_from(sr.uv()?).map_err(|_| bad("link"))?;
        let name = if flags & 2 != 0 {
            sr.string(scratch)?;
            Some(String::from_utf8(scratch.clone()).map_err(|_| bad("name"))?)
        } else {
            None
        };
        let color = if flags & 4 != 0 {
            Some([sr.u8()?, sr.u8()?, sr.u8()?])
        } else {
            None
        };
        entries.push(crate::model::GroupInfo {
            id,
            parent,
            name,
            color,
            locked: flags & 1 != 0,
            temporary: false,
            link,
        });
    }
    crate::model::GroupTable::from_entries(entries, next).map_err(|e| bad(&e))
}

fn parse_transport<R: Read>(
    sr: &mut SectionReader<'_, R>,
) -> Result<(LoopConfig, GraphConfig), LupError> {
    let at = sr.offset();
    let bad = |what: &str| LupError::malformed(at, format!("invalid transport settings ({what})"));
    let enabled = match sr.u8()? {
        0 => false,
        1 => true,
        _ => return Err(bad("loop switch")),
    };
    let range = match sr.u8()? {
        0 => None,
        1 => {
            let (a, b) = (sr.uv()?, sr.uv()?);
            if b <= a {
                return Err(bad("loop range"));
            }
            Some((a, b))
        }
        _ => return Err(bad("loop range")),
    };
    let n = sr.u8()?;
    if n != 2 {
        return Err(bad("graph slots"));
    }
    let mut g = GraphConfig::default();
    for slot in &mut g.slots {
        let visible = match sr.u8()? {
            0 => false,
            1 => true,
            _ => return Err(bad("graph visibility")),
        };
        let (kind, num, channel) = (sr.u8()?, sr.u8()?, sr.u8()?);
        let target = GraphTarget::from_code(kind, num).ok_or_else(|| bad("graph type"))?;
        if channel > 15 {
            return Err(bad("graph channel"));
        }
        *slot = GraphSlot {
            visible,
            target,
            channel,
        };
    }
    Ok((LoopConfig { range, enabled }, g))
}

fn parse_meta<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
    version: u16,
) -> Result<Meta, LupError> {
    let at = sr.offset();
    sr.string(scratch)?;
    let name = String::from_utf8(scratch.clone())
        .map_err(|_| LupError::malformed(at, "project name is not valid text"))?;
    sr.string(scratch)?; // writer (informational)
    let fat = sr.offset();
    let format = u16::try_from(sr.uv()?)
        .ok()
        .and_then(SmfFormat::from_word)
        .ok_or_else(|| LupError::malformed(fat, "invalid MIDI format"))?;
    let dat = sr.offset();
    let division = u32::try_from(sr.uv()?)
        .ok()
        .and_then(Division::from_encoded)
        .ok_or_else(|| LupError::malformed(dat, "invalid timing division"))?;
    let tat = sr.offset();
    let tempo = match sr.u8()? {
        0 => None,
        1 => {
            let m = sr.uv()?;
            let e = sr.iv()?;
            let e = i32::try_from(e).map_err(|_| LupError::malformed(tat, "invalid tempo"))?;
            Some(Bpm::new(m, e).map_err(|_| LupError::malformed(tat, "invalid tempo"))?)
        }
        _ => return Err(LupError::malformed(tat, "invalid tempo entry")),
    };
    let nat = sr.offset();
    let next_track_id =
        u32::try_from(sr.uv()?).map_err(|_| LupError::malformed(nat, "invalid track id"))?;
    let cat = sr.offset();
    let track_count = sr.uv()?;
    if track_count == 0 || track_count > u64::from(u16::MAX) {
        return Err(LupError::malformed(cat, "invalid track count"));
    }
    let xat = sr.offset();
    let extra_count = sr.uv()?;
    if extra_count > 1_000_000 {
        return Err(LupError::malformed(xat, "invalid extra chunk count"));
    }
    let kat = sr.offset();
    let keys = if version >= LUP_VERSION_256 {
        match sr.uv()? {
            0 => KeyRange::Standard,
            1 => KeyRange::Extended256,
            _ => return Err(LupError::malformed(kat, "invalid key range")),
        }
    } else {
        KeyRange::Standard
    };
    Ok(Meta {
        name,
        keys,
        format,
        division,
        tempo,
        next_track_id,
        track_count: track_count as usize,
        extra_count: extra_count as usize,
    })
}

fn parse_origin<R: Read>(
    sr: &mut SectionReader<'_, R>,
    present: bool,
) -> Result<Option<Origin>, LupError> {
    if !present {
        return Ok(None);
    }
    let at = sr.offset();
    let bad = || LupError::malformed(at, "invalid Conductor event origin");
    let track = u32::try_from(sr.uv()?).map_err(|_| bad())?;
    let anchor = u32::try_from(sr.uv()?).map_err(|_| bad())?;
    let rank = u32::try_from(sr.uv()?).map_err(|_| bad())?;
    let seq = u32::try_from(sr.uv()?).map_err(|_| bad())?;
    Ok(Some(Origin {
        track: TrackId(track),
        anchor,
        rank,
        seq,
    }))
}

fn parse_conductor<R: Read>(
    sr: &mut SectionReader<'_, R>,
    scratch: &mut Vec<u8>,
    version: u16,
) -> Result<Conductor, LupError> {
    let at = sr.offset();
    if sr.uv()? != 0 {
        return Err(LupError::malformed(at, "unknown Conductor identity"));
    }
    let sat = sr.offset();
    let scope = ConductorScope::from_code(sr.u8()?)
        .ok_or_else(|| LupError::malformed(sat, "unknown Conductor scope"))?;
    let nat = sr.offset();
    let next_id = u32::try_from(sr.uv()?).map_err(|_| LupError::malformed(nat, "invalid id"))?;
    let id_of =
        |v: u64, at: u64| u32::try_from(v).map_err(|_| LupError::malformed(at, "invalid id"));

    let n = sr.count(5, "tempo point")?;
    let mut tempo: Vec<TempoPoint> = Vec::new();
    tempo
        .try_reserve_exact(n as usize)
        .map_err(|_| LupError::OutOfMemory)?;
    let mut tick = 0u64;
    for _ in 0..n {
        let pat = sr.offset();
        let id = id_of(sr.uv()?, pat)?;
        tick = tick
            .checked_add(sr.uv()?)
            .ok_or_else(|| LupError::malformed(pat, "tempo position out of range"))?;
        let flags = sr.u8()?;
        if flags & !0b111 != 0 {
            return Err(LupError::malformed(pat, "invalid tempo point flags"));
        }
        let vat = sr.offset();
        let value = match sr.u8()? {
            0 => {
                let us = sr.uv()?;
                if us == 0 || us > 0xFF_FFFF {
                    return Err(LupError::malformed(vat, "invalid tempo"));
                }
                TempoValue::Micros(us as u32)
            }
            1 => {
                let m = sr.uv()?;
                let e = i32::try_from(sr.iv()?)
                    .map_err(|_| LupError::malformed(vat, "invalid tempo"))?;
                TempoValue::Bpm(
                    Bpm::new(m, e).map_err(|_| LupError::malformed(vat, "invalid tempo"))?,
                )
            }
            _ => return Err(LupError::malformed(vat, "invalid tempo kind")),
        };
        let origin = parse_origin(sr, flags & 4 != 0)?;
        tempo.push(TempoPoint {
            id,
            tick,
            value,
            interp: if flags & 1 != 0 {
                Interp::Linear
            } else {
                Interp::Step
            },
            implicit: flags & 2 != 0,
            origin,
        });
    }
    let n = sr.count(7, "time signature")?;
    let mut meter: Vec<TimeSignature> = Vec::new();
    meter
        .try_reserve_exact(n as usize)
        .map_err(|_| LupError::OutOfMemory)?;
    let mut tick = 0u64;
    for _ in 0..n {
        let mat = sr.offset();
        let id = id_of(sr.uv()?, mat)?;
        tick = tick
            .checked_add(sr.uv()?)
            .ok_or_else(|| LupError::malformed(mat, "time signature position out of range"))?;
        let flags = sr.u8()?;
        if flags & !0b110 != 0 {
            return Err(LupError::malformed(mat, "invalid time signature flags"));
        }
        let (numerator, denom_pow, clocks, thirty_seconds) =
            (sr.u8()?, sr.u8()?, sr.u8()?, sr.u8()?);
        if numerator == 0 || denom_pow > MAX_DENOM_POW {
            return Err(LupError::malformed(mat, "invalid time signature"));
        }
        let origin = parse_origin(sr, flags & 4 != 0)?;
        meter.push(TimeSignature {
            id,
            tick,
            numerator,
            denom_pow,
            clocks,
            thirty_seconds,
            implicit: flags & 2 != 0,
            origin,
        });
    }
    let n = sr.count(4, "marker")?;
    let mut markers: Vec<Marker> = Vec::new();
    markers
        .try_reserve_exact(n as usize)
        .map_err(|_| LupError::OutOfMemory)?;
    let mut start = 0u64;
    for _ in 0..n {
        let kat = sr.offset();
        let id = id_of(sr.uv()?, kat)?;
        start = start
            .checked_add(sr.uv()?)
            .ok_or_else(|| LupError::malformed(kat, "marker position out of range"))?;
        let flags = sr.u8()?;
        if flags & !0b1111 != 0 {
            return Err(LupError::malformed(kat, "invalid marker flags"));
        }
        let end = if flags & 1 != 0 {
            let len = sr.uv()?;
            Some(
                start
                    .checked_add(len)
                    .ok_or_else(|| LupError::malformed(kat, "chapter end out of range"))?,
            )
        } else {
            None
        };
        sr.string(scratch)?;
        let title = String::from_utf8(scratch.clone())
            .map_err(|_| LupError::malformed(kat, "marker title is not valid text"))?;
        let raw_title = if flags & 8 != 0 {
            sr.string(scratch)?;
            Some(scratch.clone().into_boxed_slice())
        } else {
            None
        };
        let origin = parse_origin(sr, flags & 4 != 0)?;
        markers.push(Marker {
            id,
            start,
            end,
            title,
            raw_title,
            cue: flags & 2 != 0,
            origin,
        });
    }
    let mut master: Vec<MasterVolumePoint> = Vec::new();
    if version >= 3 {
        let n = sr.count(5, "master volume point")?;
        master
            .try_reserve_exact(n as usize)
            .map_err(|_| LupError::OutOfMemory)?;
        let mut tick = 0u64;
        for _ in 0..n {
            let vat = sr.offset();
            let id = id_of(sr.uv()?, vat)?;
            tick = tick
                .checked_add(sr.uv()?)
                .ok_or_else(|| LupError::malformed(vat, "master volume position out of range"))?;
            let flags = sr.u8()?;
            if flags & !0b100 != 0 {
                return Err(LupError::malformed(vat, "invalid master volume flags"));
            }
            let value = sr.uv()?;
            let device = sr.u8()?;
            if value > u64::from(MASTER_VOLUME_MAX) || device > 0x7F {
                return Err(LupError::malformed(vat, "invalid master volume"));
            }
            let origin = parse_origin(sr, flags & 4 != 0)?;
            master.push(MasterVolumePoint {
                id,
                tick,
                value: value as u16,
                device,
                origin,
            });
        }
    }
    Conductor::from_parts_with_master(scope, tempo, meter, markers, master, next_id)
        .map_err(|e| LupError::malformed(at, format!("invalid Conductor: {e}")))
}

// ------------------------------------------------------------ building sink

struct Builder {
    meta: Option<Meta>,
    tracks: Vec<Track>,
    /// The id and offset the last track began with (B1-0163: a track parsed
    /// on a worker is checked against the tracks before it when it is taken
    /// back, in order).
    begun: Option<(TrackId, u64)>,
    extra: Vec<ExtraChunk>,
    project: ProjectInfo,
    conductor: Option<Conductor>,
    cur_id: TrackId,
    cur_node: Option<ParsedNode>,
    cur_notes: NoteStore,
    cur_events: EventStore,
    /// v0.9 `CLIP`: the records and where the section starts.
    clips: Option<(ParsedClips, u64)>,
    contents: Vec<crate::model::OwnedContent>,
    cur_content: Option<ParsedContent>,
}

impl Sink for Builder {
    fn clips(&mut self, head: ParsedClips, at: u64) -> Result<(), LupError> {
        self.clips = Some((head, at));
        Ok(())
    }

    fn begin_content(&mut self, c: ParsedContent, _notes: u64, _at: u64) -> Result<(), LupError> {
        self.cur_notes = NoteStore::new();
        self.cur_events = EventStore::new();
        self.cur_content = Some(c);
        Ok(())
    }

    fn end_content(&mut self, _at: u64) -> Result<(), LupError> {
        let c = self.cur_content.take().expect("begin_content");
        let mut o = crate::model::OwnedContent::new(c.id);
        o.notes = std::mem::take(&mut self.cur_notes);
        o.events = std::mem::take(&mut self.cur_events);
        o.events.shrink_to_fit();
        o.name = c.name;
        o.color = c.color;
        self.contents
            .try_reserve(1)
            .map_err(|_| LupError::OutOfMemory)?;
        self.contents.push(o);
        Ok(())
    }

    fn end_clips(&mut self, _contents: u64, _at: u64) -> Result<(), LupError> {
        Ok(())
    }

    fn meta(&mut self, m: &Meta, _at: u64) -> Result<(), LupError> {
        self.project.name = m.name.clone();
        self.project.keys = m.keys;
        self.tracks
            .try_reserve_exact(m.track_count)
            .map_err(|_| LupError::OutOfMemory)?;
        self.meta = Some(m.clone());
        Ok(())
    }

    fn begin_track(
        &mut self,
        _index: usize,
        id: TrackId,
        node: Option<&ParsedNode>,
        _notes: u64,
        at: u64,
    ) -> Result<(), LupError> {
        if self.tracks.iter().any(|t| t.id() == id) {
            return Err(LupError::malformed(at, "two tracks share an id"));
        }
        self.begun = Some((id, at));
        self.cur_id = id;
        self.cur_node = node.cloned();
        self.cur_notes = NoteStore::new();
        self.cur_events = EventStore::new();
        Ok(())
    }

    fn notes(
        &mut self,
        batch: &[NoteRecord],
        remaining_after: u64,
        at: u64,
    ) -> Result<(), LupError> {
        let hint = usize::try_from(remaining_after)
            .unwrap_or(usize::MAX)
            .saturating_add(batch.len());
        self.cur_notes
            .append_sorted(batch, hint)
            .map_err(|e| LupError::malformed(at, e.to_string()))
    }

    fn begin_events(&mut self, _count: u64) -> Result<(), LupError> {
        Ok(())
    }

    fn event(&mut self, ev: ParsedEvent<'_>, at: u64) -> Result<(), LupError> {
        let store = |events: &mut EventStore, p: &[u8]| -> Result<ArenaRange, LupError> {
            events
                .store_payload(p)
                .map_err(|e| LupError::malformed(at, e.to_string()))
        };
        let kind = match ev.kind {
            0 => RawKind::Channel {
                status: ev.a,
                data: ev.data,
            },
            4 => RawKind::System {
                status: ev.a,
                data: ev.data,
            },
            1 => RawKind::Meta {
                meta_type: ev.a,
                payload: store(&mut self.cur_events, ev.payload)?,
            },
            2 => RawKind::SysEx {
                payload: store(&mut self.cur_events, ev.payload)?,
            },
            _ => RawKind::SysExEscape {
                payload: store(&mut self.cur_events, ev.payload)?,
            },
        };
        self.cur_events
            .push(RawEvent {
                tick: ev.tick,
                anchor: ev.anchor,
                kind,
            })
            .map_err(|e| LupError::malformed(at, e.to_string()))
    }

    fn end_track(
        &mut self,
        eot: Option<u64>,
        props: Option<ParsedProps>,
        _at: u64,
    ) -> Result<(), LupError> {
        let mut events = std::mem::take(&mut self.cur_events);
        events.shrink_to_fit();
        let notes = std::mem::take(&mut self.cur_notes);
        let legacy = self.cur_node.is_none();
        let mut t = match self.cur_node.take() {
            Some(node) => {
                let mut t = Track::from_parts_exact(self.cur_id, notes, events, eot);
                t.set_parent(node.parent);
                t.set_subchannels(node.subs);
                t
            }
            // Before version 4: subchannels are inferred from the note channels.
            None => Track::from_parts(self.cur_id, notes, events, eot),
        };
        if let Some(p) = props {
            let legacy = legacy && t.subchannels().len() == 1;
            let channel = p.props.default_channel;
            t.set_props(p.props, eot);
            t.mute = p.mute;
            t.solo = p.solo;
            if legacy {
                // v0.4: the track's channel for new notes becomes the main
                // subchannel's MIDI channel.
                t.set_subchannels(SubchannelTable::single(channel));
            }
        }
        self.tracks.push(t);
        Ok(())
    }

    fn transport(&mut self, l: LoopConfig, g: GraphConfig, _at: u64) -> Result<(), LupError> {
        self.project.looping = l;
        self.project.graphs = g;
        Ok(())
    }

    fn groups(&mut self, t: crate::model::GroupTable, _at: u64) -> Result<(), LupError> {
        self.project.groups = t;
        Ok(())
    }

    fn zoom(
        &mut self,
        v: Vec<crate::model::project::ZoomRegion>,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.zoom_regions = v;
        Ok(())
    }

    fn track_list(
        &mut self,
        v: crate::model::project::TrackListView,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.track_list = Some(v);
        Ok(())
    }

    fn source(
        &mut self,
        p: crate::model::project::SourceProvenance,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.source = Some(p);
        Ok(())
    }

    fn reference(
        &mut self,
        r: crate::model::project::ReferenceAudio,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.reference = Some(r);
        Ok(())
    }

    fn metadata(&mut self, v: crate::model::metadata::Metadata, _at: u64) -> Result<(), LupError> {
        self.project.metadata = v;
        Ok(())
    }

    fn plugin_data(&mut self, v: Vec<(String, String)>, _at: u64) -> Result<(), LupError> {
        self.project.plugin_data = v;
        Ok(())
    }

    fn extra_chunk(&mut self, chunk: ExtraChunk, _at: u64) -> Result<(), LupError> {
        self.extra.push(chunk);
        Ok(())
    }

    fn folders(
        &mut self,
        f: crate::model::TrackFolders,
        collapsed: std::collections::BTreeSet<u32>,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.folders = f;
        self.project.folders_collapsed = collapsed;
        Ok(())
    }

    fn onion(&mut self, o: OnionConfig, _at: u64) -> Result<(), LupError> {
        // The per-track part comes from its own section.
        let per_track = self.project.onion.per_track;
        let entries = std::mem::take(&mut self.project.onion.track_entries);
        self.project.onion = OnionConfig {
            per_track,
            track_entries: entries,
            ..o
        };
        Ok(())
    }

    fn onion_tracks(
        &mut self,
        per_track: bool,
        entries: Vec<OnionTrackEntry>,
        _at: u64,
    ) -> Result<(), LupError> {
        self.project.onion.per_track = per_track;
        self.project.onion.track_entries = entries;
        Ok(())
    }

    fn soundfont(&mut self, sf: Option<SoundFontRef>, _at: u64) -> Result<(), LupError> {
        self.project.soundfont = sf;
        Ok(())
    }

    fn conductor(&mut self, c: Conductor, _at: u64) -> Result<(), LupError> {
        self.conductor = Some(c);
        Ok(())
    }

    fn unknown(&mut self, u: UnknownSection, _at: u64) -> Result<(), LupError> {
        self.project.unknown_sections.push(u);
        Ok(())
    }
}

/// Reads a `.lup` project. `file_len` (when known) lets truncation be detected
/// before reading.
pub fn read_project<R: Read>(
    r: R,
    file_len: Option<u64>,
    control: LupControl<'_>,
) -> Result<(PlumDocument, LupLoadReport), LupError> {
    let threads = crate::midi::workers::threads_for(file_len);
    read_project_threads(r, file_len, control, threads, crate::midi::workers::WINDOW)
}

/// [`read_project`] with `threads` workers for the track sections (0 = in
/// place) and a chunk window (B1-0163).
pub(crate) fn read_project_threads<R: Read>(
    mut r: R,
    file_len: Option<u64>,
    control: LupControl<'_>,
    threads: usize,
    window: u64,
) -> Result<(PlumDocument, LupLoadReport), LupError> {
    let started = Instant::now();
    let mut parser = Parser {
        keys: KeyRange::Standard,
        control,
        file_len,
        notes_seen: 0,
        report: LupLoadReport::default(),
        preserved_total: 0,
        note_groups: Default::default(),
        taken_back_failed: false,
    };
    let mut b = Builder {
        meta: None,
        tracks: Vec::new(),
        begun: None,
        extra: Vec::new(),
        project: ProjectInfo::default(),
        conductor: None,
        cur_id: TrackId(0),
        cur_node: None,
        cur_notes: NoteStore::new(),
        cur_events: EventStore::new(),
        clips: None,
        contents: Vec::new(),
        cur_content: None,
    };
    parser.run(&mut r, &mut b, threads, window)?;
    let m = b.meta.take().expect("parser requires META");
    let next = m.next_track_id.max(
        b.tracks
            .iter()
            .map(|t| t.id().0.saturating_add(1))
            .max()
            .unwrap_or(0),
    );
    if let Some(&g) = parser
        .note_groups
        .iter()
        .find(|&&g| b.project.groups.get(g).is_none())
    {
        return Err(LupError::malformed(
            0,
            format!("a note refers to group {g}, which the project does not have"),
        ));
    }
    let legacy = b.conductor.is_none();
    let clips = b.clips.take();
    let contents = std::mem::take(&mut b.contents);
    let (mut doc, adopted) = PlumDocument::from_native_parts(
        m.format,
        m.division,
        b.tracks,
        b.extra,
        next,
        b.project,
        b.conductor,
        m.tempo,
    )
    .map_err(|e| LupError::malformed(0, e))?;
    // v0.9 clips: the table is checked as a whole against the tracks.
    if let Some((head, at)) = clips {
        let bad = |e: String| LupError::malformed(at, format!("invalid clips: {e}"));
        *doc.clips_mut() = crate::model::ClipTable::from_parts(
            contents,
            head.placements,
            head.removed,
            head.next_clip,
            head.next_content,
        )
        .map_err(bad)?;
        doc.validate_clips().map_err(bad)?;
    }
    doc.project()
        .metadata
        .validate_targets(&doc)
        .map_err(|m| LupError::malformed(0, m))?;
    let mut report = parser.report;
    if !legacy && report.version < LUP_VERSION {
        report.migrated_from = Some(report.version);
        if report.version == 2 {
            report.warnings.push(
                "This project was saved by PlumSpirit v0.3. It opens with the newer defaults (no track corrections, Mute or Solo, loop or Master Volume); saving writes the current format, which PlumSpirit v0.3 cannot open."
                    .into(),
            );
        } else if report.version == 4 {
            report.warnings.push(
                "This project was saved by PlumSpirit v0.5 or v0.6. It opens unchanged; saving writes the v0.7 format (note groups and muted notes), which those versions cannot open."
                    .into(),
            );
        } else {
            report.warnings.push(
                "This project was saved by PlumSpirit v0.4. Its tracks open as top-level tracks, and each track gets one subchannel per MIDI channel its notes use; saving writes the current format, which PlumSpirit v0.4 cannot open."
                    .into(),
            );
        }
    }
    if legacy {
        report.migrated_from = Some(report.version);
        report.warnings.push(
            "This project was saved by PlumSpirit v0.2. Its tempo, time signatures and markers now live in the Conductor; the file itself is unchanged until you save it (then it is written in the new format, which PlumSpirit v0.2 cannot open)."
                .into(),
        );
        if let Some(a) = adopted {
            report.warnings.extend(a.notes());
        }
    }
    report.duration = started.elapsed();
    Ok((doc, report))
}

/// Reads a project from memory (tests).
pub fn project_from_bytes(bytes: &[u8]) -> Result<(PlumDocument, LupLoadReport), LupError> {
    read_project(bytes, Some(bytes.len() as u64), LupControl::default())
}

/// Opens and reads a `.lup` file.
pub fn load_project(
    path: &Path,
    control: LupControl<'_>,
) -> Result<(PlumDocument, LupLoadReport), LupError> {
    let f = File::open(path).map_err(|e| LupError::io(None, e))?;
    let len = f.metadata().ok().map(|m| m.len());
    read_project(f, len, control)
}

/// Quick check whether a file starts with the `.lup` signature.
pub fn has_lup_signature(path: &Path) -> bool {
    let mut b = [0u8; 8];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok()
        && b == LUP_MAGIC
}

// ------------------------------------------------------------ verifying sink

/// Compares a parsed project with the document it was written from.
struct Verifier<'d> {
    doc: &'d PlumDocument,
    track: usize,
    notes: Option<Box<dyn Iterator<Item = NoteView> + Send + 'd>>,
    /// The events being compared (the current track's or clip content's).
    events: Option<&'d EventStore>,
    /// "track 3" / "clip content 2" for messages.
    what: String,
    event: usize,
    clips_seen: bool,
    content: usize,
    extra: usize,
    onion_seen: bool,
    onion_tracks_seen: bool,
    sf_seen: bool,
    conductor_seen: bool,
    transport_seen: bool,
    reference_seen: bool,
    plugin_data_seen: bool,
    metadata_seen: bool,
    folders_seen: bool,
    track_list_seen: bool,
    source_seen: bool,
    unknown: usize,
}

impl<'d> Verifier<'d> {
    fn new(doc: &'d PlumDocument) -> Self {
        Verifier {
            doc,
            track: 0,
            notes: None,
            events: None,
            what: String::new(),
            event: 0,
            clips_seen: false,
            content: 0,
            extra: 0,
            onion_seen: false,
            onion_tracks_seen: false,
            sf_seen: false,
            conductor_seen: false,
            transport_seen: false,
            reference_seen: false,
            plugin_data_seen: false,
            metadata_seen: false,
            folders_seen: false,
            track_list_seen: false,
            source_seen: false,
            unknown: 0,
        }
    }

    fn fail(&self, at: u64, what: impl Into<String>) -> LupError {
        LupError::Verification {
            detail: format!("{} at byte {}", what.into(), group_digits(at)),
        }
    }
}

impl<'d> Sink for Verifier<'d> {
    fn meta(&mut self, m: &Meta, at: u64) -> Result<(), LupError> {
        let d = self.doc;
        let p = d.project();
        if m.name != p.name
            || m.keys != p.keys
            || m.format != d.source_format()
            || m.division != d.division()
            || m.tempo.is_some()
            || m.next_track_id != d.next_track_id()
            || m.track_count != d.track_count()
            || m.extra_count != d.extra_chunks().len()
        {
            return Err(self.fail(at, "project information differs"));
        }
        Ok(())
    }

    fn begin_track(
        &mut self,
        index: usize,
        id: TrackId,
        node: Option<&ParsedNode>,
        notes: u64,
        at: u64,
    ) -> Result<(), LupError> {
        let d: &'d PlumDocument = self.doc;
        let Some(t) = d.track(index) else {
            return Err(self.fail(at, "extra track"));
        };
        if t.id() != id || t.notes.len() as u64 != notes {
            return Err(self.fail(at, format!("track {} differs", index + 1)));
        }
        match node {
            Some(n) if n.parent == t.parent() && n.subs == *t.subchannels_for_saving() => {}
            _ => {
                return Err(self.fail(
                    at,
                    format!("the hierarchy or subchannels of track {} differ", index + 1),
                ));
            }
        }
        self.track = index;
        self.what = format!("track {}", index + 1);
        self.notes = Some(Box::new(t.notes.iter()));
        self.events = Some(&t.events);
        Ok(())
    }

    fn notes(
        &mut self,
        batch: &[NoteRecord],
        _remaining_after: u64,
        at: u64,
    ) -> Result<(), LupError> {
        let it = self.notes.as_mut().expect("begin_track");
        let groups = &self.doc.project().groups;
        for r in batch {
            match it.next() {
                Some(v)
                    if {
                        let mut want = v.record();
                        if !groups.saved(want.group) {
                            want.group = 0;
                        }
                        want == *r
                    } => {}
                _ => {
                    return Err(LupError::Verification {
                        detail: format!(
                            "a note in {} differs (block at byte {})",
                            self.what,
                            group_digits(at)
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn begin_events(&mut self, count: u64) -> Result<(), LupError> {
        let events = self.events.expect("begin_track / begin_content");
        if events.len() as u64 != count {
            return Err(LupError::Verification {
                detail: format!("{} event count differs", self.what),
            });
        }
        self.event = 0;
        Ok(())
    }

    fn event(&mut self, ev: ParsedEvent<'_>, at: u64) -> Result<(), LupError> {
        let events: &EventStore = self.events.expect("begin_track / begin_content");
        let Some(e) = events.events().get(self.event) else {
            return Err(self.fail(at, "extra event"));
        };
        self.event += 1;
        let same = e.tick == ev.tick
            && e.anchor == ev.anchor
            && match e.kind {
                RawKind::Channel { status, data } => {
                    ev.kind == 0 && ev.a == status && ev.data == data
                }
                RawKind::System { status, data } => {
                    ev.kind == 4 && ev.a == status && ev.data == data
                }
                RawKind::Meta { meta_type, payload } => {
                    ev.kind == 1 && ev.a == meta_type && events.payload(payload) == ev.payload
                }
                RawKind::SysEx { payload } => ev.kind == 2 && events.payload(payload) == ev.payload,
                RawKind::SysExEscape { payload } => {
                    ev.kind == 3 && events.payload(payload) == ev.payload
                }
            };
        if !same {
            return Err(self.fail(at, format!("an event in {} differs", self.what)));
        }
        Ok(())
    }

    fn end_track(
        &mut self,
        eot: Option<u64>,
        props: Option<ParsedProps>,
        at: u64,
    ) -> Result<(), LupError> {
        let t = &self.doc.tracks()[self.track];
        if self.notes.as_mut().and_then(|i| i.next()).is_some() {
            return Err(self.fail(at, format!("notes missing in track {}", self.track + 1)));
        }
        if eot != t.end_of_track_tick {
            return Err(self.fail(at, format!("end of track {} differs", self.track + 1)));
        }
        let expected = ParsedProps {
            props: t.props().clone(),
            mute: t.mute,
            solo: t.solo,
        };
        if props.as_ref() != Some(&expected) {
            return Err(self.fail(
                at,
                format!("the properties of track {} differ", self.track + 1),
            ));
        }
        self.notes = None;
        self.events = None;
        Ok(())
    }

    fn clips(&mut self, head: ParsedClips, at: u64) -> Result<(), LupError> {
        let c = self.doc.clips();
        if (head.next_clip, head.next_content) != c.next_ids()
            || head.removed != c.removed_primaries()
            || !same_placements(&head.placements, c.explicit_placements())
        {
            return Err(self.fail(at, "the clips differ"));
        }
        self.clips_seen = true;
        Ok(())
    }

    fn begin_content(&mut self, c: ParsedContent, notes: u64, at: u64) -> Result<(), LupError> {
        let d: &'d PlumDocument = self.doc;
        let Some(o) = d.clips().contents().get(self.content) else {
            return Err(self.fail(at, "extra clip content"));
        };
        self.content += 1;
        self.what = format!("clip content {}", self.content);
        if o.id != c.id || o.name != c.name || o.color != c.color || o.notes.len() as u64 != notes {
            return Err(self.fail(at, format!("{} differs", self.what)));
        }
        self.notes = Some(Box::new(o.notes.iter()));
        self.events = Some(&o.events);
        Ok(())
    }

    fn end_content(&mut self, at: u64) -> Result<(), LupError> {
        if self.notes.as_mut().and_then(|i| i.next()).is_some() {
            return Err(self.fail(at, format!("notes missing in {}", self.what)));
        }
        self.notes = None;
        self.events = None;
        Ok(())
    }

    fn end_clips(&mut self, contents: u64, at: u64) -> Result<(), LupError> {
        if contents != self.doc.clips().contents().len() as u64 {
            return Err(self.fail(at, "the clip contents differ"));
        }
        Ok(())
    }

    fn groups(&mut self, t: crate::model::GroupTable, at: u64) -> Result<(), LupError> {
        if t != saved_groups(&self.doc.project().groups) {
            return Err(self.fail(at, "the note groups differ"));
        }
        Ok(())
    }

    fn zoom(&mut self, v: Vec<crate::model::project::ZoomRegion>, at: u64) -> Result<(), LupError> {
        if v != self.doc.project().zoom_regions {
            return Err(self.fail(at, "the named zoom regions differ"));
        }
        Ok(())
    }

    fn track_list(
        &mut self,
        v: crate::model::project::TrackListView,
        at: u64,
    ) -> Result<(), LupError> {
        if Some(v) != self.doc.project().track_list {
            return Err(self.fail(at, "the Track List view differs"));
        }
        self.track_list_seen = true;
        Ok(())
    }

    fn source(
        &mut self,
        p: crate::model::project::SourceProvenance,
        at: u64,
    ) -> Result<(), LupError> {
        if Some(&p) != self.doc.project().source.as_ref() {
            return Err(self.fail(at, "the MIDI source differs"));
        }
        self.source_seen = true;
        Ok(())
    }

    fn folders(
        &mut self,
        f: crate::model::TrackFolders,
        collapsed: std::collections::BTreeSet<u32>,
        at: u64,
    ) -> Result<(), LupError> {
        self.folders_seen = true;
        if saved_folders(self.doc) != Some((f, collapsed)) {
            return Err(self.fail(at, "the track folders differ"));
        }
        Ok(())
    }

    fn reference(
        &mut self,
        r: crate::model::project::ReferenceAudio,
        at: u64,
    ) -> Result<(), LupError> {
        self.reference_seen = true;
        if Some(&r) != self.doc.project().reference.as_ref() {
            return Err(self.fail(at, "the reference audio settings differ"));
        }
        Ok(())
    }

    fn metadata(&mut self, v: crate::model::metadata::Metadata, at: u64) -> Result<(), LupError> {
        self.metadata_seen = true;
        if v != self.doc.project().metadata {
            return Err(self.fail(at, "typed metadata differs"));
        }
        Ok(())
    }

    fn plugin_data(&mut self, v: Vec<(String, String)>, at: u64) -> Result<(), LupError> {
        self.plugin_data_seen = true;
        if v != self.doc.project().plugin_data {
            return Err(self.fail(at, "the plugin data differs"));
        }
        Ok(())
    }

    fn transport(&mut self, l: LoopConfig, g: GraphConfig, at: u64) -> Result<(), LupError> {
        self.transport_seen = true;
        let p = self.doc.project();
        if l != p.looping || g != p.graphs {
            return Err(self.fail(at, "loop or graph settings differ"));
        }
        Ok(())
    }

    fn extra_chunk(&mut self, chunk: ExtraChunk, at: u64) -> Result<(), LupError> {
        let ok = self.doc.extra_chunks().get(self.extra).is_some_and(|c| {
            c.chunk_type == chunk.chunk_type
                && c.data == chunk.data
                && c.after_track_chunks.min(self.doc.track_count()) == chunk.after_track_chunks
        });
        self.extra += 1;
        if !ok {
            return Err(self.fail(at, "an extra MIDI chunk differs"));
        }
        Ok(())
    }

    fn onion(&mut self, o: OnionConfig, at: u64) -> Result<(), LupError> {
        self.onion_seen = true;
        let want = OnionConfig {
            per_track: false,
            track_entries: Vec::new(),
            ..self.doc.project().onion.clone()
        };
        if o != want {
            return Err(self.fail(at, "Onion Skin settings differ"));
        }
        Ok(())
    }

    fn onion_tracks(
        &mut self,
        per_track: bool,
        entries: Vec<OnionTrackEntry>,
        at: u64,
    ) -> Result<(), LupError> {
        self.onion_tracks_seen = true;
        let o = &self.doc.project().onion;
        if per_track != o.per_track || entries != o.track_entries {
            return Err(self.fail(at, "per-track Onion Skin settings differ"));
        }
        Ok(())
    }

    fn soundfont(&mut self, sf: Option<SoundFontRef>, at: u64) -> Result<(), LupError> {
        self.sf_seen = true;
        if sf != self.doc.project().soundfont {
            return Err(self.fail(at, "SoundFont association differs"));
        }
        Ok(())
    }

    fn conductor(&mut self, c: Conductor, at: u64) -> Result<(), LupError> {
        self.conductor_seen = true;
        if !c.same_content(self.doc.conductor()) {
            return Err(self.fail(at, "the Conductor differs"));
        }
        Ok(())
    }

    fn unknown(&mut self, u: UnknownSection, at: u64) -> Result<(), LupError> {
        let ok = self.doc.project().unknown_sections.get(self.unknown) == Some(&u);
        self.unknown += 1;
        if !ok {
            return Err(self.fail(at, "a preserved section differs"));
        }
        Ok(())
    }
}

/// Re-reads a written project and checks that it contains exactly `doc`.
/// Memory use is bounded by the read buffer (no second document is built).
pub fn verify_project<R: Read>(
    doc: &PlumDocument,
    r: R,
    file_len: Option<u64>,
) -> Result<(), LupError> {
    let threads = crate::midi::workers::threads_for(file_len);
    verify_project_threads(doc, r, file_len, threads, crate::midi::workers::WINDOW)
}

/// [`verify_project`] with `threads` workers and a chunk window (B1-0163).
pub(crate) fn verify_project_threads<R: Read>(
    doc: &PlumDocument,
    mut r: R,
    file_len: Option<u64>,
    threads: usize,
    window: u64,
) -> Result<(), LupError> {
    let mut parser = Parser {
        keys: KeyRange::Standard,
        control: LupControl::default(),
        file_len,
        notes_seen: 0,
        report: LupLoadReport::default(),
        preserved_total: 0,
        note_groups: Default::default(),
        taken_back_failed: false,
    };
    let mut v = Verifier::new(doc);
    parser
        .run(&mut r, &mut v, threads, window)
        .map_err(|e| match e {
            LupError::Verification { .. } => e,
            other => LupError::Verification {
                detail: other.user_message(),
            },
        })?;
    if !v.onion_seen
        || !v.sf_seen
        || !v.conductor_seen
        || !v.transport_seen
        || v.reference_seen != doc.project().reference.is_some()
        || v.plugin_data_seen == doc.project().plugin_data.is_empty()
        || v.metadata_seen == doc.project().metadata.is_empty()
        || v.folders_seen == doc.project().folders.is_empty()
        || v.track_list_seen != doc.project().track_list.is_some()
        || v.source_seen != doc.project().source.is_some()
        || v.unknown != doc.project().unknown_sections.len()
        || v.onion_tracks_seen != onion_tracks_used(&doc.project().onion)
        || v.clips_seen == doc.clips().is_empty()
    {
        return Err(LupError::Verification {
            detail: "project settings are missing".into(),
        });
    }
    Ok(())
}

// --------------------------------------------------------- transactional save

/// Outcome of a successful save.
#[derive(Clone, Debug)]
pub struct SaveReport {
    pub path: PathBuf,
    pub write: LupWriteStats,
    pub write_duration: Duration,
    pub verify_duration: Duration,
}

struct TempGuard {
    path: Option<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = fs::remove_file(p);
        }
    }
}

fn temp_path_for(dest: &Path, attempt: u32) -> PathBuf {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = OsString::from(".");
    name.push(
        dest.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("project")),
    );
    name.push(format!(
        ".plumspirit-{}-{}.tmp",
        std::process::id(),
        attempt
    ));
    dir.join(name)
}

/// Saves `doc` to `dest` transactionally:
///
/// 1. check the destination is not a directory;
/// 2. write a new temporary file next to it;
/// 3. flush and `sync_all`, close;
/// 4. reopen it and verify every note, event and setting against `doc`;
/// 5. replace the destination with one rename (on Windows `MoveFileExW` with
///    `MOVEFILE_REPLACE_EXISTING`).
///
/// On any failure the temporary file is removed and the destination is left as
/// it was; the caller keeps the project marked unsaved.
pub fn save_project(
    doc: &PlumDocument,
    dest: &Path,
    control: LupControl<'_>,
) -> Result<SaveReport, LupError> {
    if dest.is_dir() {
        return Err(LupError::Io {
            stage: Some(SaveStage::CreateTemp),
            kind: io::ErrorKind::InvalidInput,
            message: "the destination is a folder".into(),
        });
    }
    let started = Instant::now();
    let mut guard = TempGuard { path: None };
    let mut file = None;
    for attempt in 0..16 {
        let p = temp_path_for(dest, attempt);
        match OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&p)
        {
            Ok(f) => {
                guard.path = Some(p);
                file = Some(f);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(LupError::io(Some(SaveStage::CreateTemp), e)),
        }
    }
    let Some(file) = file else {
        return Err(LupError::Io {
            stage: Some(SaveStage::CreateTemp),
            kind: io::ErrorKind::AlreadyExists,
            message: "too many leftover temporary files".into(),
        });
    };
    let temp = guard.path.clone().expect("set with file");
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let write = write_project(doc, &mut w, control)?;
    let file = w
        .into_inner()
        .map_err(|e| LupError::io(Some(SaveStage::Flush), e.into_error()))?;
    file.sync_all()
        .map_err(|e| LupError::io(Some(SaveStage::Flush), e))?;
    drop(file);
    let write_duration = started.elapsed();

    let v_start = Instant::now();
    let f = File::open(&temp).map_err(|e| LupError::io(Some(SaveStage::Verify), e))?;
    let len = f.metadata().ok().map(|m| m.len());
    if len != Some(write.bytes) {
        return Err(LupError::Verification {
            detail: "the file size differs from what was written".into(),
        });
    }
    verify_project(doc, io::BufReader::with_capacity(1 << 20, f), len)?;
    let verify_duration = v_start.elapsed();

    fs::rename(&temp, dest).map_err(|e| LupError::io(Some(SaveStage::Replace), e))?;
    guard.path = None;
    #[cfg(unix)]
    if let Some(dir) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Make the rename durable (best effort).
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(SaveReport {
        path: dest.to_path_buf(),
        write,
        write_duration,
        verify_duration,
    })
}

/// B1-0163: track sections encoded and parsed on workers give the same file,
/// the same loaded project and the same first error as one thread.
#[cfg(test)]
mod worker_tests {
    use super::*;
    use crate::midi::test_smf::{mutations, rich};
    use crate::model::clip::test_support::{base, clipped};

    fn docs() -> Vec<PlumDocument> {
        let mut out = vec![base(), clipped()];
        let mut g = crate::fixtures::generate(crate::fixtures::FixtureShape::Dense, 30_000, 960, 3);
        g.add_track().unwrap();
        out.push(g);
        out.extend(
            std::iter::once(rich())
                .chain(mutations(&rich(), 60))
                .filter_map(|f| crate::midi::import_bytes(&f).ok().map(|(d, _)| d)),
        );
        out
    }

    fn write(
        doc: &PlumDocument,
        threads: usize,
        window: u64,
    ) -> Result<(Vec<u8>, LupWriteStats), LupError> {
        let mut cur = io::Cursor::new(Vec::new());
        let st = write_project_threads(doc, &mut cur, LupControl::default(), threads, window)?;
        Ok((cur.into_inner(), st))
    }

    type Loaded = Result<(u64, Vec<u8>, u64, u64, Vec<String>), LupError>;

    fn load(bytes: &[u8], threads: usize, window: u64) -> Loaded {
        let (d, r) = read_project_threads(
            bytes,
            Some(bytes.len() as u64),
            LupControl::default(),
            threads,
            window,
        )?;
        let again = write(&d, 0, 0)?.0;
        Ok((d.export_fingerprint(), again, r.notes, r.events, r.warnings))
    }

    #[test]
    fn workers_write_load_and_verify_projects_like_one_thread() {
        let settings = [(1, crate::midi::workers::WINDOW), (2, 1), (3, 200)];
        for doc in docs() {
            let (bytes, stats) = write(&doc, 0, 0).expect("written");
            for &(t, w) in &settings {
                assert_eq!(
                    write(&doc, t, w).as_ref(),
                    Ok(&(bytes.clone(), stats.clone()))
                );
            }
            let one = load(&bytes, 0, 0);
            assert_eq!(one.as_ref().map(|l| l.0), Ok(doc.export_fingerprint()));
            assert_eq!(
                verify_project_threads(&doc, &bytes[..], Some(bytes.len() as u64), 0, 0),
                Ok(())
            );
            for &(t, w) in &settings {
                assert_eq!(load(&bytes, t, w), one, "{t} worker(s), window {w}");
                assert_eq!(
                    verify_project_threads(&doc, &bytes[..], Some(bytes.len() as u64), t, w),
                    Ok(())
                );
            }
            // Damaged copies: the same outcome either way.
            for bad in mutations(&bytes, 40) {
                let len = Some(bad.len() as u64);
                let one = load(&bad, 0, 0);
                let v1 = verify_project_threads(&doc, &bad[..], len, 0, 0);
                for &(t, w) in &settings {
                    assert_eq!(load(&bad, t, w), one, "damaged, {t} worker(s), window {w}");
                    assert_eq!(verify_project_threads(&doc, &bad[..], len, t, w), v1);
                }
            }
        }
    }

    /// B1-0163 found by the mutations above: a meta type from 0x80 and an
    /// unmatched key above 127 in a 256-key project import, so a project
    /// must also save and reopen with them.
    #[test]
    fn projects_with_any_meta_type_or_wide_unmatched_keys_save_and_reopen() {
        use crate::midi::test_smf::{EOT, chunk, smf};
        use crate::model::project::KeyRange;
        let f = smf(
            1,
            &[chunk(
                b"MTrk",
                &[
                    (0, &[0xFF, 0x9A, 0x01, 0x42]),
                    (0, &[0x90, 0xC8, 0x40]),
                    (5, EOT),
                ],
            )],
        );
        let (doc, _) = crate::midi::import_bytes_with(&f, KeyRange::Extended256).expect("imports");
        assert!(doc.preserved_event_count() >= 2);
        let (bytes, _) = write(&doc, 0, 0).expect("saved");
        verify_project_threads(&doc, &bytes[..], Some(bytes.len() as u64), 0, 0).expect("verifies");
        let (back, _) = project_from_bytes(&bytes).expect("reopens");
        assert_eq!(back.export_fingerprint(), doc.export_fingerprint());
    }

    #[test]
    fn a_duplicate_track_id_is_found_in_order_with_workers() {
        let doc = base();
        let (mut bytes, _) = write(&doc, 0, 0).unwrap();
        // Give track 2 the id of track 1 by rewriting the id varint of the
        // second TRAK section (ids 0 / 1 / 2 are one byte each) and fixing
        // its checksum.
        let mut sections = Vec::new();
        let mut off = HEADER_SIZE as usize;
        while off + 16 <= bytes.len() {
            let len = u64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap()) as usize;
            sections.push((
                off,
                [bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]],
                len,
            ));
            off += 16 + len + 4;
        }
        let tracks: Vec<_> = sections.iter().filter(|s| s.1 == TAG_TRACK).collect();
        let (o1, _, _) = *tracks[0];
        let (o2, tag, len) = *tracks[2];
        bytes[o2 + 16] = bytes[o1 + 16];
        let flags = u32::from_le_bytes(bytes[o2 + 4..o2 + 8].try_into().unwrap());
        let sum = section_checksum(
            tag,
            flags,
            len as u64,
            Crc32::of(&bytes[o2 + 16..o2 + 16 + len]),
        );
        bytes[o2 + 16 + len..o2 + 20 + len].copy_from_slice(&sum.to_le_bytes());
        let one = load(&bytes, 0, 0);
        assert!(matches!(&one, Err(LupError::Malformed { .. })), "{one:?}");
        assert_eq!(load(&bytes, 2, 1), one);
        assert_eq!(load(&bytes, 3, crate::midi::workers::WINDOW), one);
    }
}
