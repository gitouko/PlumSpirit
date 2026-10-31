//! Streaming strict comparison of two MIDI files (v1.0-beta1 B1-0125,
//! contract §019): do they hold the same events in the same order, track by
//! track?
//!
//! Equal size or equal note count proves nothing; this compares what a
//! reader decodes, after running status is expanded: the header (format,
//! division, track count), every token of every track chunk with its tick in
//! order (notes, Note On velocity 0 against Note Off, controllers, programs,
//! pitch bend, pressure, every meta event with its payload — duplicates and
//! which track owns a tempo included — SysEx, system messages), each End of
//! Track, and every other chunk in place. Two files that differ only in
//! running status (status bytes left out or written) compare equal as
//! events and unequal as bytes.
//!
//! Memory does not grow with the files: pass 1 keeps, per track, counts by
//! kind and one digest per block of [`BLOCK`] tokens (80 million events in
//! one track are about 1,300 digests). Only when the files differ, pass 2
//! re-reads the first differing block of that track in both files and names
//! the first differing token.

use super::error::{ImportWarning, MidiError};
use super::provenance::HashingReader;
use super::reader::{ByteSource, SmfHandler, Token, read_smf};
use crate::codec::WordHash;
use crate::model::project::KeyRange;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Tokens per block digest.
pub const BLOCK: u64 = 65_536;

/// Mixes one token into `h` (the encoding `midi::strict` verifies with too).
pub(crate) fn hash_token(h: &mut WordHash, t: &Token<'_>) {
    match *t {
        Token::Channel { tick, status, data } | Token::System { tick, status, data } => {
            h.pair(
                tick,
                u64::from_le_bytes([status, data[0], data[1], 0, 0, 0, 0, 1]),
            );
        }
        Token::Meta {
            tick,
            meta_type,
            payload,
        } => {
            h.pair(tick, 0x2FF00 | u64::from(meta_type));
            h.field(payload);
        }
        Token::SysEx { tick, payload } => {
            h.pair(tick, 0x3F0);
            h.field(payload);
        }
        Token::SysExEscape { tick, payload } => {
            h.pair(tick, 0x4F7);
            h.field(payload);
        }
    }
}

/// Token counts of one track by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KindCounts {
    /// Note On with a velocity above 0.
    pub note_on: u64,
    /// Note Off (`8n`).
    pub note_off: u64,
    /// Note On with velocity 0 (a note ending).
    pub note_on_zero: u64,
    pub control_change: u64,
    pub program_change: u64,
    pub pitch_bend: u64,
    /// Polyphonic key pressure and channel pressure.
    pub pressure: u64,
    /// Meta events other than tempo (End of Track not counted).
    pub meta: u64,
    pub tempo: u64,
    /// SysEx and SysEx escape events.
    pub sysex: u64,
    pub system: u64,
}

impl KindCounts {
    fn add(&mut self, t: &Token<'_>) {
        match *t {
            Token::Channel { status, data, .. } => match status & 0xF0 {
                0x90 if data[1] > 0 => self.note_on += 1,
                0x90 => self.note_on_zero += 1,
                0x80 => self.note_off += 1,
                0xB0 => self.control_change += 1,
                0xC0 => self.program_change += 1,
                0xE0 => self.pitch_bend += 1,
                _ => self.pressure += 1,
            },
            Token::Meta {
                meta_type: 0x51, ..
            } => self.tempo += 1,
            Token::Meta { .. } => self.meta += 1,
            Token::SysEx { .. } | Token::SysExEscape { .. } => self.sysex += 1,
            Token::System { .. } => self.system += 1,
        }
    }
}

/// One track chunk as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackSummary {
    pub tokens: u64,
    pub counts: KindCounts,
    pub end_of_track: Option<u64>,
    /// The last token's tick.
    pub last_tick: u64,
    blocks: Vec<u64>,
}

/// A whole file as read (pass 1).
#[derive(Clone, Debug, Default)]
pub struct FileSummary {
    pub bytes: u64,
    /// `WordHash` of the file's bytes.
    pub hash: u64,
    pub format: u16,
    pub division: u16,
    pub declared_tracks: u16,
    pub tracks: Vec<TrackSummary>,
    /// Other chunks: (type, length, track chunks before it, digest).
    pub chunks: Vec<([u8; 4], u64, usize, u64)>,
    /// What the reader warned about (sentences).
    pub warnings: Vec<String>,
}

