//! A behavioural emulation of Kiva's MIDI loader (V09-S23, contract §169,
//! §174; `docs/references/v0_9/KIVA.md`, `docs/EXPORT_CERTIFICATION.md`).
//!
//! Written in Rust from the documented behaviour of Kiva-Modded
//! `14b67d4` (`MIDIFile.cs` header / chunk scan, `MIDITrackParser.cs` first
//! and second pass, `BufferByteReader.cs`, `MIDIMemoryFile.cs`); no Kiva
//! code is copied. Times stay in ticks (Kiva converts to seconds; the
//! conversion is not under test). What it reproduces:
//!
//! * the header must be `MThd` with length 6; Format 2 is refused; chunks
//!   are scanned for `MTrk` — any other chunk type after a track sends Kiva
//!   into its "corrupt length" recovery, which fails on a chunk that is not
//!   a track;
//! * per track, two identical passes; the second starts with the previous
//!   command the first ended with (Kiva does not reset it); a "Corrupt
//!   Track" or reading past the chunk ends the pass silently (the rest of
//!   the track is dropped);
//! * the byte reader: `Read` honours a pushed-back byte, `ReadFast` ignores
//!   it (delta times, the first status byte, the velocity, meta lengths);
//!   running status pushes the data byte back; the previous command is kept
//!   after meta and SysEx events too;
//! * SysEx `F0` is read up to the next `F7` byte (its length is not used);
//!   `F7` and most system messages carry no data; `F2` two bytes, `F3` one;
//!   `FB` is corrupt; meta types and lengths as `KIVA.md` lists;
//! * Note On velocity 0 ends a note; same key and channel overlaps close
//!   last in, first out; notes still open at the end of the pass end where
//!   the pass stopped; the note byte is read unmasked (keys 0–255).
#![allow(dead_code)]

/// A note as Kiva holds it: (start tick, end tick, key, channel, velocity).
pub type KivaNote = (u64, u64, u8, u8, u8);

/// One track chunk as Kiva read it.
#[derive(Debug, Default, Clone)]
pub struct KivaTrack {
    /// Why the pass stopped before End of Track (`None`: it reached it).
    pub stopped: Option<&'static str>,
    /// Byte offset in the chunk where it stopped.
    pub stopped_at: usize,
    /// Notes the second pass read (in the order they were closed).
    pub notes: Vec<KivaNote>,
    /// Every Note On / Off in order: (key, explicit status byte).
    pub note_events: Vec<(u8, bool)>,
    /// For each event right after a note event with a key above 127:
    /// whether it had an explicit status byte (`None`: not a channel event).
    pub after_wide: Vec<Option<bool>>,
    /// Tempo events of the first pass: (tick, microseconds per quarter).
    pub tempos: Vec<(u64, u32)>,
    /// The second pass met more notes on a key than the first counted —
    /// Kiva's arrays would overflow (the pass ends there).
    pub pass_mismatch: bool,
}

/// What Kiva makes of a file.
#[derive(Debug, Default, Clone)]
pub struct KivaFile {
    /// The load fails with this message (header, format, chunk scan).
    pub error: Option<String>,
    pub format: u16,
    pub division: u16,
    pub tracks: Vec<KivaTrack>,
}

impl KivaFile {
    /// Every note of every track, sorted.
    pub fn notes(&self) -> Vec<KivaNote> {
        let mut v: Vec<KivaNote> = self.tracks.iter().flat_map(|t| t.notes.clone()).collect();
        v.sort_unstable();
        v
    }

    /// Indices of the tracks whose rest was dropped.
    pub fn corrupt(&self) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.stopped.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    pub fn note_events(&self) -> Vec<(u8, bool)> {
        self.tracks
            .iter()
            .flat_map(|t| t.note_events.clone())
            .collect()
    }

    pub fn after_wide(&self) -> Vec<Option<bool>> {
        self.tracks
            .iter()
            .flat_map(|t| t.after_wide.clone())
            .collect()
    }

    /// Loaded completely: no error, every track up to End of Track, the
    /// passes agree.
    pub fn clean(&self) -> bool {
        self.error.is_none()
            && self
                .tracks
                .iter()
                .all(|t| t.stopped.is_none() && !t.pass_mismatch)
    }
}

