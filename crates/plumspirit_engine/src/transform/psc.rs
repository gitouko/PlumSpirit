//! PlumSpirit Score files (`.psc`, contract §31–36, §146, §156).
//!
//! A score is reusable musical / event content — never tool configuration (that
//! is a preset). Timing is relative (the earliest note or event is tick 0) at the
//! score's own PPQN, so a score can be inserted anywhere in any project.
//!
//! Binary layout (little endian, varints = LEB128 from `crate::codec`):
//!
//! ```text
//! magic   "PLUMSCR\x1A"            8 bytes
//! version u16 (1; 2 = pitches 128–255, v0.9 256-Key Mode)  2
//! flags   u16 (= 0, reserved)       2
//! ppq     u32                       4
//! section*  tag [u8;4] + length u32 + payload
//!   "META"  count, (key, value)*                    UTF-8 strings, sorted keys
//!   "SUBS"  count, (id, channel u8, name)*          subchannels the notes use
//!   "GRPS"  count, (id, parent, name)*              note groups (0 = none)
//!   "NOTE"  count, (Δstart, dur, pitch, vel, rel, ch|flags<<4, sub index, group)*
//!   "AUTO"  count, (kind u8, cc u8, channel u8, n, (Δtick, value)*)*
//! "CRC "    length 4, CRC-32 of every byte before this section
//! ```
//!
//! Parsing is defensive: the CRC is checked first, every declared count is
//! checked against the bytes that remain (each item needs at least a few bytes)
//! before anything is allocated, strings are bounded, unknown sections are
//! skipped (forward compatibility), a newer major version is refused with a
//! sentence. Nothing in a score is ever executed.

use super::{TNote, TransformError};
use crate::codec::{Crc32, get_uvarint, put_uvarint};

