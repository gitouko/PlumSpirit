//! Playback scheduling, independent of any audio backend.
//!
//! ```text
//! PlumDocument ──► PlaybackIndex (note snapshots + control events + tempo map
//!                        │         + corrections, routes, mute/solo, master volume)
//!                        │ immutable, shared (Arc)
//!                        ▼
//!                  Scheduler ──► SynthSink per output route (note on/off,
//!                                controllers, master volume) ──► synthesizer / MIDI out
//! ```
//!
//! * **No note copies.** A [`PlaybackIndex`] holds a [`NoteSnapshot`] per track —
//!   reference-counted pointers to the document's note chunks. Pressing Play or
//!   editing while playing never copies notes; an edit only copies the chunks it
//!   touches, so the audio side never sees a half-applied change.
//! * **Sample-based clock.** Time advances by audio frames. Each block of frames
//!   is mapped to a tick range with the exact [`TempoMap`] (built at the playback
//!   speed), and every event whose tick falls in that range is sent before the
//!   block is rendered (timing resolution = one block, 64 frames ≈ 1.3 ms at
//!   48 kHz). v1.0-Launch (L-B02): a sink that delivers messages at their own
//!   times ([`SynthSink::timed`], the external MIDI outputs) is told each
//!   message's frame offset inside the block ([`SynthSink::at_offset`]), so
//!   rapid repeated notes keep their exact spacing on a MIDI output.
//! * **Order at one tick.** Note-offs of earlier notes, then controller/program
//!   events, then note-ons — so a program change at tick T applies to notes at T.
//! * **Bounded work, no silent loss** (v0.8 §131 / §132). At most
//!   [`SchedulerLimits::max_note_ons_per_block`] notes start per block; the
//!   rest of a burst is *deferred* — emission stops for that block (keeping
//!   the order of every event) and continues in the next ones, so dense bursts
//!   are spread over a few milliseconds instead of being dropped (counted as
//!   `note_ons_deferred`). Only a note that would start more than
//!   [`SchedulerLimits::max_lateness_frames`] late — the density is above
//!   what the budget can render — or that finds
//!   [`SchedulerLimits::max_pending_offs`] notes already sounding is skipped:
//!   counted (`note_ons_skipped`, `late_dropped`), never deleted from the
//!   project, and without its note-off, so nothing can get stuck.
//! * **Seeking** rebuilds channel state (bank, program, controllers, RPN pitch-bend
//!   range and tuning, pitch bend, channel pressure) from checkpoints taken every
//!   [`CHECKPOINT_INTERVAL`] control events, so a seek never rescans the song.
//!   Notes that are sounding at the seek position are re-struck there with their
//!   own velocity (deterministic re-articulation; sample envelopes are not
//!   resumed mid-way).
//! * **Resume** (v1.0-beta1 B1-0082 / B1-0083). Pause ends every voice (no
//!   note is left sounding), so playing on from the paused tick is a seek —
//!   and a seek re-strikes the notes held there, which is heard as a held
//!   note playing again. [`Scheduler::seek_multi_with`] with `restrike =
//!   false` positions without re-striking: the held notes stay silent until
//!   their own end (nothing is queued for them), every later note plays.
//!   The app uses it for a resume at the paused tick unless the user asked
//!   for re-striking; any other seek re-strikes, so entering a note late
//!   still sounds it.
//! * **Track corrections** (tick / key / gate, key range) are applied as notes
//!   are scheduled: a corrected track is two sorted runs of its snapshot (before
//!   and after the correction start) merged by corrected start; channel events
//!   are shifted when the control index is built. Nothing is copied or changed
//!   in the document, so corrections are applied exactly once.
//! * **Mute / Solo** is an eligibility mask per track. A muted (or, while any
//!   track is soloed, an unsoloed) track starts no notes. Its controller,
//!   program and pitch-bend events still update the channel state, because MIDI
//!   channels are shared between tracks: muting one track never changes how
//!   another track on the same channel sounds. Changing the mask while playing
//!   ends the newly muted tracks' sounding notes — except a note whose channel
//!   and key another audible track is also holding (MIDI cannot tell the two
//!   voices apart, so the other track's note is not cut) — and re-strikes the
//!   newly audible tracks' sounding notes.
//! * **Keys above 127** (v0.9 256-Key Mode, as stored or after a key
//!   correction) are never sent: MIDI keys stop at 127, so a synthesizer or an
//!   external output never receives a folded or invalid key byte
//!   ([`SchedulerStats::above_midi`] counts them).
//! * **Output routes.** Each track plays through one route (0 = the internal
//!   synthesizer, others = external MIDI outputs); every route has its own
//!   channel state and control index, so a controller of a track on one route
//!   never reaches another.
//! * **Loop.** With a loop range, reaching its end ends every sounding note (with
//!   its release), moves each channel from its state at the loop end to its state
//!   at the loop start (sustain released, controllers, programs, bend reset to
//!   exactly what they are at the start), re-strikes the notes sounding at the
//!   loop start and continues from there. The wrap is computed on the tick
//!   timeline, so tempo ramps never drift across iterations.

use crate::model::conductor::MASTER_VOLUME_MAX;
use crate::model::correction::{Corrected, Correction};
use crate::model::document::PlumDocument;
use crate::model::events::RawKind;
use crate::model::note::{MAX_PITCH, NoteRecord};
use crate::model::note_store::{NoteSnapshot, SnapshotPos};
use crate::model::tempo::TempoMap;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

/// Control events between two channel-state checkpoints.
pub const CHECKPOINT_INTERVAL: usize = 2048;

/// Receives MIDI messages from the scheduler (implemented by the synthesizer
/// wrapper, an external MIDI output, or by a recorder in tests).
pub trait SynthSink {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8);
    fn note_off(&mut self, channel: u8, key: u8);
    /// Controller, program change, pitch bend or pressure (`status` includes the
    /// channel).
    fn channel_message(&mut self, status: u8, d0: u8, d1: u8);
    /// Stops every voice immediately.
    fn silence(&mut self);
    /// Resets every channel to power-on state (programs, controllers) and stops
    /// every voice.
    fn reset(&mut self);
    /// The project's Master Volume (0–16383, 16383 = full level).
    fn master_volume(&mut self, _value: u16) {}
    /// v1.0-beta2 (B2-F12B): a General MIDI / GS / XG system reset of the
    /// song (`message`: the complete SysEx, `F0` … `F7`), sent only when
    /// Event Processing asks for it. Default (the internal synthesizer, which
    /// takes no SysEx): every channel back to the General MIDI power-on state
    /// with channel messages; sounding notes go on. An external output sends
    /// the message itself.
    fn system_reset(&mut self, _message: &[u8]) {
        for ch in 0..16u8 {
            for (st, d0, d1) in GM_POWER_ON {
                self.channel_message(st | ch, d0, d1);
            }
        }
    }
    /// v1.0-Launch (L-B02): whether this sink delivers each message at its
    /// own time inside the block (an external MIDI output). The internal
    /// synthesizer renders whole blocks and says no (the default), so the
    /// scheduler does no extra work for it.
    fn timed(&self) -> bool {
        false
    }
    /// L-B02: the messages that follow are due `frames` frames after the
    /// start of the block being scheduled (until the next call). Only
    /// called on sinks that are [`SynthSink::timed`].
    fn at_offset(&mut self, _frames: u32) {}
}

/// The General MIDI power-on state of a channel as channel messages (status
/// without the channel, data): Reset All Controllers, bank 0, program 1,
/// pitch bend centred, volume 100, pan centre, expression 127, reverb 40,
/// chorus 0.
pub const GM_POWER_ON: [(u8, u8, u8); 10] = [
    (0xB0, 121, 0),
    (0xB0, 0, 0),
    (0xB0, 32, 0),
    (0xC0, 0, 0),
    (0xE0, 0, 64),
    (0xB0, 7, 100),
    (0xB0, 10, 64),
    (0xB0, 11, 127),
    (0xB0, 91, 40),
    (0xB0, 93, 0),
];

/// v1.0-beta2 (B2-F12B): the system mode a reset SysEx sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemMode {
    /// Universal Non-Real-Time General MIDI System On (`7E dd 09 01`).
    GmOn,
    /// General MIDI 2 System On (`7E dd 09 03`).
    Gm2On,
    /// General MIDI System Off (`7E dd 09 02`).
    GmOff,
    /// Roland GS Reset (`41 dd 42 12 40 00 7F 00 41`) or GS System Mode Set
    /// (`41 dd 42 12 00 00 7F 0m cs`).
    Gs,
    /// Yamaha XG System On (`43 1n 4C 00 00 7E 00`).
    Xg,
}

/// The system mode a SysEx payload (the bytes after `F0`, its `F7` included
/// or not) resets to, or `None` when it is not a system reset.
pub fn classify_reset(payload: &[u8]) -> Option<SystemMode> {
    let p = payload.strip_suffix(&[0xF7]).unwrap_or(payload);
    match p {
        [0x7E, d, 0x09, 0x01] if *d < 0x80 => Some(SystemMode::GmOn),
        [0x7E, d, 0x09, 0x02] if *d < 0x80 => Some(SystemMode::GmOff),
        [0x7E, d, 0x09, 0x03] if *d < 0x80 => Some(SystemMode::Gm2On),
        // Roland: address + data + checksum add up to a multiple of 128.
        [0x41, d, 0x42, 0x12, a0, a1, a2, v, cs]
            if *d < 0x80
                && matches!(
                    (a0, a1, a2, v),
                    (0x40, 0x00, 0x7F, 0x00) | (0x00, 0x00, 0x7F, 0x00 | 0x01)
                )
                && (u32::from(*a0)
                    + u32::from(*a1)
                    + u32::from(*a2)
                    + u32::from(*v)
                    + u32::from(*cs))
                    % 128
                    == 0 =>
        {
            Some(SystemMode::Gs)
        }
        [0x43, d, 0x4C, 0x00, 0x00, 0x7E, 0x00] if *d & 0xF0 == 0x10 => Some(SystemMode::Xg),
        _ => None,
    }
}

/// A system reset of the song on a route (B2-F12B).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemReset {
    pub tick: u64,
    pub track: u32,
    pub mode: SystemMode,
    /// The complete message (`F0` … `F7`).
    pub message: Arc<[u8]>,
}

/// v1.0-beta2 (B2-F12B): what playback does with the song's system resets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResetPolicy {
    /// Not sent (PlumSpirit before v1.0-beta2; the default).
    #[default]
    Skip,
    /// Every reset is sent.
    Send,
    /// A reset to the mode already set is ignored (the first one is always
    /// sent). Risk: program and controller changes made before an ignored
    /// reset stay, where a song may count on the reset to clear them.
    SendChanged,
}

impl ResetPolicy {
    pub fn key(self) -> &'static str {
        match self {
            ResetPolicy::Skip => "skip",
            ResetPolicy::Send => "send",
            ResetPolicy::SendChanged => "send_changed",
        }
    }

    pub fn from_key(k: &str) -> Option<ResetPolicy> {
        [
            ResetPolicy::Skip,
            ResetPolicy::Send,
            ResetPolicy::SendChanged,
        ]
        .into_iter()
        .find(|p| p.key() == k)
    }
}

/// A sound-relevant channel event from the preserved events of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlEvent {
    pub tick: u64,
    /// Index of the track the event belongs to.
    pub track: u32,
    pub status: u8,
    pub d0: u8,
    pub d1: u8,
}

/// Reconstructable state of one MIDI channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelState {
    pub program: Option<u8>,
    pub cc: [u8; 128],
    /// Bit n set = controller n was set.
    pub cc_set: u128,
    pub bend: Option<(u8, u8)>,
    pub pressure: Option<u8>,
    /// Selected parameter: `Some((msb, lsb, is_rpn))`.
    pub selected: Option<(u8, u8, bool)>,
    /// RPN 0 (pitch-bend range), 1 (fine tuning), 2 (coarse tuning): (MSB, LSB).
    pub rpn: [Option<(u8, Option<u8>)>; 3],
}

impl Default for ChannelState {
    fn default() -> Self {
        ChannelState {
            program: None,
            cc: [0; 128],
            cc_set: 0,
            bend: None,
            pressure: None,
            selected: None,
            rpn: [None; 3],
        }
    }
}

impl ChannelState {
    fn apply(&mut self, status: u8, d0: u8, d1: u8) {
        match status & 0xF0 {
            0xB0 => {
                let n = usize::from(d0 & 0x7F);
                match n {
                    101 | 100 => {
                        let (mut msb, mut lsb) = match self.selected {
                            Some((m, l, true)) => (m, l),
                            _ => (127, 127),
                        };
                        if n == 101 {
                            msb = d1;
                        } else {
                            lsb = d1;
                        }
                        self.selected = Some((msb, lsb, true));
                    }
                    99 | 98 => {
                        let (mut msb, mut lsb) = match self.selected {
                            Some((m, l, false)) => (m, l),
                            _ => (127, 127),
                        };
                        if n == 99 {
                            msb = d1;
                        } else {
                            lsb = d1;
                        }
                        self.selected = Some((msb, lsb, false));
                    }
                    6 | 38 => {
                        if let Some((0, l, true)) = self.selected
                            && l <= 2
                        {
                            let slot = &mut self.rpn[usize::from(l)];
                            if n == 6 {
                                *slot = Some((d1, None));
                            } else if let Some((m, _)) = *slot {
                                *slot = Some((m, Some(d1)));
                            }
                        }
                    }
                    121 => {
                        // Reset All Controllers (RP-015): modulation, expression,
                        // pedals, pitch bend, pressure; volume/pan/bank/program stay.
                        for c in [1usize, 11, 64, 65, 66, 67] {
                            self.cc_set &= !(1u128 << c);
                        }
                        self.bend = None;
                        self.pressure = None;
                        self.selected = None;
                    }
                    120 | 123..=127 => {}
                    _ => {
                        self.cc[n] = d1;
                        self.cc_set |= 1u128 << n;
                    }
                }
            }
            0xC0 => self.program = Some(d0),
            0xD0 => self.pressure = Some(d0),
            0xE0 => self.bend = Some((d0, d1)),
            _ => {}
        }
    }

    fn is_default(&self) -> bool {
        *self == ChannelState::default()
    }

    /// Sends this state to a freshly reset synthesizer channel.
    fn restore(&self, ch: u8, sink: &mut dyn SynthSink) {
        if self.is_default() {
            return;
        }
        let cc = 0xB0 | ch;
        for bank in [0usize, 32] {
            if self.cc_set & (1u128 << bank) != 0 {
                sink.channel_message(cc, bank as u8, self.cc[bank]);
            }
        }
        if let Some(p) = self.program {
            sink.channel_message(0xC0 | ch, p, 0);
        }
        for (i, v) in self.rpn.iter().enumerate() {
            if let Some((msb, lsb)) = v {
                sink.channel_message(cc, 101, 0);
                sink.channel_message(cc, 100, i as u8);
                sink.channel_message(cc, 6, *msb);
                if let Some(l) = lsb {
                    sink.channel_message(cc, 38, *l);
                }
            }
        }
        match self.selected {
            Some((m, l, true)) => {
                sink.channel_message(cc, 101, m);
                sink.channel_message(cc, 100, l);
            }
            Some((m, l, false)) => {
                sink.channel_message(cc, 99, m);
                sink.channel_message(cc, 98, l);
            }
            None if self.rpn.iter().any(Option::is_some) => {
                // Deselect so later data entry does not change the restored RPNs.
                sink.channel_message(cc, 101, 127);
                sink.channel_message(cc, 100, 127);
            }
            None => {}
        }
        for n in 0..120usize {
            if matches!(n, 0 | 32 | 6 | 38 | 98..=101) {
                continue;
            }
            if self.cc_set & (1u128 << n) != 0 {
                sink.channel_message(cc, n as u8, self.cc[n]);
            }
        }
        if let Some((l, m)) = self.bend {
            sink.channel_message(0xE0 | ch, l, m);
        }
        if let Some(p) = self.pressure {
            sink.channel_message(0xD0 | ch, p, 0);
        }
    }
}

impl ChannelState {
    /// This state without held pedals (sustain, sostenuto, soft, hold 2): what a
    /// short audition note should start from, so its release is not held.
    pub fn without_pedals(mut self) -> ChannelState {
        for n in [64u32, 66, 67, 69] {
            self.cc_set &= !(1u128 << n);
            self.cc[n as usize] = 0;
        }
        self
    }

    /// Moves a synthesizer channel that is in state `cur` to state `target`
    /// without stopping voices (loop wrap): every controller, program, RPN,
    /// bend and pressure that differs is sent — with its General MIDI power-on
    /// value where `target` has none — so the channel ends up exactly as it is
    /// at the target position. Sustain is included (a held pedal from the end of
    /// the loop is released unless it is down at the start).
    pub fn transition(cur: &ChannelState, target: &ChannelState, ch: u8, sink: &mut dyn SynthSink) {
        if cur == target {
            return;
        }
        let cc = 0xB0 | ch;
        let value = |s: &ChannelState, n: usize| (s.cc_set & (1u128 << n) != 0).then(|| s.cc[n]);
        let default = |n: usize| crate::editor::controllers::default_cc_value(n as u8);
        let bank_changed = [0usize, 32]
            .iter()
            .any(|&n| value(cur, n) != value(target, n));
        for n in [0usize, 32] {
            if value(cur, n) != value(target, n) {
                sink.channel_message(cc, n as u8, value(target, n).unwrap_or(default(n)));
            }
        }
        if cur.program != target.program || bank_changed {
            sink.channel_message(0xC0 | ch, target.program.unwrap_or(0), 0);
        }
        if cur.rpn != target.rpn || cur.selected != target.selected {
            // Power-on RPN values: bend range ±2 semitones, tuning centred.
            const DEFAULT_RPN: [(u8, Option<u8>); 3] = [(2, Some(0)), (64, Some(0)), (64, None)];
            for (i, default) in DEFAULT_RPN.iter().enumerate() {
                if cur.rpn[i] != target.rpn[i] {
                    let (msb, lsb) = target.rpn[i].unwrap_or(*default);
                    sink.channel_message(cc, 101, 0);
                    sink.channel_message(cc, 100, i as u8);
                    sink.channel_message(cc, 6, msb);
                    if let Some(l) = lsb {
                        sink.channel_message(cc, 38, l);
                    }
                }
            }
            match target.selected {
                Some((m, l, true)) => {
                    sink.channel_message(cc, 101, m);
                    sink.channel_message(cc, 100, l);
                }
                Some((m, l, false)) => {
                    sink.channel_message(cc, 99, m);
                    sink.channel_message(cc, 98, l);
                }
                None => {
                    sink.channel_message(cc, 101, 127);
                    sink.channel_message(cc, 100, 127);
                }
            }
        }
        for n in 0..120usize {
            if matches!(n, 0 | 32 | 6 | 38 | 98..=101) {
                continue;
            }
            let (a, b) = (value(cur, n), value(target, n));
            if a != b {
                sink.channel_message(cc, n as u8, b.unwrap_or(default(n)));
            }
        }
        if cur.bend != target.bend {
            let (l, m) = target.bend.unwrap_or((0, 0x40));
            sink.channel_message(0xE0 | ch, l, m);
        }
        if cur.pressure != target.pressure {
            sink.channel_message(0xD0 | ch, target.pressure.unwrap_or(0), 0);
        }
    }
}

/// Sound-relevant control events of the tracks on one output route, merged,
/// with channel-state checkpoints for fast seeking. Depends only on the
/// preserved events (and the tracks' tick corrections), so it is rebuilt only
/// when tracks are added or removed, their events change, or a correction or
/// route changes.
#[derive(Debug, Default)]
pub struct ControlIndex {
    events: Vec<ControlEvent>,
    /// B2-F12B: the song's system resets, ascending by tick.
    resets: Vec<SystemReset>,
    /// `checkpoints[k]` = state after applying `events[..k * CHECKPOINT_INTERVAL]`.
    checkpoints: Vec<[ChannelState; 16]>,
}

impl ControlIndex {
    /// Every track, as stored (no corrections).
    pub fn build(doc: &PlumDocument) -> Self {
        ControlIndex::build_filtered(doc, &|_| true, &[])
    }

    /// The tracks for which `include(index)` holds, with their tick corrections
    /// (`corrections[index]`, when present) applied to channel events.
    pub fn build_filtered(
        doc: &PlumDocument,
        include: &dyn Fn(usize) -> bool,
        corrections: &[Option<Correction>],
    ) -> Self {
        let mut events: Vec<ControlEvent> = Vec::new();
        let mut resets: Vec<SystemReset> = Vec::new();
        let identity = doc.clips_are_identity();
        for (ti, t) in doc.tracks().iter().enumerate() {
            if !include(ti) {
                continue;
            }
            let c = corrections.get(ti).and_then(Option::as_ref);
            if identity {
                for e in t.events.events() {
                    if let RawKind::SysEx { payload } = e.kind
                        && let Some(mode) = classify_reset(t.events.payload(payload))
                    {
                        resets.push(SystemReset {
                            tick: c.map_or(e.tick, |c| c.shift(e.tick).0),
                            track: ti as u32,
                            mode,
                            message: sysex_message(t.events.payload(payload)),
                        });
                    }
                    if let RawKind::Channel { status, data } = e.kind {
                        let tick = c.map_or(e.tick, |c| c.shift(e.tick).0);
                        events.push(ControlEvent {
                            tick,
                            track: ti as u32,
                            status,
                            d0: data[0],
                            d1: data[1],
                        });
                    }
                }
                continue;
            }
            // v0.9 clips: each unmuted placement plays its content's channel
            // events inside its window, moved to song time.
            for pc in doc.placed_contents(ti) {
                if pc.placement.muted {
                    continue;
                }
                let lane = Lane::of(ti, &pc);
                for e in pc.events.events() {
                    if let RawKind::SysEx { payload } = e.kind
                        && e.tick >= lane.lo
                        && e.tick < lane.hi
                        && let Some(mode) = classify_reset(pc.events.payload(payload))
                    {
                        let song = lane.to_song(e.tick);
                        resets.push(SystemReset {
                            tick: c.map_or(song, |c| c.shift(song).0),
                            track: ti as u32,
                            mode,
                            message: sysex_message(pc.events.payload(payload)),
                        });
                    }
                    if let RawKind::Channel { status, data } = e.kind
                        && e.tick >= lane.lo
                        && e.tick < lane.hi
                    {
                        let song = lane.to_song(e.tick);
                        let tick = c.map_or(song, |c| c.shift(song).0);
                        events.push(ControlEvent {
                            tick,
                            track: ti as u32,
                            status,
                            d0: data[0],
                            d1: data[1],
                        });
                    }
                }
            }
        }
        // Stable: equal ticks keep track order, then file order.
        events.sort_by_key(|e| e.tick);
        events.shrink_to_fit();
        let mut checkpoints = Vec::with_capacity(events.len() / CHECKPOINT_INTERVAL + 1);
        let mut state = [ChannelState::default(); 16];
        for (i, e) in events.iter().enumerate() {
            if i % CHECKPOINT_INTERVAL == 0 {
                checkpoints.push(state);
            }
            state[usize::from(e.status & 0x0F)].apply(e.status, e.d0, e.d1);
        }
        resets.sort_by_key(|r| r.tick);
        ControlIndex {
            events,
            resets,
            checkpoints,
        }
    }