/// Reads `bytes` the way Kiva loads a file.
pub fn kiva_load(bytes: &[u8]) -> KivaFile {
    let mut f = KivaFile::default();
    let u32_at = |i: usize| -> Option<u32> {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
    };
    if bytes.get(..4) != Some(&b"MThd"[..]) {
        f.error = Some("Corrupt chunk headers".into());
        return f;
    }
    if u32_at(4) != Some(6) {
        f.error = Some("Header chunk size isn't 6".into());
        return f;
    }
    let u16_at = |i: usize| -> u16 {
        bytes
            .get(i..i + 2)
            .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
    };
    f.format = u16_at(8);
    f.division = u16_at(12);
    if f.format == 2 {
        f.error = Some("Midi type 2 not supported".into());
        return f;
    }
    // Chunk scan (MIDIFile.Open).
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let mut pos = 14usize;
    while pos < bytes.len() {
        if bytes.get(pos..pos + 4) != Some(&b"MTrk"[..]) {
            f.error = Some(if chunks.is_empty() {
                "Corrupt first track chunk header".into()
            } else {
                format!(
                    "Could not determine length of corrupt chunk (a {:?} chunk after track {})",
                    String::from_utf8_lossy(bytes.get(pos..pos + 4).unwrap_or(&[])),
                    chunks.len()
                )
            });
            return f;
        }
        let len = u32_at(pos + 4).unwrap_or(0) as usize;
        let start = pos + 8;
        chunks.push((start, len));
        pos = (start + len).min(bytes.len().max(start));
        if start + len > bytes.len() {
            break;
        }
    }
    for (start, len) in chunks {
        // A chunk longer than the file reads zeros past its end (`Reader`).
        let body = &bytes[start.min(bytes.len())..(start + len).min(bytes.len())];
        let first = pass(body, len, 0, true);
        let mut second = pass(body, len, first.prev, false);
        second.tempos = first.tempos;
        // Kiva sizes the second pass's arrays from the first pass's counts.
        for k in 0..256 {
            if second.counts[k] > first.counts[k] {
                second.track.pass_mismatch = true;
            }
        }
        let mut t = second.track;
        t.tempos = second.tempos;
        f.tracks.push(t);
    }
    f
}

struct Pass {
    track: KivaTrack,
    counts: [u64; 256],
    prev: u8,
    tempos: Vec<(u64, u32)>,
}

/// Past the end of the file, Kiva reads zeros up to the chunk's declared
/// length. A corrupt length can declare gigabytes; the emulation stops after
/// this many zeros (the outcome — a track Kiva misreads — is the same).
const ZEROS_EMULATED: usize = 1 << 16;

/// Kiva's byte reader over one chunk: `b` is what the file holds of it,
/// `len` its declared length.
struct Reader<'a> {
    b: &'a [u8],
    len: usize,
    pos: usize,
    pushback: Option<u8>,
}

impl Reader<'_> {
    fn read(&mut self) -> Result<u8, &'static str> {
        if let Some(p) = self.pushback.take() {
            return Ok(p);
        }
        self.read_fast()
    }

    fn read_fast(&mut self) -> Result<u8, &'static str> {
        if self.pos >= self.len {
            return Err("Read past the end of the track");
        }
        let v = match self.b.get(self.pos) {
            Some(&v) => v,
            None if self.pos - self.b.len() < ZEROS_EMULATED => 0,
            None => return Err("Read zeros past the end of the file (emulation stops)"),
        };
        self.pos += 1;
        Ok(v)
    }

    fn skip(&mut self, n: usize) -> Result<(), &'static str> {
        for _ in 0..n {
            if self.pushback.take().is_some() {
                continue;
            }
            self.read_fast()?;
        }
        Ok(())
    }

    /// Kiva's 4-byte variable-length quantity (ReadFast).
    fn vlq(&mut self) -> Result<u64, &'static str> {
        let mut v = 0u64;
        for _ in 0..4 {
            let c = self.read_fast()?;
            v = (v << 7) | u64::from(c & 0x7F);
            if c < 0x80 {
                break;
            }
        }
        Ok(v)
    }
}

