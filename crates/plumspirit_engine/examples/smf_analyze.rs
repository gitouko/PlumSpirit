//! v1.0-Launch L-C01: a byte-level and semantic Standard MIDI File analyzer
//! (developer tool), and single-factor variants of a file for player
//! compatibility checks (L-C02, Kiva).
//!
//! ```text
//! cargo run -p plumspirit_engine --example smf_analyze -- song.mid            # report
//! cargo run -p plumspirit_engine --example smf_analyze -- song.mid --json     # one JSON object
//! cargo run -p plumspirit_engine --example smf_analyze -- project.lup         # exports in memory
//! cargo run -p plumspirit_engine --example smf_analyze -- song.mid --variants OUT_DIR
//! ```
//!
//! The reader here is its own (it does not use the importer), so it can
//! disagree with it: it walks every chunk and event and reports the header
//! (format, track count, division), every MTrk's offset and length, VLQ
//! sizes (and non-minimal or over-long ones), running-status use, meta
//! lengths, Note On / Off pairing per track, channel and key (orphan Note
//! Offs, same-key overlaps, notes left hanging at End of Track, zero-length
//! notes, same-tick On-before-Off), a raw Note On velocity histogram, Note
//! Off kinds and release velocities, controllers / programs / pitch bend /
//! pressure by channel, absolute ticks, End of Track, tempo / time / key
//! signatures, names and text, `FF 7F` sequencer-specific records, SysEx, and
//! anomalies with their byte offsets.
//!
//! "Raw vs decoded": the file is also imported by PlumSpirit and the decoded
//! notes (count, velocity histogram, channels) are compared with the raw
//! ones. For a `.lup` project the bytes are PlumSpirit's own export of it
//! ("plan vs reparsed output"): the project's notes are compared with what
//! the exported bytes contain.
//!
//! `--variants DIR` writes single-factor variants of the file (each differs
//! from the input in one respect; `identity` re-encodes it unchanged and must
//! equal it byte for byte) with a manifest of SHA-256 sums, so a player crash
//! can be pinned on one factor instead of calling the file corrupt.
//! Exit code: 0 = no anomaly, 2 = anomalies found, 1 = unreadable / error.

use plumspirit_engine::midi;
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::native::{self, LupControl};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

// ---------------------------------------------------------------- reading