    /// B2-F12B: the song's system resets on this route, ascending.
    pub fn resets(&self) -> &[SystemReset] {
        &self.resets
    }

    /// B2-F12B: the tick of the last reset before `tick` that `policy`
    /// sends (a `SendChanged` reset to the mode already set is not sent).
    pub fn last_sent_reset_before(&self, tick: u64, policy: ResetPolicy) -> Option<u64> {
        let end = self.resets.partition_point(|r| r.tick < tick);
        match policy {
            ResetPolicy::Skip => None,
            ResetPolicy::Send => end.checked_sub(1).map(|i| self.resets[i].tick),
            ResetPolicy::SendChanged => {
                let mut mode = None;
                let mut last = None;
                for r in &self.resets[..end] {
                    if mode != Some(r.mode) {
                        mode = Some(r.mode);
                        last = Some(r.tick);
                    }
                }
                last
            }
        }
    }

    /// B2-F12B: the system mode set before `tick` (`None`: no reset yet).
    pub fn mode_before(&self, tick: u64) -> Option<SystemMode> {
        let end = self.resets.partition_point(|r| r.tick < tick);
        end.checked_sub(1).map(|i| self.resets[i].mode)
    }

    /// Channel state before `tick` as the outputs have it under `policy`: a
    /// sent reset brings every channel to its power-on state, the control
    /// events after it apply from there (B2-F12B; `Skip` = [`Self::state_before`]).
    pub fn state_with_resets(&self, tick: u64, policy: ResetPolicy) -> [ChannelState; 16] {
        match self.last_sent_reset_before(tick, policy) {
            None => self.state_before(tick).0,
            Some(rt) => {
                let from = self.events.partition_point(|e| e.tick < rt);
                let end = self.events.partition_point(|e| e.tick < tick);
                let mut state = [ChannelState::default(); 16];
                for e in &self.events[from..end] {
                    state[usize::from(e.status & 0x0F)].apply(e.status, e.d0, e.d1);
                }
                state
            }
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn events(&self) -> &[ControlEvent] {
        &self.events
    }

    pub fn heap_bytes(&self) -> usize {
        self.resets.capacity() * std::mem::size_of::<SystemReset>()
            + self.resets.iter().map(|r| r.message.len()).sum::<usize>()
            + self.events.capacity() * std::mem::size_of::<ControlEvent>()
            + self.checkpoints.capacity() * std::mem::size_of::<[ChannelState; 16]>()
    }

    /// Channel state after every control event before `tick`, and the index of
    /// the first event at or after `tick`. Starts from the nearest checkpoint,
    /// so at most [`CHECKPOINT_INTERVAL`] events are replayed.
    pub fn state_before(&self, tick: u64) -> ([ChannelState; 16], usize) {
        let end = self.events.partition_point(|e| e.tick < tick);
        let k = end / CHECKPOINT_INTERVAL;
        let mut state = self
            .checkpoints
            .get(k)
            .copied()
            .unwrap_or([ChannelState::default(); 16]);
        for e in &self.events[(k * CHECKPOINT_INTERVAL).min(end)..end] {
            state[usize::from(e.status & 0x0F)].apply(e.status, e.d0, e.d1);
        }
        (state, end)
    }
}

/// The complete SysEx message (`F0`, the payload, `F7` when it is missing).
fn sysex_message(payload: &[u8]) -> Arc<[u8]> {
    let mut m = Vec::with_capacity(payload.len() + 2);
    m.push(0xF0);
    m.extend_from_slice(payload);
    if m.last() != Some(&0xF7) {
        m.push(0xF7);
    }
    m.into()
}

/// Which tracks play: not muted, and — while any track is soloed — soloed.
/// (Explicit Mute wins over Solo.)
///
/// v1.0-Launch L-F09 (contract §016) — the one precedence every consumer
/// follows (playback, external outputs, Progressor "Audible", keyboard
/// colors, the track list's audible marks, the MIDI Player):
///
/// 1. **Monitor set** (Play Only Selected Tracks, transient): while set,
///    exactly its tracks play — Mute and Solo are ignored, and kept.
/// 2. **Mute** wins over Solo: a muted track never plays; a track that is
///    muted and soloed is muted (its Solo has no effect).
/// 3. **Solo**: while any unmuted track is soloed, only soloed tracks play.
///
/// Flags are per track: a parent's Mute / Solo concerns its own notes, and
/// Mute / Solo Subtree set them on a whole subtree. Visibility (Onion,
/// the Piano Roll) is never changed by any of them.
pub fn eligibility(doc: &PlumDocument) -> Vec<bool> {
    if let Some(m) = &doc.project().monitor {
        return doc
            .tracks()
            .iter()
            .map(|t| m.binary_search(&t.id()).is_ok())
            .collect();
    }
    let any_solo = doc.tracks().iter().any(|t| t.solo && !t.mute);
    doc.tracks()
        .iter()
        .map(|t| !t.mute && (!any_solo || t.solo))
        .collect()
}

/// How a document is played (besides its notes and events): each track's
/// route, eligibility and correction.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSetup {
    /// Per track: output route (0 = the internal synthesizer).
    pub routes: Vec<u16>,
    /// Number of routes (at least 1).
    pub route_count: usize,
    pub eligible: Arc<[bool]>,
    pub corrections: Vec<Option<Correction>>,
}

impl PlaybackSetup {
    /// Every track on the internal synthesizer, mute/solo and corrections from
    /// the document.
    pub fn for_document(doc: &PlumDocument) -> Self {
        let meter = crate::view::grid::MeterMap::new(doc.division(), &doc.meter_events(0));
        PlaybackSetup {
            routes: vec![0; doc.track_count()],
            route_count: 1,
            eligible: eligibility(doc).into(),
            corrections: doc
                .tracks()
                .iter()
                .map(|t| Correction::resolve(t.props(), &meter, doc.project().keys))
                .collect(),
        }
    }

    /// The control index of every route.
    pub fn control_indexes(&self, doc: &PlumDocument) -> Vec<Arc<ControlIndex>> {
        (0..self.route_count.max(1))
            .map(|r| {
                Arc::new(ControlIndex::build_filtered(
                    doc,
                    &|ti| usize::from(self.routes.get(ti).copied().unwrap_or(0)) == r,
                    &self.corrections,
                ))
            })
            .collect()
    }
}

/// What one note snapshot of a [`PlaybackIndex`] plays (v0.9 clips): the
/// track it belongs to (route, Mute / Solo, correction), the content ticks
/// `[lo, hi)` whose notes start in the clip, and `shift` = song tick −
/// content tick. Identity: the track itself, everything, no shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    pub track: u32,
    pub lo: u64,
    pub hi: u64,
    pub shift: i128,
}

impl Lane {
    fn identity(track: usize) -> Lane {
        Lane {
            track: track as u32,
            lo: 0,
            hi: u64::MAX,
            shift: 0,
        }
    }

    fn of(track: usize, pc: &crate::model::PlacedContent<'_>) -> Lane {
        let (lo, hi) = pc.window();
        Lane {
            track: track as u32,
            lo,
            hi,
            shift: pc.shift(),
        }
    }

    /// Song tick of content tick `c` (`c ≥ lo`, so never negative).
    #[inline]
    pub fn to_song(&self, c: u64) -> u64 {
        (i128::from(c) + self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    /// Content tick of song tick `t` (0 below the content's start).
    #[inline]
    pub fn to_content(&self, t: u64) -> u64 {
        (i128::from(t) - self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    #[inline]
    fn is_identity(&self) -> bool {
        self.lo == 0 && self.hi == u64::MAX && self.shift == 0
    }
}

/// Everything playback reads, built from one document state. Immutable.
#[derive(Debug)]
pub struct PlaybackIndex {
    pub tempo: Arc<TempoMap>,
    /// Control events of each output route (index 0 = internal synthesizer).
    pub controls: Vec<Arc<ControlIndex>>,
    /// One snapshot per lane: per track (empty tracks included, so indices
    /// match) while the clips are the identity; otherwise one per unmuted
    /// placement, described by `lanes`.
    pub notes: Vec<NoteSnapshot>,
    /// v0.9: what each snapshot plays (empty = lane `i` is track `i`).
    pub lanes: Vec<Lane>,
    /// Per track: the correction applied while playing.
    pub corrections: Vec<Option<Correction>>,
    /// Per track: output route.
    pub routes: Vec<u16>,
    /// Per track: plays (mute/solo). The scheduler's own mask can change while
    /// playing ([`Scheduler::set_eligible`]).
    pub eligible: Arc<[bool]>,
    /// The Conductor's Master Volume changes `(tick, value)`.
    pub master: Arc<[(u64, u16)]>,
    /// Latest tick with content (corrections included, as an upper bound).
    pub end_tick: u64,
}

impl PlaybackIndex {
    /// Builds an index sharing `controls` and `tempo` (rebuild those only when
    /// tracks, events, corrections, routes or the tempo change) with fresh note
    /// snapshots (O(chunks)).
    pub fn new(
        doc: &PlumDocument,
        controls: Vec<Arc<ControlIndex>>,
        tempo: Arc<TempoMap>,
        setup: &PlaybackSetup,
    ) -> Self {
        let (notes, lanes): (Vec<NoteSnapshot>, Vec<Lane>) = if doc.clips_are_identity() {
            (
                doc.tracks().iter().map(|t| t.notes.snapshot()).collect(),
                Vec::new(),
            )
        } else {
            let mut notes = Vec::new();
            let mut lanes = Vec::new();
            for ti in 0..doc.track_count() {
                for pc in doc.placed_contents(ti) {
                    if !pc.placement.muted {
                        lanes.push(Lane::of(ti, &pc));
                        notes.push(pc.notes.snapshot());
                    }
                }
            }
            (notes, lanes)
        };
        let mut end_tick = doc.duration_ticks();
        for (l, snap) in notes.iter().enumerate() {
            let lane = lanes.get(l).copied().unwrap_or(Lane::identity(l));
            if !lane.is_identity() {
                // Upper bound: every note of the content, shifted.
                end_tick = end_tick.max(lane.to_song(snap.max_end().unwrap_or(0)));
            }
            let i = lane.track as usize;
            if let Some(Some(c)) = setup.corrections.get(i) {
                // Upper bound of the corrected end: shifted later, lengthened.
                let longer = if c.gate.hundredths() > 10_000 {
                    c.gate
                        .apply(snap.max_duration())
                        .unwrap_or(u64::MAX)
                        .saturating_sub(snap.max_duration())
                } else {
                    0
                };
                let e = lane
                    .to_song(snap.max_end().unwrap_or(0))
                    .saturating_add(c.ticks.max(0) as u64)
                    .saturating_add(longer);
                end_tick = end_tick.max(e);
            }
        }
        let master: Arc<[(u64, u16)]> = doc
            .conductor()
            .master_volume()
            .iter()
            .map(|p| (p.tick, p.value))
            .collect();
        PlaybackIndex {
            tempo,
            controls: if controls.is_empty() {
                vec![Arc::new(ControlIndex::default())]
            } else {
                controls
            },
            notes,
            lanes,
            corrections: setup.corrections.clone(),
            routes: setup.routes.clone(),
            eligible: Arc::clone(&setup.eligible),
            master,
            end_tick,
        }
    }

    /// Convenience: builds every part (internal synthesizer, normal speed).
    pub fn build(doc: &PlumDocument, sample_rate: u32) -> Self {
        let setup = PlaybackSetup::for_document(doc);
        PlaybackIndex::new(
            doc,
            setup.control_indexes(doc),
            Arc::new(TempoMap::for_document(doc, sample_rate)),
            &setup,
        )
    }

    pub fn note_count(&self) -> usize {
        self.notes.iter().map(NoteSnapshot::len).sum()
    }

    pub fn route_count(&self) -> usize {
        self.controls.len()
    }

    fn route_of(&self, track: usize) -> usize {
        usize::from(self.routes.get(track).copied().unwrap_or(0))
    }

    /// What snapshot `l` plays.
    #[inline]
    pub fn lane(&self, l: usize) -> Lane {
        self.lanes.get(l).copied().unwrap_or(Lane::identity(l))
    }

    /// Track of snapshot `l` (the identity path does no lookup).
    #[inline]
    fn lane_track(&self, l: usize) -> usize {
        if self.lanes.is_empty() {
            l
        } else {
            self.lanes[l].track as usize
        }
    }

    /// Song tick of content tick `c` of snapshot `l`.
    #[inline]
    fn lane_song(&self, l: usize, c: u64) -> u64 {
        if self.lanes.is_empty() {
            c
        } else {
            self.lanes[l].to_song(c)
        }
    }

    /// Master Volume in effect at `tick`.
    pub fn master_at(&self, tick: u64) -> u16 {
        let i = self.master.partition_point(|p| p.0 <= tick);
        i.checked_sub(1)
            .map_or(MASTER_VOLUME_MAX, |j| self.master[j].1)
    }
}

/// v1.0-beta2 (B2-F12, contract §011): opt-in event processing on the way
/// to the synthesizer and the external outputs only — the document, `.lup`,
/// export, the Player and the Inspector never see it. Off by default (the
/// delivery is then exactly what it was). A note that is not sent never
/// gets a Note Off either, so nothing sticks and nothing ends another note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventProcessing {
    /// B2-F12A: only keys 21 – 108 (a piano's 88) are sent; the drum
    /// channel (MIDI channel 10) is exempt.
    pub keys_88: bool,
    /// B2-F12C: notes whose velocity is in this inclusive band (lowest,
    /// highest; 0 – 127, lowest <= highest) are not sent.
    pub velocity_band: Option<(u8, u8)>,
    /// B2-F12D: every sent note lasts this long in playback (nanoseconds of
    /// song time, through the tempo map; above 0) instead of its own length.
    pub length_override_ns: Option<u64>,
    /// B2-F12E: every Note Off is sent this much later (nanoseconds of song
    /// time; 0 = no delay), after the length override.
    pub off_delay_ns: Option<u64>,
    /// B2-F12B: the song's GM / GS / XG system resets.
    pub resets: ResetPolicy,
}

/// B2-F12: what the options would act on in a song (its audible notes as
/// stored, before key / tick corrections; its system resets), counted in one
/// pass — the panel's event-count preflight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preflight {
    pub notes: u64,
    /// Notes outside 21 – 108 off the drum channel.
    pub outside_keys: u64,
    /// Notes whose velocity is in the band asked about.
    pub in_band: u64,
    pub resets: u64,
    /// Resets to the mode already set (what `SendChanged` ignores).
    pub repeated_resets: u64,
}

/// Why [`EventProcessing`] keeps a note from the outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filtered {
    Keys,
    Velocity,
}

impl EventProcessing {
    /// MIDI channel 10 (zero-based 9), exempt from the 88-key range.
    pub const DRUM_CHANNEL: u8 = 9;
    /// The 88 keys: A0 (21) to C8 (108).
    pub const KEYS_88: std::ops::RangeInclusive<u8> = 21..=108;

    /// Nothing is processed (the delivery is unchanged).
    pub fn is_off(&self) -> bool {
        *self == EventProcessing::default()
    }

    /// The length override or a Note Off delay changes when notes end (and
    /// with it the same-key rule, see [`Scheduler`]).
    pub fn times_notes(&self) -> bool {
        self.length_override_ns.is_some_and(|n| n > 0) || self.off_delay_ns.is_some_and(|n| n > 0)
    }

    /// Whether a Note On of `velocity` on `channel` / `key` is kept from the
    /// outputs, and why. Keys are checked before velocities.
    pub fn filters(&self, channel: u8, key: u8, velocity: u8) -> Option<Filtered> {
        if self.keys_88 && channel != Self::DRUM_CHANNEL && !Self::KEYS_88.contains(&key) {
            return Some(Filtered::Keys);
        }
        match self.velocity_band {
            Some((lo, hi)) if (lo..=hi).contains(&velocity) => Some(Filtered::Velocity),
            _ => None,
        }
    }

    /// B2-F12: the preflight of `doc` for a velocity `band` (any band, also
    /// when the option is off — the panel shows what it would do).
    pub fn preflight(doc: &PlumDocument, band: (u8, u8)) -> Preflight {
        let mut p = Preflight::default();
        let mut resets: Vec<(u64, SystemMode)> = Vec::new();
        for ti in 0..doc.tracks().len() {
            for pc in doc.placed_contents(ti) {
                if pc.placement.muted {
                    continue;
                }
                let (lo, hi) = pc.window();
                for n in pc.notes.iter_audible() {
                    if n.start_tick < lo || n.start_tick >= hi {
                        continue;
                    }
                    p.notes += 1;
                    let (ch, key, vel) = (n.attrs.channel(), n.attrs.pitch(), n.attrs.velocity());
                    if ch != Self::DRUM_CHANNEL && !Self::KEYS_88.contains(&key) {
                        p.outside_keys += 1;
                    }
                    if (band.0..=band.1).contains(&vel) {
                        p.in_band += 1;
                    }
                }
                for e in pc.events.events() {
                    if let RawKind::SysEx { payload } = e.kind
                        && e.tick >= lo
                        && e.tick < hi
                        && let Some(mode) = classify_reset(pc.events.payload(payload))
                    {
                        resets.push((e.tick, mode));
                    }
                }
            }
        }
        resets.sort_by_key(|r| r.0);
        p.resets = resets.len() as u64;
        p.repeated_resets = resets.windows(2).filter(|w| w[0].1 == w[1].1).count() as u64;
        p
    }

    /// The sentence refusing an invalid velocity band (above 127 or lowest
    /// above highest), or `None` when it is valid.
    pub fn band_problem(lo: u8, hi: u8) -> Option<String> {
        if lo > 127 || hi > 127 {
            Some("Velocities go from 0 to 127.".into())
        } else if lo > hi {
            Some(format!(
                "The lowest velocity ({lo}) must not be above the highest ({hi})."
            ))
        } else {
            None
        }
    }
}

/// Resource bounds of the scheduler (see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerLimits {
    /// Note-ons sent per block at most (the rest of a burst is deferred).
    pub max_note_ons_per_block: usize,
    /// Sounding notes tracked at most.
    pub max_pending_offs: usize,
    /// A deferred note-on later than this (frames) is skipped instead (0: no
    /// deferral — the v0.7 behaviour of skipping beyond the budget).
    pub max_lateness_frames: u64,
    /// v1.0-beta2 (B2-F12): playback-only event processing (live with
    /// [`Scheduler::set_limits`]).
    pub processing: EventProcessing,
}

/// Default note-on budget per block (v0.8, measured: a 2,048-note block costs
/// about 7 ms of synthesis on the 2-core test VM, absorbed by the ring).
pub const DEFAULT_NOTE_ONS_PER_BLOCK: usize = 2_048;
/// Default sounding-note bound (each entry 24 bytes: at most 24 MB).
pub const DEFAULT_PENDING_OFFS: usize = 1 << 20;
/// Default lateness bound for deferred note-ons: 50 ms at 48 kHz.
pub const DEFAULT_MAX_LATENESS_FRAMES: u64 = 2_400;

impl Default for SchedulerLimits {
    fn default() -> Self {
        SchedulerLimits {
            max_note_ons_per_block: DEFAULT_NOTE_ONS_PER_BLOCK,
            max_pending_offs: DEFAULT_PENDING_OFFS,
            max_lateness_frames: DEFAULT_MAX_LATENESS_FRAMES,
            processing: EventProcessing {
                keys_88: false,
                velocity_band: None,
                length_override_ns: None,
                off_delay_ns: None,
                resets: ResetPolicy::Skip,
            },
        }
    }
}

impl SchedulerLimits {
    /// The v0.7 limits (256 per block, 65,536 sounding, no deferral) —
    /// regression tests and comparisons.
    pub const V07: SchedulerLimits = SchedulerLimits {
        max_note_ons_per_block: 256,
        max_pending_offs: 65_536,
        max_lateness_frames: 0,
        processing: EventProcessing {
            keys_88: false,
            velocity_band: None,
            length_override_ns: None,
            off_delay_ns: None,
            resets: ResetPolicy::Skip,
        },
    };
}

/// Counters for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    pub note_ons: u64,
    /// Notes not started because of the limits (the project is unchanged);
    /// includes `late_dropped`.
    pub note_ons_skipped: u64,
    /// Note-ons sent after their own block because a burst exceeded the
    /// per-block budget (they started late but were not lost).
    pub note_ons_deferred: u64,
    /// Note-ons skipped because they would have started more than
    /// `max_lateness_frames` late.
    pub late_dropped: u64,
    /// Latest deferred note-on, in frames behind its time.
    pub max_lateness: u64,
    pub note_offs: u64,
    pub control_events: u64,
    pub restruck: u64,
    pub peak_pending_offs: usize,
    /// Notes not played because a key correction would move them outside 0–127
    /// (reported to the user; the project is unchanged).
    pub correction_skipped: u64,
    /// v0.9: notes not played because their key (as stored in a 256-Key Mode
    /// project, or after a key correction there) is above 127 — MIDI keys
    /// stop at 127, so no synthesizer or output ever receives one.
    pub above_midi: u64,
    /// Times the loop wrapped.
    pub loop_wraps: u64,
    /// Notes of muted tracks passed over.
    pub muted: u64,
    /// v1.0-beta2 (B2-F12A): notes not sent because they are outside the 88
    /// keys (drum channel exempt).
    pub filtered_keys: u64,
    /// v1.0-beta2 (B2-F12C): notes not sent because their velocity is in the
    /// ignored band.
    pub filtered_velocity: u64,
    /// v1.0-beta2 (B2-F12D / E): notes ended early because a newer note
    /// struck their key while the length override or the delay held them.
    pub retriggered: u64,
    /// v1.0-beta2 (B2-F12B): system resets sent, and ignored because they
    /// repeated the mode already set.
    pub resets_sent: u64,
    pub resets_ignored: u64,
}

