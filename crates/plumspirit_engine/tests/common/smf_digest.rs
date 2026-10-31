//! An independent Standard MIDI File reader for the export certification
//! (V09-S23, contract §166, §170, §171; `docs/EXPORT_CERTIFICATION.md`).
//!
//! Written here from the SMF 1.0 specification — it shares nothing with
//! `midi::reader` / `midi::import` — and reduced to the semantic digest the
//! certification compares: per track every note (paired first in, first out
//! per channel and key), controller, pitch bend, program change, channel /
//! key pressure, tempo, time signature, other meta event, SysEx and End of
//! Track. `extended_keys` reads the experimental 256-key file (a key byte
//! above 127 after an explicit Note On / Off status).
#![allow(dead_code)]

/// A note: (start, end, channel, key, velocity, off velocity, ended by a
/// Note On with velocity 0).
pub type DNote = (u64, u64, u8, u8, u8, u8, bool);

/// One track's digest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackDigest {
    /// Note Ons in order: (tick, channel, key, velocity).
    pub ons: Vec<(u64, u8, u8, u8)>,
    /// Note endings in order: (tick, channel, key, off velocity, Note On 0).
    pub offs: Vec<(u64, u8, u8, u8, bool)>,
    /// Paired notes (first in, first out), sorted.
    pub notes: Vec<DNote>,
    /// Endings with no open note, and notes never ended (start tick, channel,
    /// key).
    pub unmatched_offs: Vec<(u64, u8, u8)>,
    pub unended: Vec<(u64, u8, u8)>,
    /// Control Change: (tick, channel, controller, value).
    pub controllers: Vec<(u64, u8, u8, u8)>,
    /// Pitch bend: (tick, channel, 14-bit value).
    pub pitch_bends: Vec<(u64, u8, u16)>,
    /// Program change: (tick, channel, program).
    pub programs: Vec<(u64, u8, u8)>,
    /// Channel pressure (key 255) and key pressure: (tick, channel, key, value).
    pub pressures: Vec<(u64, u8, u8, u8)>,
    /// Tempo: (tick, microseconds per quarter note).
    pub tempos: Vec<(u64, u32)>,
    /// Time signature: (tick, numerator, denominator power, clocks, 32nds).
    pub time_signatures: Vec<(u64, u8, u8, u8, u8)>,
    /// Every other meta event: (tick, type, payload).
    pub metas: Vec<(u64, u8, Vec<u8>)>,
    /// SysEx (`F0`) and escapes (`F7`): (tick, status, payload).
    pub sysex: Vec<(u64, u8, Vec<u8>)>,
    /// System common / real-time messages: (tick, status bytes).
    pub system: Vec<(u64, Vec<u8>)>,
    /// End of Track tick (`None`: the chunk had none).
    pub end_of_track: Option<u64>,
    /// Channel messages read under running status (no status byte of
    /// their own). Not part of the semantic comparison.
    pub running_status: usize,
    /// (Plan digests only) channels and keys whose notes, in Note On order,
    /// do not end in order: a first-in-first-out reader cannot rebuild them
    /// (`midi::overlap`, V09-0102). Sorted.
    pub nested_keys: Vec<(u8, u8)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Digest {
    pub format: u16,
    pub division: u16,
    pub tracks: Vec<TrackDigest>,
    /// Chunks other than `MTrk` after the header: (type, length).
    pub other_chunks: Vec<([u8; 4], usize)>,
}

impl Digest {
    pub fn note_ons(&self) -> usize {
        self.tracks.iter().map(|t| t.ons.len()).sum()
    }
}

fn be32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

/// Reads a whole file; `Err` names the first violation of the format.
pub fn digest(bytes: &[u8], extended_keys: bool) -> Result<Digest, String> {
    if bytes.len() < 14 || &bytes[..4] != b"MThd" {
        return Err("no MThd header".into());
    }
    let hlen = be32(&bytes[4..8]);
    if hlen < 6 || 8 + hlen > bytes.len() {
        return Err(format!("header length {hlen}"));
    }
    let mut d = Digest {
        format: u16::from_be_bytes([bytes[8], bytes[9]]),
        division: u16::from_be_bytes([bytes[12], bytes[13]]),
        ..Digest::default()
    };
    let declared = u16::from_be_bytes([bytes[10], bytes[11]]) as usize;
    let mut i = 8 + hlen;
    while i < bytes.len() {
        if i + 8 > bytes.len() {
            return Err(format!("truncated chunk header at byte {i}"));
        }
        let ty: [u8; 4] = bytes[i..i + 4].try_into().unwrap();
        let len = be32(&bytes[i + 4..i + 8]);
        let body = bytes
            .get(i + 8..i + 8 + len)
            .ok_or_else(|| format!("chunk at byte {i} runs past the end"))?;
        if &ty == b"MTrk" {
            let t = track(body, extended_keys)
                .map_err(|e| format!("track {}: {e}", d.tracks.len() + 1))?;
            d.tracks.push(t);
        } else {
            d.other_chunks.push((ty, len));
        }
        i += 8 + len;
    }
    if d.tracks.len() != declared {
        return Err(format!(
            "header declares {declared} tracks, file has {}",
            d.tracks.len()
        ));
    }
    Ok(d)
}