pub const MAGIC: &[u8; 8] = b"PLUMSCR\x1A";
/// Version of a score whose pitches are all 0–127 (what v0.7 / v0.8 write
/// and read).
pub const VERSION: u16 = 1;
/// Version of a score holding a pitch 128–255 (v0.9 256-Key Mode): the same
/// layout; older PlumSpirit versions refuse it as newer instead of misreading
/// it.
pub const VERSION_256: u16 = 2;
/// Longest string (name, meta value) in bytes.
pub const MAX_STRING: usize = 1024;
/// Most notes a score may declare.
pub const MAX_NOTES: u64 = 50_000_000;
/// Most subchannels / groups / lanes.
pub const MAX_SUBS: u64 = 1 << 20;
pub const MAX_GROUPS: u64 = 1 << 24;
pub const MAX_LANES: u64 = 4096;
pub const MAX_POINTS: u64 = 50_000_000;
/// Largest file accepted (bytes).
pub const MAX_FILE: u64 = 2 << 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScoreSub {
    /// Subchannel id in the project the score came from (informational).
    pub id: u32,
    pub channel: u8,
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScoreNote {
    pub start: u64,
    pub dur: u64,
    pub pitch: u8,
    pub vel: u8,
    pub rel: u8,
    pub ch: u8,
    pub flags: u8,
    /// Index into [`Score::subs`].
    pub sub: u32,
    /// Group id (0 = none).
    pub group: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScoreGroup {
    pub id: u32,
    pub parent: u32,
    pub name: Option<String>,
}

/// Automation lane kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaneKind {
    /// Control Change `cc`.
    Cc(u8),
    PitchBend,
    /// Note velocities as a lane (PlumSpirit velocity graph).
    Velocity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScoreLane {
    pub kind: LaneKind,
    pub channel: u8,
    /// (relative tick, value) sorted by tick; value 0–127 (CC, velocity) or
    /// 0–16383 (pitch bend).
    pub points: Vec<(u64, u16)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Score {
    pub ppq: u32,
    /// Sorted by key.
    pub meta: Vec<(String, String)>,
    pub subs: Vec<ScoreSub>,
    pub groups: Vec<ScoreGroup>,
    /// Sorted (start, pitch, …).
    pub notes: Vec<ScoreNote>,
    pub lanes: Vec<ScoreLane>,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(MAX_STRING)];
    // Never cut inside a UTF-8 sequence.
    let mut n = b.len();
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    put_uvarint(out, n as u64);
    out.extend_from_slice(&b[..n]);
}

fn section(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

impl Score {
    /// Canonical order (deterministic bytes).
    pub fn normalize(&mut self) {
        self.meta.sort();
        self.notes.sort();
        for l in &mut self.lanes {
            l.points.sort();
        }
        self.lanes.sort_by_key(|l| (l.kind, l.channel));
        self.groups.sort_by_key(|g| g.id);
    }

    /// The format version this score is written with: [`VERSION_256`] when a
    /// pitch is above 127, else [`VERSION`] (byte for byte the v0.8 file).
    pub fn version(&self) -> u16 {
        if self
            .notes
            .iter()
            .any(|n| n.pitch > crate::model::note::MAX_PITCH)
        {
            VERSION_256
        } else {
            VERSION
        }
    }

    /// Highest pitch of the score (0 when it has no notes).
    pub fn max_pitch(&self) -> u8 {
        self.notes.iter().map(|n| n.pitch).max().unwrap_or(0)
    }

    pub fn meta_value(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Length in ticks (last note end / last automation point).
    pub fn length(&self) -> u64 {
        let n = self
            .notes
            .iter()
            .map(|n| n.start.saturating_add(n.dur))
            .max()
            .unwrap_or(0);
        let a = self
            .lanes
            .iter()
            .filter_map(|l| l.points.last().map(|p| p.0))
            .max()
            .unwrap_or(0);
        n.max(a)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut s = self.clone();
        s.normalize();
        let mut out = Vec::with_capacity(64 + s.notes.len() * 9);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&s.version().to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&s.ppq.to_le_bytes());
        let mut p = Vec::new();
        put_uvarint(&mut p, s.meta.len() as u64);
        for (k, v) in &s.meta {
            put_str(&mut p, k);
            put_str(&mut p, v);
        }
        section(&mut out, b"META", &p);
        p.clear();
        put_uvarint(&mut p, s.subs.len() as u64);
        for sb in &s.subs {
            put_uvarint(&mut p, u64::from(sb.id));
            p.push(sb.channel.min(15));
            put_str(&mut p, sb.name.as_deref().unwrap_or(""));
        }
        section(&mut out, b"SUBS", &p);
        p.clear();
        put_uvarint(&mut p, s.groups.len() as u64);
        for g in &s.groups {
            put_uvarint(&mut p, u64::from(g.id));
            put_uvarint(&mut p, u64::from(g.parent));
            put_str(&mut p, g.name.as_deref().unwrap_or(""));
        }
        section(&mut out, b"GRPS", &p);
        p.clear();
        put_uvarint(&mut p, s.notes.len() as u64);
        let mut prev = 0u64;
        for n in &s.notes {
            put_uvarint(&mut p, n.start - prev);
            prev = n.start;
            put_uvarint(&mut p, n.dur);
            p.push(n.pitch);
            p.push(n.vel.clamp(1, 127));
            p.push(n.rel.min(127));
            p.push((n.ch & 0x0F) | ((n.flags & 0x0F) << 4));
            put_uvarint(&mut p, u64::from(n.sub));
            put_uvarint(&mut p, u64::from(n.group));
        }
        section(&mut out, b"NOTE", &p);
        p.clear();
        put_uvarint(&mut p, s.lanes.len() as u64);
        for l in &s.lanes {
            let (k, cc) = match l.kind {
                LaneKind::Cc(c) => (0u8, c),
                LaneKind::PitchBend => (1, 0),
                LaneKind::Velocity => (2, 0),
            };
            p.push(k);
            p.push(cc.min(127));
            p.push(l.channel.min(15));
            put_uvarint(&mut p, l.points.len() as u64);
            let mut prev = 0u64;
            for &(t, v) in &l.points {
                put_uvarint(&mut p, t - prev);
                prev = t;
                put_uvarint(&mut p, u64::from(v));
            }
        }
        section(&mut out, b"AUTO", &p);
        let crc = Crc32::of(&out);
        section(&mut out, b"CRC ", &crc.to_le_bytes());
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Score, String> {
        let bad = |what: &str| format!("The score file is damaged ({what}).");
        if data.len() as u64 > MAX_FILE {
            return Err("The score file is too large.".into());
        }
        if data.len() < 16 + 12 || &data[..8] != MAGIC {
            return Err("This file is not a PlumSpirit score (.psc).".into());
        }
        let version = u16::from_le_bytes([data[8], data[9]]);
        if version > VERSION_256 {
            return Err(format!(
                "This score was saved by a newer PlumSpirit (score version {version}); update PlumSpirit to open it."
            ));
        }
        if version == 0 {
            return Err(bad("version 0"));
        }
        // CRC: the last section must be "CRC " over everything before it.
        let crc_at = data.len() - 12;
        if &data[crc_at..crc_at + 4] != b"CRC "
            || data[crc_at + 4..crc_at + 8] != 4u32.to_le_bytes()
        {
            return Err(bad("missing checksum"));
        }
        let stored = u32::from_le_bytes(data[crc_at + 8..].try_into().expect("4 bytes"));
        if Crc32::of(&data[..crc_at]) != stored {
            return Err(bad("checksum mismatch"));
        }
        let ppq = u32::from_le_bytes(data[12..16].try_into().expect("4 bytes"));
        if ppq == 0 || ppq > 0x7FFF {
            return Err(bad("timing resolution"));
        }
        let mut s = Score {
            ppq,
            ..Score::default()
        };
        let mut pos = 16usize;
        while pos < crc_at {
            if pos + 8 > crc_at {
                return Err(bad("truncated section"));
            }
            let tag: [u8; 4] = data[pos..pos + 4].try_into().expect("4");
            let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4")) as usize;
            pos += 8;
            if len > crc_at - pos {
                return Err(bad("section length"));
            }
            let body = &data[pos..pos + len];
            pos += len;
            match &tag {
                b"META" => s.meta = parse_meta(body).ok_or_else(|| bad("metadata"))?,
                b"SUBS" => s.subs = parse_subs(body).ok_or_else(|| bad("subchannels"))?,
                b"GRPS" => s.groups = parse_groups(body).ok_or_else(|| bad("groups"))?,
                b"NOTE" => {
                    s.notes = parse_notes(body, version).ok_or_else(|| bad("notes"))?;
                }
                b"AUTO" => s.lanes = parse_lanes(body).ok_or_else(|| bad("automation"))?,
                // Unknown sections from newer minor versions are skipped.
                _ => {}
            }
        }
        if s.notes
            .iter()
            .any(|n| n.sub as usize >= s.subs.len().max(1))
        {
            return Err(bad("subchannel index"));
        }
        if s.subs.is_empty() && !s.notes.is_empty() {
            return Err(bad("no subchannel table"));
        }
        Ok(s)
    }

    /// Builds a score from notes (any order) with their source subchannel
    /// tables: `subs_of(track slot, sub id) → (channel, name)`. Timing becomes
    /// relative to the earliest note.
    pub fn from_notes<F: Fn(u16, u32) -> (u8, Option<String>)>(
        notes: &[TNote],
        ppq: u32,
        subs_of: F,
    ) -> Score {
        let origin = notes.iter().map(|n| n.start).min().unwrap_or(0);
        let mut keys: Vec<(u16, u32)> = notes.iter().map(|n| (n.track, n.sub)).collect();
        keys.sort_unstable();
        keys.dedup();
        let subs: Vec<ScoreSub> = keys
            .iter()
            .map(|&(t, id)| {
                let (channel, name) = subs_of(t, id);
                ScoreSub { id, channel, name }
            })
            .collect();
        let mut groups: Vec<u32> = notes.iter().map(|n| n.group).filter(|g| *g != 0).collect();
        groups.sort_unstable();
        groups.dedup();
        let mut s = Score {
            ppq,
            meta: vec![
                (
                    "created-by".into(),
                    format!("PlumSpirit {}", crate::VERSION),
                ),
                ("notes".into(), notes.len().to_string()),
            ],
            subs,
            groups: groups
                .into_iter()
                .map(|id| ScoreGroup {
                    id,
                    parent: 0,
                    name: None,
                })
                .collect(),
            notes: notes
                .iter()
                .map(|n| ScoreNote {
                    start: n.start - origin,
                    dur: n.dur,
                    pitch: n.pitch,
                    vel: n.vel,
                    rel: n.rel,
                    ch: n.ch,
                    flags: n.flags,
                    sub: keys.binary_search(&(n.track, n.sub)).unwrap_or(0) as u32,
                    group: n.group,
                })
                .collect(),
            lanes: Vec::new(),
        };
        s.normalize();
        s
    }
}

fn get_str(b: &[u8], pos: &mut usize) -> Option<String> {
    let n = get_uvarint(b, pos)? as usize;
    if n > MAX_STRING || n > b.len() - *pos {
        return None;
    }
    let s = std::str::from_utf8(&b[*pos..*pos + n]).ok()?.to_string();
    *pos += n;
    Some(s)
}

/// A declared count, refused unless at least `min_bytes` per item remain.
fn count(b: &[u8], pos: &mut usize, max: u64, min_bytes: usize) -> Option<usize> {
    let n = get_uvarint(b, pos)?;
    if n > max || n.saturating_mul(min_bytes as u64) > (b.len() - *pos) as u64 {
        return None;
    }
    Some(n as usize)
}

fn parse_meta(b: &[u8]) -> Option<Vec<(String, String)>> {
    let mut pos = 0;
    let n = count(b, &mut pos, 4096, 2)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push((get_str(b, &mut pos)?, get_str(b, &mut pos)?));
    }
    Some(v)
}

fn parse_subs(b: &[u8]) -> Option<Vec<ScoreSub>> {
    let mut pos = 0;
    let n = count(b, &mut pos, MAX_SUBS, 3)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let id = u32::try_from(get_uvarint(b, &mut pos)?).ok()?;
        let channel = *b.get(pos)?;
        pos += 1;
        if channel > 15 {
            return None;
        }
        let name = get_str(b, &mut pos)?;
        v.push(ScoreSub {
            id,
            channel,
            name: (!name.is_empty()).then_some(name),
        });
    }
    Some(v)
}

fn parse_groups(b: &[u8]) -> Option<Vec<ScoreGroup>> {
    let mut pos = 0;
    let n = count(b, &mut pos, MAX_GROUPS, 3)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let id = u32::try_from(get_uvarint(b, &mut pos)?).ok()?;
        let parent = u32::try_from(get_uvarint(b, &mut pos)?).ok()?;
        let name = get_str(b, &mut pos)?;
        v.push(ScoreGroup {
            id,
            parent,
            name: (!name.is_empty()).then_some(name),
        });
    }
    Some(v)
}

/// Notes of a score of `version` (pitches above 127 only in version 2).
fn parse_notes(b: &[u8], version: u16) -> Option<Vec<ScoreNote>> {
    let top = if version >= VERSION_256 {
        crate::model::note::MAX_PITCH_256
    } else {
        crate::model::note::MAX_PITCH
    };
    let mut pos = 0;
    let n = count(b, &mut pos, MAX_NOTES, 8)?;
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    let mut t = 0u64;
    for _ in 0..n {
        t = t.checked_add(get_uvarint(b, &mut pos)?)?;
        let dur = get_uvarint(b, &mut pos)?;
        if dur == 0 || t.checked_add(dur).is_none() || pos + 4 > b.len() {
            return None;
        }
        let (pitch, vel, rel, cf) = (b[pos], b[pos + 1], b[pos + 2], b[pos + 3]);
        pos += 4;
        if pitch > top || vel == 0 || vel > 127 || rel > 127 {
            return None;
        }
        let sub = u32::try_from(get_uvarint(b, &mut pos)?).ok()?;
        let group = u32::try_from(get_uvarint(b, &mut pos)?).ok()?;
        v.push(ScoreNote {
            start: t,
            dur,
            pitch,
            vel,
            rel,
            ch: cf & 0x0F,
            flags: cf >> 4,
            sub,
            group,
        });
    }
    (pos == b.len()).then_some(v)
}

fn parse_lanes(b: &[u8]) -> Option<Vec<ScoreLane>> {
    let mut pos = 0;
    let n = count(b, &mut pos, MAX_LANES, 4)?;
    let mut v = Vec::with_capacity(n);
    let mut total = 0u64;
    for _ in 0..n {
        if pos + 3 > b.len() {
            return None;
        }
        let (k, cc, ch) = (b[pos], b[pos + 1], b[pos + 2]);
        pos += 3;
        let kind = match k {
            0 if cc <= 127 => LaneKind::Cc(cc),
            1 => LaneKind::PitchBend,
            2 => LaneKind::Velocity,
            _ => return None,
        };
        if ch > 15 {
            return None;
        }
        let np = count(b, &mut pos, MAX_POINTS, 2)?;
        total += np as u64;
        if total > MAX_POINTS {
            return None;
        }
        let max = if kind == LaneKind::PitchBend {
            16383
        } else {
            127
        };
        let mut pts = Vec::new();
        pts.try_reserve_exact(np).ok()?;
        let mut t = 0u64;
        for _ in 0..np {
            t = t.checked_add(get_uvarint(b, &mut pos)?)?;
            let val = get_uvarint(b, &mut pos)?;
            if val > max {
                return None;
            }
            pts.push((t, val as u16));
        }
        v.push(ScoreLane {
            kind,
            channel: ch,
            points: pts,
        });
    }
    (pos == b.len()).then_some(v)
}

/// How score subchannels map to the destination track's subchannels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubMapping {
    /// Each score subchannel goes to a destination subchannel with the same MIDI
    /// channel (each destination used once), creating new ones when missing.
    ByChannel,
    /// Every score subchannel becomes a new subchannel of the destination.
    CreateNew,
    /// Same id when the destination has it, otherwise a new subchannel.
    ById,
    /// Everything into one destination subchannel (explicit flattening).
    Single(u32),
}