/// A loop wrap inside an [`Scheduler::advance_multi`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wrap {
    /// Frames of the block rendered before the (last) wrap.
    pub offset: u32,
    /// Playback frame right after the wrap (the loop start).
    pub frame: u64,
}

/// One sorted run of a track's notes.
#[derive(Clone, Copy, Debug)]
struct Stream {
    /// The lane (snapshot) it walks; its track is `index.lane(lane).track`.
    lane: u32,
    pos: SnapshotPos,
    /// Run end (exclusive): the uncorrected part of a corrected track, or a
    /// clip's window end.
    end: Option<SnapshotPos>,
    /// Starts are corrected (the part at or after the correction start).
    corrected: bool,
}

/// (end tick, sequence, channel, key, track, route) of a sounding note.
type Off = (u64, u64, u8, u8, u32, u16);

/// Walks a [`PlaybackIndex`] in audio time and emits MIDI messages.
pub struct Scheduler {
    index: Arc<PlaybackIndex>,
    limits: SchedulerLimits,
    /// Next frame to render.
    frame: u64,
    /// Every event with a tick below this has been emitted.
    tick: u64,
    streams: Vec<Stream>,
    /// (next corrected start tick, stream) for streams with notes left.
    starts: BinaryHeap<Reverse<(u64, u32)>>,
    offs: BinaryHeap<Reverse<Off>>,
    seq: u64,
    control_pos: Vec<usize>,
    /// B2-F12B: per route, the next system reset and the mode set so far.
    reset_pos: Vec<usize>,
    modes: Vec<Option<SystemMode>>,
    master_pos: usize,
    eligible: Arc<[bool]>,
    loop_range: Option<(u64, u64)>,
    stats: SchedulerStats,
    /// B2-F12: unpaired raw Note Ons kept from an output, per (route,
    /// channel, key) — their Note Offs are dropped too (first in, first out).
    raw_filtered: std::collections::HashMap<(u16, u8, u8), u32>,
    /// B2-F12D / E: the sequence of the note sounding on each (route,
    /// channel, key) while the length override or the delay is on, and the
    /// sequences whose Note Off was already sent when a newer note re-struck
    /// their key (their scheduled Note Off is skipped).
    held_key: std::collections::HashMap<(u16, u8, u8), u64>,
    cancelled: HashSet<u64>,
    /// L-B02: inside [`Scheduler::advance_multi`], (frames of the block
    /// already scheduled before the current segment, playback frame where
    /// the segment starts, frames in the block) — how timed sinks learn
    /// each message's offset in the block.
    block_origin: Option<(u64, u64, u64)>,
}

impl Scheduler {
    pub fn new(index: Arc<PlaybackIndex>, limits: SchedulerLimits) -> Self {
        let eligible = Arc::clone(&index.eligible);
        let mut s = Scheduler {
            index,
            limits,
            frame: 0,
            tick: 0,
            streams: Vec::new(),
            starts: BinaryHeap::new(),
            offs: BinaryHeap::new(),
            seq: 0,
            control_pos: Vec::new(),
            reset_pos: Vec::new(),
            modes: Vec::new(),
            master_pos: 0,
            eligible,
            loop_range: None,
            stats: SchedulerStats::default(),
            raw_filtered: std::collections::HashMap::new(),
            held_key: std::collections::HashMap::new(),
            cancelled: HashSet::new(),
            block_origin: None,
        };
        s.reset_route_cursors();
        s.build_streams();
        s
    }

    /// v1.0-Launch L-A01: every per-route cursor (control events, system
    /// resets, the mode the sent resets left) has exactly one entry per route
    /// of the current index. Called whenever the index is (re)placed, before
    /// anything indexes them — the route count changes when an output opens
    /// or closes while playing (Internal ↔ KDMAPI). [`Self::position`] then
    /// sets each cursor at the playback tick; a new route starts with no mode
    /// (nothing was sent to it yet). Route-keyed maps (`raw_filtered`,
    /// `held_key`, `cancelled`) and the sounding notes are cleared by every
    /// seek, which follows each replacement.
    fn reset_route_cursors(&mut self) {
        let routes = self.index.route_count();
        self.control_pos = vec![0; routes];
        self.reset_pos = vec![0; routes];
        self.modes = vec![None; routes];
    }

    /// The per-route cursors match the index (debug invariant).
    fn route_cursors_match(&self) -> bool {
        let n = self.index.route_count();
        self.control_pos.len() == n && self.reset_pos.len() == n && self.modes.len() == n
    }

    fn build_streams(&mut self) {
        self.streams.clear();
        for (l, snap) in self.index.notes.iter().enumerate() {
            let lane = self.index.lane(l);
            let i = lane.track as usize;
            // A clip plays the notes starting in its window.
            let (first, last) = if lane.is_identity() {
                (SnapshotPos::default(), None)
            } else {
                (
                    snap.lower_bound(lane.lo),
                    (lane.hi != u64::MAX).then(|| snap.lower_bound(lane.hi)),
                )
            };
            let earlier = |a: SnapshotPos, b: Option<SnapshotPos>| match b {
                Some(b) if pos_after(a, b) => b,
                _ => a,
            };
            match self.index.corrections.get(i).and_then(Option::as_ref) {
                Some(c) if !c.filters_only() => {
                    // The correction starts at song tick `from_tick`.
                    let b = if lane.is_identity() {
                        snap.lower_bound(c.from_tick)
                    } else {
                        let b = snap.lower_bound(lane.to_content(c.from_tick).max(lane.lo));
                        earlier(if pos_after(first, b) { first } else { b }, last)
                    };
                    self.streams.push(Stream {
                        lane: l as u32,
                        pos: first,
                        end: Some(b),
                        corrected: false,
                    });
                    self.streams.push(Stream {
                        lane: l as u32,
                        pos: b,
                        end: last,
                        corrected: true,
                    });
                }
                _ => self.streams.push(Stream {
                    lane: l as u32,
                    pos: first,
                    end: last,
                    corrected: false,
                }),
            }
        }
    }

    pub fn index(&self) -> &Arc<PlaybackIndex> {
        &self.index
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats
    }

    /// New limits while playing (v0.8 Audio Settings); applied from the next
    /// block.
    pub fn set_limits(&mut self, limits: SchedulerLimits) {
        self.limits = limits;
    }

    /// B2-F12D / E: the tick a note struck at `start` (its own end `end`)
    /// ends at in playback — the length override from its start, then the
    /// delay, through the tempo map; its own end without them.
    fn playback_end(&self, start: u64, end: Option<u64>) -> u64 {
        let p = &self.limits.processing;
        if !p.times_notes() {
            return end.unwrap_or(u64::MAX);
        }
        let tempo = &self.index.tempo;
        let frames = |ns: u64| -> u64 {
            let f =
                (u128::from(ns) * u128::from(tempo.sample_rate()) + 500_000_000) / 1_000_000_000;
            u64::try_from(f).unwrap_or(u64::MAX)
        };
        let base = match p.length_override_ns.filter(|&n| n > 0) {
            Some(len) => tempo.frame_at_tick(start).saturating_add(frames(len)),
            None => match end {
                Some(e) => tempo.frame_at_tick(e),
                None => return u64::MAX,
            },
        };
        let f = base.saturating_add(p.off_delay_ns.map_or(0, frames));
        tempo.first_tick_at_or_after_frame(f).max(start)
    }

    /// B2-F12D / E: before a note is struck on `key` while the override or
    /// the delay is on, the note still held there ends now (its scheduled
    /// Note Off is skipped later) — a key carries one note at a time, so an
    /// extended note never cuts a newer one short. `seq` is the new note's.
    fn retrigger(&mut self, key: (u16, u8, u8), seq: u64, sink: Option<&mut &mut dyn SynthSink>) {
        if !self.limits.processing.times_notes() {
            return;
        }
        if let Some(prev) = self.held_key.insert(key, seq) {
            if let Some(s) = sink {
                s.note_off(key.1, key.2);
            }
            self.cancelled.insert(prev);
            self.stats.note_offs += 1;
            self.stats.retriggered += 1;
        }
    }

    /// A pending Note Off popped: `false` when it was already sent by
    /// [`Self::retrigger`] (skip it).
    fn off_due(&mut self, o: &Off) -> bool {
        if self.cancelled.remove(&o.1) {
            return false;
        }
        let key = (o.5, o.2, o.3);
        if self.held_key.get(&key) == Some(&o.1) {
            self.held_key.remove(&key);
        }
        true
    }

    fn count_filtered(&mut self, f: Filtered) {
        match f {
            Filtered::Keys => self.stats.filtered_keys += 1,
            Filtered::Velocity => self.stats.filtered_velocity += 1,
        }
    }

    /// Frame of the next block.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Tick up to which events have been emitted.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Sounding notes being tracked.
    pub fn pending_offs(&self) -> usize {
        self.offs.len()
    }

    /// True once every event of the song has been emitted and every note ended
    /// (never while a loop keeps playing).
    pub fn finished(&self) -> bool {
        !self.loop_engaged() && self.tick > self.index.end_tick && self.offs.is_empty()
    }

    /// The loop will wrap: playback is before the loop's end (by frame, so a
    /// block ending just before the end frame does not disengage it).
    fn loop_engaged(&self) -> bool {
        self.loop_range.is_some_and(|(s, e)| {
            let t = &self.index.tempo;
            let end = t.frame_at_tick(e);
            self.frame < end && end > t.frame_at_tick(s)
        })
    }

    /// Sets (or clears) the loop range `[start, end)` in ticks. A loop only
    /// engages while playback is before its end (starting after it plays on to
    /// the end of the song).
    pub fn set_loop(&mut self, range: Option<(u64, u64)>) {
        self.loop_range = range.filter(|(a, b)| b > a);
    }

    pub fn loop_range(&self) -> Option<(u64, u64)> {
        self.loop_range
    }

    /// Replaces the index (after an edit) and continues from `tick`.
    pub fn set_index(&mut self, index: Arc<PlaybackIndex>, tick: u64, sink: &mut dyn SynthSink) {
        self.set_index_multi(index, tick, &mut [sink]);
    }

    pub fn set_index_multi(
        &mut self,
        index: Arc<PlaybackIndex>,
        tick: u64,
        sinks: &mut [&mut dyn SynthSink],
    ) {
        self.eligible = Arc::clone(&index.eligible);
        self.index = index;
        // L-A01: all route-indexed state follows the new route count (only
        // `control_pos` was resized before, so `position` panicked on
        // `reset_pos[1]` when an external output opened while playing).
        self.reset_route_cursors();
        self.build_streams();
        self.seek_multi(tick, sinks);
    }

    /// v1.0-Launch L-B06: route `route`, which heard nothing so far (an
    /// external output that finished opening after Play, or the internal
    /// synthesizer once its SoundFont is loaded), joins at the playback
    /// position without disturbing the other routes: its sink is reset, its
    /// channels are brought to their state at the position (resets
    /// included), the master volume is set, and the notes of its tracks held
    /// there are struck (the seek re-strike rule, within the synthesizer's
    /// budget on route 0). The sounding notes the scheduler tracked for it
    /// were never heard: they are forgotten, and the re-struck ones end at
    /// their own time. Nothing before the position is sent; the stream and
    /// control cursors are unchanged.
    pub fn join_route(&mut self, route: usize, sinks: &mut [&mut dyn SynthSink]) {
        let idx = Arc::clone(&self.index);
        if route >= sinks.len() || route >= idx.route_count() {
            return;
        }
        let tick = self.tick;
        {
            let sink = &mut *sinks[route];
            sink.reset();
            let state = idx.controls[route].state_with_resets(tick, self.limits.processing.resets);
            for (ch, s) in state.iter().enumerate() {
                s.restore(ch as u8, sink);
            }
            if !idx.master.is_empty() {
                sink.master_volume(idx.master_at(tick));
            }
        }
        let r = route as u16;
        let mut kept = BinaryHeap::with_capacity(self.offs.len());
        for Reverse(o) in self.offs.drain() {
            if o.5 == r {
                self.cancelled.remove(&o.1);
            } else {
                kept.push(Reverse(o));
            }
        }
        self.offs = kept;
        self.held_key.retain(|k, _| k.0 != r);
        self.raw_filtered.retain(|k, _| k.0 != r);
        if tick > 0 {
            let mut budget = self.limits.max_note_ons_per_block;
            for l in 0..idx.notes.len() {
                let t = idx.lane(l).track as usize;
                if idx.route_of(t) == route && self.eligible.get(t).copied().unwrap_or(true) {
                    self.restrike(l, tick, &mut budget, sinks);
                }
            }
        }
        self.stats.peak_pending_offs = self.stats.peak_pending_offs.max(self.offs.len());
    }

    /// Positions playback at `tick`: resets the synthesizer, restores channel
    /// state from the control events before `tick`, and re-strikes notes that
    /// are sounding at `tick`.
    pub fn seek(&mut self, tick: u64, sink: &mut dyn SynthSink) {
        self.seek_multi(tick, &mut [sink]);
    }

    pub fn seek_multi(&mut self, tick: u64, sinks: &mut [&mut dyn SynthSink]) {
        self.seek_multi_with(tick, sinks, true);
    }

    /// [`Self::seek_multi`]; `restrike_held = false` leaves the notes sounding
    /// at `tick` (started before it) silent — a resume after a pause (see the
    /// module docs). Channel state is restored either way.
    pub fn seek_multi_with(
        &mut self,
        tick: u64,
        sinks: &mut [&mut dyn SynthSink],
        restrike_held: bool,
    ) {
        let idx = Arc::clone(&self.index);
        for (r, sink) in sinks.iter_mut().enumerate() {
            sink.reset();
            if let Some(c) = idx.controls.get(r) {
                let state = c.state_with_resets(tick, self.limits.processing.resets);
                for (ch, s) in state.iter().enumerate() {
                    s.restore(ch as u8, &mut **sink);
                }
            }
            if !idx.master.is_empty() {
                sink.master_volume(idx.master_at(tick));
            }
        }
        self.offs.clear();
        self.raw_filtered.clear();
        self.held_key.clear();
        self.cancelled.clear();
        self.position(tick, sinks, restrike_held);
    }

    /// Cursors, control positions and re-struck notes at `tick` (after the
    /// channels were brought to their state there; `restrike`: the notes held
    /// at `tick` sound again).
    fn position(&mut self, tick: u64, sinks: &mut [&mut dyn SynthSink], restrike: bool) {
        let idx = Arc::clone(&self.index);
        debug_assert!(self.route_cursors_match(), "route cursors out of date");
        for (r, c) in idx.controls.iter().enumerate() {
            self.control_pos[r] = c.events.partition_point(|e| e.tick < tick);
            self.reset_pos[r] = c.resets.partition_point(|x| x.tick < tick);
            self.modes[r] = c.mode_before(tick);
        }
        self.master_pos = idx.master.partition_point(|p| p.0 < tick);
        self.frame = idx.tempo.frame_at_tick(tick);
        self.tick = tick;
        self.starts.clear();
        let mut budget = self.limits.max_note_ons_per_block;
        if tick > 0 && restrike {
            for l in 0..idx.notes.len() {
                let t = idx.lane(l).track as usize;
                if self.eligible.get(t).copied().unwrap_or(true) {
                    self.restrike(l, tick, &mut budget, sinks);
                }
            }
        }
        for si in 0..self.streams.len() {
            let st = self.streams[si];
            let snap = &idx.notes[st.lane as usize];
            let lane = idx.lane(st.lane as usize);
            let c = idx
                .corrections
                .get(lane.track as usize)
                .and_then(Option::as_ref);
            // Content tick of a song tick, never before the clip's window.
            let content = |song: u64| {
                if lane.is_identity() {
                    song
                } else {
                    lane.to_content(song).max(lane.lo)
                }
            };
            let p = match (st.corrected, c) {
                // Corrected start ≥ tick ⇔ source start ≥ tick − shift.
                (true, Some(c)) => {
                    let src = (i128::from(tick) - i128::from(c.ticks)).max(0) as u64;
                    let lb = snap.lower_bound(content(src.max(c.from_tick)));
                    if c.ticks < 0 && tick == 0 {
                        // Everything clamped to 0 plays from the start.
                        snap.lower_bound(content(c.from_tick))
                    } else {
                        lb
                    }
                }
                _ => snap.lower_bound(content(tick)),
            };
            let p = match st.end {
                Some(e) if pos_after(p, e) => e,
                _ => p,
            };
            self.streams[si].pos = p;
            self.push_start(si);
        }
        self.stats.peak_pending_offs = self.stats.peak_pending_offs.max(self.offs.len());
    }

    /// Queues the next note of stream `si`.
    fn push_start(&mut self, si: usize) {
        let st = self.streams[si];
        if st.end.is_some_and(|e| pos_after(st.pos, e)) {
            return;
        }
        let l = st.lane as usize;
        let snap = &self.index.notes[l];
        if let Some(s) = snap.start_at(st.pos) {
            let s = self.index.lane_song(l, s);
            let t = if st.corrected {
                self.index.corrections[self.index.lane_track(l)]
                    .as_ref()
                    .map_or(s, |c| c.shift(s).0)
            } else {
                s
            };
            self.starts.push(Reverse((t, si as u32)));
        }
    }

    /// The note as it plays on track `t` (correction applied), or why not.
    fn corrected(&self, t: usize, rec: NoteRecord) -> Corrected {
        // Muted notes (v0.7) never sound.
        if rec.attrs.muted() {
            return Corrected::OutsideRange;
        }
        match self.index.corrections.get(t).and_then(Option::as_ref) {
            Some(c) => c.note(rec),
            None => Corrected::Note {
                note: rec,
                clamped: false,
            },
        }
    }

    /// Re-strikes lane `l`'s notes that sound at `tick` (started before it).
    fn restrike(
        &mut self,
        l: usize,
        tick: u64,
        budget: &mut usize,
        sinks: &mut [&mut dyn SynthSink],
    ) {
        let idx = Arc::clone(&self.index);
        let snap = &idx.notes[l];
        if snap.is_empty() {
            return;
        }
        let lane = idx.lane(l);
        let t = lane.track as usize;
        let route = idx.route_of(t);
        let c = idx.corrections.get(t).and_then(Option::as_ref).copied();
        // Source window that can hold notes sounding at `tick` once corrected.
        let (q0, q1) = match c {
            None => (tick, tick),
            Some(c) => {
                let back = (i128::from(tick) - i128::from(c.ticks.max(0))).max(0) as u64;
                let fwd = (i128::from(tick) - i128::from(c.ticks.min(0))).max(0) as u64;
                let extra = if c.gate.hundredths() > 10_000 {
                    c.gate
                        .apply(snap.max_duration())
                        .unwrap_or(u64::MAX)
                        .saturating_sub(snap.max_duration())
                } else {
                    0
                };
                (back.min(tick).saturating_sub(extra), fwd.max(tick))
            }
        };
        // B2-F12D / E: a note held past its own end by the override or the
        // delay may have started (or ended) up to their time before `tick`.
        let q0 = if self.limits.processing.times_notes() {
            let p = &self.limits.processing;
            let span = p
                .length_override_ns
                .unwrap_or(0)
                .saturating_add(p.off_delay_ns.unwrap_or(0));
            let ns =
                (u128::from(span) * u128::from(idx.tempo.sample_rate())).div_ceil(1_000_000_000);
            let back = idx
                .tempo
                .frame_at_tick(tick)
                .saturating_sub(u64::try_from(ns).unwrap_or(u64::MAX));
            q0.min(idx.tempo.tick_at_frame(back))
        } else {
            q0
        };
        let mut found: Vec<NoteRecord> = Vec::new();
        if lane.is_identity() {
            snap.query(q0, q1, |_, v| found.push(v.record()));
        } else {
            // The same song window in content ticks; only notes starting in
            // the clip's window, moved to song time.
            snap.query(lane.to_content(q0), lane.to_content(q1), |_, v| {
                if v.start_tick >= lane.lo && v.start_tick < lane.hi {
                    let mut r = v.record();
                    r.start_tick = lane.to_song(r.start_tick);
                    found.push(r);
                }
            });
        }
        let mut sounding: Vec<(NoteRecord, u64)> = Vec::with_capacity(found.len());
        for rec in found {
            let Corrected::Note { note, .. } = self.corrected(t, rec) else {
                continue;
            };
            // Sounding at `tick` as playback has it (B2-F12D / E: the
            // override or delay may hold a note longer or end it sooner).
            let end = self.playback_end(note.start_tick, note.end_tick());
            if note.start_tick < tick && end > tick && note.attrs.pitch() <= MAX_PITCH {
                sounding.push((note, end));
            }
        }
        // B2-F12D / E: one note per key, the latest struck (what playing
        // through to `tick` leaves sounding).
        if self.limits.processing.times_notes() {
            sounding.sort_by_key(|(n, _)| n.start_tick);
            let mut seen: HashSet<(u8, u8)> = HashSet::new();
            let mut keep: Vec<(NoteRecord, u64)> = sounding
                .into_iter()
                .rev()
                .filter(|(n, _)| seen.insert((n.attrs.channel(), n.attrs.pitch())))
                .collect();
            keep.reverse();
            sounding = keep;
        }
        for (note, end) in sounding {
            // B2-F12: a note kept from the outputs is not re-struck either.
            let a = note.attrs;
            if let Some(f) = self
                .limits
                .processing
                .filters(a.channel(), a.pitch(), a.velocity())
            {
                self.count_filtered(f);
                continue;
            }
            // L-B04: the synthesizer's budget, not an external output's.
            if (route != 0 || *budget > 0) && self.offs.len() < self.limits.max_pending_offs {
                if route == 0 {
                    *budget -= 1;
                }
                let a = note.attrs;
                self.seq += 1;
                let seq = self.seq;
                self.retrigger(
                    (route as u16, a.channel(), a.pitch()),
                    seq,
                    sinks.get_mut(route),
                );
                if let Some(s) = sinks.get_mut(route) {
                    s.note_on(a.channel(), a.pitch(), a.velocity());
                }
                self.offs.push(Reverse((
                    end,
                    seq,
                    a.channel(),
                    a.pitch(),
                    t as u32,
                    route as u16,
                )));
                self.stats.restruck += 1;
            } else {
                self.stats.note_ons_skipped += 1;
            }
        }
    }