fn track(b: &[u8], extended_keys: bool) -> Result<TrackDigest, String> {
    let mut t = TrackDigest::default();
    let mut p = 0usize;
    let mut tick = 0u64;
    let mut running: Option<u8> = None;
    let mut open: std::collections::HashMap<(u8, u8), std::collections::VecDeque<(u64, u8)>> =
        Default::default();
    let byte = |p: &mut usize| -> Result<u8, String> {
        let v = *b.get(*p).ok_or("event runs past the end of the track")?;
        *p += 1;
        Ok(v)
    };
    let vlq = |p: &mut usize| -> Result<u64, String> {
        let mut v = 0u64;
        for n in 0..4 {
            let c = byte(p)?;
            v = (v << 7) | u64::from(c & 0x7F);
            if c < 0x80 {
                return Ok(v);
            }
            if n == 3 {
                return Err("a variable-length number longer than 4 bytes".into());
            }
        }
        unreachable!()
    };
    let data = |p: &mut usize, key: bool, explicit: bool| -> Result<u8, String> {
        let v = byte(p)?;
        if v > 0x7F && !(key && explicit && extended_keys) {
            return Err(format!("data byte {v:#04X} at offset {}", *p - 1));
        }
        Ok(v)
    };
    while p < b.len() {
        tick += vlq(&mut p)?;
        let first = byte(&mut p)?;
        let (status, explicit) = if first & 0x80 != 0 {
            (first, true)
        } else {
            p -= 1;
            t.running_status += 1;
            (
                running.ok_or("running status without a previous status")?,
                false,
            )
        };
        match status {
            0x80..=0xEF => {
                running = Some(status);
                let ch = status & 0x0F;
                match status & 0xF0 {
                    0x80 | 0x90 => {
                        let key = data(&mut p, true, explicit)?;
                        let vel = data(&mut p, false, explicit)?;
                        if status & 0xF0 == 0x90 && vel > 0 {
                            t.ons.push((tick, ch, key, vel));
                            open.entry((ch, key)).or_default().push_back((tick, vel));
                        } else {
                            let on0 = status & 0xF0 == 0x90;
                            let off_vel = if on0 { 0 } else { vel };
                            t.offs.push((tick, ch, key, off_vel, on0));
                            match open.get_mut(&(ch, key)).and_then(|q| q.pop_front()) {
                                Some((s, v)) => t.notes.push((s, tick, ch, key, v, off_vel, on0)),
                                None => t.unmatched_offs.push((tick, ch, key)),
                            }
                        }
                    }
                    0xA0 => {
                        let k = data(&mut p, false, explicit)?;
                        let v = data(&mut p, false, explicit)?;
                        t.pressures.push((tick, ch, k, v));
                    }
                    0xB0 => {
                        let c = data(&mut p, false, explicit)?;
                        let v = data(&mut p, false, explicit)?;
                        t.controllers.push((tick, ch, c, v));
                    }
                    0xC0 => t.programs.push((tick, ch, data(&mut p, false, explicit)?)),
                    0xD0 => t
                        .pressures
                        .push((tick, ch, 255, data(&mut p, false, explicit)?)),
                    _ => {
                        let lo = data(&mut p, false, explicit)?;
                        let hi = data(&mut p, false, explicit)?;
                        t.pitch_bends
                            .push((tick, ch, u16::from(lo) | (u16::from(hi) << 7)));
                    }
                }
            }
            0xF0 | 0xF7 => {
                // SysEx and escapes cancel running status (SMF 1.0).
                running = None;
                let n = vlq(&mut p)? as usize;
                let payload = b.get(p..p + n).ok_or("SysEx runs past the end")?.to_vec();
                p += n;
                t.sysex.push((tick, status, payload));
            }
            0xFF => {
                running = None;
                let ty = byte(&mut p)?;
                let n = vlq(&mut p)? as usize;
                let payload = b
                    .get(p..p + n)
                    .ok_or("meta event runs past the end")?
                    .to_vec();
                p += n;
                match ty {
                    0x2F => {
                        if n != 0 {
                            return Err("End of Track with data".into());
                        }
                        t.end_of_track = Some(tick);
                        if p != b.len() {
                            return Err(format!("{} bytes after End of Track", b.len() - p));
                        }
                    }
                    0x51 if n == 3 => t.tempos.push((
                        tick,
                        u32::from_be_bytes([0, payload[0], payload[1], payload[2]]),
                    )),
                    0x58 if n == 4 => t
                        .time_signatures
                        .push((tick, payload[0], payload[1], payload[2], payload[3])),
                    _ => t.metas.push((tick, ty, payload)),
                }
            }
            s => {
                // System common / real-time inside a track (rare).
                running = None;
                let n = match s {
                    0xF2 => 2,
                    0xF1 | 0xF3 => 1,
                    _ => 0,
                };
                let mut v = vec![s];
                for _ in 0..n {
                    v.push(data(&mut p, false, true)?);
                }
                t.system.push((tick, v));
            }
        }
    }
    if t.end_of_track.is_none() {
        return Err("no End of Track".into());
    }
    for ((ch, key), q) in open {
        for (s, _) in q {
            t.unended.push((s, ch, key));
        }
    }
    t.notes.sort_unstable();
    t.unended.sort_unstable();
    Ok(t)
}