pub const MAPPING_NAMES: &[&str] = &[
    "Match MIDI channels (create missing)",
    "Create new subchannels",
    "Keep subchannel numbers",
    "All into the active subchannel",
];

/// A planned insertion: destination subchannel per score subchannel and the
/// subchannels to create (id, channel, name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubPlan {
    pub map: Vec<u32>,
    pub create: Vec<(u32, u8, Option<String>)>,
}

/// Plans the subchannel mapping against `dest` = (id, channel) in table order.
/// Deterministic: the same score and table always give the same plan.
pub fn plan_subs(score: &Score, dest: &[(u32, u8)], mapping: SubMapping) -> SubPlan {
    let mut next_id = dest.iter().map(|d| d.0).max().map_or(0, |m| m + 1);
    let mut create = Vec::new();
    let mut alloc = |ch: u8, name: &Option<String>, create: &mut Vec<(u32, u8, Option<String>)>| {
        let id = next_id;
        next_id += 1;
        create.push((id, ch, name.clone()));
        id
    };
    let mut used = vec![false; dest.len()];
    let mut map = Vec::with_capacity(score.subs.len());
    for s in &score.subs {
        let id = match mapping {
            SubMapping::Single(id) => id,
            SubMapping::CreateNew => alloc(s.channel, &s.name, &mut create),
            SubMapping::ById => match dest.iter().find(|d| d.0 == s.id) {
                Some(d) => d.0,
                None => alloc(s.channel, &s.name, &mut create),
            },
            SubMapping::ByChannel => {
                match dest
                    .iter()
                    .enumerate()
                    .find(|(i, d)| d.1 == s.channel && !used[*i])
                {
                    Some((i, d)) => {
                        used[i] = true;
                        d.0
                    }
                    None => alloc(s.channel, &s.name, &mut create),
                }
            }
        };
        map.push(id);
    }
    SubPlan { map, create }
}