    /// Changes which tracks play while playing: newly muted tracks' sounding
    /// notes end (unless another audible track holds the same channel and key on
    /// the same route), newly audible tracks' sounding notes are re-struck.
    pub fn set_eligible(&mut self, eligible: Arc<[bool]>, sink: &mut dyn SynthSink) {
        self.set_eligible_multi(eligible, &mut [sink]);
    }

    pub fn set_eligible_multi(&mut self, eligible: Arc<[bool]>, sinks: &mut [&mut dyn SynthSink]) {
        let old = std::mem::replace(&mut self.eligible, eligible);
        let now = |t: u32| self.eligible.get(t as usize).copied().unwrap_or(true);
        let mut keep: Vec<Reverse<Off>> = Vec::with_capacity(self.offs.len());
        let mut ended: Vec<Off> = Vec::new();
        for Reverse(o) in std::mem::take(&mut self.offs) {
            if now(o.4) {
                keep.push(Reverse(o));
            } else {
                ended.push(o);
            }
        }
        // B2-F12D / E: an ended note whose Note Off a re-strike already sent.
        ended.retain(|o| self.off_due(o));
        let held: HashSet<(u16, u8, u8)> = keep.iter().map(|Reverse(o)| (o.5, o.2, o.3)).collect();
        let mut sent: HashSet<(u16, u8, u8)> = HashSet::new();
        for o in ended {
            let key = (o.5, o.2, o.3);
            if !held.contains(&key) && sent.insert(key) {
                if let Some(s) = sinks.get_mut(usize::from(o.5)) {
                    s.note_off(o.2, o.3);
                }
                self.stats.note_offs += 1;
            }
        }
        self.offs = keep.into_iter().collect();
        let mut budget = self.limits.max_note_ons_per_block;
        let tick = self.tick;
        for l in 0..self.index.notes.len() {
            let t = self.index.lane(l).track as usize;
            let was = old.get(t).copied().unwrap_or(true);
            let is = self.eligible.get(t).copied().unwrap_or(true);
            if is && !was && tick > 0 {
                self.restrike(l, tick, &mut budget, sinks);
            }
        }
    }

    pub fn eligible(&self) -> &Arc<[bool]> {
        &self.eligible
    }

    /// Emits every event that sounds in the next `frames` frames and advances.
    /// Call right before rendering those frames.
    pub fn advance(&mut self, frames: u32, sink: &mut dyn SynthSink) -> Option<Wrap> {
        self.advance_multi(frames, &mut [sink])
    }

    /// [`Scheduler::advance`] for several output routes (`sinks[r]` receives
    /// route `r`'s messages). Handles the loop: returns where in the block the
    /// (last) wrap happened.
    pub fn advance_multi(&mut self, frames: u32, sinks: &mut [&mut dyn SynthSink]) -> Option<Wrap> {
        let mut remaining = u64::from(frames);
        let mut wrap = None;
        let mut done = 0u64;
        let timed = sinks.iter().any(|s| s.timed());
        let block = u64::from(frames);
        // A loop shorter than a block can wrap several times; bounded.
        for _ in 0..64 {
            let next_frame = self.frame.saturating_add(remaining);
            if timed {
                self.block_origin = Some((done, self.frame, block));
            }
            if let Some((ls, le)) = self.loop_range
                && self.loop_engaged()
            {
                let end_frame = self.index.tempo.frame_at_tick(le);
                if end_frame <= next_frame {
                    let part = end_frame.saturating_sub(self.frame);
                    // Everything before the loop end is sent (a deferred
                    // burst would otherwise be lost at the wrap).
                    self.emit_until_budget(le, usize::MAX, sinks);
                    // L-B02: the wrap's note-offs, transitions and re-strikes
                    // happen at the loop end's offset in the block.
                    if timed {
                        let at = (done + part).min(block.saturating_sub(1)) as u32;
                        for s in sinks.iter_mut() {
                            s.at_offset(at);
                        }
                    }
                    self.wrap_to(ls, sinks);
                    done += part;
                    remaining -= part.min(remaining);
                    wrap = Some(Wrap {
                        offset: done.min(u64::from(frames)) as u32,
                        frame: self.frame,
                    });
                    if remaining == 0 {
                        self.block_origin = None;
                        return wrap;
                    }
                    continue;
                }
            }
            let end_tick = self.index.tempo.first_tick_at_or_after_frame(next_frame);
            self.emit_until(end_tick, sinks);
            self.frame = next_frame;
            self.block_origin = None;
            return wrap;
        }
        self.block_origin = None;
        wrap
    }

    /// L-B02: tells every timed sink that the next messages are due at tick
    /// `t` (as an offset in the block being scheduled). Messages of earlier
    /// blocks (deferred) are due at the segment's start.
    fn time_sinks(&self, t: u64, sinks: &mut [&mut dyn SynthSink]) {
        let Some((done, seg, block)) = self.block_origin else {
            return;
        };
        let f = self.index.tempo.frame_at_tick(t);
        let at = (done + f.saturating_sub(seg)).min(block.saturating_sub(1)) as u32;
        for s in sinks.iter_mut() {
            if s.timed() {
                s.at_offset(at);
            }
        }
    }

    /// Emits every event with a tick below `end_tick` — or, when a burst uses
    /// up the note-on budget, every event before the first deferred note-on
    /// (the rest follows in the next blocks, in order).
    fn emit_until(&mut self, end_tick: u64, sinks: &mut [&mut dyn SynthSink]) {
        self.emit_until_budget(end_tick, self.limits.max_note_ons_per_block, sinks);
    }

    /// [`Self::emit_until`] with a note-on budget (`usize::MAX` before a loop
    /// wrap: nothing may be left behind there).
    fn emit_until_budget(
        &mut self,
        end_tick: u64,
        budget: usize,
        sinks: &mut [&mut dyn SynthSink],
    ) {
        let idx = Arc::clone(&self.index);
        let mut note_budget = budget;
        // Events before this tick belong to an earlier block (deferred).
        let block_start = self.tick;
        // The block's end frame, looked up only when a note is deferred or
        // over budget (v0.8 §256: one tempo lookup per block cost ~10 % of
        // whole-song scheduling).
        let mut end_frame: Option<u64> = None;
        loop {
            // Earliest pending tick among the streams.
            let off_t = self.offs.peek().map(|Reverse(o)| o.0);
            let mut ctl: Option<(u64, usize)> = None;
            for (r, c) in idx.controls.iter().enumerate() {
                if let Some(e) = c.events.get(self.control_pos[r])
                    && ctl.is_none_or(|(t, _)| e.tick < t)
                {
                    ctl = Some((e.tick, r));
                }
            }
            let ctl_t = ctl.map(|c| c.0);
            // B2-F12B: the next system reset (only when they are sent).
            let mut rst: Option<(u64, usize)> = None;
            if self.limits.processing.resets != ResetPolicy::Skip {
                for (r, c) in idx.controls.iter().enumerate() {
                    if let Some(x) = c.resets.get(self.reset_pos[r])
                        && rst.is_none_or(|(t, _)| x.tick < t)
                    {
                        rst = Some((x.tick, r));
                    }
                }
            }
            let rst_t = rst.map(|r| r.0);
            let master_t = idx.master.get(self.master_pos).map(|p| p.0);
            let on_t = self.starts.peek().map(|Reverse((t, _))| *t);
            let Some(t) = [off_t, ctl_t, master_t, rst_t, on_t]
                .into_iter()
                .flatten()
                .min()
            else {
                break;
            };
            if t >= end_tick {
                break;
            }
            if self.block_origin.is_some() {
                self.time_sinks(t, sinks);
            }
            if off_t == Some(t) {
                if let Some(Reverse(o)) = self.offs.pop()
                    && self.off_due(&o)
                {
                    if let Some(s) = sinks.get_mut(usize::from(o.5)) {
                        s.note_off(o.2, o.3);
                    }
                    self.stats.note_offs += 1;
                }
                continue;
            }
            if master_t == Some(t) {
                let v = idx.master[self.master_pos].1;
                self.master_pos += 1;
                for s in sinks.iter_mut() {
                    s.master_volume(v);
                }
                continue;
            }
            // B2-F12B: a reset goes before the channel events of its tick.
            if let Some((rt, r)) = rst
                && rt == t
            {
                // Like controllers, a reset reaches its route whether its
                // track is muted or not (Mute / Solo silence notes only).
                let x = &idx.controls[r].resets[self.reset_pos[r]];
                self.reset_pos[r] += 1;
                let repeat = self.limits.processing.resets == ResetPolicy::SendChanged
                    && self.modes[r] == Some(x.mode);
                if repeat {
                    self.stats.resets_ignored += 1;
                } else {
                    if let Some(s) = sinks.get_mut(r) {
                        s.system_reset(&x.message);
                    }
                    self.stats.resets_sent += 1;
                }
                self.modes[r] = Some(x.mode);
                continue;
            }
            if let Some((ct, r)) = ctl
                && ct == t
            {
                let e = idx.controls[r].events[self.control_pos[r]];
                self.control_pos[r] += 1;
                self.stats.control_events += 1;
                let audible = self.eligible.get(e.track as usize).copied().unwrap_or(true);
                let Some(sink) = sinks.get_mut(r) else {
                    continue;
                };
                match e.status & 0xF0 {
                    // Unpaired note messages belong to their track (muted: silent).
                    0x90 if e.d1 > 0 => {
                        if audible {
                            let (ch, key) = (e.status & 0x0F, e.d0);
                            // B2-F12: a kept Note On keeps its Note Off too.
                            if let Some(f) = self.limits.processing.filters(ch, key, e.d1) {
                                self.count_filtered(f);
                                *self.raw_filtered.entry((r as u16, ch, key)).or_default() += 1;
                            } else {
                                sink.note_on(ch, key, e.d1);
                            }
                        }
                    }
                    // A Note Off, or a Note On of velocity 0 (= a Note Off).
                    0x80 | 0x90 => {
                        if audible {
                            let k = (r as u16, e.status & 0x0F, e.d0);
                            match self.raw_filtered.get_mut(&k) {
                                Some(n) if *n > 1 => *n -= 1,
                                Some(_) => {
                                    self.raw_filtered.remove(&k);
                                }
                                None => sink.note_off(e.status & 0x0F, e.d0),
                            }
                        }
                    }
                    _ => sink.channel_message(e.status, e.d0, e.d1),
                }
                continue;
            }
            // Note start. v1.0-Launch L-B04: the budget and the lateness
            // bound protect the internal synthesizer's rendering time, so they
            // apply to notes of route 0 only — a note for an external output
            // costs the worker a queue entry, never synthesis. Budget used up
            // by the synthesizer: defer (stop here, keep the order) unless
            // this note is already too late — then it is skipped below.
            let internal = self
                .starts
                .peek()
                .map(|Reverse((_, si))| {
                    let l = self.streams[*si as usize].lane as usize;
                    idx.route_of(idx.lane_track(l)) == 0
                })
                .unwrap_or(true);
            let late = if t < block_start || (internal && note_budget == 0) {
                let ef = *end_frame.get_or_insert_with(|| idx.tempo.frame_at_tick(end_tick));
                ef.saturating_sub(idx.tempo.frame_at_tick(t))
            } else {
                0
            };
            let too_late = internal && late > self.limits.max_lateness_frames;
            if internal && note_budget == 0 && !too_late {
                break;
            }
            let Some(Reverse((_, si))) = self.starts.pop() else {
                break;
            };
            let si = si as usize;
            let st = self.streams[si];
            let l = st.lane as usize;
            let ti = idx.lane_track(l);
            let snap = &idx.notes[l];
            let Some((mut rec, next)) = snap.next_at(st.pos) else {
                continue;
            };
            rec.start_tick = idx.lane_song(l, rec.start_tick);
            self.streams[si].pos = next;
            self.push_start(si);
            if !self.eligible.get(ti).copied().unwrap_or(true) {
                self.stats.muted += 1;
                continue;
            }
            let note = match self.corrected(ti, rec) {
                Corrected::Note { note, .. } => note,
                Corrected::OutsideRange => continue,
                Corrected::PitchOverflow | Corrected::EndOverflow => {
                    self.stats.correction_skipped += 1;
                    continue;
                }
            };
            if note.attrs.pitch() > MAX_PITCH {
                self.stats.above_midi += 1;
                continue;
            }
            let a = note.attrs;
            // B2-F12: kept from the outputs — never struck, so no Note Off is
            // scheduled for it and nothing it could end is touched.
            if let Some(f) = self
                .limits
                .processing
                .filters(a.channel(), a.pitch(), a.velocity())
            {
                self.count_filtered(f);
                continue;
            }
            let route = idx.route_of(ti);
            if too_late {
                self.stats.note_ons_skipped += 1;
                self.stats.late_dropped += 1;
                continue;
            }
            if (route != 0 || note_budget > 0) && self.offs.len() < self.limits.max_pending_offs {
                if route == 0 {
                    note_budget -= 1;
                }
                // B2-F12D / E: the end playback gives it.
                let end = self.playback_end(note.start_tick, note.end_tick());
                self.seq += 1;
                let seq = self.seq;
                self.retrigger(
                    (route as u16, a.channel(), a.pitch()),
                    seq,
                    sinks.get_mut(route),
                );
                if let Some(s) = sinks.get_mut(route) {
                    s.note_on(a.channel(), a.pitch(), a.velocity());
                }
                self.stats.note_ons += 1;
                if t < block_start {
                    self.stats.note_ons_deferred += 1;
                    self.stats.max_lateness = self.stats.max_lateness.max(late);
                }
                // A zero-length note ends in the same block.
                self.offs.push(Reverse((
                    end,
                    seq,
                    a.channel(),
                    a.pitch(),
                    ti as u32,
                    route as u16,
                )));
                self.stats.peak_pending_offs = self.stats.peak_pending_offs.max(self.offs.len());
            } else {
                self.stats.note_ons_skipped += 1;
            }
        }
        self.tick = self.tick.max(end_tick);
    }

    /// Loop wrap to `start`: every sounding note is released, every channel is
    /// moved from its state at the loop end to its state at `start`, and the
    /// notes sounding at `start` are re-struck (see module docs).
    fn wrap_to(&mut self, start: u64, sinks: &mut [&mut dyn SynthSink]) {
        let idx = Arc::clone(&self.index);
        let mut sent: HashSet<(u16, u8, u8)> = HashSet::new();
        let cancelled = std::mem::take(&mut self.cancelled);
        self.held_key.clear();
        for Reverse(o) in std::mem::take(&mut self.offs) {
            if !cancelled.contains(&o.1) && sent.insert((o.5, o.2, o.3)) {
                if let Some(s) = sinks.get_mut(usize::from(o.5)) {
                    s.note_off(o.2, o.3);
                }
                self.stats.note_offs += 1;
            }
        }
        for (r, sink) in sinks.iter_mut().enumerate() {
            let Some(c) = idx.controls.get(r) else {
                continue;
            };
            let policy = self.limits.processing.resets;
            let cur = c.state_with_resets(self.tick, policy);
            let target = c.state_with_resets(start, policy);
            for ch in 0..16 {
                ChannelState::transition(&cur[ch], &target[ch], ch as u8, &mut **sink);
            }
            if !idx.master.is_empty() {
                sink.master_volume(idx.master_at(start));
            }
        }
        self.stats.loop_wraps += 1;
        self.position(start, sinks, true);
    }

    /// Stops every sounding note (pause, stop, SoundFont change).
    pub fn silence(&mut self, sink: &mut dyn SynthSink) {
        self.silence_multi(&mut [sink]);
    }

    pub fn silence_multi(&mut self, sinks: &mut [&mut dyn SynthSink]) {
        for s in sinks.iter_mut() {
            s.silence();
        }
        self.offs.clear();
        self.raw_filtered.clear();
        self.held_key.clear();
        self.cancelled.clear();
    }
}

