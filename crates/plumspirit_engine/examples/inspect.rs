//! Prints what a `.lup` project or MIDI file contains, and compares two files
//! musically (used to verify Save → Reopen and Export → Re-import).
//!
//! ```text
//! cargo run -p plumspirit_engine --example inspect -- song.lup
//! cargo run -p plumspirit_engine --example inspect -- song.lup song.mid
//! ```
//!
//! Musical comparison is exact: note times are compared as reduced fractions of a
//! quarter note (`tick / PPQN`), so files with different PPQN compare equal when they
//! describe the same music. Pitch, velocity, channel, release velocity and note-off
//! kind must match exactly, per track, in order. Tempo is compared through the
//! microseconds-per-quarter a MIDI file can store. When either file has linear
//! tempo ramps (which a MIDI file stores as many small steps), the tempo lists are
//! not compared; instead every note start must play at the same time within 1 ms.
//! Channel events other than notes (pitch bend, controllers, program changes …) are
//! compared per track, in order, by time (in quarter notes) and bytes; the listing
//! flags any two of the same kind on the same channel at the same tick (a
//! duplicate: only the last one would be heard).
//! Notes that start at the same tick may come back in another order from a file
//! whose track was split into several MIDI tracks (subchannel layers); that is
//! reported separately and does not count as a difference.
//!
//! The PlumSpirit structure (track tree, each track's subchannels and every
//! note's subchannel) is compared too and reported on its own line.
//! Exit code 0 = identical music (and structure), 1 = different music,
//! 3 = identical music with a different structure.