/// Scales a score tick to `ppq` (exact rational, rounded to nearest).
pub fn rescale(tick: u64, from: u32, to: u64) -> Option<u64> {
    if u64::from(from) == to {
        return Some(tick);
    }
    let v = (u128::from(tick) * u128::from(to) + u128::from(from) / 2) / u128::from(from);
    u64::try_from(v).ok()
}

/// The score's notes placed at `anchor` (destination ticks, `ppq`), transposed
/// by `semis`, mapped through `plan`, as platform notes of track slot 0, for a
/// project of key range `keys`. Notes that would leave its keys (0–127, or
/// 0–255 in 256-Key Mode) or overflow are refused (error), never dropped or
/// clamped.
pub fn place(
    score: &Score,
    anchor: u64,
    ppq: u64,
    semis: i32,
    plan: &SubPlan,
    keys: crate::model::project::KeyRange,
) -> Result<Vec<TNote>, TransformError> {
    let top = i32::from(keys.max_pitch());
    let mut out = Vec::new();
    out.try_reserve_exact(score.notes.len())
        .map_err(|_| TransformError::TooLarge("Not enough memory to insert the score.".into()))?;
    for n in &score.notes {
        let start = rescale(n.start, score.ppq, ppq)
            .and_then(|s| s.checked_add(anchor))
            .ok_or(TransformError::Invalid(
                "The score would end beyond the largest tick.".into(),
            ))?;
        let end = rescale(n.start + n.dur, score.ppq, ppq)
            .and_then(|e| e.checked_add(anchor))
            .ok_or(TransformError::Invalid(
                "The score would end beyond the largest tick.".into(),
            ))?;
        let p = i32::from(n.pitch) + semis;
        if !(0..=top).contains(&p) {
            return Err(TransformError::Invalid(if semis == 0 {
                format!(
                    "The score reaches pitch {}, above this project's highest key ({top}); transpose it down or turn on 256-Key Mode.",
                    score.max_pitch()
                )
            } else {
                format!("Transposed by {semis}, the score would leave the pitch range 0–{top}.")
            }));
        }
        out.push(TNote {
            start,
            dur: (end - start).max(1),
            sub: plan.map.get(n.sub as usize).copied().unwrap_or(0),
            group: n.group,
            track: 0,
            pitch: p as u8,
            vel: n.vel,
            rel: n.rel,
            ch: n.ch,
            flags: n.flags & 1,
        });
    }
    out.sort_by_key(|n| (n.start, n.pitch));
    Ok(out)
}