impl FileSummary {
    /// Every track's counts added up.
    pub fn totals(&self) -> KindCounts {
        let mut k = KindCounts::default();
        for t in &self.tracks {
            let c = t.counts;
            k.note_on += c.note_on;
            k.note_off += c.note_off;
            k.note_on_zero += c.note_on_zero;
            k.control_change += c.control_change;
            k.program_change += c.program_change;
            k.pitch_bend += c.pitch_bend;
            k.pressure += c.pressure;
            k.meta += c.meta;
            k.tempo += c.tempo;
            k.sysex += c.sysex;
            k.system += c.system;
        }
        k
    }
}

struct Pass1 {
    s: FileSummary,
    cur: TrackSummary,
    block: WordHash,
    wide: bool,
}

impl SmfHandler for Pass1 {
    fn header(&mut self, format: u16, declared: u16, division: u16) -> Result<(), MidiError> {
        self.s.format = format;
        self.s.declared_tracks = declared;
        self.s.division = division;
        Ok(())
    }
    fn begin_track(&mut self, _index: usize) -> Result<(), MidiError> {
        self.cur = TrackSummary::default();
        self.block = WordHash::new();
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        hash_token(&mut self.block, &t);
        self.cur.counts.add(&t);
        self.cur.last_tick = t.tick();
        self.cur.tokens += 1;
        if self.cur.tokens.is_multiple_of(BLOCK) {
            self.cur.blocks.push(self.block.finish());
            self.block = WordHash::new();
        }
        Ok(())
    }
    fn end_track(&mut self, _index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        if !self.cur.tokens.is_multiple_of(BLOCK) {
            self.cur.blocks.push(self.block.finish());
        }
        self.cur.end_of_track = eot;
        self.s.tracks.push(std::mem::take(&mut self.cur));
        Ok(())
    }
    fn extra_chunk(&mut self, t: [u8; 4], data: Vec<u8>, after: usize) -> Result<(), MidiError> {
        let mut h = WordHash::new();
        h.field(&data);
        self.s
            .chunks
            .push((t, data.len() as u64, after, h.finish()));
        Ok(())
    }
    fn warning(&mut self, w: ImportWarning) {
        self.s.warnings.push(w.user_message());
    }
    fn extended_keys(&self) -> bool {
        self.wide
    }
}

/// Reads `path` once (pass 1). `keys`: [`KeyRange::Extended256`] reads a
/// 256-key file.
pub fn summarize(path: &Path, keys: KeyRange) -> Result<FileSummary, MidiError> {
    let f = File::open(path)?;
    let len = f.metadata().ok().map(|m| m.len());
    let mut hashing = HashingReader::new(BufReader::with_capacity(1 << 20, f));
    let mut p = Pass1 {
        s: FileSummary::default(),
        cur: TrackSummary::default(),
        block: WordHash::new(),
        wide: keys == KeyRange::Extended256,
    };
    {
        let mut src = ByteSource::new(&mut hashing, len);
        read_smf(&mut src, &mut p)?;
    }
    let (bytes, hash) = hashing.finish()?;
    p.s.bytes = bytes;
    p.s.hash = hash;
    Ok(p.s)
}