/// `a` is at or after `b` in snapshot order.
fn pos_after(a: SnapshotPos, b: SnapshotPos) -> bool {
    (a.chunk, a.index) >= (b.chunk, b.index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::events::RawEvent;
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
    use crate::model::project::Bpm;
    use crate::model::timing::Division;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Msg {
        On(u8, u8, u8),
        Off(u8, u8),
        Ch(u8, u8, u8),
        Silence,
        Reset,
        System(Vec<u8>),
    }

    #[derive(Default)]
    struct Rec {
        msgs: Vec<(u64, Msg)>,
        frame: u64,
    }

    impl SynthSink for Rec {
        fn note_on(&mut self, c: u8, k: u8, v: u8) {
            self.msgs.push((self.frame, Msg::On(c, k, v)));
        }
        fn note_off(&mut self, c: u8, k: u8) {
            self.msgs.push((self.frame, Msg::Off(c, k)));
        }
        fn channel_message(&mut self, s: u8, a: u8, b: u8) {
            self.msgs.push((self.frame, Msg::Ch(s, a, b)));
        }
        fn silence(&mut self) {
            self.msgs.push((self.frame, Msg::Silence));
        }
        fn reset(&mut self) {
            self.msgs.push((self.frame, Msg::Reset));
        }
        fn system_reset(&mut self, message: &[u8]) {
            self.msgs.push((self.frame, Msg::System(message.to_vec())));
        }
    }

    fn note(s: u64, d: u64, p: u8, v: u8, ch: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, v, ch, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    /// 120 BPM, 480 PPQN, 48 kHz → 50 frames per tick.
    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[note(0, 480, 60, 100, 0), note(960, 480, 64, 30, 0)],
                false,
            )
            .unwrap();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(480, 960, 36, 127, 9)], false)
            .unwrap();
        let ev = &mut d.track_mut(1).unwrap().events;
        for (tick, status, a, b) in [
            (0u64, 0xC0u8, 5u8, 0u8),
            (0, 0xB0, 7, 90),
            (0, 0xB0, 101, 0),
            (0, 0xB0, 100, 0),
            (0, 0xB0, 6, 12),
            (700, 0xE0, 0, 80),
            (1000, 0xB0, 64, 127),
        ] {
            ev.push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Channel {
                    status,
                    data: [a, b],
                },
            })
            .unwrap();
        }
        d
    }

    fn run(s: &mut Scheduler, r: &mut Rec, blocks: usize) {
        for _ in 0..blocks {
            r.frame = s.frame();
            s.advance(64, r);
        }
    }

    /// v1.0-Launch L-B06: route 1 (track 1) heard nothing until tick ≈ 800
    /// (its output was still opening); joining it sends that route alone a
    /// reset, its channel state at the position (program 5, volume 90, the
    /// RPN, the pitch bend at 700) and the note held there (key 36, 480 –
    /// 1440), whose Note Off then comes once at its own end. Nothing earlier
    /// is sent, and route 0 is untouched.
    #[test]
    fn a_route_joining_late_gets_its_state_and_held_notes_only() {
        let d = doc();
        let mut setup = PlaybackSetup::for_document(&d);
        setup.routes[1] = 1;
        setup.route_count = 2;
        let idx = Arc::new(PlaybackIndex::new(
            &d,
            setup.control_indexes(&d),
            Arc::new(crate::model::TempoMap::for_document(&d, 48_000)),
            &setup,
        ));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let (mut r0, mut deaf) = (Rec::default(), Rec::default());
        s.seek_multi(0, &mut [&mut r0, &mut deaf]);
        while s.tick() < 800 {
            s.advance_multi(64, &mut [&mut r0, &mut deaf]);
        }
        let at = s.tick();
        let before0 = r0.msgs.len();
        let mut late = Rec::default();
        s.join_route(1, &mut [&mut r0, &mut late]);
        assert_eq!(r0.msgs.len(), before0, "route 0 untouched");
        let m: Vec<Msg> = late.msgs.iter().map(|x| x.1.clone()).collect();
        assert_eq!(m.first(), Some(&Msg::Reset));
        let p = |x: &Msg| m.iter().position(|y| y == x);
        let program = p(&Msg::Ch(0xC0, 5, 0)).unwrap_or_else(|| panic!("program restored: {m:?}"));
        assert!(p(&Msg::Ch(0xB0, 7, 90)).is_some(), "volume restored: {m:?}");
        assert!(
            p(&Msg::Ch(0xE0, 0, 80)).is_some(),
            "pitch bend at 700 restored"
        );
        let on = p(&Msg::On(9, 36, 127)).expect("held note struck");
        assert!(program < on);
        assert_eq!(
            m.iter().filter(|x| matches!(x, Msg::On(..))).count(),
            1,
            "only the note held at tick {at}"
        );
        while !s.finished() {
            s.advance_multi(64, &mut [&mut r0, &mut late]);
        }
        let offs = late.msgs.iter().filter(|x| x.1 == Msg::Off(9, 36)).count();
        assert_eq!(offs, 1, "one Note Off for the re-struck note");
        assert!(
            late.msgs.iter().any(|x| x.1 == Msg::Ch(0xB0, 64, 127)),
            "later events play"
        );
        assert!(!r0.msgs.iter().any(|x| matches!(x.1, Msg::On(9, ..))));
    }

    /// B1-0138 (audio matrix): zero-length notes and overlapping notes on
    /// one key send balanced Note On / Off — nothing sounds after the end, a
    /// seek into the overlap or a pause — in either order of their ends.
    #[test]
    fn zero_length_and_overlapping_same_key_notes_never_stay_sounding() {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[
                    note(0, 0, 60, 100, 0),
                    note(0, 960, 62, 100, 0),
                    note(240, 240, 62, 90, 0),
                    note(240, 1440, 64, 80, 0),
                    note(480, 0, 64, 70, 0),
                    note(480, 240, 64, 60, 0),
                ],
                false,
            )
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let balance = |msgs: &[(u64, Msg)]| {
            let mut sounding = std::collections::HashMap::<(u8, u8), i64>::new();
            for (_, m) in msgs {
                match m {
                    Msg::On(c, k, _) => *sounding.entry((*c, *k)).or_default() += 1,
                    Msg::Off(c, k) => *sounding.entry((*c, *k)).or_default() -= 1,
                    Msg::Silence | Msg::Reset => sounding.clear(),
                    Msg::Ch(..) | Msg::System(_) => {}
                }
            }
            sounding.values().all(|&n| n <= 0)
        };
        // Through the end.
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 3000);
        assert!(s.finished());
        let ons = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .count();
        let offs = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Off(..)))
            .count();
        assert_eq!((ons, offs), (6, 6), "{:?}", r.msgs);
        assert!(balance(&r.msgs));
        // A zero-length note is an On then its Off, never the other way round.
        let c4: Vec<&Msg> = r
            .msgs
            .iter()
            .map(|(_, m)| m)
            .filter(|m| matches!(m, Msg::On(0, 60, _) | Msg::Off(0, 60)))
            .collect();
        assert_eq!(c4, vec![&Msg::On(0, 60, 100), &Msg::Off(0, 60)]);
        // Seek into the overlap, play a little, seek away: nothing left on.
        let mut r = Rec::default();
        s.seek(300, &mut r);
        run(&mut s, &mut r, 40);
        s.seek(5000, &mut r);
        assert!(balance(&r.msgs), "{:?}", r.msgs);
    }

    #[test]
    fn events_are_emitted_at_their_frames_in_order() {
        let d = doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2000); // 128,000 frames = 2,560 ticks
        let notes: Vec<(u64, Msg)> = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..) | Msg::Off(..)))
            .cloned()
            .collect();
        // Frame of tick t = 50 t; block-quantized to multiples of 64 (event sent
        // at the start of the block that contains its frame).
        let q = |t: u64| (t * 50) / 64 * 64;
        assert_eq!(
            notes,
            vec![
                (q(0), Msg::On(0, 60, 100)),
                (q(480), Msg::Off(0, 60)),
                (q(480), Msg::On(9, 36, 127)),
                (q(960), Msg::On(0, 64, 30)),
                (q(1440), Msg::Off(9, 36)),
                (q(1440), Msg::Off(0, 64)),
            ]
        );
        // Program/volume at tick 0 come before the first note-on.
        let first_on = r
            .msgs
            .iter()
            .position(|(_, m)| matches!(m, Msg::On(..)))
            .unwrap();
        assert!(r.msgs[..first_on].contains(&(0, Msg::Ch(0xC0, 5, 0))));
        assert!(s.finished());
        assert_eq!(s.stats().note_ons, 3);
        assert_eq!(s.pending_offs(), 0);
    }

    #[test]
    fn seek_restores_state_and_restrikes() {
        let d = doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(1100, &mut r);
        // Reset first, then program, RPN pitch-bend range, volume, bend, sustain.
        assert_eq!(r.msgs[0].1, Msg::Reset);
        let msgs: Vec<Msg> = r.msgs.iter().map(|(_, m)| m.clone()).collect();
        assert!(msgs.contains(&Msg::Ch(0xC0, 5, 0)));
        assert!(msgs.contains(&Msg::Ch(0xB0, 6, 12)));
        assert!(msgs.contains(&Msg::Ch(0xB0, 7, 90)));
        assert!(msgs.contains(&Msg::Ch(0xE0, 0, 80)));
        assert!(msgs.contains(&Msg::Ch(0xB0, 64, 127)));
        // Sounding at 1100: the drum (480..1440) and note 64 (960..1440).
        let ons: Vec<&Msg> = msgs.iter().filter(|m| matches!(m, Msg::On(..))).collect();
        assert_eq!(ons, vec![&Msg::On(0, 64, 30), &Msg::On(9, 36, 127)]);
        assert_eq!(s.frame(), 1100 * 50);
        r.msgs.clear();
        run(&mut s, &mut r, 500);
        let offs = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Off(..)))
            .count();
        assert_eq!(offs, 2, "re-struck notes end at their own end");
        assert!(s.finished());
    }

    /// v1.0-beta1 B1-0082 / B1-0083: a resume at the paused tick restores
    /// the channels but does not strike the held notes again; nothing is
    /// queued for them, and the next notes play as before.
    #[test]
    fn a_resume_without_restrike_leaves_held_notes_silent() {
        let d = doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek_multi_with(1100, &mut [&mut r], false);
        let msgs: Vec<Msg> = r.msgs.iter().map(|(_, m)| m.clone()).collect();
        assert_eq!(msgs[0], Msg::Reset);
        assert!(
            msgs.contains(&Msg::Ch(0xC0, 5, 0)),
            "channel state is restored"
        );
        assert!(!msgs.iter().any(|m| matches!(m, Msg::On(..))), "{msgs:?}");
        assert_eq!(s.stats().restruck, 0);
        r.msgs.clear();
        run(&mut s, &mut r, 500);
        assert!(
            !r.msgs.iter().any(|(_, m)| matches!(m, Msg::Off(..))),
            "no Note Off for a note that was not struck: {:?}",
            r.msgs
        );
        assert!(s.finished());
        // The same seek with re-strike (any other seek) still sounds them.
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek_multi_with(1100, &mut [&mut r], true);
        assert_eq!(
            r.msgs
                .iter()
                .filter(|(_, m)| matches!(m, Msg::On(..)))
                .count(),
            2
        );
    }

    /// v1.0-beta1 B1-0084: the pause / resume regression matrix. For each
    /// document — same-key overlaps, a held sustain pedal, a tempo ramp,
    /// 2,000-note polyphony on every channel — and each pause point (on a
    /// Note On tick, on a Note Off tick, inside notes, twice in one run), a
    /// resume without re-strike never strikes a note that started before the
    /// resume tick, strikes every later note exactly when an uninterrupted
    /// run does, never sends a Note Off for a note it did not strike, and
    /// leaves nothing sounding at the end. With re-strike, exactly the notes
    /// held at the resume tick are struck in addition.
    #[test]
    fn pause_resume_matrix_never_doubles_a_note_and_never_sticks_one() {
        use crate::model::conductor::Interp;
        use crate::model::tempo::TempoValue;
        let limits = SchedulerLimits {
            max_note_ons_per_block: 1 << 20,
            max_pending_offs: 1 << 20,
            max_lateness_frames: u64::MAX,
            processing: EventProcessing::default(),
        };
        let cc = |d: &mut PlumDocument, tick: u64, a: u8, b: u8| {
            d.track_mut(0)
                .unwrap()
                .events
                .push(RawEvent {
                    tick,
                    anchor: 0,
                    kind: RawKind::Channel {
                        status: 0xB0,
                        data: [a, b],
                    },
                })
                .unwrap();
        };
        let with = |notes: Vec<NoteRecord>| {
            let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 480);
            let mut notes = notes;
            notes.sort_by_key(|n| n.start_tick);
            d.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&notes, false)
                .unwrap();
            d
        };
        let overlaps = with(vec![
            note(0, 960, 60, 100, 0),
            note(480, 960, 60, 90, 0),
            note(480, 10, 60, 80, 0),
            note(960, 480, 62, 70, 0),
            note(1440, 960, 60, 60, 0),
        ]);
        let mut sustain = with(
            (0..12)
                .map(|k| note(k * 240, 120, 50 + k as u8, 100, 0))
                .collect(),
        );
        cc(&mut sustain, 100, 64, 127);
        cc(&mut sustain, 2000, 64, 0);
        let mut ramp = with(
            (0..24)
                .map(|k| note(k * 200, 300, 40 + k as u8, 100, 1))
                .collect(),
        );
        for (tick, bpm, interp) in [(0u64, "60", Interp::Linear), (2400, "240", Interp::Step)] {
            let e = ramp
                .conductor()
                .prepare_set_tempo(
                    None,
                    tick,
                    TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                    interp,
                )
                .unwrap();
            ramp.conductor_mut().apply(&e).unwrap();
        }
        let dense = with(
            (0..2_000u64)
                .map(|i| {
                    note(
                        (i % 7) * 240,
                        200 + (i % 9) * 120,
                        (i % 128) as u8,
                        1 + (i % 127) as u8,
                        (i % 16) as u8,
                    )
                })
                .collect(),
        );
        // Note Ons (frame, channel, key, velocity) and the per-key balance.
        type On = (u64, u8, u8, u8);
        fn ons(msgs: &[(u64, Msg)]) -> Vec<On> {
            let mut v: Vec<On> = msgs
                .iter()
                .filter_map(|(f, m)| match *m {
                    Msg::On(c, k, vel) => Some((*f, c, k, vel)),
                    _ => None,
                })
                .collect();
            v.sort_unstable();
            v
        }
        fn balanced(msgs: &[(u64, Msg)]) -> Result<(), String> {
            let mut held = std::collections::HashMap::<(u8, u8), i64>::new();
            for (f, m) in msgs {
                match *m {
                    Msg::On(c, k, _) => *held.entry((c, k)).or_default() += 1,
                    Msg::Off(c, k) => {
                        let h = held.entry((c, k)).or_default();
                        *h -= 1;
                        if *h < 0 {
                            return Err(format!(
                                "Note Off ch {c} key {k} at frame {f} without its Note On"
                            ));
                        }
                    }
                    _ => {}
                }
            }
            match held.iter().find(|(_, n)| **n != 0) {
                Some(((c, k), n)) => Err(format!("ch {c} key {k}: {n} still sounding")),
                None => Ok(()),
            }
        }
        for (name, d) in [
            ("overlaps", &overlaps),
            ("sustain", &sustain),
            ("ramp", &ramp),
            ("dense", &dense),
        ] {
            let idx = Arc::new(PlaybackIndex::build(d, 48_000));
            // Uninterrupted.
            let mut s = Scheduler::new(Arc::clone(&idx), limits);
            let mut full = Rec::default();
            s.seek(0, &mut full);
            while !s.finished() {
                run(&mut s, &mut full, 1);
            }
            let all = ons(&full.msgs);
            balanced(&full.msgs).unwrap();
            for pauses in [
                vec![480u64],
                vec![960],
                vec![700],
                vec![300, 1700],
                vec![1440, 1441],
            ] {
                let mut s = Scheduler::new(Arc::clone(&idx), limits);
                let mut r = Rec::default();
                s.seek(0, &mut r);
                for &p in &pauses {
                    while s.tick() < p && !s.finished() {
                        run(&mut s, &mut r, 1);
                    }
                    // Pause, then resume at the paused tick.
                    s.silence(&mut r);
                    r.msgs.clear();
                    s.seek_multi_with(p, &mut [&mut r], false);
                }
                while !s.finished() {
                    run(&mut s, &mut r, 1);
                }
                let last = *pauses.last().unwrap();
                let from = idx.tempo.frame_at_tick(last);
                let want: Vec<On> = all.iter().copied().filter(|o| o.0 >= from).collect();
                // The recorder stamps a block's start frame (64 frames): the
                // same notes, each within one block of the uninterrupted run.
                let key = |v: Vec<On>| {
                    let mut v: Vec<(u8, u8, u8, u64)> =
                        v.into_iter().map(|o| (o.1, o.2, o.3, o.0)).collect();
                    v.sort_unstable();
                    v
                };
                let (got, exp) = (key(ons(&r.msgs)), key(want.clone()));
                assert_eq!(
                    got.len(),
                    exp.len(),
                    "{name} {pauses:?}: {got:?} vs {exp:?}"
                );
                for (g, e) in got.iter().zip(&exp) {
                    assert!(
                        g.0 == e.0 && g.1 == e.1 && g.2 == e.2 && g.3.abs_diff(e.3) < 64,
                        "{name} {pauses:?}: {g:?} vs {e:?}"
                    );
                }
                balanced(&r.msgs).unwrap_or_else(|e| panic!("{name} {pauses:?}: {e}"));
                // With re-strike: the same, plus exactly the notes held there.
                let mut s = Scheduler::new(Arc::clone(&idx), limits);
                let mut r2 = Rec::default();
                s.seek_multi_with(last, &mut [&mut r2], true);
                let held = s.stats().restruck;
                while !s.finished() {
                    run(&mut s, &mut r2, 1);
                }
                assert_eq!(
                    ons(&r2.msgs).len() as u64,
                    want.len() as u64 + held,
                    "{name} {pauses:?}"
                );
                balanced(&r2.msgs).unwrap_or_else(|e| panic!("{name} {pauses:?} re-strike: {e}"));
            }
        }
    }

    #[test]
    fn dense_bursts_are_bounded_without_stuck_notes() {
        let mut d = PlumDocument::new_project("dense".into(), Bpm::DEFAULT, 480);
        let recs: Vec<NoteRecord> = (0..20_000u64)
            .map(|i| {
                note(
                    0,
                    100 + i % 50,
                    (i % 128) as u8,
                    1 + (i % 127) as u8,
                    (i % 16) as u8,
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let limits = SchedulerLimits {
            max_note_ons_per_block: 100,
            max_pending_offs: 1000,
            max_lateness_frames: 0,
            processing: EventProcessing::default(),
        };
        let mut s = Scheduler::new(idx, limits);
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 400);
        let st = s.stats();
        assert_eq!(st.note_ons + st.note_ons_skipped, 20_000);
        assert!(st.note_ons <= 100 * 400);
        assert!(st.peak_pending_offs <= 1000);
        // Every started note was ended.
        let ons = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .count();
        let offs = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Off(..)))
            .count();
        assert_eq!(ons, offs);
        assert!(s.finished());
    }

    #[test]
    fn checkpoints_match_full_replay() {
        let mut d = PlumDocument::new_project("cc".into(), Bpm::DEFAULT, 96);
        let ev = &mut d.track_mut(0).unwrap().events;
        for i in 0..10_000u64 {
            let ch = (i % 16) as u8;
            let (status, a, b) = match i % 5 {
                0 => (0xB0 | ch, 7, (i % 128) as u8),
                1 => (0xC0 | ch, (i % 128) as u8, 0),
                2 => (0xE0 | ch, (i % 128) as u8, ((i / 3) % 128) as u8),
                3 => (0xB0 | ch, 10, ((i * 7) % 128) as u8),
                _ => (0xB0 | ch, 121, 0),
            };
            ev.push(RawEvent {
                tick: i,
                anchor: 0,
                kind: RawKind::Channel {
                    status,
                    data: [a, b],
                },
            })
            .unwrap();
        }
        let ci = ControlIndex::build(&d);
        for tick in [0u64, 1, 2047, 2048, 2049, 5000, 9999, 10_000, 20_000] {
            let (fast, pos) = ci.state_before(tick);
            let mut slow = [ChannelState::default(); 16];
            for e in ci.events.iter().take_while(|e| e.tick < tick) {
                slow[usize::from(e.status & 0x0F)].apply(e.status, e.d0, e.d1);
            }
            assert_eq!(fast, slow, "tick {tick}");
            assert_eq!(pos, ci.events.partition_point(|e| e.tick < tick));
        }
    }

    #[test]
    fn tempo_changes_move_events() {
        // Division check: SMPTE 25 fps × 40 → 1000 ticks per second.
        let mut d = PlumDocument::new_empty(Division::from_raw(0xE728));
        d.track_mut(0)
            .unwrap()
            .notes
            .insert(note(1000, 1000, 60, 90, 0), false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2000);
        let on = r
            .msgs
            .iter()
            .find(|(_, m)| matches!(m, Msg::On(..)))
            .unwrap()
            .0;
        let off = r
            .msgs
            .iter()
            .find(|(_, m)| matches!(m, Msg::Off(..)))
            .unwrap()
            .0;
        assert_eq!(on, 48_000 / 64 * 64);
        assert_eq!(off, 96_000 / 64 * 64);
    }

    // ------------------------------------------------------------ v0.4

    fn two_tracks(shared_key: bool) -> PlumDocument {
        // Track 0: a long note (ch 0, key 60). Track 1: a long note on ch 0
        // with key 60 (shared) or 64.
        let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[note(0, 4800, 60, 100, 0), note(1920, 480, 62, 100, 0)],
                false,
            )
            .unwrap();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[note(0, 4800, if shared_key { 60 } else { 64 }, 90, 0)],
                false,
            )
            .unwrap();
        d
    }

    fn ons(r: &Rec) -> Vec<Msg> {
        r.msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .map(|(_, m)| m.clone())
            .collect()
    }

    #[test]
    fn mute_solo_and_precedence_decide_what_starts() {
        let mut d = two_tracks(false);
        let play = |d: &PlumDocument| {
            let idx = Arc::new(PlaybackIndex::build(d, 48_000));
            let mut s = Scheduler::new(idx, SchedulerLimits::default());
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 3000);
            (ons(&r), s.stats())
        };
        let (all, _) = play(&d);
        assert_eq!(all.len(), 3);
        d.track_mut(1).unwrap().mute = true;
        let (m, st) = play(&d);
        assert_eq!(m, vec![Msg::On(0, 60, 100), Msg::On(0, 62, 100)]);
        assert_eq!(st.muted, 1);
        // Solo track 1 while it is muted: Mute wins, and a muted solo does not
        // restrict the others, so track 0 still plays.
        d.track_mut(1).unwrap().solo = true;
        let (m, _) = play(&d);
        assert_eq!(m.len(), 2);
        // Unmuted solo: only track 1.
        d.track_mut(1).unwrap().mute = false;
        let (m, _) = play(&d);
        assert_eq!(m, vec![Msg::On(0, 64, 90)]);
        // Two solos: both.
        d.track_mut(0).unwrap().solo = true;
        let (m, _) = play(&d);
        assert_eq!(m.len(), 3);
        // Play All (clear every flag): everything again.
        for t in 0..2 {
            let t = d.track_mut(t).unwrap();
            t.mute = false;
            t.solo = false;
        }
        assert_eq!(play(&d).0.len(), 3);
    }

    /// L-F08 / L-F09: a monitor set (Play Only Selected Tracks) plays
    /// exactly its tracks — over Mute and Solo, which it keeps — switches
    /// live like a Mute, and is never saved.
    #[test]
    fn a_monitor_set_plays_exactly_its_tracks_and_keeps_mute_and_solo() {
        let mut d = two_tracks(false);
        let play = |d: &PlumDocument| {
            let idx = Arc::new(PlaybackIndex::build(d, 48_000));
            let mut s = Scheduler::new(idx, SchedulerLimits::default());
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 3000);
            ons(&r)
        };
        let t1 = d.track(1).unwrap().id();
        d.track_mut(1).unwrap().mute = true;
        d.track_mut(0).unwrap().solo = true;
        assert_eq!(eligibility(&d), vec![true, false]);
        // Monitor track 1: it plays although it is muted, track 0 (soloed)
        // does not; the flags stay as they are.
        d.project_mut().monitor = Some(vec![t1]);
        assert_eq!(eligibility(&d), vec![false, true]);
        assert_eq!(play(&d), vec![Msg::On(0, 64, 90)]);
        assert!(d.track(1).unwrap().mute && d.track(0).unwrap().solo);
        // Live: switching the monitor during playback ends the excluded
        // track's notes and strikes the included one's, as a Mute does.
        d.project_mut().monitor = None;
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 200);
        r.msgs.clear();
        d.project_mut().monitor = Some(vec![t1]);
        s.set_eligible(eligibility(&d).into(), &mut r);
        let msgs: Vec<Msg> = r.msgs.iter().map(|(_, m)| m.clone()).collect();
        assert!(msgs.contains(&Msg::Off(0, 60)), "{msgs:?}");
        assert!(msgs.contains(&Msg::On(0, 64, 90)), "{msgs:?}");
        // Restore All: back to the song's own Mute / Solo.
        d.project_mut().monitor = None;
        assert_eq!(eligibility(&d), vec![true, false]);
        // Never saved: a .lup round trip has no monitor, the flags are kept.
        d.project_mut().monitor = Some(vec![t1]);
        let bytes = crate::native::lup::project_to_bytes(&d).unwrap();
        let (back, _) = crate::native::lup::project_from_bytes(&bytes).unwrap();
        assert_eq!(back.project().monitor, None);
        assert!(back.track(1).unwrap().mute && back.track(0).unwrap().solo);
    }

    #[test]
    fn live_mute_never_cuts_another_tracks_note_on_a_shared_key() {
        for shared in [true, false] {
            let d = two_tracks(shared);
            let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
            let mut s = Scheduler::new(idx, SchedulerLimits::default());
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 200); // both long notes sounding
            r.msgs.clear();
            s.set_eligible(Arc::from(vec![true, false]), &mut r);
            let offs: Vec<Msg> = r.msgs.iter().map(|(_, m)| m.clone()).collect();
            if shared {
                // Track 0 still holds ch 0 key 60: no note-off may be sent.
                assert!(offs.is_empty(), "{offs:?}");
            } else {
                assert_eq!(offs, vec![Msg::Off(0, 64)]);
            }
            assert_eq!(s.pending_offs(), 1, "only track 0's note is still tracked");
            // Unmuting re-strikes track 1's sounding note.
            r.msgs.clear();
            s.set_eligible(Arc::from(vec![true, true]), &mut r);
            assert_eq!(ons(&r).len(), 1);
            // Everything that started also ends.
            run(&mut s, &mut r, 5000);
            let on = r
                .msgs
                .iter()
                .filter(|(_, m)| matches!(m, Msg::On(..)))
                .count();
            let off = r
                .msgs
                .iter()
                .filter(|(_, m)| matches!(m, Msg::Off(..)))
                .count();
            assert_eq!(s.pending_offs(), 0);
            assert!(off >= on, "{on} on / {off} off");
        }
    }

    #[test]
    fn corrections_shift_transpose_scale_and_filter_once() {
        use crate::model::track::{GatePercent, TrackProps};
        let mut d = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[
                    note(0, 480, 60, 100, 0),
                    note(1920, 480, 62, 100, 0),
                    note(1920, 480, 120, 100, 0),
                    note(2400, 480, 30, 100, 0),
                ],
                false,
            )
            .unwrap();
        let props = TrackProps {
            tick_correction: -960,
            correction_from_measure: 2,
            key_correction: 12,
            gate: GatePercent::parse("50").unwrap(),
            key_range: (40, 127),
            ..TrackProps::default()
        };
        d.track_mut(0).unwrap().set_props(props, None);
        let fp = d.fingerprint();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 3000);
        let q = |t: u64| (t * 50) / 64 * 64;
        let notes: Vec<(u64, Msg)> = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..) | Msg::Off(..)))
            .cloned()
            .collect();
        assert_eq!(
            notes,
            vec![
                (q(0), Msg::On(0, 60, 100)),
                // 1920 → 960, +12 = 74, 240 ticks long.
                (q(480), Msg::Off(0, 60)),
                (q(960), Msg::On(0, 74, 100)),
                (q(1200), Msg::Off(0, 74)),
                // 2400 → 1440; 30 + 12 = 42 is inside 40–127.
                (q(1440), Msg::On(0, 42, 100)),
                (q(1680), Msg::Off(0, 42)),
            ]
        );
        // 120 + 12 does not wrap: skipped and counted.
        assert_eq!(s.stats().correction_skipped, 1);
        // The document is untouched.
        assert_eq!(d.fingerprint(), fp);
        // Seeking into a corrected note re-strikes it at its corrected pitch.
        let mut r = Rec::default();
        s.seek(1000, &mut r);
        assert!(ons(&r).contains(&Msg::On(0, 74, 100)));
    }

    #[test]
    fn loops_wrap_on_the_tick_timeline_without_stuck_notes_or_drift() {
        use crate::model::conductor::Interp;
        use crate::model::tempo::TempoValue;
        // A note across the loop start, one inside, one across the loop end,
        // sustain pressed inside the loop, and a tempo ramp over the loop.
        let mut d = PlumDocument::new_project("l".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[
                    note(0, 960, 50, 100, 0),
                    note(960, 240, 60, 100, 0),
                    note(1800, 600, 70, 100, 0),
                ],
                false,
            )
            .unwrap();
        d.track_mut(0)
            .unwrap()
            .events
            .push(RawEvent {
                tick: 1000,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xB0,
                    data: [64, 127],
                },
            })
            .unwrap();
        let e = d
            .conductor()
            .prepare_set_tempo(
                None,
                0,
                TempoValue::Bpm(Bpm::parse("100").unwrap()),
                Interp::Linear,
            )
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        let e = d
            .conductor()
            .prepare_set_tempo(
                None,
                2400,
                TempoValue::Bpm(Bpm::parse("300").unwrap()),
                Interp::Step,
            )
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let tempo = Arc::clone(&idx.tempo);
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        s.set_loop(Some((480, 1920)));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        // (frames rendered when the wrap happened, playback frame after it)
        let mut wraps = Vec::new();
        let mut rendered = 0u64;
        for _ in 0..20_000 {
            r.frame = s.frame();
            if let Some(w) = s.advance(64, &mut r) {
                wraps.push((rendered + u64::from(w.offset), w.frame));
            }
            rendered += 64;
        }
        assert!(wraps.len() >= 5, "{} wraps", wraps.len());
        let loop_frames = tempo.frame_at_tick(1920) - tempo.frame_at_tick(480);
        for w in &wraps {
            assert_eq!(w.1, tempo.frame_at_tick(480));
        }
        // Each iteration takes exactly the loop's length (no drift).
        for pair in wraps.windows(2) {
            let played = pair[1].0 - pair[0].0;
            assert_eq!(played, loop_frames);
        }
        assert!(!s.finished(), "a loop keeps playing");
        // Note 50 (crossing the start) is re-struck every iteration, note 60
        // plays every iteration, note 70 (crossing the end) starts each time and
        // is ended at the wrap.
        let count = |k: u8| {
            r.msgs
                .iter()
                .filter(|(_, m)| *m == Msg::On(0, k, 100))
                .count()
        };
        // (The iteration running when the test stops may not have reached them.)
        let n = wraps.len();
        assert!(count(50) >= n);
        assert!((n..=n + 1).contains(&count(60)), "{}", count(60));
        assert!((n..=n + 1).contains(&count(70)), "{}", count(70));
        // Sustain from inside the loop is released at every wrap.
        let releases = r
            .msgs
            .iter()
            .filter(|(_, m)| *m == Msg::Ch(0xB0, 64, 0))
            .count();
        assert!((n..=n + 1).contains(&releases), "{releases}");
        // Nothing is left sounding when the loop is cleared and the song ends.
        s.set_loop(None);
        run(&mut s, &mut r, 20_000);
        assert!(s.finished());
        assert_eq!(s.pending_offs(), 0);
        assert_eq!(s.stats().loop_wraps as usize, wraps.len());
    }

    #[test]
    fn channel_transition_reaches_the_target_state_exactly() {
        let mut cur = ChannelState::default();
        for (s, a, b) in [
            (0xB0u8, 7u8, 30u8),
            (0xB0, 64, 127),
            (0xB0, 74, 10),
            (0xC0, 12, 0),
            (0xE0, 0, 100),
            (0xB0, 101, 0),
            (0xB0, 100, 0),
            (0xB0, 6, 12),
        ] {
            cur.apply(s, a, b);
        }
        let mut target = ChannelState::default();
        target.apply(0xB0, 10, 20);
        target.apply(0xC0, 3, 0);
        let mut r = Rec::default();
        ChannelState::transition(&cur, &target, 0, &mut r);
        // Replaying what was sent onto `cur` gives a state that sounds like the
        // target (defaults explicitly sent where the target has none).
        let mut got = cur;
        for (_, m) in &r.msgs {
            if let Msg::Ch(s, a, b) = m {
                got.apply(*s, *a, *b);
            }
        }
        assert_eq!(got.program, Some(3));
        assert_eq!(got.cc[7], 100, "volume back to its default");
        assert_eq!(got.cc[64], 0, "sustain released");
        assert_eq!(got.cc[74], 64, "cutoff back to its default");
        assert_eq!(got.cc[10], 20);
        assert_eq!(got.bend, Some((0, 0x40)));
        assert_eq!(got.rpn[0], Some((2, Some(0))), "bend range back to ±2");
        // Nothing is sent when the states are equal.
        let mut r = Rec::default();
        ChannelState::transition(&target, &target, 0, &mut r);
        assert!(r.msgs.is_empty());
    }

    #[test]
    fn master_volume_plays_at_its_ticks_and_is_restored_on_seek() {
        #[derive(Default)]
        struct M(Vec<(u64, u16)>, u64);
        impl SynthSink for M {
            fn note_on(&mut self, _: u8, _: u8, _: u8) {}
            fn note_off(&mut self, _: u8, _: u8) {}
            fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
            fn silence(&mut self) {}
            fn reset(&mut self) {}
            fn master_volume(&mut self, v: u16) {
                self.0.push((self.1, v));
            }
        }
        let mut d = doc();
        for (t, v) in [(480u64, 8000u16), (960, 100)] {
            let e = d.conductor().prepare_set_master(None, t, v).unwrap();
            d.conductor_mut().apply(&e).unwrap();
        }
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut m = M::default();
        s.seek(0, &mut m);
        for _ in 0..2000 {
            m.1 = s.frame();
            s.advance(64, &mut m);
        }
        let q = |t: u64| (t * 50) / 64 * 64;
        assert_eq!(m.0, vec![(0, 16383), (q(480), 8000), (q(960), 100)]);
        let mut m = M::default();
        s.seek(700, &mut m);
        assert_eq!(m.0, vec![(0, 8000)]);
    }

    #[test]
    fn speed_scales_event_frames_and_leaves_the_tempo_map_alone() {
        use crate::model::tempo::Speed;
        let d = doc();
        let tempo_rev = d.tempo_revision();
        for (speed, factor) in [("2", 2.0f64), ("0.5", 0.5), ("100", 100.0)] {
            let sp = Speed::parse(speed).unwrap();
            let setup = PlaybackSetup::for_document(&d);
            let idx = Arc::new(PlaybackIndex::new(
                &d,
                setup.control_indexes(&d),
                Arc::new(TempoMap::for_document_at(&d, 48_000, sp)),
                &setup,
            ));
            let mut s = Scheduler::new(idx, SchedulerLimits::default());
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 20_000);
            let first_64 = r
                .msgs
                .iter()
                .find(|(_, m)| *m == Msg::On(0, 64, 30))
                .unwrap()
                .0;
            // Tick 960 = 48,000 frames at 1×.
            let expected = (48_000.0 / factor) as u64;
            assert!(
                first_64.abs_diff(expected) <= 64,
                "{speed}: {first_64} vs {expected}"
            );
        }
        assert_eq!(d.tempo_revision(), tempo_rev);
    }

    #[test]
    fn routes_keep_their_own_events_and_state() {
        let mut d = doc();
        // Track 1 (drum + controllers) goes to route 1.
        let mut setup = PlaybackSetup::for_document(&d);
        setup.routes = vec![0, 1];
        setup.route_count = 2;
        d.add_track().unwrap();
        setup.routes.push(0);
        setup.eligible = eligibility(&d).into();
        setup.corrections.push(None);
        let idx = Arc::new(PlaybackIndex::new(
            &d,
            setup.control_indexes(&d),
            Arc::new(TempoMap::for_document(&d, 48_000)),
            &setup,
        ));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let (mut a, mut b) = (Rec::default(), Rec::default());
        s.seek_multi(0, &mut [&mut a, &mut b]);
        for _ in 0..2000 {
            s.advance_multi(64, &mut [&mut a, &mut b]);
        }
        assert!(ons(&a).iter().all(|m| matches!(m, Msg::On(0, _, _))));
        assert_eq!(ons(&b), vec![Msg::On(9, 36, 127)]);
        assert!(
            a.msgs
                .iter()
                .all(|(_, m)| !matches!(m, Msg::Ch(0xC0, 5, 0)))
        );
        assert!(b.msgs.iter().any(|(_, m)| matches!(m, Msg::Ch(0xC0, 5, 0))));
    }

    /// An index of `d` with track `i` on route `routes[i]` and `count` routes
    /// (what the app builds: 1 + the open external outputs).
    fn routed(d: &PlumDocument, routes: &[u16], count: usize) -> Arc<PlaybackIndex> {
        let mut setup = PlaybackSetup::for_document(d);
        setup.routes = routes.to_vec();
        setup.route_count = count;
        Arc::new(PlaybackIndex::new(
            d,
            setup.control_indexes(d),
            Arc::new(TempoMap::for_document(d, 48_000)),
            &setup,
        ))
    }

    /// Three tracks of long notes (channels 0 / 1 / 2) with programs and a
    /// GS reset then an XG reset on track 0, so every route-indexed cursor
    /// (controls, resets, modes) is used while the routes change.
    fn three_route_doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("routes".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.add_track().unwrap();
        for t in 0..3u8 {
            let notes: Vec<NoteRecord> = (0..40u64)
                .map(|i| note(i * 240, 900, 60 + t, 90, t))
                .collect();
            d.track_mut(usize::from(t))
                .unwrap()
                .notes
                .insert_sorted_batch(&notes, false)
                .unwrap();
            let ev = &mut d.track_mut(usize::from(t)).unwrap().events;
            let sysex = |tick: u64, bytes: &[u8], ev: &mut crate::model::events::EventStore| {
                let payload = ev.store_payload(bytes).unwrap();
                ev.push(RawEvent {
                    tick,
                    anchor: 0,
                    kind: RawKind::SysEx { payload },
                })
                .unwrap();
            };
            if t == 0 {
                sysex(0, &GS_RESET, ev);
            }
            for (tick, prog) in [(0u64, 10 + t), (2_400, 20 + t)] {
                ev.push(RawEvent {
                    tick,
                    anchor: 0,
                    kind: RawKind::Channel {
                        status: 0xC0 | t,
                        data: [prog, 0],
                    },
                })
                .unwrap();
            }
            if t == 0 {
                sysex(4_800, &XG_ON, ev);
            }
        }
        d
    }

    /// Note Ons (channel) a recorder received at or after `frame`.
    fn channels_struck_after(r: &Rec, frame: u64) -> Vec<u8> {
        let mut v: Vec<u8> = r
            .msgs
            .iter()
            .filter(|(f, _)| *f >= frame)
            .filter_map(|(_, m)| match m {
                Msg::On(c, _, _) => Some(*c),
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// v1.0-Launch L-A01 (contract §003, `plumspirit-flopp.log`: panic at
    /// playback.rs:1506 "the len is 1 but the index is 1" right after the
    /// KDMAPI output opened while playing): an index replacement that changes
    /// the number of routes — 1 → 2 → 3 → 1 → 2, with seeks and a loop wrap
    /// in between and the resets being sent — rebuilds every route-indexed
    /// cursor. Nothing panics, each route's notes reach only its own output,
    /// and no output is left with a sounding note.
    #[test]
    fn route_count_changes_while_playing_rebuild_every_route_cursor() {
        let d = three_route_doc();
        let limits = policy(ResetPolicy::Send);
        let mut s = Scheduler::new(routed(&d, &[0, 0, 0], 1), limits);
        let (mut a, mut b, mut c) = (Rec::default(), Rec::default(), Rec::default());
        let step = |s: &mut Scheduler, sinks: &mut [&mut Rec], blocks: usize| {
            for _ in 0..blocks {
                let f = s.frame();
                for r in sinks.iter_mut() {
                    r.frame = f;
                }
                let mut dynsinks: Vec<&mut dyn SynthSink> = sinks
                    .iter_mut()
                    .map(|r| &mut **r as &mut dyn SynthSink)
                    .collect();
                s.advance_multi(64, &mut dynsinks);
            }
        };
        s.seek_multi(0, &mut [&mut a]);
        step(&mut s, &mut [&mut a], 300);
        // Internal → KDMAPI: every track now plays on route 1 (two routes).
        let switch1 = s.frame();
        let tick = s.tick();
        (a.frame, b.frame) = (switch1, switch1);
        s.set_index_multi(routed(&d, &[1, 1, 1], 2), tick, &mut [&mut a, &mut b]);
        step(&mut s, &mut [&mut a, &mut b], 300);
        assert!(
            channels_struck_after(&a, switch1).is_empty(),
            "route 0 is silent"
        );
        assert_eq!(channels_struck_after(&b, switch1), vec![0, 1, 2]);
        // A second device: track 1 on route 2, track 2 back to the synthesizer.
        let switch2 = s.frame();
        let tick = s.tick();
        (a.frame, b.frame, c.frame) = (switch2, switch2, switch2);
        s.set_index_multi(
            routed(&d, &[1, 2, 0], 3),
            tick,
            &mut [&mut a, &mut b, &mut c],
        );
        step(&mut s, &mut [&mut a, &mut b, &mut c], 300);
        assert_eq!(channels_struck_after(&a, switch2), vec![2]);
        assert_eq!(channels_struck_after(&b, switch2), vec![0]);
        assert_eq!(channels_struck_after(&c, switch2), vec![1]);
        // Seek and a loop wrap with three routes (resets restored per route).
        s.seek_multi(1_200, &mut [&mut a, &mut b, &mut c]);
        s.set_loop(Some((1_200, 3_600)));
        step(&mut s, &mut [&mut a, &mut b, &mut c], 2_000);
        assert!(s.stats().loop_wraps > 0);
        s.set_loop(None);
        // Both devices closed (N → 1): the closed outputs are flushed by the
        // engine (`SetRoutes`), the synthesizer plays everything again.
        b.silence();
        c.silence();
        let switch3 = s.frame();
        let tick = s.tick();
        a.frame = switch3;
        s.set_index_multi(routed(&d, &[0, 0, 0], 1), tick, &mut [&mut a]);
        step(&mut s, &mut [&mut a], 300);
        assert_eq!(channels_struck_after(&a, switch3), vec![0, 1, 2]);
        // And back to two routes, then stop.
        let tick = s.tick();
        s.set_index_multi(routed(&d, &[1, 0, 1], 2), tick, &mut [&mut a, &mut b]);
        step(&mut s, &mut [&mut a, &mut b], 300);
        s.silence_multi(&mut [&mut a, &mut b]);
        c.silence();
        for (name, r) in [("route 0", &a), ("route 1", &b), ("route 2", &c)] {
            assert!(balanced(r), "{name} left a note sounding");
        }
        assert!(
            s.stats().resets_sent > 0,
            "the reset cursors were exercised"
        );
    }

    /// A timed recorder: each message at (time of its block + its offset).
    #[derive(Default)]
    struct TimedRec {
        msgs: Vec<(u64, Msg)>,
        /// Time (frames since play) of the block being scheduled.
        block: u64,
        at: u32,
    }

    impl SynthSink for TimedRec {
        fn note_on(&mut self, c: u8, k: u8, v: u8) {
            self.msgs
                .push((self.block + u64::from(self.at), Msg::On(c, k, v)));
        }
        fn note_off(&mut self, c: u8, k: u8) {
            self.msgs
                .push((self.block + u64::from(self.at), Msg::Off(c, k)));
        }
        fn channel_message(&mut self, s: u8, a: u8, b: u8) {
            self.msgs
                .push((self.block + u64::from(self.at), Msg::Ch(s, a, b)));
        }
        fn silence(&mut self) {}
        fn reset(&mut self) {}
        fn timed(&self) -> bool {
            true
        }
        fn at_offset(&mut self, frames: u32) {
            self.at = frames;
        }
    }

    /// Plays `blocks` blocks of 64 frames into `r` (times continue across
    /// loop wraps, like the ring).
    fn run_timed(s: &mut Scheduler, r: &mut TimedRec, blocks: u64) {
        for b in 0..blocks {
            r.block = b * 64;
            r.at = 0;
            s.advance(64, r);
        }
    }

    /// v1.0-Launch L-B02: a timed sink (external output) learns the exact
    /// frame of every message inside its 64-frame block — notes 1, 2, 4, 6
    /// ticks apart (50 frames per tick) arrive at exactly their frames, not
    /// at their block's start — while an untimed sink (the synthesizer) is
    /// unchanged: block starts, no extra work.
    #[test]
    fn timed_sinks_receive_each_message_at_its_own_frame_in_the_block() {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        let starts = [0u64, 1, 3, 7, 13, 14, 15, 40];
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &starts
                    .iter()
                    .map(|&t| note(t, 1, 60 + t as u8, 100, 0))
                    .collect::<Vec<_>>(),
                false,
            )
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut t = TimedRec::default();
        s.seek(0, &mut t);
        run_timed(&mut s, &mut t, 60);
        let ons: Vec<u64> = t
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .map(|(f, _)| *f)
            .collect();
        assert_eq!(ons, starts.iter().map(|t| t * 50).collect::<Vec<_>>());
        let offs: Vec<u64> = t
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Off(..)))
            .map(|(f, _)| *f)
            .collect();
        assert_eq!(
            offs,
            starts.iter().map(|t| (t + 1) * 50).collect::<Vec<_>>()
        );
        // The synthesizer path is unchanged: block starts.
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 60);
        let ons: Vec<u64> = ons_frames(&r);
        assert_eq!(
            ons,
            starts.iter().map(|t| t * 50 / 64 * 64).collect::<Vec<_>>()
        );
    }

    fn ons_frames(r: &Rec) -> Vec<u64> {
        r.msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .map(|(f, _)| *f)
            .collect()
    }

    /// L-B02: a loop wrap inside a block puts the wrap's note-offs and the
    /// re-struck / restarted notes at the loop end's offset, so a short
    /// loop repeats with its exact period on a timed sink (500 frames).
    #[test]
    fn timed_sinks_keep_the_exact_period_of_a_loop_that_wraps_inside_a_block() {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(0, 4, 60, 100, 0)], false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        s.set_loop(Some((0, 10)));
        let mut t = TimedRec::default();
        s.seek(0, &mut t);
        run_timed(&mut s, &mut t, 40);
        let ons: Vec<u64> = t
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::On(..)))
            .map(|(f, _)| *f)
            .collect();
        assert!(ons.len() >= 5, "{ons:?}");
        for (i, f) in ons.iter().enumerate() {
            assert_eq!(*f, i as u64 * 500, "start {i}: {ons:?}");
        }
    }

    /// v1.0-Launch L-B04: the per-block note-on budget and the lateness
    /// bound protect the internal synthesizer only. 6,000 notes at tick 0
    /// on an external route are all sent in the first block — none deferred,
    /// none skipped — while the same burst on the synthesizer is spread over
    /// blocks as before; a mixed project budgets only its route-0 notes.
    #[test]
    fn the_synthesizer_budget_never_defers_or_drops_external_notes() {
        let mut d = PlumDocument::new_project("b".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        let burst: Vec<NoteRecord> = (0..6_000usize)
            .map(|i| note(0, 480, (i % 128) as u8, 100, ((i / 128) % 16) as u8))
            .collect();
        for t in 0..2 {
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&burst, false)
                .unwrap();
        }
        let limits = SchedulerLimits {
            max_note_ons_per_block: 256,
            max_lateness_frames: 64,
            ..SchedulerLimits::default()
        };
        let ons = |r: &Rec| {
            r.msgs
                .iter()
                .filter(|(_, m)| matches!(m, Msg::On(..)))
                .count()
        };
        // Both tracks external (route 1): everything in the first block.
        let mut s = Scheduler::new(routed(&d, &[1, 1], 2), limits);
        let (mut a, mut b) = (Rec::default(), Rec::default());
        s.seek_multi(0, &mut [&mut a, &mut b]);
        s.advance_multi(64, &mut [&mut a, &mut b]);
        assert_eq!(ons(&b), 12_000, "all external notes in the first block");
        assert_eq!(ons(&a), 0);
        let st = s.stats();
        assert_eq!(
            (st.note_ons_deferred, st.note_ons_skipped, st.late_dropped),
            (0, 0, 0)
        );
        // Track 0 internal, track 1 external: only route 0 is budgeted.
        let mut s = Scheduler::new(routed(&d, &[0, 1], 2), limits);
        let (mut a, mut b) = (Rec::default(), Rec::default());
        s.seek_multi(0, &mut [&mut a, &mut b]);
        for _ in 0..200 {
            s.advance_multi(64, &mut [&mut a, &mut b]);
        }
        assert_eq!(ons(&b), 6_000, "every external note sent");
        let st = s.stats();
        assert!(
            st.note_ons_skipped > 0,
            "the synthesizer's burst is over budget and late: {st:?}"
        );
        assert_eq!(
            ons(&a) as u64 + st.note_ons_skipped,
            6_000,
            "route 0: sent + counted skips = its notes"
        );
    }

    /// L-A01: an index whose route count differs from the scheduler's is
    /// accepted by every entry point that positions playback — the update
    /// itself, a later seek, a resume without re-strike and Mute / Solo
    /// changes — with fewer sinks than routes too (a route's messages are
    /// then not delivered anywhere; nothing indexes past the end).
    #[test]
    fn every_positioning_entry_point_accepts_a_new_route_count() {
        let d = three_route_doc();
        for limits in [
            SchedulerLimits::default(),
            policy(ResetPolicy::Send),
            policy(ResetPolicy::SendChanged),
        ] {
            for (from, to) in [(1usize, 2usize), (2, 1), (1, 4), (4, 2), (3, 3)] {
                let routes = |n: usize| -> Vec<u16> { (0..3).map(|t| (t % n) as u16).collect() };
                let mut s = Scheduler::new(routed(&d, &routes(from), from), limits);
                let mut recs: Vec<Rec> = (0..4).map(|_| Rec::default()).collect();
                let mut sinks: Vec<&mut dyn SynthSink> =
                    recs.iter_mut().map(|r| r as &mut dyn SynthSink).collect();
                s.seek_multi(0, &mut sinks[..from]);
                for _ in 0..100 {
                    s.advance_multi(64, &mut sinks[..from]);
                }
                let tick = s.tick();
                s.set_index_multi(routed(&d, &routes(to), to), tick, &mut sinks[..to]);
                for _ in 0..100 {
                    s.advance_multi(64, &mut sinks[..to]);
                }
                s.seek_multi_with(3_000, &mut sinks[..to], false);
                let mask: Arc<[bool]> = vec![true, false, true].into();
                s.set_eligible_multi(mask, &mut sinks[..to]);
                // Fewer sinks than routes: nothing indexes past the sinks.
                for _ in 0..100 {
                    s.advance_multi(64, &mut sinks[..1]);
                }
                s.seek_multi(6_000, &mut sinks[..1]);
                s.silence_multi(&mut sinks);
            }
        }
    }

    /// v0.8 §132 / §229: a burst larger than the per-block budget is spread
    /// over the next blocks (deferred, in order) instead of being dropped;
    /// only notes beyond the lateness bound are skipped, and counted.
    #[test]
    fn bursts_beyond_the_budget_are_deferred_not_dropped() {
        // 6,000 notes at tick 0 (16 channels × 128 keys, duplicates), 1 beat long.
        let mut d = PlumDocument::new_project("b".into(), Bpm::DEFAULT, 480);
        let mut v: Vec<NoteRecord> = (0..6_000usize)
            .map(|i| note(0, 480, (i % 128) as u8, 100, ((i / 128) % 16) as u8))
            .collect();
        // A program change at tick 1 must still follow every deferred note-on.
        v.push(note(1, 10, 60, 100, 3));
        v.sort_by_key(|n| n.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        d.track_mut(0)
            .unwrap()
            .events
            .push(RawEvent {
                tick: 1,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xC3,
                    data: [9, 0],
                },
            })
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let limits = SchedulerLimits::default();
        let mut s = Scheduler::new(Arc::clone(&idx), limits);
        let mut r = Rec::default();
        s.seek(0, &mut r);
        let mut per_block = Vec::new();
        for b in 0..2_000u64 {
            r.frame = b * 64;
            let before = r.msgs.iter().filter(|m| matches!(m.1, Msg::On(..))).count();
            s.advance(64, &mut r);
            per_block.push(r.msgs.iter().filter(|m| matches!(m.1, Msg::On(..))).count() - before);
        }
        let st = s.stats();
        assert_eq!(st.note_ons, 6_001, "nothing lost");
        assert_eq!(st.note_ons_skipped, 0);
        assert!(
            per_block
                .iter()
                .all(|&n| n <= limits.max_note_ons_per_block)
        );
        assert_eq!(per_block[0], limits.max_note_ons_per_block);
        assert!(st.note_ons_deferred >= 6_001 - limits.max_note_ons_per_block as u64);
        assert!(st.max_lateness <= limits.max_lateness_frames);
        // Order kept: the program change (tick 1) comes after all 6,000
        // tick-0 note-ons, however many blocks they were spread over.
        let pc = r
            .msgs
            .iter()
            .position(|m| m.1 == Msg::Ch(0xC3, 9, 0))
            .unwrap();
        let ons_before = r.msgs[..pc]
            .iter()
            .filter(|m| matches!(m.1, Msg::On(..)))
            .count();
        assert_eq!(ons_before, 6_000);
        // Every started note ends.
        assert_eq!(st.note_offs, st.note_ons);
        assert_eq!(s.pending_offs(), 0);
    }

    #[test]
    fn sustained_overload_drops_only_late_notes_and_counts_them() {
        // 10,000 notes every 2 ticks (100 frames): far above 2,048 per 64 frames.
        let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 480);
        let mut v = Vec::new();
        for step in 0..40u64 {
            for i in 0..10_000usize {
                v.push(note(
                    step * 2,
                    4,
                    (i % 128) as u8,
                    90,
                    ((i / 128) % 16) as u8,
                ));
            }
        }
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let limits = SchedulerLimits::default();
        let mut s = Scheduler::new(idx, limits);
        let mut r = Rec::default();
        s.seek(0, &mut r);
        for b in 0..3_000u64 {
            r.frame = b * 64;
            s.advance(64, &mut r);
        }
        let st = s.stats();
        assert_eq!(st.note_ons + st.note_ons_skipped, 400_000);
        assert!(st.late_dropped > 0, "overload is reported");
        assert_eq!(st.late_dropped, st.note_ons_skipped);
        assert!(st.max_lateness <= limits.max_lateness_frames);
        assert_eq!(st.note_offs, st.note_ons, "nothing stuck");
        // The v0.7 limits skip at once (no deferral).
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::V07);
        let mut r = Rec::default();
        s.seek(0, &mut r);
        for b in 0..3_000u64 {
            r.frame = b * 64;
            s.advance(64, &mut r);
        }
        assert_eq!(s.stats().note_ons_deferred, 0);
        assert!(s.stats().note_ons_skipped > st.note_ons_skipped);
    }

    /// §146: note timing follows the canonical tempo map at extreme BPM
    /// (step jumps from 2 to 100,000 BPM and a ramp), and a pause / seek /
    /// resume continues on the same map: every note-on lands in the 64-frame
    /// block that contains its exact frame.
    #[test]
    fn timing_follows_the_tempo_map_at_extreme_bpm_and_across_seeks() {
        use crate::model::conductor::Interp;
        use crate::model::tempo::TempoValue;
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        let v: Vec<NoteRecord> = (0..40u64)
            .map(|i| note(i * 240, 120, 60 + (i % 12) as u8, 90, 0))
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        for (tick, bpm, interp) in [
            (0u64, "2", Interp::Step),
            (480, "100000", Interp::Step),
            (2_400, "120", Interp::Linear),
            (4_800, "900", Interp::Step),
            (7_200, "0.5", Interp::Step),
        ] {
            let e = d
                .conductor()
                .prepare_set_tempo(
                    None,
                    tick,
                    TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                    interp,
                )
                .unwrap();
            d.conductor_mut().apply(&e).unwrap();
        }
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let tempo = Arc::clone(&idx.tempo);
        let ons = |s: &mut Scheduler, r: &mut Rec, blocks: u64| {
            for _ in 0..blocks {
                r.frame = s.frame();
                s.advance(64, r);
            }
        };
        let starts: Vec<u64> = v.iter().map(|n| n.start_tick).collect();
        let check = |r: &Rec, from_tick: u64| {
            let on_frames: Vec<u64> = r
                .msgs
                .iter()
                .filter(|m| matches!(m.1, Msg::On(..)))
                .map(|m| m.0)
                .collect();
            let expected: Vec<u64> = starts
                .iter()
                .filter(|&&t| {
                    t >= from_tick
                        && tempo.frame_at_tick(t) < 30 * 48_000 + tempo.frame_at_tick(from_tick)
                })
                .map(|&t| tempo.frame_at_tick(t))
                .collect();
            assert_eq!(on_frames.len(), expected.len(), "notes from {from_tick}");
            for (got, want) in on_frames.iter().zip(&expected) {
                assert!(
                    *got <= *want && want - got < 64,
                    "on at {got}, exact frame {want}"
                );
            }
        };
        // Continuous from the start (30 s of audio).
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        ons(&mut s, &mut r, 30 * 48_000 / 64);
        check(&r, 0);
        // Pause and resume / seek: continue at tick 2,640 (inside the ramp).
        let mut s2 = Scheduler::new(idx, SchedulerLimits::default());
        let mut r2 = Rec::default();
        s2.seek(2_640, &mut r2);
        assert_eq!(s2.frame(), tempo.frame_at_tick(2_640));
        r2.msgs.clear();
        ons(&mut s2, &mut r2, 30 * 48_000 / 64);
        check(&r2, 2_640);
    }

    /// v0.9 256-Key Mode (S21 step 4a): keys above 127 — as stored or after a
    /// key correction — never reach a sink, live or when a seek re-strikes
    /// sounding notes; key 60 still plays; the skipped notes are counted.
    #[test]
    fn keys_above_127_are_never_sent_to_a_synthesizer_or_output() {
        use crate::model::project::KeyRange;
        use crate::model::track::TrackProps;
        let wide = |s: u64, d: u64, p: u8| NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new_wide(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        };
        let mut d = PlumDocument::new_project("w".into(), Bpm::DEFAULT, 480);
        d.project_mut().keys = KeyRange::Extended256;
        d.add_track().unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[wide(0, 960, 60), wide(0, 960, 200), wide(480, 960, 255)],
                false,
            )
            .unwrap();
        // Track 1: +100 semitones moves 100 to 200 (kept in a 256-key project).
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&[wide(0, 960, 20), wide(0, 960, 100)], false)
            .unwrap();
        let props = TrackProps {
            key_correction: 100,
            ..TrackProps::default()
        };
        d.track_mut(1).unwrap().set_props(props, None);
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2000);
        let keys = |r: &Rec| -> Vec<u8> {
            r.msgs
                .iter()
                .filter_map(|(_, m)| match m {
                    Msg::On(_, k, _) | Msg::Off(_, k) => Some(*k),
                    _ => None,
                })
                .collect()
        };
        let mut played = keys(&r);
        played.sort_unstable();
        played.dedup();
        assert_eq!(
            played,
            vec![60, 120],
            "60 and 20 + 100 play; 200, 255 and 100 + 100 never"
        );
        assert_eq!(s.stats().above_midi, 3);
        // A seek into the middle re-strikes 60 and 120 only.
        let mut r = Rec::default();
        s.seek(600, &mut r);
        let mut struck = keys(&r);
        struck.sort_unstable();
        assert_eq!(struck, vec![60, 120]);
        assert!(keys(&r).iter().all(|&k| k <= 127));
    }

    // ---- v1.0-beta2 B2-F12A / B2-F12C: playback-only event processing ----

    fn processed(p: EventProcessing) -> SchedulerLimits {
        SchedulerLimits {
            processing: p,
            ..SchedulerLimits::default()
        }
    }

    /// Every Note On has its own Note Off on (channel, key), never before it
    /// (Silence / Reset end everything sounding).
    fn balanced(r: &Rec) -> bool {
        let mut open = std::collections::HashMap::<(u8, u8), i64>::new();
        for (_, m) in &r.msgs {
            match m {
                Msg::Silence | Msg::Reset => open.clear(),
                Msg::On(c, k, _) => *open.entry((*c, *k)).or_default() += 1,
                Msg::Off(c, k) => {
                    let n = open.entry((*c, *k)).or_default();
                    if *n == 0 {
                        return false;
                    }
                    *n -= 1;
                }
                _ => {}
            }
        }
        open.values().all(|n| *n == 0)
    }

    fn keys_of(r: &Rec) -> Vec<(u8, u8)> {
        let mut v: Vec<(u8, u8)> = r
            .msgs
            .iter()
            .filter_map(|(_, m)| match m {
                Msg::On(c, k, _) | Msg::Off(c, k) => Some((*c, *k)),
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Options that match nothing deliver exactly what no processing does.
    #[test]
    fn event_processing_that_matches_nothing_changes_nothing() {
        let d = doc(); // keys 60 / 64 on channel 1, 36 on the drum channel
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let play = |p: EventProcessing| {
            let mut s = Scheduler::new(Arc::clone(&idx), processed(p));
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 400);
            s.seek(700, &mut r);
            run(&mut s, &mut r, 400);
            (r.msgs, s.stats())
        };
        let (plain, st) = play(EventProcessing::default());
        assert!(EventProcessing::default().is_off());
        for p in [
            EventProcessing {
                keys_88: true,
                velocity_band: None,
                ..Default::default()
            },
            EventProcessing {
                keys_88: false,
                velocity_band: Some((0, 0)),
                ..Default::default()
            },
            EventProcessing {
                keys_88: true,
                velocity_band: Some((101, 126)),
                ..Default::default()
            },
        ] {
            let (got, gst) = play(p);
            assert_eq!(got, plain, "{p:?}");
            assert_eq!(gst, st, "{p:?}");
        }
    }

    /// B2-F12A: keys outside 21 – 108 are never sent — neither their Note On
    /// nor their Note Off, also when re-struck after a seek or a loop wrap —
    /// except on the drum channel; overlapping notes on one kept key keep
    /// their own ends.
    #[test]
    fn the_88_keys_option_keeps_notes_outside_21_to_108_from_the_outputs_except_drums() {
        let mut d = PlumDocument::new_project("k".into(), Bpm::DEFAULT, 480);
        let notes = vec![
            note(0, 960, 20, 90, 0),
            note(0, 960, 21, 90, 0),
            note(0, 960, 108, 90, 0),
            note(0, 960, 109, 90, 0),
            note(240, 960, 110, 90, 0),
            note(480, 240, 110, 90, 0),
            note(0, 960, 15, 90, 9),
            note(0, 480, 60, 90, 0),
            note(240, 960, 60, 90, 0),
        ];
        let mut sorted = notes.clone();
        sorted.sort_by_key(|n| n.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&sorted, false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let p = EventProcessing {
            keys_88: true,
            velocity_band: None,
            ..Default::default()
        };
        let mut s = Scheduler::new(Arc::clone(&idx), processed(p));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2_000);
        assert_eq!(keys_of(&r), vec![(0, 21), (0, 60), (0, 108), (9, 15)]);
        assert!(balanced(&r));
        assert_eq!(s.stats().filtered_keys, 4);
        assert_eq!(s.stats().filtered_velocity, 0);
        // Seek into the middle (re-strike) and loop over it.
        // Without processing the same journey balances too (the check).
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.set_loop(Some((300, 700)));
        s.seek(500, &mut r);
        run(&mut s, &mut r, 3_000);
        s.silence(&mut r);
        assert!(balanced(&r));
        let mut s = Scheduler::new(Arc::clone(&idx), processed(p));
        let mut r = Rec::default();
        s.set_loop(Some((300, 700)));
        s.seek(500, &mut r);
        run(&mut s, &mut r, 3_000);
        s.silence(&mut r);
        assert!(
            keys_of(&r)
                .iter()
                .all(|&(c, k)| c == 9 || (21..=108).contains(&k)),
            "{:?}",
            keys_of(&r)
        );
        assert!(balanced(&r), "nothing sticks across seeks and wraps");
        assert!(s.stats().loop_wraps > 0 && s.stats().filtered_keys > 0);
        // The 256-key model is untouched: the document still holds every note.
        assert_eq!(d.track(0).unwrap().notes.len(), notes.len());
    }

    /// B2-F12C: notes whose velocity is in the inclusive band are not sent,
    /// with their Note Offs; a kept note on the same key keeps its own end;
    /// the band's edges are inclusive; invalid bands are refused in words.
    #[test]
    fn the_velocity_band_keeps_its_notes_and_their_note_offs_from_the_outputs() {
        let mut d = PlumDocument::new_project("v".into(), Bpm::DEFAULT, 480);
        let mut notes = vec![
            note(0, 480, 40, 1, 0),
            note(0, 480, 41, 10, 0),
            note(0, 480, 42, 11, 0),
            note(0, 480, 43, 127, 0),
            // Same key: the quiet one is ignored, the loud one ends on time.
            note(0, 960, 60, 5, 0),
            note(240, 240, 60, 100, 0),
        ];
        notes.sort_by_key(|n| n.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let band = |lo, hi| EventProcessing {
            keys_88: false,
            velocity_band: Some((lo, hi)),
            ..Default::default()
        };
        let mut s = Scheduler::new(Arc::clone(&idx), processed(band(1, 10)));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2_000);
        assert_eq!(keys_of(&r), vec![(0, 42), (0, 43), (0, 60)]);
        assert!(balanced(&r));
        assert_eq!(s.stats().filtered_velocity, 3);
        let on60: Vec<u64> = r
            .msgs
            .iter()
            .filter_map(|(f, m)| matches!(m, Msg::On(0, 60, 100)).then_some(*f))
            .collect();
        let off60: Vec<u64> = r
            .msgs
            .iter()
            .filter_map(|(f, m)| matches!(m, Msg::Off(0, 60)).then_some(*f))
            .collect();
        assert_eq!(on60.len(), 1);
        assert_eq!(off60.len(), 1, "only the kept note's Note Off");
        assert!(
            off60[0] >= 480 * 50 - 64 && off60[0] <= 480 * 50 + 64,
            "{off60:?}: at tick 480"
        );
        // The top edge alone.
        let mut s = Scheduler::new(Arc::clone(&idx), processed(band(127, 127)));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 2_000);
        assert!(!keys_of(&r).contains(&(0, 43)) && keys_of(&r).contains(&(0, 40)));
        assert_eq!(s.stats().filtered_velocity, 1);
        // Live: switched on while playing, notes already sounding still end.
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 100);
        s.set_limits(processed(band(1, 127)));
        run(&mut s, &mut r, 2_000);
        assert!(balanced(&r), "a change while playing never strands a note");
        assert_eq!(EventProcessing::band_problem(0, 127), None);
        assert_eq!(
            EventProcessing::band_problem(10, 5).as_deref(),
            Some("The lowest velocity (10) must not be above the highest (5).")
        );
        assert_eq!(
            EventProcessing::band_problem(0, 128).as_deref(),
            Some("Velocities go from 0 to 127.")
        );
    }

    /// B2-F12: unpaired note messages of a track (no stored note) follow the
    /// same rules: a kept Note On keeps the next Note Off on its key (a Note
    /// On of velocity 0 is a Note Off) and nothing else.
    #[test]
    fn unpaired_note_messages_keep_their_pairs_under_event_processing() {
        let mut d = PlumDocument::new_project("u".into(), Bpm::DEFAULT, 480);
        let ev = &mut d.track_mut(0).unwrap().events;
        for (tick, status, a, b) in [
            (0u64, 0x90u8, 50u8, 5u8),
            (10, 0x90, 50, 100),
            (20, 0x80, 50, 0),
            (30, 0x90, 50, 0),
            (40, 0x90, 5, 100),
            (50, 0x80, 5, 0),
            (60, 0x99, 5, 100),
            (70, 0x89, 5, 0),
        ] {
            ev.push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Channel {
                    status,
                    data: [a, b],
                },
            })
            .unwrap();
        }
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(
            idx,
            processed(EventProcessing {
                keys_88: true,
                velocity_band: Some((1, 10)),
                ..Default::default()
            }),
        );
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 200);
        let notes: Vec<String> = r
            .msgs
            .iter()
            .filter_map(|(_, m)| match m {
                Msg::On(c, k, v) => Some(format!("on {c} {k} {v}")),
                Msg::Off(c, k) => Some(format!("off {c} {k}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            ["on 0 50 100", "off 0 50", "on 9 5 100", "off 9 5"],
            "the quiet On and the Off after it are kept back; key 5 only on drums"
        );
        assert_eq!(s.stats().filtered_velocity, 1);
        assert_eq!(s.stats().filtered_keys, 1);
    }

    // ---- v1.0-beta2 B2-F12D / B2-F12E: length override, Note Off delay ----

    fn timed(len_ms: Option<u64>, delay_ms: Option<u64>) -> SchedulerLimits {
        processed(EventProcessing {
            length_override_ns: len_ms.map(|m| m * 1_000_000),
            off_delay_ns: delay_ms.map(|m| m * 1_000_000),
            ..EventProcessing::default()
        })
    }

    /// (on frame, off frame) of every note on `key` in order.
    fn spans_of(r: &Rec, key: u8) -> Vec<(u64, u64)> {
        let mut open: Vec<u64> = Vec::new();
        let mut out = Vec::new();
        for (f, m) in &r.msgs {
            match m {
                Msg::On(_, k, _) if *k == key => open.push(*f),
                Msg::Off(_, k) if *k == key => {
                    if let Some(on) = open.pop() {
                        out.push((on, *f));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// A song at 120 BPM doubling to 240 BPM at tick 960 (480 PPQN, 48 kHz:
    /// 50 then 25 frames per tick), notes of different lengths on keys 60 –
    /// 63 and one across the tempo change on 64.
    fn timing_doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        let mut notes = vec![
            note(0, 10, 60, 90, 0),
            note(0, 480, 61, 90, 0),
            note(0, 960, 62, 90, 0),
            note(1_200, 2_000, 63, 90, 0),
            note(900, 100, 64, 90, 0),
        ];
        notes.sort_by_key(|n| n.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
        let edit = d
            .conductor()
            .prepare_set_tempo(
                None,
                960,
                crate::model::tempo::TempoValue::Bpm(Bpm::parse("240").unwrap()),
                crate::model::conductor::Interp::Step,
            )
            .unwrap();
        crate::history::History::default()
            .apply(
                &mut d,
                crate::history::Operation::Conductor {
                    edit,
                    label: "Tempo",
                },
            )
            .unwrap();
        d
    }

    /// B2-F12D: every sent note lasts the override in song time — whatever
    /// its own length and across a tempo change (through the tempo map) —
    /// and the original lengths are untouched.
    #[test]
    fn the_length_override_gives_every_sent_note_one_length_through_the_tempo_map() {
        let d = timing_doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let mut s = Scheduler::new(Arc::clone(&idx), timed(Some(250), None));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 3_000);
        assert!(balanced(&r));
        // 0.25 s = 12,000 frames, within a 64-frame block and one tick.
        for key in 60..=64u8 {
            let sp = spans_of(&r, key);
            assert_eq!(sp.len(), 1, "{key}");
            let len = sp[0].1 - sp[0].0;
            assert!(len.abs_diff(12_000) <= 64 + 50, "{key}: {len} frames");
        }
        // Without the override: the notes' own lengths (key 62: 960 ticks
        // at 50 frames = 48,000 frames).
        let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 3_000);
        let own = spans_of(&r, 62)[0];
        assert!((own.1 - own.0).abs_diff(48_000) <= 64);
        assert_eq!(
            d.track(0).unwrap().notes.len(),
            5,
            "the document keeps its notes"
        );
    }

    /// B2-F12E: Note Offs come the delay later — after the note's own end, or
    /// after the override's end when both are on; 0 changes nothing.
    #[test]
    fn the_note_off_delay_comes_after_the_length_and_the_override() {
        let d = timing_doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let play = |l: SchedulerLimits| {
            let mut s = Scheduler::new(Arc::clone(&idx), l);
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 3_000);
            assert!(balanced(&r));
            r
        };
        let plain = play(SchedulerLimits::default());
        let delayed = play(timed(None, Some(100)));
        for key in 60..=64u8 {
            let (a, b) = (spans_of(&plain, key)[0], spans_of(&delayed, key)[0]);
            assert_eq!(a.0, b.0, "{key}: same start");
            let extra = (b.1 - b.0) as i64 - (a.1 - a.0) as i64;
            assert!((extra - 4_800).abs() <= 64 + 50, "{key}: {extra}");
        }
        let both = play(timed(Some(250), Some(100)));
        for key in 60..=64u8 {
            let sp = spans_of(&both, key)[0];
            assert!((sp.1 - sp.0).abs_diff(16_800) <= 64 + 50, "{key}");
        }
        assert_eq!(
            play(timed(None, Some(0))).msgs,
            plain.msgs,
            "a 0 delay is no delay"
        );
    }

    /// B2-F12D / E same-key rule: while the override or the delay holds a
    /// note, a newer note on its channel and key ends it first (its own
    /// later Note Off is skipped) — the newer note always sounds its full
    /// time; without the options overlapping notes behave as before.
    #[test]
    fn a_held_note_ends_when_its_key_is_struck_again() {
        let mut d = PlumDocument::new_project("k".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(0, 240, 60, 90, 0), note(480, 240, 60, 90, 0)], false)
            .unwrap();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        // 1 s = 960 ticks: the first note would still sound at 480.
        let mut s = Scheduler::new(Arc::clone(&idx), timed(Some(1_000), None));
        let mut r = Rec::default();
        s.seek(0, &mut r);
        run(&mut s, &mut r, 3_000);
        assert!(balanced(&r));
        let sp = spans_of(&r, 60);
        assert_eq!(sp.len(), 2);
        assert!(
            sp[0].1.abs_diff(480 * 50) <= 64,
            "ended when struck again: {sp:?}"
        );
        assert!(
            (sp[1].1 - sp[1].0).abs_diff(48_000) <= 64 + 50,
            "the newer note's full second"
        );
        assert_eq!(s.stats().retriggered, 1);
        let offs = r
            .msgs
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Off(0, 60)))
            .count();
        assert_eq!(offs, 2, "no extra Note Off");
    }

    /// Seek and pause: the notes struck at a position are exactly the ones
    /// playing through to it leaves sounding — notes held past their end by
    /// the override / delay included, notes shortened by it excluded, one per
    /// key; a loop repeats the same messages every time round.
    #[test]
    fn seeks_and_loops_are_deterministic_with_the_override_and_the_delay() {
        let d = timing_doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        for limits in [
            timed(Some(1_500), None),
            timed(Some(5), None),
            timed(None, Some(700)),
            timed(Some(300), Some(400)),
        ] {
            for at in [100u64, 500, 970, 1_300, 2_000] {
                // Playing through to `at`: what is sounding there.
                let mut s = Scheduler::new(Arc::clone(&idx), limits);
                let mut r = Rec::default();
                s.seek(0, &mut r);
                while s.tick() < at {
                    r.frame = s.frame();
                    s.advance(1, &mut r);
                }
                let mut held: std::collections::HashMap<u8, i64> = Default::default();
                for (_, m) in &r.msgs {
                    match m {
                        Msg::On(_, k, _) => *held.entry(*k).or_default() += 1,
                        Msg::Off(_, k) => *held.entry(*k).or_default() -= 1,
                        _ => {}
                    }
                }
                let mut through: Vec<u8> = held
                    .into_iter()
                    .filter(|&(_, n)| n > 0)
                    .map(|(k, _)| k)
                    .collect();
                through.sort_unstable();
                // A seek to `at` strikes the same keys.
                let mut s = Scheduler::new(Arc::clone(&idx), limits);
                let mut r = Rec::default();
                s.seek(at, &mut r);
                let mut struck: Vec<u8> = r
                    .msgs
                    .iter()
                    .filter_map(|(_, m)| match m {
                        Msg::On(_, k, _) => Some(*k),
                        _ => None,
                    })
                    .collect();
                struck.sort_unstable();
                // Advancing one frame at a time crosses at most a tick, so a
                // note starting exactly at `at` may already be on.
                let starting: Vec<u8> = (60..=64)
                    .filter(|k| {
                        d.track(0)
                            .unwrap()
                            .notes
                            .iter()
                            .any(|n| n.attrs.pitch() == *k && n.start_tick == at)
                    })
                    .collect();
                through.retain(|k| !starting.contains(k));
                assert_eq!(struck, through, "{limits:?} at {at}");
                // The cases this covers: a 10-tick note held a second and a
                // half by the override; two long notes cut to 5 ms by it.
                if limits.processing.length_override_ns == Some(1_500_000_000) && at == 500 {
                    assert!(struck.contains(&60), "{struck:?}");
                }
                if limits.processing.length_override_ns == Some(5_000_000) && at == 100 {
                    assert!(!struck.contains(&61) && !struck.contains(&62), "{struck:?}");
                }
            }
            // Loop: every time round the same messages, nothing left on.
            let mut s = Scheduler::new(Arc::clone(&idx), limits);
            let mut r = Rec::default();
            s.set_loop(Some((0, 1_200)));
            s.seek(0, &mut r);
            let mut rounds: Vec<Vec<String>> = Vec::new();
            for _ in 0..3 {
                let mut round = Vec::new();
                let wraps = s.stats().loop_wraps;
                while s.stats().loop_wraps == wraps {
                    r.msgs.clear();
                    r.frame = s.frame();
                    s.advance(64, &mut r);
                    round.extend(r.msgs.iter().map(|(_, m)| format!("{m:?}")));
                }
                rounds.push(round);
            }
            assert_eq!(rounds[1], rounds[2], "{limits:?}: each round the same");
            s.silence(&mut r);
        }
    }

    // ---- v1.0-beta2 B2-F12B: the song's system resets ----

    const GS_RESET: [u8; 10] = [0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7];
    const XG_ON: [u8; 8] = [0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7];

    /// System resets are told apart by their bytes; nothing else counts.
    #[test]
    fn system_resets_are_classified_by_their_bytes() {
        use SystemMode::*;
        for (bytes, want) in [
            (&[0x7E, 0x7F, 0x09, 0x01, 0xF7][..], Some(GmOn)),
            (&[0x7E, 0x10, 0x09, 0x01][..], Some(GmOn)),
            (&[0x7E, 0x7F, 0x09, 0x02, 0xF7][..], Some(GmOff)),
            (&[0x7E, 0x7F, 0x09, 0x03, 0xF7][..], Some(Gm2On)),
            (&GS_RESET[..], Some(Gs)),
            (
                &[0x41, 0x10, 0x42, 0x12, 0x00, 0x00, 0x7F, 0x00, 0x01, 0xF7][..],
                Some(Gs),
            ),
            (
                &[0x41, 0x10, 0x42, 0x12, 0x00, 0x00, 0x7F, 0x01, 0x00, 0xF7][..],
                Some(Gs),
            ),
            (
                &[0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x42, 0xF7][..],
                None,
            ),
            (&XG_ON[..], Some(Xg)),
            (&[0x43, 0x20, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7][..], None),
            (&[0x7F, 0x7F, 0x04, 0x01, 0x00, 0x7F, 0xF7][..], None),
            (&[0x7E, 0x7F, 0x09][..], None),
            (&[][..], None),
        ] {
            assert_eq!(classify_reset(bytes), want, "{bytes:02X?}");
        }
        assert_eq!(
            ResetPolicy::from_key("send_changed"),
            Some(ResetPolicy::SendChanged)
        );
        assert_eq!(ResetPolicy::from_key("never"), None);
        assert_eq!(ResetPolicy::default(), ResetPolicy::Skip);
    }

    /// A song with a GS reset at 0 and 200, an XG reset at 300 and 400,
    /// program 11 on channel 1 at 0 (after the first reset) and at 100,
    /// program 21 at 250 — and notes so it plays.
    fn reset_doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("r".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(0, 600, 60, 90, 0)], false)
            .unwrap();
        let ev = &mut d.track_mut(0).unwrap().events;
        let sysex = |tick: u64, bytes: &[u8], ev: &mut crate::model::events::EventStore| {
            let payload = ev.store_payload(bytes).unwrap();
            ev.push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::SysEx { payload },
            })
            .unwrap();
        };
        let ch = |tick: u64, st: u8, a: u8, ev: &mut crate::model::events::EventStore| {
            ev.push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Channel {
                    status: st,
                    data: [a, 0],
                },
            })
            .unwrap();
        };
        sysex(0, &GS_RESET, ev);
        ch(0, 0xC0, 10, ev);
        ch(100, 0xC0, 11, ev);
        sysex(200, &GS_RESET, ev);
        ch(250, 0xC0, 20, ev);
        sysex(300, &XG_ON, ev);
        sysex(400, &XG_ON, ev);
        d
    }

    fn policy(p: ResetPolicy) -> SchedulerLimits {
        processed(EventProcessing {
            resets: p,
            ..EventProcessing::default()
        })
    }

    fn system_msgs(r: &Rec) -> Vec<(u64, Vec<u8>)> {
        r.msgs
            .iter()
            .filter_map(|(f, m)| match m {
                Msg::System(b) => Some((*f, b.clone())),
                _ => None,
            })
            .collect()
    }

    /// B2-F12B: by default no reset is sent (as before); `Send` sends every
    /// one as it is (before the channel events of its tick); `SendChanged`
    /// sends the first and every change of mode and ignores repeats.
    #[test]
    fn system_resets_are_skipped_by_default_and_sent_when_asked() {
        let d = reset_doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let play = |l: SchedulerLimits| {
            let mut s = Scheduler::new(Arc::clone(&idx), l);
            let mut r = Rec::default();
            s.seek(0, &mut r);
            run(&mut s, &mut r, 1_000);
            (r, s.stats())
        };
        let (skip, st) = play(SchedulerLimits::default());
        assert!(system_msgs(&skip).is_empty());
        assert_eq!((st.resets_sent, st.resets_ignored), (0, 0));
        let (send, st) = play(policy(ResetPolicy::Send));
        let sent = system_msgs(&send);
        assert_eq!(sent.len(), 4);
        let mut gs = vec![0xF0];
        gs.extend_from_slice(&GS_RESET);
        assert_eq!(sent[0].1, gs, "the message itself, F0 ... F7");
        assert_eq!((st.resets_sent, st.resets_ignored), (4, 0));
        // The reset at tick 0 comes before the program change at tick 0.
        let first = send
            .msgs
            .iter()
            .position(|(_, m)| matches!(m, Msg::System(_)))
            .unwrap();
        let prog = send
            .msgs
            .iter()
            .position(|(_, m)| *m == Msg::Ch(0xC0, 10, 0))
            .unwrap();
        assert!(first < prog);
        let (changed, st) = play(policy(ResetPolicy::SendChanged));
        assert_eq!(system_msgs(&changed).len(), 2, "GS at 0, XG at 300");
        assert_eq!((st.resets_sent, st.resets_ignored), (2, 2));
        // Notes and controllers are the same in every case.
        let rest = |r: &Rec| -> Vec<Msg> {
            r.msgs
                .iter()
                .filter(|(_, m)| !matches!(m, Msg::System(_)))
                .map(|(_, m)| m.clone())
                .collect()
        };
        assert_eq!(rest(&send), rest(&skip));
        assert_eq!(rest(&changed), rest(&skip));
    }

    /// B2-F12B, before / after state: a seek or a loop wrap restores the
    /// channel state the outputs have — a sent reset clears the programs set
    /// before it (`Send`), an ignored repeat keeps them (`SendChanged`, the
    /// stated risk), and without resets the state is as before.
    #[test]
    fn seeks_and_loops_restore_the_state_the_sent_resets_leave() {
        let d = reset_doc();
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let programs_after_seek = |l: SchedulerLimits, at: u64| -> Vec<u8> {
            let mut s = Scheduler::new(Arc::clone(&idx), l);
            let mut r = Rec::default();
            s.seek(at, &mut r);
            r.msgs
                .iter()
                .filter_map(|(_, m)| match m {
                    Msg::Ch(0xC0, p, _) => Some(*p),
                    _ => None,
                })
                .collect()
        };
        // At 220: Skip keeps program 11; Send's GS reset at 200 cleared it;
        // SendChanged ignored that repeat, so 11 stays.
        assert_eq!(
            programs_after_seek(SchedulerLimits::default(), 220),
            vec![11]
        );
        assert_eq!(
            programs_after_seek(policy(ResetPolicy::Send), 220),
            Vec::<u8>::new()
        );
        assert_eq!(
            programs_after_seek(policy(ResetPolicy::SendChanged), 220),
            vec![11]
        );
        // At 350: the XG reset at 300 is a change, sent by both: program 20
        // (set at 250) is gone; without resets it stays.
        assert_eq!(
            programs_after_seek(SchedulerLimits::default(), 350),
            vec![20]
        );
        assert_eq!(
            programs_after_seek(policy(ResetPolicy::Send), 350),
            Vec::<u8>::new()
        );
        assert_eq!(
            programs_after_seek(policy(ResetPolicy::SendChanged), 350),
            Vec::<u8>::new()
        );
        let c = &idx.controls[0];
        assert_eq!(c.mode_before(350), Some(SystemMode::Xg));
        assert_eq!(
            c.last_sent_reset_before(250, ResetPolicy::SendChanged),
            Some(0)
        );
        assert_eq!(c.last_sent_reset_before(250, ResetPolicy::Send), Some(200));
        // A loop from 350 back to 150 moves the channel to its state at 150
        // under each policy (program 11 there with Skip / SendChanged; with
        // Send too, as 150 is before the second reset) and repeats the
        // resets in the loop each time round.
        for p in [
            ResetPolicy::Skip,
            ResetPolicy::Send,
            ResetPolicy::SendChanged,
        ] {
            let mut s = Scheduler::new(Arc::clone(&idx), policy(p));
            let mut r = Rec::default();
            s.set_loop(Some((150, 350)));
            s.seek(150, &mut r);
            let wraps = |s: &Scheduler| s.stats().loop_wraps;
            while wraps(&s) < 1 {
                r.frame = s.frame();
                s.advance(64, &mut r);
            }
            let at_wrap = r.msgs.len();
            while wraps(&s) < 2 {
                r.frame = s.frame();
                s.advance(64, &mut r);
            }
            let second: Vec<Msg> = r.msgs[at_wrap..].iter().map(|(_, m)| m.clone()).collect();
            assert!(
                second.contains(&Msg::Ch(0xC0, 11, 0)),
                "{p:?}: program 11 back at the loop start"
            );
            let resets = second
                .iter()
                .filter(|m| matches!(m, Msg::System(_)))
                .count();
            let want = match p {
                ResetPolicy::Skip => 0,
                ResetPolicy::Send => 2,
                ResetPolicy::SendChanged => 1,
            };
            assert_eq!(resets, want, "{p:?}");
        }
    }

    /// B2-F12 preflight: one pass over what playback hears — notes outside
    /// the 88 keys off the drum channel, notes in a band, resets and repeats;
    /// muted notes are not counted.
    #[test]
    fn the_preflight_counts_what_each_option_would_act_on() {
        let mut d = reset_doc(); // key 60 v90 at 0, GS GS XG XG
        let mut extra = vec![
            note(10, 10, 15, 5, 0),
            note(20, 10, 15, 5, 9),
            note(30, 10, 110, 127, 0),
        ];
        extra[2].attrs = extra[2].attrs.with_muted(true);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&extra, false)
            .unwrap();
        let p = EventProcessing::preflight(&d, (1, 10));
        assert_eq!(
            p,
            Preflight {
                notes: 3,
                outside_keys: 1,
                in_band: 2,
                resets: 4,
                repeated_resets: 2,
            }
        );
        assert_eq!(EventProcessing::preflight(&d, (90, 90)).in_band, 1);
    }
}

