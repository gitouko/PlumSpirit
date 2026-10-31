//! Hand-written MIDI byte builders for fixtures.
//!
//! These deliberately do NOT use PlumSpirit's exporter, so fixtures are an
//! independent reference. Callers control running status explicitly.
#![allow(dead_code)]

pub mod corpus;
pub mod kiva;
pub mod mido;
pub mod smf_digest;

use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};

pub fn vlq(mut v: u64) -> Vec<u8> {
    let mut groups = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        groups.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
    groups.reverse();
    groups
}

/// Builds one track's event bytes with absolute ticks.
#[derive(Default, Clone)]
pub struct TrackBytes {
    pub data: Vec<u8>,
    last: u64,
}

impl TrackBytes {
    pub fn new() -> Self {
        TrackBytes::default()
    }

    /// Raw event bytes at an absolute tick (status byte included or omitted by caller).
    pub fn ev(&mut self, tick: u64, bytes: &[u8]) -> &mut Self {
        assert!(tick >= self.last, "ticks must be non-decreasing");
        self.data.extend(vlq(tick - self.last));
        self.data.extend_from_slice(bytes);
        self.last = tick;
        self
    }

    pub fn on(&mut self, tick: u64, ch: u8, key: u8, vel: u8) -> &mut Self {
        self.ev(tick, &[0x90 | ch, key, vel])
    }

    pub fn off(&mut self, tick: u64, ch: u8, key: u8, vel: u8) -> &mut Self {
        self.ev(tick, &[0x80 | ch, key, vel])
    }

    pub fn meta(&mut self, tick: u64, ty: u8, payload: &[u8]) -> &mut Self {
        let mut b = vec![0xFF, ty];
        b.extend(vlq(payload.len() as u64));
        b.extend_from_slice(payload);
        self.ev(tick, &b)
    }

    pub fn sysex(&mut self, tick: u64, payload: &[u8]) -> &mut Self {
        let mut b = vec![0xF0];
        b.extend(vlq(payload.len() as u64));
        b.extend_from_slice(payload);
        self.ev(tick, &b)
    }

    pub fn eot(&mut self, tick: u64) -> Vec<u8> {
        self.meta(tick, 0x2F, &[]);
        self.data.clone()
    }
}

pub fn smf(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"MThd".to_vec();
    out.extend(6u32.to_be_bytes());
    out.extend(format.to_be_bytes());
    out.extend((tracks.len() as u16).to_be_bytes());
    out.extend(division.to_be_bytes());
    for t in tracks {
        out.extend(b"MTrk");
        out.extend((t.len() as u32).to_be_bytes());
        out.extend(t);
    }
    out
}

pub fn attrs(pitch: u8, vel: u8, ch: u8) -> NoteAttrs {
    NoteAttrs::new(pitch, vel, ch, 0, NoteOffKind::NoteOff).unwrap()
}

pub fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: attrs(pitch, 100, 0),
        sub: Default::default(),
        group: 0,
    }
}

/// (start, duration, pitch, velocity, channel) of every note in a track.
pub fn notes_of(
    doc: &plumspirit_engine::model::PlumDocument,
    track: usize,
) -> Vec<(u64, u64, u8, u8, u8)> {
    doc.track(track)
        .unwrap()
        .notes
        .iter()
        .map(|n| {
            (
                n.start_tick,
                n.duration_ticks,
                n.attrs.pitch(),
                n.attrs.velocity(),
                n.attrs.channel(),
            )
        })
        .collect()
}

/// Canonical note multiset of a track: sorted (start, dur, raw attrs).
pub fn canonical_notes(
    doc: &plumspirit_engine::model::PlumDocument,
    track: usize,
) -> Vec<(u64, u64, u32)> {
    let mut v: Vec<(u64, u64, u32)> = doc
        .track(track)
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
        .collect();
    v.sort_unstable();
    v
}

/// Deterministic xorshift PRNG for property/fuzz tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Generates a deterministic Black-MIDI-like single-track file with `notes` notes
/// using running status and NoteOn-velocity-0 terminations, plus some CC/tempo.
pub fn generate_dense_smf(notes: usize, ppqn: u16, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    // Build (tick, order, bytes) events, then serialize with running status.
    let mut events: Vec<(u64, u64, [u8; 3])> = Vec::with_capacity(notes * 2 + 64);
    let mut t = 0u64;
    let mut order = 0u64;
    for i in 0..notes {
        if i % 8 == 0 {
            t += rng.below(u64::from(ppqn) / 4 + 1);
        }
        let key = 21 + rng.below(88) as u8;
        let ch = (i % 16) as u8;
        let dur = 1 + rng.below(u64::from(ppqn) * 2);
        events.push((t, order, [0x90 | ch, key, 1 + rng.below(127) as u8]));
        order += 1;
        events.push((t + dur, order, [0x90 | ch, key, 0]));
        order += 1;
        if i % 5000 == 0 {
            events.push((t, order, [0xB0 | ch, 7, rng.below(128) as u8]));
            order += 1;
        }
    }
    // Terminations before starts at the same tick keep FIFO pairing unambiguous.
    events.sort_by_key(|(tick, ord, b)| (*tick, u8::from(b[2] != 0 || b[0] & 0xF0 != 0x90), *ord));
    let mut tr = TrackBytes::new();
    tr.meta(0, 0x03, b"Dense");
    tr.meta(0, 0x51, &[0x07, 0xA1, 0x20]);
    let mut running = 0u8;
    let mut last = 0u64;
    for (tick, _, b) in &events {
        tr.data.extend(vlq(tick - last));
        last = *tick;
        if b[0] != running {
            tr.data.push(b[0]);
            running = b[0];
        }
        tr.data.extend_from_slice(&b[1..]);
    }
    // EOT directly (running status is irrelevant for meta).
    tr.data.extend(vlq(0));
    tr.data.extend_from_slice(&[0xFF, 0x2F, 0x00]);
    smf(0, ppqn, &[tr.data])
}