/// One event as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// Channel message: full status byte and its 1 or 2 data bytes.
    Channel { status: u8, data: Vec<u8> },
    /// `FF type len data`.
    Meta { kind: u8, data: Vec<u8> },
    /// `F0 len data` or `F7 len data`.
    Sysex { status: u8, data: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Event {
    /// Absolute tick.
    tick: u64,
    /// The delta as stored (for re-encoding).
    delta: u64,
    /// Byte offset of the event's delta in the file.
    offset: usize,
    /// The status byte was omitted (running status).
    running: bool,
    kind: Kind,
}

#[derive(Clone, Debug, Default)]
struct TrackData {
    /// Offset of the `MTrk` id and the chunk's declared length.
    offset: usize,
    length: u32,
    events: Vec<Event>,
    /// Bytes after End of Track inside the chunk.
    after_eot: usize,
}

#[derive(Clone, Debug, Default)]
struct Smf {
    format: u16,
    ntrks: u16,
    division: u16,
    header_len: u32,
    tracks: Vec<TrackData>,
    /// (offset, id, length) of chunks that are not MTrk.
    other_chunks: Vec<(usize, String, u32)>,
    /// Trailing bytes after the last complete chunk.
    trailing: usize,
    anomalies: Vec<String>,
    /// VLQ statistics: longest, non-minimal encodings, over-long (> 4 bytes).
    vlq_max_len: usize,
    vlq_non_minimal: usize,
    vlq_over_long: usize,
    running_status_events: usize,
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    /// A variable-length quantity: (value, bytes used, minimal encoding).
    fn vlq(&mut self) -> Option<(u64, usize, bool)> {
        let start = self.pos;
        let mut v = 0u64;
        loop {
            let c = self.byte()?;
            v = (v << 7) | u64::from(c & 0x7F);
            if c & 0x80 == 0 {
                break;
            }
            if self.pos - start >= 8 {
                return None;
            }
        }
        let n = self.pos - start;
        Some((v, n, n == 1 || self.b[start] != 0x80))
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn data_len(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        _ => 2,
    }
}

impl Smf {
    fn note_vlq(&mut self, n: usize, minimal: bool, at: usize) {
        self.vlq_max_len = self.vlq_max_len.max(n);
        if !minimal {
            self.vlq_non_minimal += 1;
        }
        if n > 4 {
            self.vlq_over_long += 1;
            self.anomalies.push(format!(
                "@{at:#x}: variable-length quantity of {n} bytes (SMF allows 4)"
            ));
        }
    }

    fn parse(bytes: &[u8]) -> Result<Smf, String> {
        let mut s = Smf::default();
        if bytes.len() < 14 || &bytes[0..4] != b"MThd" {
            return Err("not a Standard MIDI File (no MThd header)".into());
        }
        s.header_len = be32(&bytes[4..8]);
        if s.header_len < 6 {
            return Err(format!("MThd length {} (< 6)", s.header_len));
        }
        if s.header_len != 6 {
            s.anomalies.push(format!(
                "MThd length {} (6 expected; extra bytes skipped)",
                s.header_len
            ));
        }
        s.format = u16::from_be_bytes([bytes[8], bytes[9]]);
        s.ntrks = u16::from_be_bytes([bytes[10], bytes[11]]);
        s.division = u16::from_be_bytes([bytes[12], bytes[13]]);
        if s.format > 2 {
            s.anomalies
                .push(format!("format {} (0, 1 or 2 expected)", s.format));
        }
        if s.format == 0 && s.ntrks != 1 {
            s.anomalies
                .push(format!("format 0 with {} tracks declared", s.ntrks));
        }
        if s.division == 0 {
            s.anomalies.push("division 0".into());
        }
        let mut pos = 8 + s.header_len as usize;
        while pos < bytes.len() {
            if bytes.len() - pos < 8 {
                s.trailing = bytes.len() - pos;
                s.anomalies.push(format!(
                    "@{pos:#x}: {} trailing bytes after the last chunk",
                    s.trailing
                ));
                break;
            }
            let id = &bytes[pos..pos + 4];
            let len = be32(&bytes[pos + 4..pos + 8]);
            let body = pos + 8;
            let end = body.saturating_add(len as usize);
            if end > bytes.len() {
                s.anomalies.push(format!(
                    "@{pos:#x}: chunk {} declares {len} bytes, only {} remain",
                    String::from_utf8_lossy(id),
                    bytes.len() - body
                ));
            }
            let end = end.min(bytes.len());
            if id == b"MTrk" {
                let t = s.parse_track(bytes, pos, len, body, end);
                s.tracks.push(t);
            } else {
                s.other_chunks
                    .push((pos, String::from_utf8_lossy(id).into_owned(), len));
            }
            pos = end;
        }
        if s.tracks.len() != usize::from(s.ntrks) {
            s.anomalies.push(format!(
                "header declares {} tracks, the file has {} MTrk chunks",
                s.ntrks,
                s.tracks.len()
            ));
        }
        Ok(s)
    }

    fn parse_track(
        &mut self,
        bytes: &[u8],
        offset: usize,
        length: u32,
        body: usize,
        end: usize,
    ) -> TrackData {
        let mut t = TrackData {
            offset,
            length,
            ..Default::default()
        };
        let ti = self.tracks.len();
        let mut c = Cursor {
            b: &bytes[..end],
            pos: body,
        };
        let mut tick = 0u64;
        let mut running: Option<u8> = None;
        let mut eot = false;
        while c.pos < end {
            let at = c.pos;
            let Some((delta, n, minimal)) = c.vlq() else {
                self.anomalies
                    .push(format!("@{at:#x}: track {ti}: unreadable delta time"));
                break;
            };
            self.note_vlq(n, minimal, at);
            tick += delta;
            let Some(first) = c.byte() else {
                self.anomalies.push(format!(
                    "@{at:#x}: track {ti}: event cut off after its delta"
                ));
                break;
            };
            let (status, was_running) = if first & 0x80 != 0 {
                (first, false)
            } else if let Some(r) = running {
                c.pos -= 1;
                (r, true)
            } else {
                self.anomalies.push(format!(
                    "@{at:#x}: track {ti}: data byte {first:#04x} without a status (no running status)"
                ));
                break;
            };
            let kind = match status {
                0xFF => {
                    running = None;
                    let Some(kind) = c.byte() else { break };
                    let lat = c.pos;
                    let Some((len, n, minimal)) = c.vlq() else {
                        break;
                    };
                    self.note_vlq(n, minimal, lat);
                    let Some(data) = c.take(len as usize) else {
                        self.anomalies.push(format!(
                            "@{at:#x}: track {ti}: meta {kind:#04x} declares {len} bytes past the chunk end"
                        ));
                        break;
                    };
                    Kind::Meta {
                        kind,
                        data: data.to_vec(),
                    }
                }
                0xF0 | 0xF7 => {
                    running = None;
                    let lat = c.pos;
                    let Some((len, n, minimal)) = c.vlq() else {
                        break;
                    };
                    self.note_vlq(n, minimal, lat);
                    let Some(data) = c.take(len as usize) else {
                        self.anomalies.push(format!(
                            "@{at:#x}: track {ti}: SysEx declares {len} bytes past the chunk end"
                        ));
                        break;
                    };
                    Kind::Sysex {
                        status,
                        data: data.to_vec(),
                    }
                }
                0x80..=0xEF => {
                    running = Some(status);
                    let Some(data) = c.take(data_len(status)) else {
                        self.anomalies
                            .push(format!("@{at:#x}: track {ti}: channel message cut off"));
                        break;
                    };
                    if let Some(b) = data.iter().find(|b| **b & 0x80 != 0) {
                        self.anomalies.push(format!(
                            "@{at:#x}: track {ti}: data byte {b:#04x} with the high bit set"
                        ));
                    }
                    Kind::Channel {
                        status,
                        data: data.to_vec(),
                    }
                }
                _ => {
                    self.anomalies.push(format!(
                        "@{at:#x}: track {ti}: system message {status:#04x} inside a track"
                    ));
                    running = None;
                    continue;
                }
            };
            if was_running {
                self.running_status_events += 1;
            }
            if eot {
                self.anomalies
                    .push(format!("@{at:#x}: track {ti}: event after End of Track"));
            }
            let is_eot = matches!(kind, Kind::Meta { kind: 0x2F, .. });
            if let Kind::Meta { kind: 0x2F, data } = &kind
                && !data.is_empty()
            {
                self.anomalies.push(format!(
                    "@{at:#x}: track {ti}: End of Track with {} data bytes",
                    data.len()
                ));
            }
            t.events.push(Event {
                tick,
                delta,
                offset: at,
                running: was_running,
                kind,
            });
            if is_eot {
                eot = true;
                t.after_eot = end - c.pos;
                if t.after_eot > 0 {
                    self.anomalies.push(format!(
                        "@{:#x}: track {ti}: {} bytes after End of Track",
                        c.pos, t.after_eot
                    ));
                }
                break;
            }
        }
        if !eot {
            self.anomalies.push(format!("track {ti}: no End of Track"));
        }
        t
    }
}

// ---------------------------------------------------------------- semantics

struct Stats {
    notes: u64,
    velocity: [u64; 128],
    /// Note Offs: 0x80 (by release velocity) and 0x90 velocity 0.
    off_80: u64,
    off_90_vel0: u64,
    release: BTreeMap<u8, u64>,
    orphan_offs: u64,
    same_key_overlaps: u64,
    hanging: u64,
    zero_length: u64,
    /// A Note On before a Note Off of the same channel / key at one tick.
    on_before_off_same_tick: u64,
    channels: BTreeMap<u8, u64>,
    keys_used: u128,
    controllers: BTreeMap<(u8, u8), u64>,
    programs: BTreeMap<(u8, u8), u64>,
    bends: u64,
    bend_range: Option<(u16, u16)>,
    pressure: u64,
    tempos: Vec<(u64, u32)>,
    time_sigs: Vec<(u64, [u8; 4])>,
    key_sigs: u64,
    texts: BTreeMap<u8, u64>,
    ff7f: Vec<(usize, usize, Vec<u8>)>,
    sysex: u64,
    non_ascii_text: u64,
    first_note: Option<u64>,
    last_note: Option<u64>,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            notes: 0,
            velocity: [0; 128],
            off_80: 0,
            off_90_vel0: 0,
            release: BTreeMap::new(),
            orphan_offs: 0,
            same_key_overlaps: 0,
            hanging: 0,
            zero_length: 0,
            on_before_off_same_tick: 0,
            channels: BTreeMap::new(),
            keys_used: 0,
            controllers: BTreeMap::new(),
            programs: BTreeMap::new(),
            bends: 0,
            bend_range: None,
            pressure: 0,
            tempos: Vec::new(),
            time_sigs: Vec::new(),
            key_sigs: 0,
            texts: BTreeMap::new(),
            ff7f: Vec::new(),
            sysex: 0,
            non_ascii_text: 0,
            first_note: None,
            last_note: None,
        }
    }
}