/// The digest of what an export snapshot says the file holds, built from
/// its streams and notes (no bytes).
pub fn snapshot_digest(s: &plumspirit_engine::midi::ExportSnapshot) -> Digest {
    use plumspirit_engine::midi::export::OutEvent;
    use plumspirit_engine::model::NoteOffKind;
    use plumspirit_engine::model::events::RawKind;
    let chunks = s.chunks();
    let mut d = Digest {
        format: s.format_word(),
        division: s.division_word(),
        tracks: vec![TrackDigest::default(); chunks.len()],
        other_chunks: s
            .document()
            .extra_chunks()
            .iter()
            .map(|c| (c.chunk_type, c.data.len()))
            .collect(),
    };
    // Latest end per (chunk, channel, key) in Note On order, to find the
    // keys whose notes do not end in the order they start.
    let mut last_end: std::collections::HashMap<(u32, u8, u8), u64> = Default::default();
    s.for_each_note(|n| {
        let on0 = n.off == NoteOffKind::NoteOnZero;
        let t = &mut d.tracks[n.chunk as usize];
        t.notes.push((
            n.start,
            n.end,
            n.channel,
            n.key,
            n.velocity,
            if on0 { 0 } else { n.off_velocity },
            on0,
        ));
        let e = last_end.entry((n.chunk, n.channel, n.key)).or_insert(0);
        if n.end < *e {
            t.nested_keys.push((n.channel, n.key));
        }
        *e = (*e).max(n.end);
    });
    for (ci, out) in chunks.iter().enumerate() {
        let tr = s.document().track(out.track).unwrap();
        let t = &mut d.tracks[ci];
        t.notes.sort_unstable();
        t.nested_keys.sort_unstable();
        t.nested_keys.dedup();
        let mut stream = s.stream(ci).unwrap();
        let mut last = 0;
        let meta = |t: &mut TrackDigest, tick: u64, ty: u8, p: &[u8]| match ty {
            0x51 if p.len() == 3 => t
                .tempos
                .push((tick, u32::from_be_bytes([0, p[0], p[1], p[2]]))),
            0x58 if p.len() == 4 => t.time_signatures.push((tick, p[0], p[1], p[2], p[3])),
            _ => t.metas.push((tick, ty, p.to_vec())),
        };
        for (tick, ev) in stream.by_ref() {
            last = tick;
            if let Some(status) = ev.channel_status() {
                let [a, b] = ev.channel_data();
                let ch = status & 0x0F;
                match status & 0xF0 {
                    0x90 if b > 0 => t.ons.push((tick, ch, a, b)),
                    0x90 => t.offs.push((tick, ch, a, 0, true)),
                    0x80 => t.offs.push((tick, ch, a, b, false)),
                    0xA0 => t.pressures.push((tick, ch, a, b)),
                    0xB0 => t.controllers.push((tick, ch, a, b)),
                    0xC0 => t.programs.push((tick, ch, a)),
                    0xD0 => t.pressures.push((tick, ch, 255, a)),
                    _ => t
                        .pitch_bends
                        .push((tick, ch, u16::from(a) | (u16::from(b) << 7))),
                }
                continue;
            }
            match ev {
                OutEvent::Tempo(us) => t.tempos.push((tick, us)),
                OutEvent::Meta { meta_type, payload } => meta(t, tick, meta_type, payload),
                OutEvent::SysEx { payload } => t.sysex.push((tick, 0xF0, payload.to_vec())),
                OutEvent::Raw(kind) => match *kind {
                    RawKind::Meta { meta_type, payload } => {
                        meta(t, tick, meta_type, tr.events.payload(payload))
                    }
                    RawKind::SysEx { payload } => {
                        t.sysex
                            .push((tick, 0xF0, tr.events.payload(payload).to_vec()))
                    }
                    RawKind::SysExEscape { payload } => {
                        t.sysex
                            .push((tick, 0xF7, tr.events.payload(payload).to_vec()))
                    }
                    RawKind::System { status, data } => {
                        let n = match status {
                            0xF2 => 2,
                            0xF1 | 0xF3 => 1,
                            _ => 0,
                        };
                        let mut v = vec![status];
                        v.extend_from_slice(&data[..n]);
                        t.system.push((tick, v));
                    }
                    RawKind::Channel { .. } => unreachable!("has a status"),
                },
                OutEvent::NoteOn { .. } | OutEvent::NoteOff { .. } => unreachable!("has a status"),
            }
        }
        t.end_of_track = Some(stream.end_of_track().max(last));
    }
    d
}