use plumspirit_engine::midi::{self, ImportControl};
use plumspirit_engine::model::conductor::{ConductorScope, Interp, format_tempo};
use plumspirit_engine::model::events::RawKind;
use plumspirit_engine::model::{PlumDocument, TempoMap, TempoValue};
use plumspirit_engine::native::{self, LupControl};
use std::path::Path;
use std::process::ExitCode;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// A tick as a reduced fraction of a quarter note.
fn beats(tick: u64, ppqn: u64) -> (u64, u64) {
    let g = gcd(tick, ppqn).max(1);
    (tick / g, ppqn / g)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Note {
    start: (u64, u64),
    duration: (u64, u64),
    pitch: u8,
    velocity: u8,
    channel: u8,
    off_velocity: u8,
    off_kind: String,
    /// PlumSpirit subchannel (compared as structure, not music).
    sub: u32,
}

/// A subchannel: id, MIDI channel, name.
type SubEntry = (u32, u8, Option<String>);

struct Music {
    ppqn: u64,
    tempos: Vec<((u64, u64), u32)>,
    ramps: usize,
    /// Seconds at each note start, per track (for comparing ramped tempo maps).
    seconds: Vec<Vec<f64>>,
    tracks: Vec<Vec<Note>>,
    /// Non-note channel events per track: (time in quarter notes, tick, bytes).
    channel_events: Vec<Vec<ChannelEvent>>,
    /// Per track: depth in the tree, the subchannel table (id, MIDI channel, name).
    structure: Vec<(u32, Vec<SubEntry>)>,
}

#[derive(Debug, PartialEq, Eq)]
struct ChannelEvent {
    at: (u64, u64),
    tick: u64,
    bytes: Vec<u8>,
}

impl ChannelEvent {
    /// Events that replace each other when they share a tick: same status byte
    /// (kind + channel), and for control changes the same controller number.
    fn slot(&self) -> (u8, Option<u8>) {
        let status = self.bytes[0];
        let cc = (status & 0xF0 == 0xB0).then(|| self.bytes[1]);
        (status, cc)
    }

    fn describe(&self) -> String {
        let status = self.bytes[0];
        let ch = (status & 0x0F) + 1;
        match status & 0xF0 {
            0xE0 => {
                let v = u16::from(self.bytes[1]) | (u16::from(self.bytes[2]) << 7);
                format!("pitch bend {v} ({:+})  ch {ch}", i32::from(v) - 8192)
            }
            0xB0 if self.bytes[1] == 10 => format!("pan {}  ch {ch}", self.bytes[2]),
            0xB0 => format!("CC {} = {}  ch {ch}", self.bytes[1], self.bytes[2]),
            0xC0 => format!("program {}  ch {ch}", self.bytes[1]),
            _ => format!("{:02X?}", self.bytes),
        }
    }
}

fn load(path: &Path) -> Result<PlumDocument, String> {
    if native::has_lup_signature(path) {
        native::load_project(path, LupControl::default())
            .map(|(d, _)| d)
            .map_err(|e| e.user_message())
    } else {
        midi::import_file(path, ImportControl::default())
            .map(|(d, s)| {
                for w in &s.warnings {
                    eprintln!("  import note: {}", w.user_message());
                }
                d
            })
            .map_err(|e| e.to_string())
    }
}

fn music(doc: &PlumDocument) -> Result<Music, String> {
    let ppqn = doc
        .division()
        .musical_ppqn()
        .ok_or("SMPTE timing: no musical comparison")?;
    let mut tempos = Vec::new();
    let mut tracks = Vec::new();
    let mut seconds: Vec<Vec<f64>> = Vec::new();
    let mut channel_events = Vec::new();
    let map = TempoMap::for_document(doc, 48_000);
    let c = doc.conductor();
    let mut ramps = 0;
    if c.scope() == ConductorScope::Timeline {
        // The Conductor's tempo map, as a MIDI file would store it.
        for p in c.tempo_points() {
            let us = match p.value {
                TempoValue::Micros(us) => us,
                TempoValue::Bpm(b) => b.to_micros_per_quarter().micros,
            };
            if !p.implicit {
                tempos.push((beats(p.tick, ppqn), us));
            }
            if p.interp == Interp::Linear {
                ramps += 1;
            }
        }
    }
    let rows = doc.tree_rows();
    let mut structure = Vec::new();
    for (i, t) in doc.tracks().iter().enumerate() {
        structure.push((
            rows[i].0,
            t.subchannels()
                .entries()
                .iter()
                .map(|e| (e.id.0, e.midi_channel, e.name.clone()))
                .collect(),
        ));
        for (tick, us) in t.events.tempos() {
            tempos.push((beats(tick, ppqn), us));
        }
        channel_events.push(
            t.events
                .events()
                .iter()
                .filter(|e| matches!(e.kind, RawKind::Channel { .. }))
                .map(|e| {
                    let mut bytes = Vec::new();
                    t.events.encode_event(&e.kind, &mut bytes);
                    ChannelEvent {
                        at: beats(e.tick, ppqn),
                        tick: e.tick,
                        bytes,
                    }
                })
                .collect(),
        );
        seconds.push(
            t.notes
                .iter()
                .map(|n| map.seconds_at_tick(n.start_tick))
                .collect(),
        );
        if let Some(v) = seconds.last_mut() {
            v.sort_by(f64::total_cmp);
        }
        let notes = t
            .notes
            .iter()
            .map(|n| Note {
                start: beats(n.start_tick, ppqn),
                duration: beats(n.duration_ticks, ppqn),
                pitch: n.attrs.pitch(),
                velocity: n.attrs.velocity(),
                channel: n.attrs.channel(),
                off_velocity: n.attrs.off_velocity(),
                off_kind: format!("{:?}", n.attrs.off_kind()),
                sub: n.sub.0,
            })
            .collect();
        tracks.push(notes);
    }
    tempos.sort();
    Ok(Music {
        ppqn,
        tempos,
        ramps,
        seconds,
        tracks,
        channel_events,
        structure,
    })
}

fn describe(path: &Path, doc: &PlumDocument, m: &Music) {
    let p = doc.project();
    println!("{}", path.display());
    println!("  name        {}", p.name);
    println!("  PPQN        {}", m.ppqn);
    let c = doc.conductor();
    println!("  conductor   {:?}", c.scope());
    for t in c.tempo_points() {
        let us = match t.value {
            TempoValue::Micros(us) => format!("  ({us} µs per quarter)"),
            TempoValue::Bpm(_) => String::new(),
        };
        println!(
            "  tempo       tick {:>12}  {}{us}{}{}",
            t.tick,
            format_tempo(t.value),
            if t.interp == Interp::Linear {
                "  (linear ramp to the next point)"
            } else {
                ""
            },
            if t.implicit { "  (default)" } else { "" }
        );
    }
    for s in c.time_signatures() {
        println!(
            "  signature   tick {:>12}  {}{}",
            s.tick,
            s.label(),
            if s.implicit { "  (default)" } else { "" }
        );
    }
    for mk in c.markers() {
        match mk.end {
            Some(end) => println!("  chapter     tick {:>12}–{end}  {:?}", mk.start, mk.title),
            None => println!("  marker      tick {:>12}  {:?}", mk.start, mk.title),
        }
    }
    println!(
        "  onion       {} (opacity {}%, specific {:?})",
        p.onion.mode.label(),
        p.onion.opacity,
        p.onion.specific.iter().map(|t| t.0).collect::<Vec<_>>()
    );
    println!(
        "  soundfont   {}",
        p.soundfont.as_ref().map_or("(none)", |s| s.path.as_str())
    );
    let mut duplicates = 0;
    for (i, ((t, notes), events)) in doc
        .tracks()
        .iter()
        .zip(&m.tracks)
        .zip(&m.channel_events)
        .enumerate()
    {
        println!(
            "  track {} (id {}) {:?}: {} notes, {} other events ({} channel events)",
            i + 1,
            t.id().0,
            t.name().unwrap_or(""),
            notes.len(),
            t.events.len(),
            events.len()
        );
        for (k, e) in events.iter().enumerate() {
            let dup = events[..k]
                .iter()
                .rev()
                .take_while(|p| p.tick == e.tick)
                .any(|p| p.slot() == e.slot());
            if dup {
                duplicates += 1;
            }
            if k < 40 {
                println!(
                    "    event at {:>3}/{:<3} q  {}{}",
                    e.at.0,
                    e.at.1,
                    e.describe(),
                    if dup { "  DUPLICATE" } else { "" }
                );
            }
        }
        if events.len() > 40 {
            println!("    …");
        }
        for n in notes.iter().take(40) {
            println!(
                "    at {:>3}/{:<3} q  len {:>3}/{:<3} q  pitch {:>3}  vel {:>3}  ch {:>2}",
                n.start.0,
                n.start.1,
                n.duration.0,
                n.duration.1,
                n.pitch,
                n.velocity,
                n.channel + 1
            );
        }
        if notes.len() > 40 {
            println!("    …");
        }
    }
    println!("  duplicates  {duplicates} channel event(s) replaced at the same tick");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        eprintln!("usage: inspect FILE [OTHER]   (.lup or .mid)");
        return ExitCode::from(2);
    }
    let mut all = Vec::new();
    for a in &args {
        let path = Path::new(a);
        let doc = match load(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{a}: {e}");
                return ExitCode::from(2);
            }
        };
        let m = match music(&doc) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{a}: {e}");
                return ExitCode::from(2);
            }
        };
        describe(path, &doc, &m);
        all.push(m);
    }
    if let [a, b] = &all[..] {
        let mut same = true;
        if a.ramps == 0 && b.ramps == 0 {
            if a.tempos != b.tempos {
                println!("DIFFERENT tempo: {:?} vs {:?}", a.tempos, b.tempos);
                same = false;
            }
        } else {
            let worst = a
                .seconds
                .iter()
                .flatten()
                .zip(b.seconds.iter().flatten())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f64, f64::max);
            println!(
                "tempo ramps: note starts differ by at most {:.3} ms",
                worst * 1000.0
            );
            if worst > 0.001 {
                println!("DIFFERENT timing (more than 1 ms)");
                same = false;
            }
        }
        if a.tracks.len() != b.tracks.len() {
            println!(
                "DIFFERENT track count: {} vs {}",
                a.tracks.len(),
                b.tracks.len()
            );
            same = false;
        }
        let mut reordered = 0usize;
        let mut subs_differ = Vec::new();
        let music_key = |v: &[Note]| {
            let mut k: Vec<Note> = v
                .iter()
                .map(|n| Note {
                    sub: 0,
                    ..n.clone()
                })
                .collect();
            k.sort();
            k
        };
        for (i, (x, y)) in a.tracks.iter().zip(&b.tracks).enumerate() {
            let strip = |v: &[Note]| {
                v.iter()
                    .map(|n| Note {
                        sub: 0,
                        ..n.clone()
                    })
                    .collect::<Vec<_>>()
            };
            if strip(x) != strip(y) {
                if music_key(x) == music_key(y) {
                    // Same notes; only notes starting together changed places.
                    reordered += strip(x)
                        .iter()
                        .zip(strip(y))
                        .filter(|(p, q)| *p != q)
                        .count();
                } else {
                    same = false;
                    let at = x.iter().zip(y).position(|(p, q)| p != q);
                    println!(
                        "DIFFERENT track {}: {} vs {} notes, first difference at note {:?}",
                        i + 1,
                        x.len(),
                        y.len(),
                        at
                    );
                }
            }
            let mut sx: Vec<Note> = x.clone();
            let mut sy: Vec<Note> = y.clone();
            sx.sort();
            sy.sort();
            if sx != sy {
                subs_differ.push(i + 1);
            }
        }
        if reordered > 0 {
            println!(
                "ORDER: {reordered} note(s) that start at the same tick are in another order (not a musical difference)"
            );
        }
        for (i, (x, y)) in a.channel_events.iter().zip(&b.channel_events).enumerate() {
            let strip = |v: &[ChannelEvent]| {
                v.iter()
                    .map(|e| (e.at, e.bytes.clone()))
                    .collect::<Vec<_>>()
            };
            if strip(x) != strip(y) {
                same = false;
                println!(
                    "DIFFERENT track {} channel events: {} vs {}",
                    i + 1,
                    x.len(),
                    y.len()
                );
            }
        }
        let n: usize = a.tracks.iter().map(Vec::len).sum();
        let e: usize = a.channel_events.iter().map(Vec::len).sum();
        let structure_same = a.structure == b.structure && subs_differ.is_empty();
        if structure_same {
            let subs: usize = a.structure.iter().map(|s| s.1.len()).sum();
            let nested = a.structure.iter().filter(|s| s.0 > 0).count();
            println!(
                "SAME STRUCTURE: {nested} subtrack(s), {subs} subchannel(s) (ids, MIDI channels, names), every note in the same subchannel"
            );
        } else {
            if a.structure
                .iter()
                .map(|s| s.0)
                .ne(b.structure.iter().map(|s| s.0))
            {
                println!("STRUCTURE: the track trees differ");
            }
            for (i, (x, y)) in a.structure.iter().zip(&b.structure).enumerate() {
                if x.1 != y.1 {
                    println!(
                        "STRUCTURE: track {} subchannels differ: {} vs {}",
                        i + 1,
                        x.1.len(),
                        y.1.len()
                    );
                }
            }
            if !subs_differ.is_empty() {
                println!("STRUCTURE: notes in other subchannels in track(s) {subs_differ:?}");
            }
        }
        if same {
            println!(
                "SAME MUSIC: {} tracks, {n} notes, {e} other channel events, identical timing (in quarter notes), pitches, velocities, channels and controller values",
                a.tracks.len()
            );
            if !structure_same {
                return ExitCode::from(3);
            }
        } else {
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}