/// Events edits that write the score's controller lanes (Control Change and
/// Pitch Bend) into `track` at `anchor`, one per lane, prepared in sequence so
/// they apply one after the other inside one undo group. Velocity lanes are not
/// events (see [`apply_velocity_lanes`]). `budget` bounds the points per lane.
pub fn lane_edits<'a>(
    track: impl Into<crate::editor::HostView<'a>>,
    score: &Score,
    anchor: u64,
    ppq: u64,
    budget: usize,
) -> Result<Vec<crate::editor::EventsEdit>, crate::editor::EditError> {
    use crate::editor::controllers::{ControllerKind, WritePolicy, prepare_points};
    let mut work = crate::editor::EventsWork::of(track.into());
    let mut out = Vec::new();
    for lane in &score.lanes {
        let kind = match lane.kind {
            LaneKind::Cc(n) => ControllerKind::Cc(n),
            LaneKind::PitchBend => ControllerKind::PitchBend,
            LaneKind::Velocity => continue,
        };
        let mut pts: Vec<(u64, u16)> = Vec::with_capacity(lane.points.len());
        for &(t, v) in &lane.points {
            let tick = rescale(t, score.ppq, ppq)
                .and_then(|t| t.checked_add(anchor))
                .ok_or(crate::editor::EditError::TickOverflow {
                    operation: "Insert Score",
                })?;
            // One point per tick (the last one wins, like playback).
            match pts.last_mut() {
                Some(last) if last.0 == tick => last.1 = v,
                _ => pts.push((tick, v)),
            }
        }
        let edit = prepare_points(
            work.view(),
            kind,
            lane.channel,
            &pts,
            WritePolicy::Merge,
            budget,
        )?;
        if !work.apply(&edit) {
            return Err(crate::editor::EditError::Stale);
        }
        out.push(edit);
    }
    Ok(out)
}

/// Velocity lanes of a score applied to its own placed notes: each note takes
/// the lane value at or before its start (step), relative to `anchor`.
pub fn apply_velocity_lanes(score: &Score, notes: &mut [TNote], anchor: u64, ppq: u64) {
    for lane in score.lanes.iter().filter(|l| l.kind == LaneKind::Velocity) {
        if lane.points.is_empty() {
            continue;
        }
        for n in notes.iter_mut() {
            let rel = n.start.saturating_sub(anchor);
            let i = lane
                .points
                .partition_point(|p| rescale(p.0, score.ppq, ppq).is_some_and(|t| t <= rel));
            if i > 0 {
                n.vel = lane.points[i - 1].1.clamp(1, 127) as u8;
            }
        }
    }
}