/// Same-key, same-channel notes that overlap: (channel, key) of each such
/// group in each track. Standard MIDI cannot tell their pairs apart (§172).
pub fn overlapping_keys(t: &TrackDigest) -> std::collections::HashSet<(u8, u8)> {
    let mut by: std::collections::HashMap<(u8, u8), Vec<(u64, u64)>> = Default::default();
    for n in &t.notes {
        by.entry((n.2, n.3)).or_default().push((n.0, n.1));
    }
    by.into_iter()
        .filter(|(_, v)| {
            let mut v = v.clone();
            v.sort_unstable();
            let mut end = 0u64;
            let mut first = true;
            v.iter().any(|&(s, e)| {
                let overlaps = !first && s < end;
                first = false;
                end = end.max(e);
                overlaps
            })
        })
        .map(|(k, _)| k)
        .collect()
}

/// Compares the plan's digest with the file's (V09-0099): header, chunks,
/// every event list in order, End of Track, and every note exactly —
/// paired first in, first out — except on the plan's
/// [`TrackDigest::nested_keys`], where no Standard MIDI file can carry the
/// pairs to such a reader and the Note Ons and endings (already compared in
/// order) are the certified content (V09-0102, `midi::overlap`). Keys with
/// other overlaps (staggered, starting together) compare every note.
pub fn compare(plan: &Digest, file: &Digest) -> Result<(), String> {
    if (plan.format, plan.division) != (file.format, file.division) {
        return Err(format!(
            "header: plan {:?}, file {:?}",
            (plan.format, plan.division),
            (file.format, file.division)
        ));
    }
    if plan.other_chunks != file.other_chunks {
        return Err(format!(
            "chunks: {:?} vs {:?}",
            plan.other_chunks, file.other_chunks
        ));
    }
    if plan.tracks.len() != file.tracks.len() {
        return Err(format!(
            "{} tracks vs {}",
            plan.tracks.len(),
            file.tracks.len()
        ));
    }
    for (i, (p, f)) in plan.tracks.iter().zip(&file.tracks).enumerate() {
        macro_rules! same {
            ($field:ident) => {
                if p.$field != f.$field {
                    return Err(format!(
                        "track {}: {} differ ({} vs {})",
                        i + 1,
                        stringify!($field),
                        p.$field.len(),
                        f.$field.len()
                    ));
                }
            };
        }
        same!(ons);
        same!(offs);
        same!(controllers);
        same!(pitch_bends);
        same!(programs);
        same!(pressures);
        same!(tempos);
        same!(time_signatures);
        same!(metas);
        same!(sysex);
        same!(system);
        if p.end_of_track != f.end_of_track {
            return Err(format!(
                "track {}: End of Track {:?} vs {:?}",
                i + 1,
                p.end_of_track,
                f.end_of_track
            ));
        }
        let ambiguous = &p.nested_keys;
        let clear = |v: &[DNote]| -> Vec<DNote> {
            v.iter()
                .filter(|n| ambiguous.binary_search(&(n.2, n.3)).is_err())
                .copied()
                .collect()
        };
        let (pn, fnotes) = (clear(&p.notes), clear(&f.notes));
        if pn != fnotes {
            let first = pn.iter().zip(&fnotes).find(|(a, b)| a != b);
            return Err(format!(
                "track {}: notes differ ({} vs {}), first {:?}",
                i + 1,
                pn.len(),
                fnotes.len(),
                first
            ));
        }
    }
    Ok(())
}
