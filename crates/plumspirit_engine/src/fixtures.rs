//! Deterministic synthetic documents for stress tests, benchmarks and manual testing.
//!
//! These are generated on demand (never committed as large binaries). The same
//! `(shape, notes, seed)` always produces the same document.

use crate::model::document::{PlumDocument, SmfFormat};
use crate::model::events::{EventStore, RawEvent, RawKind, meta};
use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
use crate::model::note_store::NoteStore;
use crate::model::timing::Division;
use crate::model::track::{Track, TrackId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureShape {
    /// Black-MIDI-like: chords of notes every sixteenth, spread over many bars.
    Dense,
    /// Every note inside the first four beats: extreme on-screen density.
    ExtremeDensity,
    /// Notes ~200 million ticks apart: a very long timeline (still exportable).
    LongSparse,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Builds a two-track Format 1 document (conductor track + note track) with
/// `notes` notes.
pub fn generate(shape: FixtureShape, notes: usize, ppqn: u32, seed: u64) -> PlumDocument {
    let mut rng = Rng(seed | 1);
    let ppq = u64::from(ppqn.max(1));
    let mut recs: Vec<NoteRecord> = Vec::with_capacity(notes);
    let mut t = 0u64;
    for i in 0..notes {
        let (start, dur) = match shape {
            FixtureShape::Dense => {
                if i % 12 == 0 {
                    t += ppq / 4;
                }
                (t, 1 + rng.below(ppq))
            }
            FixtureShape::ExtremeDensity => (rng.below(ppq * 4), 1 + rng.below(ppq / 2 + 1)),
            FixtureShape::LongSparse => (
                i as u64 * 200_000_000 + rng.below(1000),
                1 + rng.below(ppq * 8),
            ),
        };
        let pitch = (21 + rng.below(88)) as u8;
        let vel = (1 + rng.below(127)) as u8;
        let ch = (i % 16) as u8;
        let kind = if i % 3 == 0 {
            NoteOffKind::NoteOff
        } else {
            NoteOffKind::NoteOnZero
        };
        let attrs = NoteAttrs::new(pitch, vel, ch, 0, kind).expect("valid by construction");
        recs.push(NoteRecord::new(start, dur, attrs));
    }
    recs.sort_by_key(|r| r.start_tick);
    let notes_store = NoteStore::from_sorted_records(&recs, false).expect("valid records");
    drop(recs);

    let mut conductor = EventStore::new();
    let push_meta = |store: &mut EventStore, tick: u64, ty: u8, payload: &[u8]| {
        let r = store.store_payload(payload).expect("small payload");
        store
            .push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Meta {
                    meta_type: ty,
                    payload: r,
                },
            })
            .expect("sorted");
    };
    push_meta(&mut conductor, 0, meta::TRACK_NAME, b"Conductor");
    push_meta(&mut conductor, 0, meta::TIME_SIGNATURE, &[4, 2, 24, 8]);
    push_meta(&mut conductor, 0, meta::TEMPO, &[0x07, 0xA1, 0x20]);
    let mut lead = EventStore::new();
    push_meta(&mut lead, 0, meta::TRACK_NAME, b"Notes");
    lead.push(RawEvent {
        tick: 0,
        anchor: 0,
        kind: RawKind::Channel {
            status: 0xC0,
            data: [0, 0],
        },
    })
    .expect("sorted");
    let tracks = vec![
        Track::from_parts(TrackId(0), NoteStore::new(), conductor, Some(0)),
        Track::from_parts(TrackId(1), notes_store, lead, None),
    ];
    PlumDocument::from_parts(
        SmfFormat::Parallel,
        Division::ppqn(ppqn),
        tracks,
        Vec::new(),
    )
}

// ------------------------------------------------------------ v0.3 fixtures

/// Replaces the Conductor's tempo map with `points` points `spacing` ticks apart
/// (the first at tick 0), between 40 and 400 BPM, alternating linear ramps and
/// steps. Deterministic for a given `seed`.
pub fn set_dense_tempo(doc: &mut PlumDocument, points: usize, spacing: u64, seed: u64) {
    use crate::model::Bpm;
    use crate::model::conductor::{Conductor, Interp, TempoPoint};
    use crate::model::tempo::TempoValue;
    let mut rng = Rng(seed | 1);
    let c = doc.conductor();
    let mut next_id = c.next_id();
    let tempo: Vec<TempoPoint> = (0..points.max(1))
        .map(|i| {
            let bpm = 40 + rng.below(361);
            let id = next_id;
            next_id += 1;
            TempoPoint {
                id,
                tick: i as u64 * spacing.max(1),
                value: TempoValue::Bpm(Bpm::parse(&bpm.to_string()).expect("valid BPM")),
                interp: if i % 2 == 0 {
                    Interp::Linear
                } else {
                    Interp::Step
                },
                implicit: false,
                origin: None,
            }
        })
        .collect();
    let conductor = Conductor::from_parts(
        c.scope(),
        tempo,
        c.time_signatures().to_vec(),
        c.markers().to_vec(),
        next_id,
    )
    .expect("valid tempo map");
    *doc.conductor_mut() = conductor;
}

/// Replaces the Conductor's markers with `count` markers `spacing` ticks apart;
/// every other one is a chapter three spacings long.
pub fn set_many_markers(doc: &mut PlumDocument, count: usize, spacing: u64) {
    use crate::model::conductor::{Conductor, Marker};
    let c = doc.conductor();
    let mut next_id = c.next_id();
    let spacing = spacing.max(1);
    let markers: Vec<Marker> = (0..count)
        .map(|i| {
            let id = next_id;
            next_id += 1;
            let start = i as u64 * spacing;
            Marker {
                id,
                start,
                end: (i % 2 == 1).then(|| start + 3 * spacing),
                title: format!("Marker {i}"),
                raw_title: None,
                cue: false,
                origin: None,
            }
        })
        .collect();
    let conductor = Conductor::from_parts(
        c.scope(),
        c.tempo_points().to_vec(),
        c.time_signatures().to_vec(),
        markers,
        next_id,
    )
    .expect("valid markers");
    *doc.conductor_mut() = conductor;
}

/// Appends `count` controller events to track `track_index`, `spacing` ticks
/// apart after the track's last event: pitch bend and pan (CC 10) on channel 1,
/// alternating, with seeded values. At most one event of a kind per tick.
pub fn add_dense_controllers(
    doc: &mut PlumDocument,
    track_index: usize,
    count: usize,
    spacing: u64,
    seed: u64,
) {
    let mut rng = Rng(seed | 1);
    let Some(track) = doc.track_mut(track_index) else {
        return;
    };
    let start = track.events.last_tick().unwrap_or(0);
    for i in 0..count {
        let tick = start + i as u64 * spacing.max(1);
        let kind = if i % 2 == 0 {
            let v = rng.below(16384) as u16;
            RawKind::Channel {
                status: 0xE0,
                data: [(v & 0x7F) as u8, (v >> 7) as u8],
            }
        } else {
            RawKind::Channel {
                status: 0xB0,
                data: [10, rng.below(128) as u8],
            }
        };
        track
            .events
            .push(RawEvent {
                tick,
                anchor: 0,
                kind,
            })
            .expect("sorted by construction");
    }
    track.bump_events_revision();
}