struct TrackSummary {
    name: Option<String>,
    notes: u64,
    channels: u16,
    eot: Option<u64>,
    events: usize,
}

fn analyze(s: &Smf) -> (Stats, Vec<TrackSummary>) {
    let mut st = Stats::default();
    let mut tracks = Vec::new();
    for t in &s.tracks {
        let mut sum = TrackSummary {
            name: None,
            notes: 0,
            channels: 0,
            eot: None,
            events: t.events.len(),
        };
        // (channel, key) -> start ticks of sounding notes (first in, first out).
        let mut open: BTreeMap<(u8, u8), Vec<u64>> = BTreeMap::new();
        // (channel, key) -> tick of the last Note On (same-tick order).
        let mut last_on: BTreeMap<(u8, u8), u64> = BTreeMap::new();
        for e in &t.events {
            match &e.kind {
                Kind::Channel { status, data } => {
                    let ch = status & 0x0F;
                    *st.channels.entry(ch).or_default() += 1;
                    sum.channels |= 1 << ch;
                    match status & 0xF0 {
                        0x90 if data[1] > 0 => {
                            let key = (ch, data[0]);
                            st.notes += 1;
                            sum.notes += 1;
                            st.velocity[usize::from(data[1])] += 1;
                            st.keys_used |= 1u128 << data[0];
                            st.first_note = Some(st.first_note.map_or(e.tick, |f| f.min(e.tick)));
                            st.last_note = Some(st.last_note.map_or(e.tick, |l| l.max(e.tick)));
                            let v = open.entry(key).or_default();
                            if !v.is_empty() {
                                st.same_key_overlaps += 1;
                            }
                            v.push(e.tick);
                            last_on.insert(key, e.tick);
                        }
                        0x80 | 0x90 => {
                            let key = (ch, data[0]);
                            if status & 0xF0 == 0x80 {
                                st.off_80 += 1;
                                *st.release.entry(data[1]).or_default() += 1;
                            } else {
                                st.off_90_vel0 += 1;
                            }
                            match open.get_mut(&key).filter(|v| !v.is_empty()) {
                                Some(v) => {
                                    let start = v.remove(0);
                                    if start == e.tick {
                                        st.zero_length += 1;
                                        if last_on.get(&key) == Some(&e.tick) && !v.is_empty() {
                                            st.on_before_off_same_tick += 1;
                                        }
                                    }
                                }
                                None => st.orphan_offs += 1,
                            }
                        }
                        0xB0 => *st.controllers.entry((ch, data[0])).or_default() += 1,
                        0xC0 => *st.programs.entry((ch, data[0])).or_default() += 1,
                        0xE0 => {
                            st.bends += 1;
                            let v = u16::from(data[0]) | (u16::from(data[1]) << 7);
                            st.bend_range =
                                Some(st.bend_range.map_or((v, v), |(a, b)| (a.min(v), b.max(v))));
                        }
                        _ => st.pressure += 1,
                    }
                }
                Kind::Meta { kind, data } => match kind {
                    0x2F => sum.eot = Some(e.tick),
                    0x51 if data.len() == 3 => st.tempos.push((
                        e.tick,
                        u32::from(data[0]) << 16 | u32::from(data[1]) << 8 | u32::from(data[2]),
                    )),
                    0x58 if data.len() == 4 => st
                        .time_sigs
                        .push((e.tick, [data[0], data[1], data[2], data[3]])),
                    0x59 => st.key_sigs += 1,
                    0x7F => st.ff7f.push((
                        e.offset,
                        data.len(),
                        data.iter().take(12).copied().collect(),
                    )),
                    0x01..=0x0F => {
                        *st.texts.entry(*kind).or_default() += 1;
                        if data.iter().any(|b| *b >= 0x80) {
                            st.non_ascii_text += 1;
                        }
                        if *kind == 0x03 && sum.name.is_none() {
                            sum.name = Some(String::from_utf8_lossy(data).into_owned());
                        }
                    }
                    _ => {}
                },
                Kind::Sysex { .. } => st.sysex += 1,
            }
        }
        st.hanging += open.values().map(|v| v.len() as u64).sum::<u64>();
        tracks.push(sum);
    }
    (st, tracks)
}