/// One pass over a chunk; `record`: the first pass (counts, tempos).
fn pass(body: &[u8], len: usize, prev: u8, record: bool) -> Pass {
    let mut r = Reader {
        b: body,
        len,
        pos: 0,
        pushback: None,
    };
    let mut out = Pass {
        track: KivaTrack::default(),
        counts: [0; 256],
        prev,
        tempos: Vec::new(),
    };
    let mut open: Vec<Vec<(u64, u8)>> = vec![Vec::new(); 256 * 16];
    let mut tick = 0u64;
    let mut last_wide = false;
    let res = (|| -> Result<(), &'static str> {
        loop {
            tick += r.vlq()?;
            let mut command = r.read_fast()?;
            let explicit = command >= 0x80;
            if !explicit {
                r.pushback = Some(command);
                command = out.prev;
            }
            out.prev = command;
            if last_wide {
                out.track
                    .after_wide
                    .push((command < 0xF0).then_some(explicit));
            }
            last_wide = false;
            let ch = command & 0x0F;
            match command & 0xF0 {
                0x90 | 0x80 => {
                    let key = r.read()?;
                    let vel = r.read_fast()?;
                    out.track.note_events.push((key, explicit));
                    last_wide = key > 0x7F;
                    let slot = usize::from(key) * 16 + usize::from(ch);
                    if command & 0xF0 == 0x90 && vel != 0 {
                        out.counts[usize::from(key)] += 1;
                        open[slot].push((tick, vel));
                    } else if let Some((s, v)) = open[slot].pop() {
                        out.track.notes.push((s, tick, key, ch, v));
                    }
                }
                0xA0 | 0xB0 | 0xE0 => {
                    r.read()?;
                    r.read()?;
                }
                0xC0 | 0xD0 => {
                    r.read()?;
                }
                _ => match command {
                    0xF0 => while r.read()? != 0xF7 {},
                    0xF1 | 0xF4 | 0xF5 | 0xF6 | 0xF7 | 0xF8 | 0xF9 | 0xFA | 0xFC | 0xFD | 0xFE => {}
                    0xF2 => {
                        r.read()?;
                        r.read()?;
                    }
                    0xF3 => {
                        r.read()?;
                    }
                    0xFF => {
                        let t = r.read()?;
                        let fixed = |r: &mut Reader, n: u8| -> Result<(), &'static str> {
                            if r.read()? != n {
                                return Err("Corrupt Track");
                            }
                            for _ in 0..n {
                                r.read()?;
                            }
                            Ok(())
                        };
                        match t {
                            0x00 => fixed(&mut r, 2)?,
                            0x01..=0x0A | 0x7F => {
                                let size = r.vlq()? as usize;
                                if t == 0x0A && (size == 8 || size == 12) {
                                    // A colour event: 00 0F channel 00 + colours.
                                    if r.read()? != 0x00 {
                                        r.skip(size - 1)?;
                                    } else if r.read()? != 0x0F {
                                        r.skip(size - 2)?;
                                    } else {
                                        let c = r.read()?;
                                        if c >= 16 && c != 0x7F {
                                            r.skip(size - 3)?;
                                        } else {
                                            // 00: the colours (read), else
                                            // skipped — the same bytes.
                                            r.read()?;
                                            r.skip(size - 4)?;
                                        }
                                    }
                                } else {
                                    r.skip(size)?;
                                }
                            }
                            0x20 | 0x21 => fixed(&mut r, 1)?,
                            0x2F => {
                                if r.read()? != 0 {
                                    return Err("Corrupt Track");
                                }
                                return Ok(());
                            }
                            0x51 => {
                                if r.read()? != 3 {
                                    return Err("Corrupt Track");
                                }
                                let mut us = 0u32;
                                for _ in 0..3 {
                                    us = (us << 8) | u32::from(r.read()?);
                                }
                                if record {
                                    out.tempos.push((tick, us));
                                }
                            }
                            0x54 => fixed(&mut r, 5)?,
                            0x58 => fixed(&mut r, 4)?,
                            0x59 => fixed(&mut r, 2)?,
                            _ => return Err("Corrupt Track"),
                        }
                    }
                    _ => return Err("Corrupt Track"),
                },
            }
        }
    })();
    if let Err(e) = res {
        out.track.stopped = Some(e);
        out.track.stopped_at = r.pos;
    }
    // Notes still open end where the pass stopped.
    for (slot, stack) in open.iter_mut().enumerate() {
        while let Some((s, v)) = stack.pop() {
            out.track
                .notes
                .push((s, tick, (slot / 16) as u8, (slot % 16) as u8, v));
        }
    }
    out
}