/// A token in words.
pub fn describe(t: &Token<'_>) -> String {
    let hex = |b: &[u8]| -> String {
        let mut s: String = b.iter().take(16).map(|x| format!("{x:02X} ")).collect();
        if b.len() > 16 {
            s.push_str("… ");
        }
        s.trim_end().to_owned()
    };
    match *t {
        Token::Channel { tick, status, data } => {
            let ch = (status & 0x0F) + 1;
            let what = match status & 0xF0 {
                0x90 if data[1] == 0 => format!("Note On velocity 0 (ends) key {}", data[0]),
                0x90 => format!("Note On key {} velocity {}", data[0], data[1]),
                0x80 => format!("Note Off key {} velocity {}", data[0], data[1]),
                0xA0 => format!("Key Pressure key {} value {}", data[0], data[1]),
                0xB0 => format!("Control Change {} value {}", data[0], data[1]),
                0xC0 => format!("Program Change {}", data[0]),
                0xD0 => format!("Channel Pressure {}", data[0]),
                _ => format!(
                    "Pitch Bend {}",
                    i32::from(data[0]) | i32::from(data[1]) << 7
                ),
            };
            format!("tick {tick}: {what}, channel {ch}")
        }
        Token::Meta {
            tick,
            meta_type: 0x51,
            payload,
        } if payload.len() == 3 => {
            let us =
                u32::from(payload[0]) << 16 | u32::from(payload[1]) << 8 | u32::from(payload[2]);
            format!(
                "tick {tick}: Tempo {us} µs per quarter (FF 51 03 {})",
                hex(payload)
            )
        }
        Token::Meta {
            tick,
            meta_type,
            payload,
        } => format!(
            "tick {tick}: meta FF {meta_type:02X}, {} bytes ({})",
            payload.len(),
            hex(payload)
        ),
        Token::SysEx { tick, payload } => {
            format!(
                "tick {tick}: SysEx F0, {} bytes ({})",
                payload.len(),
                hex(payload)
            )
        }
        Token::SysExEscape { tick, payload } => {
            format!(
                "tick {tick}: escape F7, {} bytes ({})",
                payload.len(),
                hex(payload)
            )
        }
        Token::System { tick, status, data } => {
            format!(
                "tick {tick}: system {status:02X} {:02X} {:02X}",
                data[0], data[1]
            )
        }
    }
}

/// Collects the descriptions of tokens `from .. from + n` of one track.
struct Window {
    track: usize,
    from: u64,
    n: u64,
    cur_track: Option<usize>,
    index: u64,
    out: Vec<String>,
    wide: bool,
}

impl SmfHandler for Window {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, index: usize) -> Result<(), MidiError> {
        self.cur_track = Some(index);
        self.index = 0;
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        if self.cur_track == Some(self.track)
            && self.index >= self.from
            && self.index < self.from + self.n
        {
            self.out.push(describe(&t));
        }
        self.index += 1;
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        self.cur_track = None;
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, _: ImportWarning) {}
    fn extended_keys(&self) -> bool {
        self.wide
    }
}

fn window(
    path: &Path,
    keys: KeyRange,
    track: usize,
    from: u64,
    n: u64,
) -> Result<Vec<String>, MidiError> {
    let f = File::open(path)?;
    let len = f.metadata().ok().map(|m| m.len());
    let mut w = Window {
        track,
        from,
        n,
        cur_track: None,
        index: 0,
        out: Vec::new(),
        wide: keys == KeyRange::Extended256,
    };
    let mut src = ByteSource::new(BufReader::with_capacity(1 << 20, f), len);
    read_smf(&mut src, &mut w)?;
    Ok(w.out)
}

/// The first place where two files' events differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    /// Track chunk (0-based).
    pub track: usize,
    /// Token index in the track (0-based).
    pub index: u64,
    /// That token in each file (`None`: the track ends before it).
    pub a: Option<String>,
    pub b: Option<String>,
}

/// The comparison of two files.
#[derive(Clone, Debug)]
pub struct CompareReport {
    pub a: FileSummary,
    pub b: FileSummary,
    /// The same bytes.
    pub byte_identical: bool,
    /// The same events in the same order in every track, the same header
    /// and the same other chunks.
    pub same_events: bool,
    /// Header, track count, End of Track and chunk differences (sentences).
    pub structure: Vec<String>,
    /// The first differing token, when events differ.
    pub first_difference: Option<Difference>,
}