/// V09-S07: placements play exactly like the same music written into plain
/// tracks ("flattened"): moved, windowed, muted, removed and linked clips,
/// owned contents, track corrections, seeks, loops and Mute / Solo changes.
#[cfg(test)]
mod clip_tests {
    use super::*;
    use crate::model::ClipPlacement;
    use crate::model::clip::test_support::{clipped, flatten};
    use crate::model::track::TrackProps;

    #[derive(Default)]
    struct Rec {
        msgs: Vec<(u64, String)>,
        frame: u64,
    }

    impl SynthSink for Rec {
        fn note_on(&mut self, c: u8, k: u8, v: u8) {
            self.msgs.push((self.frame, format!("on {c} {k} {v}")));
        }
        fn note_off(&mut self, c: u8, k: u8) {
            self.msgs.push((self.frame, format!("off {c} {k}")));
        }
        fn channel_message(&mut self, s: u8, a: u8, b: u8) {
            self.msgs.push((self.frame, format!("ch {s:x} {a} {b}")));
        }
        fn silence(&mut self) {
            self.msgs.push((self.frame, "silence".into()));
        }
        fn reset(&mut self) {
            self.msgs.push((self.frame, "reset".into()));
        }
    }

    /// What plays, per 64-frame block, order-normalized within a block (two
    /// equal-tick notes may start in either order).
    fn run(d: &PlumDocument, script: &[(&str, u64)]) -> Vec<Vec<String>> {
        let idx = Arc::new(PlaybackIndex::build(d, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut rec = Rec::default();
        let mut blocks = Vec::new();
        for &(what, arg) in script {
            match what {
                "seek" => s.seek(arg, &mut rec),
                "loop" => s.set_loop(Some((arg, arg + 1900))),
                "mute1" => {
                    let e: Arc<[bool]> = vec![true, false, true].into();
                    s.set_eligible(e, &mut rec);
                }
                _ => {
                    for _ in 0..arg {
                        rec.frame = s.frame();
                        s.advance(64, &mut rec);
                        let mut b: Vec<String> = rec.msgs.drain(..).map(|m| m.1).collect();
                        b.sort();
                        blocks.push(b);
                    }
                }
            }
        }
        blocks
    }

    fn script() -> Vec<(&'static str, u64)> {
        // 120 BPM, 480 PPQN, 48 kHz: 50 frames per tick, 1.28 ticks a block.
        vec![
            ("play", 3000),
            ("seek", 2500),
            ("play", 1000),
            ("mute1", 0),
            ("play", 500),
            ("seek", 7300),
            ("play", 2500),
            ("loop", 9100),
            ("seek", 9000),
            ("play", 3500),
        ]
    }

    #[test]
    fn placements_play_exactly_like_the_flattened_music() {
        let d = clipped();
        let f = flatten(&d);
        assert!(f.clips_are_identity() && !d.clips_are_identity());
        let (a, b) = (run(&d, &script()), run(&f, &script()));
        assert_eq!(a.len(), b.len());
        let ons = a.iter().flatten().filter(|m| m.starts_with("on")).count();
        assert!(ons > 60, "{ons} notes played");
        assert_eq!(a, b);
        // The muted owned-content placement and the removed primary are silent:
        // fewer notes than the unclipped song would play in the same time.
        let plain = run(&crate::model::clip::test_support::base(), &script());
        assert_ne!(a, plain, "clips change what plays");
    }

    #[test]
    fn corrections_apply_after_placement() {
        let mut d = clipped();
        // Track 3 (hosting a linked instance) gets a tick, key and gate
        // correction from measure 2.
        let t = d.track_mut(2).unwrap();
        let end = t.end_of_track_tick;
        let props = TrackProps {
            tick_correction: -37,
            correction_from_measure: 2,
            key_correction: 5,
            gate: crate::model::track::GatePercent::from_hundredths(15_000).unwrap(),
            ..t.props().clone()
        };
        t.set_props(props, end);
        let f = flatten(&d);
        assert_eq!(run(&d, &script()), run(&f, &script()));
    }

    #[test]
    fn an_identity_table_builds_exactly_the_v08_index() {
        let d = crate::model::clip::test_support::base();
        let mut named = d.clone();
        // A named primary at the identity is a record, but plays the same.
        let t0 = named.track(0).unwrap().id();
        named.clips_mut().set_placement(ClipPlacement {
            name: Some("Intro".into()),
            ..ClipPlacement::primary(t0)
        });
        assert!(PlaybackIndex::build(&d, 48_000).lanes.is_empty());
        assert_eq!(run(&d, &script()), run(&named, &script()));
    }
}