/// The meta lengths SMF fixes; a different one is an anomaly.
fn meta_anomalies(s: &Smf) -> Vec<String> {
    let mut v = Vec::new();
    for (ti, t) in s.tracks.iter().enumerate() {
        for e in &t.events {
            if let Kind::Meta { kind, data } = &e.kind {
                let want = match kind {
                    0x00 => Some(2),
                    0x20 | 0x21 => Some(1),
                    0x51 => Some(3),
                    0x54 => Some(5),
                    0x58 => Some(4),
                    0x59 => Some(2),
                    _ => None,
                };
                if let Some(w) = want
                    && data.len() != w
                {
                    v.push(format!(
                        "@{:#x}: track {ti}: meta {kind:#04x} has {} data bytes ({w} expected)",
                        e.offset,
                        data.len()
                    ));
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------- writing

fn put_vlq(mut v: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 10];
    let mut i = buf.len() - 1;
    buf[i] = (v & 0x7F) as u8;
    v >>= 7;
    while v > 0 {
        i -= 1;
        buf[i] = 0x80 | (v & 0x7F) as u8;
        v >>= 7;
    }
    out.extend_from_slice(&buf[i..]);
}

/// Re-encodes `s`: deltas from the absolute ticks, running status where the
/// event used it (and the status allows it) unless `explicit`.
fn encode(s: &Smf, explicit: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&s.format.to_be_bytes());
    out.extend_from_slice(&(s.tracks.len() as u16).to_be_bytes());
    out.extend_from_slice(&s.division.to_be_bytes());
    for t in &s.tracks {
        let mut body = Vec::new();
        let mut prev = 0u64;
        let mut running: Option<u8> = None;
        for e in &t.events {
            put_vlq(e.tick - prev, &mut body);
            prev = e.tick;
            match &e.kind {
                Kind::Channel { status, data } => {
                    if explicit || !e.running || running != Some(*status) {
                        body.push(*status);
                    }
                    running = Some(*status);
                    body.extend_from_slice(data);
                }
                Kind::Meta { kind, data } => {
                    running = None;
                    body.push(0xFF);
                    body.push(*kind);
                    put_vlq(data.len() as u64, &mut body);
                    body.extend_from_slice(data);
                }
                Kind::Sysex { status, data } => {
                    running = None;
                    body.push(*status);
                    put_vlq(data.len() as u64, &mut body);
                    body.extend_from_slice(data);
                }
            }
        }
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }
    out
}

/// Every single-factor variant: (name, what differs, bytes).
fn variants(s: &Smf) -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let map = |f: &dyn Fn(&mut Event) -> bool| {
        let mut v = s.clone();
        for t in &mut v.tracks {
            t.events.retain_mut(|e| f(e));
        }
        v
    };
    let note_on_vel = |op: fn(u8) -> u8| {
        map(&move |e: &mut Event| {
            if let Kind::Channel { status, data } = &mut e.kind
                && *status & 0xF0 == 0x90
                && data[1] > 0
            {
                data[1] = op(data[1]);
            }
            true
        })
    };
    let mut out = vec![
        (
            "identity",
            "re-encoded unchanged (must equal the input byte for byte)",
            encode(s, false),
        ),
        (
            "vel15",
            "Note On velocities below 15 raised to 15 (Kiva's default Audio Note Threshold)",
            encode(&note_on_vel(|v| v.max(15)), false),
        ),
        (
            "vel64",
            "every Note On velocity 64",
            encode(&note_on_vel(|_| 64), false),
        ),
        (
            "explicit-status",
            "no running status (every channel message has its status byte)",
            encode(s, true),
        ),
        (
            "no-ff7f",
            "every FF 7F sequencer-specific record removed",
            encode(
                &map(&|e: &mut Event| !matches!(e.kind, Kind::Meta { kind: 0x7F, .. })),
                false,
            ),
        ),
        (
            "off-vel64",
            "Note Off (0x80) release velocity 64 instead of the stored one",
            encode(
                &map(&|e: &mut Event| {
                    if let Kind::Channel { status, data } = &mut e.kind
                        && *status & 0xF0 == 0x80
                    {
                        data[1] = 64;
                    }
                    true
                }),
                false,
            ),
        ),
        (
            "off-as-noteon0",
            "Note Offs written as Note On velocity 0",
            encode(
                &map(&|e: &mut Event| {
                    if let Kind::Channel { status, data } = &mut e.kind
                        && *status & 0xF0 == 0x80
                    {
                        *status = 0x90 | (*status & 0x0F);
                        data[1] = 0;
                    }
                    true
                }),
                false,
            ),
        ),
        (
            "ascii-names",
            "bytes >= 0x80 in text / name metas replaced by '?'",
            encode(
                &map(&|e: &mut Event| {
                    if let Kind::Meta {
                        kind: 0x01..=0x0F,
                        data,
                    } = &mut e.kind
                    {
                        for b in data.iter_mut().filter(|b| **b >= 0x80) {
                            *b = b'?';
                        }
                    }
                    true
                }),
                false,
            ),
        ),
    ];
    // One loud note: the first Note On of the file at velocity 100.
    let mut loud = s.clone();
    'find: for t in &mut loud.tracks {
        for e in &mut t.events {
            if let Kind::Channel { status, data } = &mut e.kind
                && *status & 0xF0 == 0x90
                && data[1] > 0
            {
                data[1] = 100;
                break 'find;
            }
        }
    }
    out.push((
        "one-loud",
        "only the first Note On at velocity 100",
        encode(&loud, false),
    ));
    // One control event: Program Change 0 (the General MIDI default, so
    // nothing sounds different) on the first channel used, at tick 0 of the
    // first track — a file whose only channel events are notes gets a
    // non-empty control-event set (Kiva merges control and audio events).
    let mut prog = s.clone();
    let ch = s
        .tracks
        .iter()
        .flat_map(|t| &t.events)
        .find_map(|e| match &e.kind {
            Kind::Channel { status, .. } => Some(status & 0x0F),
            _ => None,
        })
        .unwrap_or(0);
    if let Some(t) = prog.tracks.first_mut() {
        let at = t
            .events
            .iter()
            .position(|e| {
                e.tick > 0 || matches!(e.kind, Kind::Channel { .. } | Kind::Meta { kind: 0x2F, .. })
            })
            .unwrap_or(t.events.len());
        t.events.insert(
            at,
            Event {
                tick: 0,
                delta: 0,
                offset: 0,
                running: false,
                kind: Kind::Channel {
                    status: 0xC0 | ch,
                    data: vec![0],
                },
            },
        );
        // The event after it keeps its own status byte.
        if let Some(next) = t.events.get_mut(at + 1) {
            next.running = false;
        }
    }
    out.push((
        "program0",
        "one Program Change 0 (GM default) at tick 0 - the only non-note channel event",
        encode(&prog, false),
    ));
    // Format 0: every track merged in tick order (stable by track), one EOT.
    let mut f0 = s.clone();
    let mut all: Vec<Event> = s
        .tracks
        .iter()
        .flat_map(|t| {
            t.events
                .iter()
                .filter(|e| !matches!(e.kind, Kind::Meta { kind: 0x2F, .. }))
                .cloned()
        })
        .collect();
    all.sort_by_key(|e| e.tick);
    let end = s
        .tracks
        .iter()
        .flat_map(|t| t.events.last().map(|e| e.tick))
        .max()
        .unwrap_or(0);
    all.push(Event {
        tick: end,
        delta: 0,
        offset: 0,
        running: false,
        kind: Kind::Meta {
            kind: 0x2F,
            data: Vec::new(),
        },
    });
    for e in &mut all {
        e.running = false;
    }
    f0.format = 0;
    f0.tracks = vec![TrackData {
        events: all,
        ..Default::default()
    }];
    out.push((
        "format0",
        "all tracks merged into one (format 0)",
        encode(&f0, false),
    ));
    out
}

// ---------------------------------------------------------------- report

fn sha256_hex(bytes: &[u8]) -> String {
    // FIPS 180-4, for the manifest (std only).
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bits = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn histogram(v: &[u64; 128]) -> String {
    let mut s = String::new();
    for (i, n) in v.iter().enumerate().filter(|(_, n)| **n > 0) {
        let _ = write!(s, "{}{i}:{n}", if s.is_empty() { "" } else { " " });
    }
    if s.is_empty() { "none".into() } else { s }
}

/// The importer's view of the same bytes: (notes, velocity histogram).
fn decoded(bytes: &[u8]) -> Result<(u64, [u64; 128]), String> {
    let (doc, _) = midi::import_bytes(bytes).map_err(|e| e.to_string())?;
    Ok(doc_notes(&doc))
}

fn doc_notes(doc: &PlumDocument) -> (u64, [u64; 128]) {
    let mut h = [0u64; 128];
    let mut n = 0;
    for i in 0..doc.track_count() {
        for pc in doc.placed_contents(i) {
            if pc.placement.muted {
                continue;
            }
            let (lo, hi) = pc.window();
            for note in pc.notes.iter() {
                if note.start_tick >= lo && note.start_tick < hi {
                    h[usize::from(note.attrs.velocity())] += 1;
                    n += 1;
                }
            }
        }
    }
    (n, h)
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn run(args: &[String]) -> Result<bool, String> {
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("usage: smf_analyze FILE.mid|PROJECT.lup [--json] [--variants DIR]")?;
    let json = args.iter().any(|a| a == "--json");
    let p = Path::new(path);
    let (bytes, project) = if native::has_lup_signature(p) {
        let (doc, _) =
            native::load_project(p, LupControl::default()).map_err(|e| e.user_message())?;
        let b = midi::export_to_bytes(&doc).map_err(|e| e.user_message())?;
        (b, Some(doc))
    } else {
        (std::fs::read(p).map_err(|e| format!("{path}: {e}"))?, None)
    };
    let s = Smf::parse(&bytes)?;
    let (st, tracks) = analyze(&s);
    let mut anomalies = s.anomalies.clone();
    anomalies.extend(meta_anomalies(&s));
    if st.orphan_offs > 0 {
        anomalies.push(format!(
            "{} Note Offs without a sounding note",
            st.orphan_offs
        ));
    }
    if st.hanging > 0 {
        anomalies.push(format!(
            "{} notes still sounding at End of Track",
            st.hanging
        ));
    }
    // Raw vs decoded, and (projects) plan vs reparsed.
    let mut checks: Vec<(String, bool)> = Vec::new();
    match decoded(&bytes) {
        Ok((n, h)) => {
            checks.push((
                format!("importer decodes {n} notes, raw has {}", st.notes),
                n == st.notes,
            ));
            checks.push((
                "importer velocity histogram equals the raw one".into(),
                h == st.velocity,
            ));
        }
        Err(e) => checks.push((format!("importer refused the bytes: {e}"), false)),
    }
    if let Some(doc) = &project {
        let (n, h) = doc_notes(doc);
        checks.push((
            format!("project plays {n} notes, the export has {}", st.notes),
            n == st.notes,
        ));
        checks.push((
            "project velocity histogram equals the export's".into(),
            h == st.velocity,
        ));
    }
    for (c, ok) in &checks {
        if !ok {
            anomalies.push(format!("mismatch: {c}"));
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--variants") {
        let dir = args.get(i + 1).ok_or("--variants needs a directory")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("file");
        let mut manifest = String::new();
        let _ = writeln!(
            manifest,
            "# Single-factor variants of {path} (sha256 {})",
            sha256_hex(&bytes)
        );
        for (name, what, b) in variants(&s) {
            if name == "identity" && b != bytes {
                anomalies
                    .push("the re-encoder does not reproduce the input (identity differs)".into());
            }
            let f = format!("{stem}.{name}.mid");
            std::fs::write(Path::new(dir).join(&f), &b).map_err(|e| e.to_string())?;
            let _ = writeln!(
                manifest,
                "{}  {f}  {} bytes — {what}",
                sha256_hex(&b),
                b.len()
            );
        }
        std::fs::write(
            Path::new(dir).join(format!("{stem}.variants.txt")),
            &manifest,
        )
        .map_err(|e| e.to_string())?;
        eprint!("{manifest}");
    }
    let ticks_per = if s.division & 0x8000 == 0 {
        format!("{} ticks per quarter", s.division)
    } else {
        format!("SMPTE {:#06x}", s.division)
    };
    if json {
        let mut o = String::from("{");
        let _ = write!(
            o,
            "\"file\":{},\"bytes\":{},\"sha256\":\"{}\",\"format\":{},\"tracks\":{},\"division\":{},\"notes\":{},\"velocity\":{{",
            json_str(path),
            bytes.len(),
            sha256_hex(&bytes),
            s.format,
            s.tracks.len(),
            s.division,
            st.notes
        );
        let mut first = true;
        for (v, n) in st.velocity.iter().enumerate().filter(|(_, n)| **n > 0) {
            let _ = write!(o, "{}\"{v}\":{n}", if first { "" } else { "," });
            first = false;
        }
        let _ = write!(
            o,
            "}},\"running_status_events\":{},\"ff7f\":{},\"anomalies\":[{}]}}",
            s.running_status_events,
            st.ff7f.len(),
            anomalies
                .iter()
                .map(|a| json_str(a))
                .collect::<Vec<_>>()
                .join(",")
        );
        println!("{o}");
        return Ok(anomalies.is_empty());
    }
    let mut r = String::new();
    let _ = writeln!(
        r,
        "{path}: {} bytes, sha256 {}",
        bytes.len(),
        sha256_hex(&bytes)
    );
    if project.is_some() {
        let _ = writeln!(r, "  (PlumSpirit's export of the project, in memory)");
    }
    let _ = writeln!(
        r,
        "Header: MThd length {}, format {}, {} tracks declared, {ticks_per}",
        s.header_len, s.format, s.ntrks
    );
    for (i, (t, sum)) in s.tracks.iter().zip(&tracks).enumerate() {
        let chans: Vec<String> = (0..16)
            .filter(|c| sum.channels & (1 << c) != 0)
            .map(|c| (c + 1).to_string())
            .collect();
        let _ = writeln!(
            r,
            "  MTrk {i} @{:#x} length {}: {} events, {} notes, channels [{}], EOT tick {}, name {}",
            t.offset,
            t.length,
            sum.events,
            sum.notes,
            chans.join(","),
            sum.eot.map_or("none".into(), |x| x.to_string()),
            sum.name.as_deref().map_or("none".into(), json_str)
        );
    }
    for (off, id, len) in &s.other_chunks {
        let _ = writeln!(r, "  other chunk {id:?} @{off:#x} length {len}");
    }
    let _ = writeln!(
        r,
        "VLQs: longest {} bytes, non-minimal {}, over-long {}; running status on {} events",
        s.vlq_max_len, s.vlq_non_minimal, s.vlq_over_long, s.running_status_events
    );
    let _ = writeln!(
        r,
        "Notes: {} (keys used {}), first at tick {}, last at tick {}",
        st.notes,
        st.keys_used.count_ones(),
        st.first_note.map_or("-".into(), |x| x.to_string()),
        st.last_note.map_or("-".into(), |x| x.to_string())
    );
    let _ = writeln!(r, "Note On velocities (raw): {}", histogram(&st.velocity));
    let _ = writeln!(
        r,
        "Note Offs: {} as 0x80 (release velocities {:?}), {} as Note On velocity 0",
        st.off_80, st.release, st.off_90_vel0
    );
    let _ = writeln!(
        r,
        "Pairing: orphan Note Offs {}, same-key overlaps {}, hanging at EOT {}, zero-length {}, On-before-Off at one tick {}",
        st.orphan_offs,
        st.same_key_overlaps,
        st.hanging,
        st.zero_length,
        st.on_before_off_same_tick
    );
    let _ = writeln!(
        r,
        "Channel events by channel: {:?}",
        st.channels
            .iter()
            .map(|(c, n)| (c + 1, *n))
            .collect::<Vec<_>>()
    );
    let _ = writeln!(
        r,
        "Controllers (channel, cc): {:?}; programs {:?}; pitch bends {} (range {:?}); pressure {}",
        st.controllers
            .iter()
            .map(|((c, k), n)| ((c + 1, *k), *n))
            .collect::<Vec<_>>(),
        st.programs
            .iter()
            .map(|((c, p), n)| ((c + 1, *p), *n))
            .collect::<Vec<_>>(),
        st.bends,
        st.bend_range,
        st.pressure
    );
    let _ = writeln!(
        r,
        "Tempo: {:?} (tick, µs per quarter); time signatures {:?}; key signatures {}",
        st.tempos.iter().take(8).collect::<Vec<_>>(),
        st.time_sigs.iter().take(8).collect::<Vec<_>>(),
        st.key_sigs
    );
    let _ = writeln!(
        r,
        "Text / names by meta type: {:?}; with non-ASCII bytes {}; SysEx {}",
        st.texts, st.non_ascii_text, st.sysex
    );
    let _ = writeln!(r, "FF 7F records: {}", st.ff7f.len());
    for (off, len, head) in st.ff7f.iter().take(8) {
        let _ = writeln!(r, "  @{off:#x} {len} bytes, starts {head:02x?}");
    }
    let _ = writeln!(r, "Checks:");
    for (c, ok) in &checks {
        let _ = writeln!(r, "  [{}] {c}", if *ok { "ok" } else { "MISMATCH" });
    }
    let _ = writeln!(r, "Anomalies: {}", anomalies.len());
    for a in &anomalies {
        let _ = writeln!(r, "  {a}");
    }
    print!("{r}");
    Ok(anomalies.is_empty())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("smf_analyze: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smf(tracks: &[&[u8]]) -> Vec<u8> {
        let mut b = b"MThd\0\0\0\x06\0\x01".to_vec();
        b.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        b.extend_from_slice(&[0x01, 0xE0]);
        for t in tracks {
            b.extend_from_slice(b"MTrk");
            b.extend_from_slice(&(t.len() as u32).to_be_bytes());
            b.extend_from_slice(t);
        }
        b
    }

    #[test]
    fn running_status_pairing_and_velocities_are_read_from_the_bytes() {
        // On 60 v1, (running) On 62 v1, delta 0x81 0x00 = 128: Off 60, (running) Off 62 v0.
        let t: &[u8] = &[
            0x00, 0x90, 60, 1, 0x00, 62, 1, 0x81, 0x00, 0x80, 60, 0, 0x00, 62, 0, 0x00, 0xFF, 0x2F,
            0x00,
        ];
        let b = smf(&[t]);
        let s = Smf::parse(&b).unwrap();
        assert!(s.anomalies.is_empty(), "{:?}", s.anomalies);
        assert_eq!(s.running_status_events, 2);
        assert_eq!(s.vlq_max_len, 2);
        let (st, tr) = analyze(&s);
        assert_eq!(
            (
                st.notes,
                st.velocity[1],
                st.off_80,
                st.orphan_offs,
                st.hanging
            ),
            (2, 2, 2, 0, 0)
        );
        assert_eq!(tr[0].eot, Some(128));
        assert_eq!(encode(&s, false), b, "identity");
        let explicit = encode(&s, true);
        assert_eq!(explicit.len(), b.len() + 2);
        let again = Smf::parse(&explicit).unwrap();
        assert_eq!(again.running_status_events, 0);
    }

    #[test]
    fn malformed_bytes_are_named_with_their_offsets() {
        // Data byte without status, missing EOT.
        let s = Smf::parse(&smf(&[&[0x00, 0x3C, 0x40]])).unwrap();
        assert!(
            s.anomalies.iter().any(|a| a.contains("without a status")),
            "{:?}",
            s.anomalies
        );
        assert!(s.anomalies.iter().any(|a| a.contains("no End of Track")));
        // A meta that runs past the chunk, a non-minimal and an over-long VLQ.
        let s = Smf::parse(&smf(&[&[0x80, 0x00, 0xFF, 0x01, 0x09, b'a']])).unwrap();
        assert_eq!(s.vlq_non_minimal, 1);
        assert!(s.anomalies.iter().any(|a| a.contains("past the chunk end")));
        let s = Smf::parse(&smf(&[&[0x81, 0x80, 0x80, 0x80, 0x00, 0xFF, 0x2F, 0x00]])).unwrap();
        assert_eq!(s.vlq_over_long, 1);
        // Event after EOT and an orphan Note Off.
        let s = Smf::parse(&smf(&[&[
            0x00, 0x80, 1, 0, 0x00, 0xFF, 0x2F, 0x00, 0x00, 0x90,
        ]]))
        .unwrap();
        assert!(s.anomalies.iter().any(|a| a.contains("after End of Track")));
        let (st, _) = analyze(&s);
        assert_eq!(st.orphan_offs, 1);
        // Declared track count differs.
        let mut b = smf(&[&[0x00, 0xFF, 0x2F, 0x00]]);
        b[11] = 2;
        assert!(
            Smf::parse(&b)
                .unwrap()
                .anomalies
                .iter()
                .any(|a| a.contains("declares 2 tracks"))
        );
    }

    #[test]
    fn variants_change_one_factor_each() {
        let t: &[u8] = &[
            0x00, 0xFF, 0x03, 0x03, b'T', 0xE2, 0x80, 0x00, 0x90, 60, 1, 0x60, 0x80, 60, 0, 0x00,
            0xFF, 0x7F, 0x02, 0x7D, 0x01, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let b = smf(&[t, t]);
        let s = Smf::parse(&b).unwrap();
        let vs = variants(&s);
        let get = |n: &str| Smf::parse(&vs.iter().find(|v| v.0 == n).unwrap().2).unwrap();
        assert_eq!(vs[0].2, b, "identity");
        assert_eq!(analyze(&get("vel15")).0.velocity[15], 2);
        assert_eq!(analyze(&get("vel64")).0.velocity[64], 2);
        assert_eq!(analyze(&get("one-loud")).0.velocity[100], 1);
        let p0 = get("program0");
        assert!(p0.anomalies.is_empty(), "{:?}", p0.anomalies);
        assert_eq!(analyze(&p0).0.programs.get(&(0, 0)), Some(&1));
        assert_eq!(analyze(&p0).0.notes, 2);
        assert_eq!(analyze(&get("no-ff7f")).0.ff7f.len(), 0);
        assert_eq!(analyze(&get("off-vel64")).0.release.get(&64), Some(&2));
        assert_eq!(analyze(&get("off-as-noteon0")).0.off_90_vel0, 2);
        assert_eq!(analyze(&get("ascii-names")).0.non_ascii_text, 0);
        let f0 = get("format0");
        assert_eq!((f0.format, f0.tracks.len()), (0, 1));
        let (st, _) = analyze(&f0);
        assert_eq!((st.notes, st.hanging), (2, 0));
        assert!(f0.anomalies.is_empty(), "{:?}", f0.anomalies);
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