impl CompareReport {
    /// The report as lines of text.
    pub fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        let (a, b) = (&self.a, &self.b);
        v.push(format!(
            "A: {} bytes, format {}, division {}, {} tracks, {} events",
            a.bytes,
            a.format,
            a.division,
            a.tracks.len(),
            a.tracks.iter().map(|t| t.tokens).sum::<u64>()
        ));
        v.push(format!(
            "B: {} bytes, format {}, division {}, {} tracks, {} events",
            b.bytes,
            b.format,
            b.division,
            b.tracks.len(),
            b.tracks.iter().map(|t| t.tokens).sum::<u64>()
        ));
        v.push(if self.byte_identical {
            "Byte for byte identical.".to_owned()
        } else if self.same_events {
            "Different bytes, the same events in the same order (status-byte encoding only)."
                .to_owned()
        } else {
            "The events differ.".to_owned()
        });
        v.extend(self.structure.iter().cloned());
        if let Some(d) = &self.first_difference {
            v.push(format!(
                "First difference: track {} (chunk {}), event {}:",
                d.track + 1,
                d.track,
                d.index + 1
            ));
            v.push(format!(
                "  A: {}",
                d.a.as_deref().unwrap_or("(the track ends)")
            ));
            v.push(format!(
                "  B: {}",
                d.b.as_deref().unwrap_or("(the track ends)")
            ));
        }
        let (ka, kb) = (a.totals(), b.totals());
        if ka != kb {
            v.push(format!("Counts A: {ka:?}"));
            v.push(format!("Counts B: {kb:?}"));
        }
        for (i, (ta, tb)) in a.tracks.iter().zip(&b.tracks).enumerate() {
            if ta.counts != tb.counts {
                v.push(format!(
                    "Track {}: counts differ (A {:?} / B {:?}).",
                    i + 1,
                    ta.counts,
                    tb.counts
                ));
            }
        }
        v
    }
}

/// Compares two MIDI files (see the module docs).
pub fn compare_files(a: &Path, b: &Path, keys: KeyRange) -> Result<CompareReport, MidiError> {
    let sa = summarize(a, keys)?;
    let sb = summarize(b, keys)?;
    let mut structure = Vec::new();
    if (sa.format, sa.division) != (sb.format, sb.division) {
        structure.push(format!(
            "Header: format {} / division {} against format {} / division {}.",
            sa.format, sa.division, sb.format, sb.division
        ));
    }
    if sa.tracks.len() != sb.tracks.len() {
        structure.push(format!(
            "Track chunks: {} against {}.",
            sa.tracks.len(),
            sb.tracks.len()
        ));
    }
    if sa.chunks != sb.chunks {
        structure.push("The other (non-MTrk) chunks differ.".into());
    }
    let mut first = None;
    for (i, (ta, tb)) in sa.tracks.iter().zip(&sb.tracks).enumerate() {
        if ta.blocks != tb.blocks || ta.tokens != tb.tokens {
            if first.is_none() {
                let k = ta
                    .blocks
                    .iter()
                    .zip(&tb.blocks)
                    .position(|(x, y)| x != y)
                    .unwrap_or(ta.blocks.len().min(tb.blocks.len())) as u64;
                let from = k * BLOCK;
                let wa = window(a, keys, i, from, BLOCK)?;
                let wb = window(b, keys, i, from, BLOCK)?;
                let j = (0..wa.len().max(wb.len()))
                    .find(|&j| wa.get(j) != wb.get(j))
                    .unwrap_or(0);
                first = Some(Difference {
                    track: i,
                    index: from + j as u64,
                    a: wa.get(j).cloned(),
                    b: wb.get(j).cloned(),
                });
            }
        } else if ta.end_of_track != tb.end_of_track {
            structure.push(format!(
                "Track {}: End of Track at {:?} against {:?}.",
                i + 1,
                ta.end_of_track,
                tb.end_of_track
            ));
        }
    }
    if first.is_none() && sa.tracks.len() != sb.tracks.len() {
        let i = sa.tracks.len().min(sb.tracks.len());
        first = Some(Difference {
            track: i,
            index: 0,
            a: (i < sa.tracks.len()).then(|| "(a track chunk)".to_owned()),
            b: (i < sb.tracks.len()).then(|| "(a track chunk)".to_owned()),
        });
    }
    let same_events = structure.is_empty() && first.is_none();
    Ok(CompareReport {
        byte_identical: sa.bytes == sb.bytes && sa.hash == sb.hash,
        same_events,
        a: sa,
        b: sb,
        structure,
        first_difference: first,
    })
}
