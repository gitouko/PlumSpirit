//! MIDI controller events: Pitch Bend and Control Change 0–127.
//!
//! Controllers are ordinary MIDI channel messages, and PlumSpirit edits them **in
//! place** in the track's preserved event list — the same list import fills and
//! export writes. There is no second, "editable" copy of these events, so an
//! edited event can never coexist with a stale raw duplicate, and an event that
//! was imported keeps its exact bytes and position until it is edited.
//!
//! * Pitch Bend: `En lsb msb`, a 14-bit value 0–16383, centre 8192. It is not a
//!   Control Change and keeps its full resolution.
//! * Control Change: `Bn cc vv`, controller 0–127, value 0–127. The controller
//!   number decides how a value is shown and edited ([`CcCategory`]):
//!   0–31 are the MSB halves of high-resolution controllers, 32–63 their LSB
//!   halves (both halves are kept as the two separate messages they are),
//!   64–69 are switches (0–63 off, 64–127 on; values are kept exactly as
//!   entered), 70–119 general and sound controllers, and 120–127 channel-mode
//!   messages (All Sound Off, Reset All Controllers, Local Control, All Notes
//!   Off, Omni/Mono/Poly), which are commands, never curves.
//!
//! Channels are the event's own (0–15 internally, shown as 1–16). Between two
//! messages the last value holds, which is how the graphs draw them.
//!
//! Same tick: adding an event where one of the same kind and channel already is
//! updates that event in place (keeping its position among the tick's events),
//! and moving an event onto another replaces it. Imported files can hold several
//! at one tick; the last one (in file order) is the one that sounds and the one
//! the graph shows and edits. New events go after the tick's other events that
//! precede every note, so a controller written at a note's start tick applies
//! before that note (in playback and in exported files).

use super::EditError;
use super::target::HostView;
use crate::model::clip::ContentRef;
use crate::model::events::{ArenaRange, EventStore, RawEvent, RawKind, meta};
use crate::model::patch::ListPatch;
use crate::model::project::GraphTarget;
use crate::model::track::Track;

pub const PITCH_BEND_MAX: u16 = 16_383;
pub const PITCH_BEND_CENTER: u16 = 8_192;
pub const CC_MAX: u16 = 127;
pub const PAN_MAX: u16 = 127;
pub const PAN_CENTER: u16 = 64;
/// Control Change number of Pan.
pub const CC_PAN: u8 = 10;

/// A controller stream: Pitch Bend, or one Control Change number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ControllerKind {
    PitchBend,
    /// Control Change `0..=127`.
    Cc(u8),
}

/// How a Control Change number behaves (MIDI 1.0 controller groups).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CcCategory {
    /// 0–31: the most significant half of a high-resolution controller.
    HighResMsb,
    /// 32–63: the least significant half of controller `n − 32`.
    HighResLsb,
    /// 64–69: pedals and switches (0–63 off, 64–127 on).
    Switch,
    /// 70–119: general and sound controllers.
    General,
    /// 120–127: channel-mode messages (commands with special meaning).
    ChannelMode,
}

impl CcCategory {
    pub fn of(cc: u8) -> CcCategory {
        match cc {
            0..=31 => CcCategory::HighResMsb,
            32..=63 => CcCategory::HighResLsb,
            64..=69 => CcCategory::Switch,
            70..=119 => CcCategory::General,
            _ => CcCategory::ChannelMode,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CcCategory::HighResMsb => "high-resolution controller (MSB)",
            CcCategory::HighResLsb => "high-resolution controller (LSB)",
            CcCategory::Switch => "switch",
            CcCategory::General => "general / sound controller",
            CcCategory::ChannelMode => "channel-mode message",
        }
    }
}

/// Standard names of Control Change 0–127 (MIDI 1.0 / General MIDI).
const CC_NAMES: [&str; 128] = [
    "Bank Select",
    "Modulation",
    "Breath Controller",
    "Controller 3",
    "Foot Controller",
    "Portamento Time",
    "Data Entry",
    "Volume",
    "Balance",
    "Controller 9",
    "Pan",
    "Expression",
    "Effect Control 1",
    "Effect Control 2",
    "Controller 14",
    "Controller 15",
    "General Purpose 1",
    "General Purpose 2",
    "General Purpose 3",
    "General Purpose 4",
    "Controller 20",
    "Controller 21",
    "Controller 22",
    "Controller 23",
    "Controller 24",
    "Controller 25",
    "Controller 26",
    "Controller 27",
    "Controller 28",
    "Controller 29",
    "Controller 30",
    "Controller 31",
    "Bank Select (LSB)",
    "Modulation (LSB)",
    "Breath Controller (LSB)",
    "Controller 3 (LSB)",
    "Foot Controller (LSB)",
    "Portamento Time (LSB)",
    "Data Entry (LSB)",
    "Volume (LSB)",
    "Balance (LSB)",
    "Controller 9 (LSB)",
    "Pan (LSB)",
    "Expression (LSB)",
    "Effect Control 1 (LSB)",
    "Effect Control 2 (LSB)",
    "Controller 14 (LSB)",
    "Controller 15 (LSB)",
    "General Purpose 1 (LSB)",
    "General Purpose 2 (LSB)",
    "General Purpose 3 (LSB)",
    "General Purpose 4 (LSB)",
    "Controller 20 (LSB)",
    "Controller 21 (LSB)",
    "Controller 22 (LSB)",
    "Controller 23 (LSB)",
    "Controller 24 (LSB)",
    "Controller 25 (LSB)",
    "Controller 26 (LSB)",
    "Controller 27 (LSB)",
    "Controller 28 (LSB)",
    "Controller 29 (LSB)",
    "Controller 30 (LSB)",
    "Controller 31 (LSB)",
    "Hold (Sustain)",
    "Portamento",
    "Sostenuto",
    "Soft Pedal",
    "Legato Footswitch",
    "Hold 2",
    "Sound Variation",
    "LPF Resonance",
    "Release Time",
    "Attack Time",
    "LPF Cutoff",
    "Decay Time",
    "Vibrato Rate",
    "Vibrato Depth",
    "Vibrato Delay",
    "Sound Controller 10",
    "General Purpose 5",
    "General Purpose 6",
    "General Purpose 7",
    "General Purpose 8",
    "Portamento Control",
    "Controller 85",
    "Controller 86",
    "Controller 87",
    "High Resolution Velocity Prefix",
    "Controller 89",
    "Controller 90",
    "Reverb Send",
    "Tremolo Depth",
    "Chorus Send",
    "Celeste (Detune) Depth",
    "Phaser Depth",
    "Data Increment",
    "Data Decrement",
    "NRPN (LSB)",
    "NRPN (MSB)",
    "RPN (LSB)",
    "RPN (MSB)",
    "Controller 102",
    "Controller 103",
    "Controller 104",
    "Controller 105",
    "Controller 106",
    "Controller 107",
    "Controller 108",
    "Controller 109",
    "Controller 110",
    "Controller 111",
    "Controller 112",
    "Controller 113",
    "Controller 114",
    "Controller 115",
    "Controller 116",
    "Controller 117",
    "Controller 118",
    "Controller 119",
    "All Sound Off",
    "Reset All Controllers",
    "Local Control",
    "All Notes Off",
    "Omni Mode Off",
    "Omni Mode On",
    "Mono Mode On",
    "Poly Mode On",
];

/// Controllers the internal SoundFont synthesizer (RustySynth) responds to. The
/// others are stored, saved, exported and sent to external MIDI outputs, but
/// the internal synthesizer ignores them (it has no resonant filter, portamento
/// or sostenuto, and uses only the MSB of Bank Select).
pub fn internal_synth_responds(kind: ControllerKind) -> bool {
    match kind {
        ControllerKind::PitchBend => true,
        ControllerKind::Cc(n) => matches!(
            n,
            0 | 1 | 6 | 7 | 10 | 11 | 33 | 38 | 39 | 42 | 43 | 64 | 91 | 93 | 98
                ..=101 | 120 | 121 | 123
        ),
    }
}

impl ControllerKind {
    pub const PAN: ControllerKind = ControllerKind::Cc(CC_PAN);
    pub const MODULATION: ControllerKind = ControllerKind::Cc(1);
    pub const VOLUME: ControllerKind = ControllerKind::Cc(7);
    pub const EXPRESSION: ControllerKind = ControllerKind::Cc(11);
    pub const HOLD: ControllerKind = ControllerKind::Cc(64);
    pub const RESONANCE: ControllerKind = ControllerKind::Cc(71);
    pub const CUTOFF: ControllerKind = ControllerKind::Cc(74);

    /// Control Change `n`, if it is a valid controller number.
    pub fn cc(n: u8) -> Option<ControllerKind> {
        (n <= 127).then_some(ControllerKind::Cc(n))
    }

    /// Controller name ("Pitch Bend", "Volume", "Controller 3", …).
    pub fn label(self) -> &'static str {
        match self {
            ControllerKind::PitchBend => "Pitch Bend",
            ControllerKind::Cc(n) => CC_NAMES[usize::from(n & 0x7F)],
        }
    }

    /// Name with the controller number ("CC 7 Volume"; "Pitch Bend").
    pub fn full_label(self) -> String {
        match self {
            ControllerKind::PitchBend => "Pitch Bend".into(),
            ControllerKind::Cc(n) => format!("CC {n} {}", self.label()),
        }
    }

    pub fn category(self) -> Option<CcCategory> {
        match self {
            ControllerKind::PitchBend => None,
            ControllerKind::Cc(n) => Some(CcCategory::of(n)),
        }
    }

    /// 64–69: drawn as on/off steps.
    pub fn is_switch(self) -> bool {
        self.category() == Some(CcCategory::Switch)
    }

    /// 120–127: commands; they are never generated by curve tools.
    pub fn is_channel_mode(self) -> bool {
        self.category() == Some(CcCategory::ChannelMode)
    }

    /// Curve tools may generate this controller (everything but channel-mode
    /// messages).
    pub fn curve_drawable(self) -> bool {
        !self.is_channel_mode()
    }

    /// Why editing this controller needs care, if it does: messages whose meaning
    /// depends on other messages (RPN/NRPN selection and data entry, bank select)
    /// or that are commands.
    pub fn caution(self) -> Option<&'static str> {
        let ControllerKind::Cc(n) = self else {
            return None;
        };
        match n {
            0 | 32 => Some(
                "Bank Select changes which instrument bank the next Program Change selects; it is not a continuous control.",
            ),
            6 | 38 | 96 | 97 => Some(
                "Data Entry / Increment / Decrement change whichever RPN or NRPN parameter was selected before them (for example the pitch-bend range); their meaning depends on the messages around them.",
            ),
            98..=101 => Some(
                "RPN / NRPN numbers select which parameter later Data Entry messages change; they are part of multi-message sequences, not continuous controls.",
            ),
            120..=127 => Some(
                "Channel-mode messages are commands (they stop sounds, reset controllers or change the channel's mode); add them one at a time.",
            ),
            _ => None,
        }
    }

    pub fn max(self) -> u16 {
        match self {
            ControllerKind::PitchBend => PITCH_BEND_MAX,
            ControllerKind::Cc(_) => CC_MAX,
        }
    }

    /// The value a synthesizer assumes before the first event (General MIDI
    /// power-on state): drawn as the baseline and used as the neutral line.
    pub fn center(self) -> u16 {
        match self {
            ControllerKind::PitchBend => PITCH_BEND_CENTER,
            ControllerKind::Cc(n) => u16::from(default_cc_value(n)),
        }
    }

    /// Shown around a centre (Pitch Bend, Pan, Balance).
    pub fn is_bipolar(self) -> bool {
        matches!(
            self,
            ControllerKind::PitchBend | ControllerKind::Cc(8) | ControllerKind::Cc(10)
        )
    }

    /// The value of `e` if it is this kind of event on `channel` (0–15).
    #[inline]
    pub fn value_of(self, e: &RawEvent, channel: u8) -> Option<u16> {
        let RawKind::Channel { status, data } = e.kind else {
            return None;
        };
        match self {
            ControllerKind::PitchBend if status == 0xE0 | channel => {
                Some(u16::from(data[0] & 0x7F) | (u16::from(data[1] & 0x7F) << 7))
            }
            ControllerKind::Cc(n) if status == 0xB0 | channel && data[0] == n => {
                Some(u16::from(data[1] & 0x7F))
            }
            _ => None,
        }
    }

    /// Kind and channel of a channel event, if it is a controller.
    pub fn of_event(e: &RawEvent) -> Option<(ControllerKind, u8, u16)> {
        let RawKind::Channel { status, data } = e.kind else {
            return None;
        };
        let ch = status & 0x0F;
        match status & 0xF0 {
            0xE0 => Some((
                ControllerKind::PitchBend,
                ch,
                u16::from(data[0] & 0x7F) | (u16::from(data[1] & 0x7F) << 7),
            )),
            0xB0 => Some((
                ControllerKind::Cc(data[0] & 0x7F),
                ch,
                u16::from(data[1] & 0x7F),
            )),
            _ => None,
        }
    }

    /// The event for `value` on `channel` at `tick` (anchor 0: before the tick's
    /// notes).
    pub fn event(self, channel: u8, tick: u64, value: u16) -> RawEvent {
        let kind = match self {
            ControllerKind::PitchBend => RawKind::Channel {
                status: 0xE0 | (channel & 0x0F),
                data: [(value & 0x7F) as u8, ((value >> 7) & 0x7F) as u8],
            },
            ControllerKind::Cc(n) => RawKind::Channel {
                status: 0xB0 | (channel & 0x0F),
                data: [n & 0x7F, (value & 0x7F) as u8],
            },
        };
        RawEvent {
            tick,
            anchor: 0,
            kind,
        }
    }

    /// Value shown to the user: pitch bend is signed around the centre
    /// (−8192…+8191), pan shows left/right, switches on/off, channel-mode
    /// messages their meaning.
    pub fn display(self, value: u16) -> String {
        match self {
            ControllerKind::PitchBend => {
                let v = i32::from(value) - i32::from(PITCH_BEND_CENTER);
                if v > 0 {
                    format!("+{v}")
                } else {
                    v.to_string()
                }
            }
            ControllerKind::Cc(10) | ControllerKind::Cc(8) => match value.cmp(&PAN_CENTER) {
                std::cmp::Ordering::Equal => "64 (center)".into(),
                std::cmp::Ordering::Less => format!("{value} (left)"),
                std::cmp::Ordering::Greater => format!("{value} (right)"),
            },
            ControllerKind::Cc(n) if CcCategory::of(n) == CcCategory::Switch => {
                format!("{value} ({})", if value >= 64 { "on" } else { "off" })
            }
            ControllerKind::Cc(122) => {
                format!("{value} ({})", if value >= 64 { "on" } else { "off" })
            }
            ControllerKind::Cc(126) if value == 0 => "0 (as many channels as available)".into(),
            ControllerKind::Cc(126) => format!("{value} channel(s)"),
            ControllerKind::Cc(_) => value.to_string(),
        }
    }

    /// Parses a typed value: pitch bend accepts the signed form (-8192…+8191, as
    /// shown; a typographic minus `−` works too); controllers accept 0–127, and
    /// switches also "on" / "off". Values out of range are refused, never clamped.
    pub fn parse(self, input: &str) -> Result<u16, EditError> {
        let t = input.trim();
        let t = t
            .split_whitespace()
            .next()
            .unwrap_or("")
            .replace('\u{2212}', "-");
        if self.is_switch() || self == ControllerKind::Cc(122) {
            match t.to_ascii_lowercase().as_str() {
                "on" => return Ok(127),
                "off" => return Ok(0),
                _ => {}
            }
        }
        let v: i64 = t
            .trim_start_matches('+')
            .parse()
            .map_err(|_| EditError::InvalidController(self))?;
        let raw = match self {
            ControllerKind::PitchBend => v + i64::from(PITCH_BEND_CENTER),
            ControllerKind::Cc(_) => v,
        };
        if (0..=i64::from(self.max())).contains(&raw) {
            Ok(raw as u16)
        } else {
            Err(EditError::InvalidController(self))
        }
    }

    /// Range text for messages and dialogs.
    pub fn range_text(self) -> &'static str {
        match self {
            ControllerKind::PitchBend => "−8192 … +8191 (0 = no bend)",
            ControllerKind::Cc(8) | ControllerKind::Cc(10) => {
                "0 (left) … 64 (center) … 127 (right)"
            }
            ControllerKind::Cc(n) if CcCategory::of(n) == CcCategory::Switch => {
                "0–63 off, 64–127 on"
            }
            ControllerKind::Cc(_) => "0 … 127",
        }
    }

    /// The conventional value of a channel-mode command (what a new one gets).
    pub fn command_value(self) -> u16 {
        match self {
            ControllerKind::Cc(122) => 127,
            _ => 0,
        }
    }
}

/// Graph targets offered by name in the graph menus (in this order); any other
/// controller is reached through "Generic CC".
pub const NAMED_GRAPH_TARGETS: [GraphTarget; 11] = [
    GraphTarget::Velocity,
    GraphTarget::PitchBend,
    GraphTarget::Cc(1),
    GraphTarget::Cc(11),
    GraphTarget::Cc(7),
    GraphTarget::Cc(10),
    GraphTarget::Cc(64),
    GraphTarget::Cc(74),
    GraphTarget::Cc(71),
    GraphTarget::MasterVolume,
    GraphTarget::Bpm,
];

/// The channel controller a graph target edits (Pitch Bend or a CC).
pub fn graph_controller(t: GraphTarget) -> Option<ControllerKind> {
    match t {
        GraphTarget::PitchBend => Some(ControllerKind::PitchBend),
        GraphTarget::Cc(n) => ControllerKind::cc(n),
        _ => None,
    }
}

/// Menu name of a graph target ("Velocity", "Modulation (CC 1)", "Panpot (CC 10)",
/// "CC 3", "Master Volume", "BPM (Conductor)").
pub fn graph_label(t: GraphTarget) -> String {
    match t {
        GraphTarget::Velocity => "Velocity".into(),
        GraphTarget::PitchBend => "Pitch Bend".into(),
        GraphTarget::Cc(10) => "Panpot (CC 10)".into(),
        GraphTarget::Cc(64) => "Hold (CC 64)".into(),
        GraphTarget::Cc(74) => "LPF Cutoff (CC 74)".into(),
        GraphTarget::Cc(71) => "LPF Resonance (CC 71)".into(),
        GraphTarget::Cc(n) => {
            let name = CC_NAMES[usize::from(n & 0x7F)];
            if name.starts_with("Controller") {
                format!("CC {n}")
            } else {
                format!("{name} (CC {n})")
            }
        }
        GraphTarget::MasterVolume => "Master Volume".into(),
        GraphTarget::Bpm => "BPM (Conductor)".into(),
    }
}

/// Short name for a graph header ("Vel", "Bend", "CC 7", "Master", "BPM").
pub fn graph_short_label(t: GraphTarget) -> String {
    match t {
        GraphTarget::Velocity => "Vel".into(),
        GraphTarget::PitchBend => "Bend".into(),
        GraphTarget::Cc(n) => format!("CC {n}"),
        GraphTarget::MasterVolume => "Master".into(),
        GraphTarget::Bpm => "BPM".into(),
    }
}

/// Conductor-owned targets (global, not in a track).
pub fn graph_is_global(t: GraphTarget) -> bool {
    matches!(t, GraphTarget::MasterVolume | GraphTarget::Bpm)
}

/// General MIDI power-on value of a controller.
pub fn default_cc_value(n: u8) -> u8 {
    match n {
        7 => 100,
        8 | 10 => 64,
        11 => 127,
        71..=79 => 64,
        91 => 40,
        _ => 0,
    }
}

/// One controller event as the graph sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerPoint {
    /// Index in the track's event list (valid for the track's events revision).
    pub index: usize,
    pub tick: u64,
    pub value: u16,
}

/// Every event of `kind` on `channel`, in order; at one tick only the last (the
/// one that sounds) is listed. O(events); the graph caches the result per events
/// revision.
pub fn collect_points(
    events: &EventStore,
    kind: ControllerKind,
    channel: u8,
) -> Vec<ControllerPoint> {
    let mut out: Vec<ControllerPoint> = Vec::new();
    for (index, e) in events.events().iter().enumerate() {
        if let Some(value) = kind.value_of(e, channel) {
            let p = ControllerPoint {
                index,
                tick: e.tick,
                value,
            };
            match out.last_mut() {
                Some(last) if last.tick == e.tick => *last = p,
                _ => out.push(p),
            }
        }
    }
    out
}

/// Every controller event of the track (any kind and channel) with its index,
/// for the event list. O(events).
pub fn all_controller_events(events: &EventStore) -> Vec<(usize, ControllerKind, u8, u64, u16)> {
    events
        .events()
        .iter()
        .enumerate()
        .filter_map(|(i, e)| ControllerKind::of_event(e).map(|(k, c, v)| (i, k, c, e.tick, v)))
        .collect()
}

/// A prepared change of one content's events: a track's own (controllers,
/// the track name) or an owned content's (controllers; v0.9 V09-S11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsEdit {
    pub host: ContentRef,
    /// Events revision of the content the edit was prepared from.
    pub base_revision: u64,
    pub patch: ListPatch<RawEvent>,
    /// Payload bytes to append to the track's arena when the edit is first
    /// applied (a renamed Track Name); inserted meta events refer to them at
    /// `append_at`, the arena length the edit was prepared for.
    pub append: Vec<u8>,
    pub append_at: u32,
}

impl EventsEdit {
    fn new(src: HostView<'_>) -> Self {
        EventsEdit {
            host: src.host(),
            base_revision: src.events_revision(),
            patch: ListPatch::new(),
            append: Vec::new(),
            append_at: 0,
        }
    }

    /// Events the edit inserts (for the selection after it is applied).
    pub fn inserted_indices(&self) -> Vec<usize> {
        self.patch.inserted().iter().map(|(i, _)| *i).collect()
    }
}

/// Index at which a new anchor-0 event at `tick` goes: after the tick's events
/// that precede every note (anchor 0), before those placed after notes.
fn insert_position(events: &[RawEvent], tick: u64) -> usize {
    let start = events.partition_point(|e| e.tick < tick);
    let end = events.partition_point(|e| e.tick <= tick);
    start + events[start..end].partition_point(|e| e.anchor == 0)
}

/// Indices of every `kind`/`channel` event at `tick`.
fn same_at(events: &[RawEvent], kind: ControllerKind, channel: u8, tick: u64) -> Vec<usize> {
    let start = events.partition_point(|e| e.tick < tick);
    let end = events.partition_point(|e| e.tick <= tick);
    (start..end)
        .filter(|&i| kind.value_of(&events[i], channel).is_some())
        .collect()
}

/// Builds a patch that removes the events at `remove` (strictly increasing
/// indices) and inserts `add` (sorted by tick). An added event with a slot takes
/// the place of that removed event (an update keeps its position among its
/// tick's events); one without goes after its tick's anchor-0 events.
fn build(track: HostView<'_>, remove: &[usize], add: &[(RawEvent, Option<usize>)]) -> EventsEdit {
    let events = track.events().events();
    let mut edit = EventsEdit::new(track);
    for &i in remove {
        edit.patch.remove(i, events[i]);
    }
    // Position of each added event in the old list, then in the new one.
    let mut placed: Vec<(usize, usize, RawEvent)> = add
        .iter()
        .enumerate()
        .map(|(k, (e, slot))| {
            (
                slot.unwrap_or_else(|| insert_position(events, e.tick)),
                k,
                *e,
            )
        })
        .collect();
    placed.sort_by_key(|p| (p.0, p.1));
    for (k, (pos_old, _, e)) in placed.into_iter().enumerate() {
        let removed_before = remove.partition_point(|&r| r < pos_old);
        edit.patch.insert(pos_old - removed_before + k, e);
    }
    edit
}

fn check(kind: ControllerKind, channel: u8, value: u16) -> Result<(), EditError> {
    if channel > 15 {
        return Err(EditError::InvalidChannel(channel));
    }
    if value > kind.max() {
        return Err(EditError::InvalidController(kind));
    }
    Ok(())
}

/// Adds an event (or updates the one already at `tick`), or — with `target`, an
/// event index — changes that event, moving it to `tick` (replacing any event of
/// the same kind and channel there).
pub fn prepare_set<'a>(
    track: impl Into<HostView<'a>>,
    kind: ControllerKind,
    channel: u8,
    target: Option<usize>,
    tick: u64,
    value: u16,
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    check(kind, channel, value)?;
    let events = track.events().events();
    match target {
        Some(i) => {
            let old = *events.get(i).ok_or(EditError::Stale)?;
            if kind.value_of(&old, channel).is_none() {
                return Err(EditError::Stale);
            }
            if old.tick == tick {
                // Same tick: update in place, keeping its order among the tick's events.
                let new = RawEvent {
                    anchor: old.anchor,
                    ..kind.event(channel, tick, value)
                };
                if new == old {
                    return Ok(EventsEdit::new(track));
                }
                Ok(build(track, &[i], &[(new, Some(i))]))
            } else {
                let mut removed = vec![i];
                removed.extend(same_at(events, kind, channel, tick));
                removed.sort_unstable();
                removed.dedup();
                Ok(build(
                    track,
                    &removed,
                    &[(kind.event(channel, tick, value), None)],
                ))
            }
        }
        None => {
            let existing = same_at(events, kind, channel, tick);
            match existing.last() {
                Some(&i) => {
                    let old = events[i];
                    let new = RawEvent {
                        anchor: old.anchor,
                        ..kind.event(channel, tick, value)
                    };
                    if new == old {
                        return Ok(EventsEdit::new(track));
                    }
                    Ok(build(track, &[i], &[(new, Some(i))]))
                }
                None => Ok(build(
                    track,
                    &[],
                    &[(kind.event(channel, tick, value), None)],
                )),
            }
        }
    }
}

/// v1.0-beta1 (B1-0030, Lua scripts): removes the channel events at `remove`
/// and adds the channel messages `add` in one edit — each added event after
/// the events already at its tick (before the notes' anchors there), added
/// events at one tick in the given order. Meta, SysEx and system events can
/// be neither removed nor added this way (they stay as they are).
pub fn prepare_replace<'a>(
    track: impl Into<HostView<'a>>,
    remove: &[usize],
    add: &[RawEvent],
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    let events = track.events().events();
    let mut idx = remove.to_vec();
    idx.sort_unstable();
    idx.dedup();
    for &i in &idx {
        let e = events.get(i).ok_or(EditError::Stale)?;
        if !matches!(e.kind, RawKind::Channel { .. }) {
            return Err(EditError::Stale);
        }
    }
    for e in add {
        match e.kind {
            RawKind::Channel { status, data }
                if (0x80..0xF0).contains(&status) && data[0] < 0x80 && data[1] < 0x80 => {}
            _ => {
                return Err(EditError::Invalid(
                    "Only channel messages (controllers, pitch bend, program changes, pressure) can be added.".into(),
                ));
            }
        }
    }
    // `build` keeps the given order among events placed at one position, so
    // they must come in tick order (stable: one tick keeps the given order).
    let mut add: Vec<(RawEvent, Option<usize>)> = add.iter().map(|e| (*e, None)).collect();
    add.sort_by_key(|(e, _)| e.tick);
    Ok(build(track, &idx, &add))
}

/// Deletes the events at `indices` (every one must be a channel message).
pub fn prepare_delete<'a>(
    track: impl Into<HostView<'a>>,
    indices: &[usize],
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    let events = track.events().events();
    let mut idx = indices.to_vec();
    idx.sort_unstable();
    idx.dedup();
    for &i in &idx {
        let e = events.get(i).ok_or(EditError::Stale)?;
        if !matches!(e.kind, RawKind::Channel { .. }) {
            return Err(EditError::Stale);
        }
    }
    Ok(build(track, &idx, &[]))
}

/// What happens to existing events of the same kind and channel when points are
/// written over a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WritePolicy {
    /// Every event of the target kind and channel from the first to the last new
    /// point (inclusive) is replaced by the new points.
    #[default]
    Replace,
    /// New points are added; at a tick that already has an event of the target
    /// kind and channel, that event takes the new value (no duplicates).
    Merge,
}

impl WritePolicy {
    pub fn label(self) -> &'static str {
        match self {
            WritePolicy::Replace => "Replace events in range",
            WritePolicy::Merge => "Merge with existing events",
        }
    }
}

/// Writes `points` (`(tick, value)`, sorted by tick, one per tick) as events of
/// `kind` on `channel` in one edit (curves, free-hand drawing, paste). Other
/// controllers, other channels and every other event are untouched. `limit`
/// bounds the number of points (the caller derives it from the memory budget);
/// more are refused, never silently truncated.
pub fn prepare_points<'a>(
    track: impl Into<HostView<'a>>,
    kind: ControllerKind,
    channel: u8,
    points: &[(u64, u16)],
    policy: WritePolicy,
    limit: usize,
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    if points.len() > limit {
        return Err(EditError::TooManyEvents {
            requested: points.len(),
            limit,
        });
    }
    if kind.is_channel_mode() && points.len() > 1 {
        return Err(EditError::ChannelModeCurve);
    }
    for w in points.windows(2) {
        if w[0].0 >= w[1].0 {
            return Err(EditError::Stale);
        }
    }
    for &(_, v) in points {
        check(kind, channel, v)?;
    }
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return Ok(EventsEdit::new(track));
    };
    let events = track.events().events();
    let matching = |i: usize| kind.value_of(&events[i], channel).is_some();
    let mut remove: Vec<usize> = Vec::new();
    // Last removed event at each new point's tick (its slot).
    let mut slot_at: Vec<Option<usize>> = vec![None; points.len()];
    match policy {
        WritePolicy::Replace => {
            let lo = events.partition_point(|e| e.tick < first.0);
            let hi = events.partition_point(|e| e.tick <= last.0);
            let mut p = 0usize;
            for (i, e) in events.iter().enumerate().take(hi).skip(lo) {
                if matching(i) {
                    remove.push(i);
                    let t = e.tick;
                    while p < points.len() && points[p].0 < t {
                        p += 1;
                    }
                    if p < points.len() && points[p].0 == t {
                        slot_at[p] = Some(i);
                    }
                }
            }
        }
        WritePolicy::Merge => {
            for (p, &(t, _)) in points.iter().enumerate() {
                let same = same_at(events, kind, channel, t);
                slot_at[p] = same.last().copied();
                remove.extend(same);
            }
        }
    }
    let add: Vec<(RawEvent, Option<usize>)> = points
        .iter()
        .zip(&slot_at)
        .map(|(&(t, v), &slot)| {
            let anchor = slot.map_or(0, |i| events[i].anchor);
            (
                RawEvent {
                    anchor,
                    ..kind.event(channel, t, v)
                },
                slot,
            )
        })
        .collect();
    let edit = build(track, &remove, &add);
    Ok(edit)
}

/// Moves the events at `indices` (all of `kind` on `channel`) by `dtick` ticks
/// and changes their values by `dvalue` (clamped to the controller's range). An
/// event of the same kind and channel already at a destination tick is
/// replaced. Refused when an event would move before tick 0 or past the largest
/// tick.
pub fn prepare_move<'a>(
    track: impl Into<HostView<'a>>,
    kind: ControllerKind,
    channel: u8,
    indices: &[usize],
    dtick: i64,
    dvalue: i32,
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    let events = track.events().events();
    let mut idx = indices.to_vec();
    idx.sort_unstable();
    idx.dedup();
    let mut moved: Vec<(u64, usize, u16)> = Vec::with_capacity(idx.len());
    for &i in &idx {
        let e = events.get(i).ok_or(EditError::Stale)?;
        let v = kind.value_of(e, channel).ok_or(EditError::Stale)?;
        let t = i128::from(e.tick) + i128::from(dtick);
        if t < 0 {
            return Err(EditError::NegativeTick {
                operation: "Moving these events",
                earliest: t,
            });
        }
        let t = u64::try_from(t).map_err(|_| EditError::TickOverflow {
            operation: "Moving these events",
        })?;
        let v = (i32::from(v) + dvalue).clamp(0, i32::from(kind.max())) as u16;
        moved.push((t, i, v));
    }
    if dtick == 0 && dvalue == 0 {
        return Ok(EventsEdit::new(track));
    }
    // Several moved events at one destination tick (duplicates at one source
    // tick): the last in list order is the one that sounds, so it is kept.
    moved.sort_by_key(|m| (m.0, m.1));
    let mut dedup: Vec<(u64, usize, u16)> = Vec::with_capacity(moved.len());
    for m in moved {
        match dedup.last_mut() {
            Some(l) if l.0 == m.0 => *l = m,
            _ => dedup.push(m),
        }
    }
    let mut remove = idx.clone();
    let selected: std::collections::HashSet<usize> = idx.iter().copied().collect();
    for &(t, _, _) in &dedup {
        for j in same_at(events, kind, channel, t) {
            if !selected.contains(&j) {
                remove.push(j);
            }
        }
    }
    remove.sort_unstable();
    remove.dedup();
    let add: Vec<(RawEvent, Option<usize>)> = dedup
        .iter()
        .map(|&(t, i, v)| {
            if dtick == 0 {
                // Value-only change: keep the position among the tick's events.
                (
                    RawEvent {
                        anchor: events[i].anchor,
                        ..kind.event(channel, t, v)
                    },
                    Some(i),
                )
            } else {
                (kind.event(channel, t, v), None)
            }
        })
        .collect();
    Ok(build(track, &remove, &add))
}

/// Changes the channel of the controller events at `indices` to `channel`
/// (explicit remap; nothing else changes).
pub fn prepare_rechannel<'a>(
    track: impl Into<HostView<'a>>,
    indices: &[usize],
    channel: u8,
) -> Result<EventsEdit, EditError> {
    let track: HostView<'a> = track.into();
    if channel > 15 {
        return Err(EditError::InvalidChannel(channel));
    }
    let events = track.events().events();
    let mut idx = indices.to_vec();
    idx.sort_unstable();
    idx.dedup();
    let mut add = Vec::with_capacity(idx.len());
    for &i in &idx {
        let e = *events.get(i).ok_or(EditError::Stale)?;
        let RawKind::Channel { status, data } = e.kind else {
            return Err(EditError::Stale);
        };
        add.push((
            RawEvent {
                kind: RawKind::Channel {
                    status: (status & 0xF0) | channel,
                    data,
                },
                ..e
            },
            Some(i),
        ));
    }
    Ok(build(track, &idx, &add))
}

/// Longest track name, in characters.
pub const MAX_TRACK_NAME_CHARS: usize = 200;

/// Validates a typed track name: trimmed, at most 200 characters, no control
/// characters. An empty name removes the name (the list shows "Track N").
pub fn parse_track_name(input: &str) -> Result<String, EditError> {
    let t = input.trim();
    if t.chars().count() > MAX_TRACK_NAME_CHARS {
        return Err(EditError::TrackName(format!(
            "A track name can be at most {MAX_TRACK_NAME_CHARS} characters."
        )));
    }
    if t.chars().any(char::is_control) {
        return Err(EditError::TrackName(
            "A track name cannot contain control characters.".into(),
        ));
    }
    Ok(t.to_owned())
}

/// Renames the track: the first Track Name meta event gets the new text (UTF-8),
/// or one is added at tick 0 before every other event; an empty name removes it.
/// The name is part of the track's events, so it is saved and exported exactly
/// once, like the Track Name of an imported file.
pub fn prepare_rename(track: &Track, name: &str) -> Result<EventsEdit, EditError> {
    let view = HostView::from(track);
    let name = parse_track_name(name)?;
    let events = track.events.events();
    let existing = events.iter().position(|e| {
        matches!(
            e.kind,
            RawKind::Meta {
                meta_type: meta::TRACK_NAME,
                ..
            }
        )
    });
    if track.name().unwrap_or("") == name
        && existing.is_some_and(|i| {
            let RawKind::Meta { payload, .. } = events[i].kind else {
                return false;
            };
            track.events.payload(payload) == name.as_bytes()
        })
    {
        return Ok(EventsEdit::new(view));
    }
    if name.is_empty() && existing.is_none() {
        return Ok(EventsEdit::new(view));
    }
    let arena_len = u32::try_from(track.events.arena_len())
        .map_err(|_| EditError::OutOfMemory { bytes: usize::MAX })?;
    let len = u32::try_from(name.len()).map_err(|_| EditError::Stale)?;
    arena_len
        .checked_add(len)
        .ok_or(EditError::OutOfMemory { bytes: usize::MAX })?;
    let named = |tick: u64, anchor: u32| RawEvent {
        tick,
        anchor,
        kind: RawKind::Meta {
            meta_type: meta::TRACK_NAME,
            payload: ArenaRange {
                offset: arena_len,
                len,
            },
        },
    };
    let mut edit = match (existing, name.is_empty()) {
        (Some(i), true) => build(view, &[i], &[]),
        (Some(i), false) => build(
            view,
            &[i],
            &[(named(events[i].tick, events[i].anchor), Some(i))],
        ),
        (None, _) => build(view, &[], &[(named(0, 0), Some(0))]),
    };
    if !name.is_empty() {
        edit.append = name.into_bytes();
        edit.append_at = arena_len;
    }
    Ok(edit)
}

/// The value in effect just before `tick` (the last event before it), if any.
pub fn value_before(points: &[ControllerPoint], tick: u64) -> Option<u16> {
    let i = points.partition_point(|p| p.tick < tick);
    i.checked_sub(1).map(|j| points[j].value)
}

/// The value in effect at `tick` (the last event at or before it), if any.
pub fn value_at(points: &[ControllerPoint], tick: u64) -> Option<u16> {
    let i = points.partition_point(|p| p.tick <= tick);
    i.checked_sub(1).map(|j| points[j].value)
}

/// Copied controller events: offsets from the first copied tick and values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventClip {
    pub kind: Option<ControllerKind>,
    pub points: Vec<(u64, u16)>,
}

impl EventClip {
    /// Copies the events at `indices` (all of `kind` on `channel`).
    pub fn copy<'a>(
        track: impl Into<HostView<'a>>,
        kind: ControllerKind,
        channel: u8,
        indices: &[usize],
    ) -> Self {
        let track: HostView<'a> = track.into();
        let events = track.events().events();
        let mut pts: Vec<(u64, u16)> = indices
            .iter()
            .filter_map(|&i| {
                let e = events.get(i)?;
                kind.value_of(e, channel).map(|v| (e.tick, v))
            })
            .collect();
        pts.sort_unstable();
        pts.dedup_by_key(|p| p.0);
        let base = pts.first().map_or(0, |p| p.0);
        EventClip {
            kind: Some(kind),
            points: pts.into_iter().map(|(t, v)| (t - base, v)).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The points placed at `at` (checked for tick overflow).
    pub fn placed(&self, at: u64) -> Result<Vec<(u64, u16)>, EditError> {
        self.points
            .iter()
            .map(|&(o, v)| {
                at.checked_add(o)
                    .map(|t| (t, v))
                    .ok_or(EditError::TickOverflow {
                        operation: "Pasting these events",
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::events::EventStore;
    use crate::model::note_store::NoteStore;
    use crate::model::track::TrackId;

    fn track_with(events: &[(u64, u32, u8, u8, u8)]) -> Track {
        let mut s = EventStore::new();
        for &(tick, anchor, status, a, b) in events {
            s.push(RawEvent {
                tick,
                anchor,
                kind: RawKind::Channel {
                    status,
                    data: [a, b],
                },
            })
            .unwrap();
        }
        Track::from_parts(TrackId(3), NoteStore::new(), s, None)
    }

    fn apply(t: &mut Track, e: EventsEdit) {
        assert!(t.apply_events_edit(&e));
    }

    #[test]
    fn pitch_bend_uses_fourteen_bits_and_pan_is_cc10() {
        for v in [0u16, 1, 127, 128, 8192, 16383] {
            let e = ControllerKind::PitchBend.event(5, 0, v);
            assert_eq!(ControllerKind::PitchBend.value_of(&e, 5), Some(v));
            assert_eq!(ControllerKind::PitchBend.value_of(&e, 4), None);
        }
        let e = ControllerKind::PAN.event(0, 9, 127);
        assert_eq!(
            e.kind,
            RawKind::Channel {
                status: 0xB0,
                data: [10, 127]
            }
        );
        assert_eq!(ControllerKind::PitchBend.parse("-8192"), Ok(0));
        assert_eq!(ControllerKind::PitchBend.parse("+8191"), Ok(16383));
        assert_eq!(ControllerKind::PitchBend.parse("0"), Ok(8192));
        assert_eq!(ControllerKind::PitchBend.parse("\u{2212}168"), Ok(8024));
        assert!(ControllerKind::PitchBend.parse("8192").is_err());
        assert!(ControllerKind::PAN.parse("128").is_err());
        assert!(ControllerKind::PAN.parse("-1").is_err());
        assert!(ControllerKind::PAN.parse("abc").is_err());
        assert_eq!(ControllerKind::PAN.parse("64 (center)"), Ok(64));
        assert_eq!(ControllerKind::PitchBend.display(8192), "0");
        assert_eq!(ControllerKind::PitchBend.display(16383), "+8191");
    }

    #[test]
    fn controller_numbers_have_names_categories_and_safe_semantics() {
        assert_eq!(ControllerKind::Cc(1).label(), "Modulation");
        assert_eq!(ControllerKind::Cc(7).label(), "Volume");
        assert_eq!(ControllerKind::Cc(11).label(), "Expression");
        assert_eq!(ControllerKind::Cc(64).label(), "Hold (Sustain)");
        assert_eq!(ControllerKind::Cc(71).label(), "LPF Resonance");
        assert_eq!(ControllerKind::Cc(74).label(), "LPF Cutoff");
        assert_eq!(ControllerKind::Cc(0).full_label(), "CC 0 Bank Select");
        assert_eq!(ControllerKind::Cc(32).label(), "Bank Select (LSB)");
        assert_eq!(CcCategory::of(0), CcCategory::HighResMsb);
        assert_eq!(CcCategory::of(31), CcCategory::HighResMsb);
        assert_eq!(CcCategory::of(32), CcCategory::HighResLsb);
        assert_eq!(CcCategory::of(63), CcCategory::HighResLsb);
        for n in 64..=69 {
            assert!(ControllerKind::Cc(n).is_switch(), "{n}");
        }
        assert_eq!(CcCategory::of(70), CcCategory::General);
        assert_eq!(CcCategory::of(119), CcCategory::General);
        for n in 120..=127 {
            let k = ControllerKind::Cc(n);
            assert!(k.is_channel_mode() && !k.curve_drawable() && k.caution().is_some());
        }
        for n in [0u8, 6, 32, 38, 96, 97, 98, 99, 100, 101] {
            assert!(ControllerKind::Cc(n).caution().is_some(), "{n}");
        }
        assert!(ControllerKind::Cc(1).caution().is_none());
        assert!(ControllerKind::cc(128).is_none());
        // Switches keep exact values; "on"/"off" are accepted.
        assert_eq!(ControllerKind::HOLD.parse("127"), Ok(127));
        assert_eq!(ControllerKind::HOLD.parse("64"), Ok(64));
        assert_eq!(ControllerKind::HOLD.parse("on"), Ok(127));
        assert_eq!(ControllerKind::HOLD.parse("off"), Ok(0));
        assert_eq!(ControllerKind::HOLD.display(100), "100 (on)");
        assert_eq!(ControllerKind::HOLD.display(63), "63 (off)");
        assert_eq!(ControllerKind::VOLUME.center(), 100);
        assert_eq!(ControllerKind::EXPRESSION.center(), 127);
        assert!(!internal_synth_responds(ControllerKind::CUTOFF));
        assert!(!internal_synth_responds(ControllerKind::RESONANCE));
        assert!(internal_synth_responds(ControllerKind::VOLUME));
        let e = ControllerKind::Cc(74).event(3, 5, 90);
        assert_eq!(
            ControllerKind::of_event(&e),
            Some((ControllerKind::Cc(74), 3, 90))
        );
    }

    #[test]
    fn add_update_move_and_delete_without_duplicates() {
        // A volume CC and a pan at tick 0, a note-anchored event at tick 10.
        let mut t = track_with(&[
            (0, 0, 0xB0, 7, 100),
            (0, 0, 0xB0, 10, 20),
            (10, 2, 0xC0, 1, 0),
        ]);
        let pan = ControllerKind::PAN;
        // Adding at an occupied tick updates in place.
        let e = prepare_set(&t, pan, 0, None, 0, 90).unwrap();
        apply(&mut t, e);
        assert_eq!(t.events.len(), 3);
        assert_eq!(collect_points(&t.events, pan, 0)[0].value, 90);
        // Adding at tick 10 goes before the note-anchored event.
        let e = prepare_set(&t, pan, 0, None, 10, 5).unwrap();
        apply(&mut t, e);
        assert_eq!(t.events.events()[2].kind, pan.event(0, 10, 5).kind);
        // Moving the tick-0 pan onto tick 10 replaces the one there.
        let pts = collect_points(&t.events, pan, 0);
        let e = prepare_set(&t, pan, 0, Some(pts[0].index), 10, 77).unwrap();
        let before = t.events.clone();
        apply(&mut t, e.clone());
        let pts = collect_points(&t.events, pan, 0);
        assert_eq!(pts.len(), 1);
        assert_eq!((pts[0].tick, pts[0].value), (10, 77));
        // The unrelated volume CC is untouched.
        assert!(t.events.events().iter().any(|e| e.kind
            == RawKind::Channel {
                status: 0xB0,
                data: [7, 100]
            }));
        // Undo restores both pans exactly.
        assert!(t.events.apply_patch(&e.patch, true));
        assert_eq!(t.events, before);
        assert!(t.events.apply_patch(&e.patch, false));
        let d = prepare_delete(&t, &[collect_points(&t.events, pan, 0)[0].index]).unwrap();
        apply(&mut t, d);
        assert!(collect_points(&t.events, pan, 0).is_empty());
        assert_eq!(t.events.len(), 2);
    }

    #[test]
    fn stale_and_invalid_edits_are_refused() {
        let t = track_with(&[(0, 0, 0xE0, 0, 64)]);
        assert!(prepare_set(&t, ControllerKind::PitchBend, 16, None, 0, 1).is_err());
        assert!(prepare_set(&t, ControllerKind::PitchBend, 0, None, 0, 16_384).is_err());
        assert!(prepare_set(&t, ControllerKind::PAN, 0, Some(0), 5, 1).is_err());
        assert!(prepare_set(&t, ControllerKind::PitchBend, 0, Some(9), 5, 1).is_err());
        assert!(prepare_set(&t, ControllerKind::Cc(1), 0, None, 5, 128).is_err());
        // A patch prepared for another state is refused by the store.
        let mut t2 = t.clone();
        let e = prepare_set(&t, ControllerKind::PitchBend, 0, Some(0), 5, 1).unwrap();
        apply(&mut t2, e.clone());
        assert!(!t2.events.apply_patch(&e.patch, false));
    }

    #[test]
    fn replace_writes_only_the_target_stream_in_range() {
        // CC1 on channels 0 and 1, CC7 on channel 0, a program change.
        let mut t = track_with(&[
            (0, 0, 0xB0, 1, 10),
            (0, 0, 0xB1, 1, 11),
            (10, 0, 0xB0, 1, 20),
            (10, 0, 0xB0, 7, 99),
            (15, 1, 0xC0, 3, 0),
            (20, 0, 0xB0, 1, 30),
            (40, 0, 0xB0, 1, 40),
        ]);
        let m = ControllerKind::MODULATION;
        let pts = [(5u64, 50u16), (10, 60), (15, 70), (30, 80)];
        let e = prepare_points(&t, m, 0, &pts, WritePolicy::Replace, 100).unwrap();
        let before = t.events.clone();
        apply(&mut t, e.clone());
        let got: Vec<(u64, u16)> = collect_points(&t.events, m, 0)
            .iter()
            .map(|p| (p.tick, p.value))
            .collect();
        // 0 and 40 are outside [5, 30]; 10 and 20 inside are replaced.
        assert_eq!(
            got,
            vec![(0, 10), (5, 50), (10, 60), (15, 70), (30, 80), (40, 40)]
        );
        // Channel 1, CC7 and the program change are untouched.
        assert_eq!(collect_points(&t.events, m, 1).len(), 1);
        assert_eq!(
            collect_points(&t.events, ControllerKind::VOLUME, 0)[0].value,
            99
        );
        assert!(
            t.events
                .events()
                .iter()
                .any(|e| matches!(e.kind, RawKind::Channel { status: 0xC0, .. }))
        );
        // The new CC at tick 15 goes before the note-anchored program change.
        let i15 = t.events.events().iter().position(|e| e.tick == 15).unwrap();
        assert_eq!(
            ControllerKind::of_event(&t.events.events()[i15]).map(|x| x.2),
            Some(70)
        );
        // Exactly reversible.
        assert!(t.events.apply_patch(&e.patch, true));
        assert_eq!(t.events, before);
    }

    #[test]
    fn merge_updates_same_tick_events_and_adds_the_rest() {
        let mut t = track_with(&[
            (0, 0, 0xB0, 11, 10),
            (10, 0, 0xB0, 11, 20),
            (10, 0, 0xB0, 11, 21),
        ]);
        let x = ControllerKind::EXPRESSION;
        let e = prepare_points(&t, x, 0, &[(10, 99), (12, 5)], WritePolicy::Merge, 100).unwrap();
        apply(&mut t, e);
        let got: Vec<(u64, u16)> = t
            .events
            .events()
            .iter()
            .filter_map(|e| x.value_of(e, 0).map(|v| (e.tick, v)))
            .collect();
        // The two events at tick 10 (an imported duplicate) became one.
        assert_eq!(got, vec![(0, 10), (10, 99), (12, 5)]);
    }

    #[test]
    fn limits_and_channel_mode_curves_are_refused_without_changes() {
        let t = track_with(&[]);
        let pts: Vec<(u64, u16)> = (0..11).map(|i| (i, 1)).collect();
        assert_eq!(
            prepare_points(&t, ControllerKind::Cc(1), 0, &pts, WritePolicy::Replace, 10),
            Err(EditError::TooManyEvents {
                requested: 11,
                limit: 10
            })
        );
        assert_eq!(
            prepare_points(
                &t,
                ControllerKind::Cc(123),
                0,
                &pts[..3],
                WritePolicy::Replace,
                10
            ),
            Err(EditError::ChannelModeCurve)
        );
        // A single channel-mode command is fine.
        assert!(
            prepare_points(
                &t,
                ControllerKind::Cc(123),
                0,
                &[(5, 0)],
                WritePolicy::Merge,
                10
            )
            .is_ok()
        );
        // Unsorted or duplicate ticks are refused.
        assert!(
            prepare_points(
                &t,
                ControllerKind::Cc(1),
                0,
                &[(5, 1), (5, 2)],
                WritePolicy::Merge,
                10
            )
            .is_err()
        );
    }

    #[test]
    fn moving_events_replaces_collisions_and_refuses_negative_ticks() {
        let mut t = track_with(&[
            (0, 0, 0xB0, 1, 10),
            (10, 0, 0xB0, 1, 20),
            (20, 0, 0xB0, 1, 30),
            (20, 0, 0xB0, 7, 99),
        ]);
        let m = ControllerKind::MODULATION;
        let pts = collect_points(&t.events, m, 0);
        assert!(prepare_move(&t, m, 0, &[pts[0].index], -1, 0).is_err());
        // Move the first two by +10: the one at 10 lands on 20 (replacing it).
        let e = prepare_move(&t, m, 0, &[pts[0].index, pts[1].index], 10, 5).unwrap();
        let before = t.events.clone();
        apply(&mut t, e.clone());
        let got: Vec<(u64, u16)> = collect_points(&t.events, m, 0)
            .iter()
            .map(|p| (p.tick, p.value))
            .collect();
        assert_eq!(got, vec![(10, 15), (20, 25)]);
        assert_eq!(
            collect_points(&t.events, ControllerKind::VOLUME, 0).len(),
            1
        );
        assert!(t.events.apply_patch(&e.patch, true));
        assert_eq!(t.events, before);
        // Value-only change keeps positions.
        let pts = collect_points(&t.events, m, 0);
        let e = prepare_move(&t, m, 0, &[pts[2].index], 0, 200).unwrap();
        apply(&mut t, e);
        assert_eq!(collect_points(&t.events, m, 0)[2].value, 127);
    }

    #[test]
    fn rechannel_and_clipboard_round_trip() {
        let mut t = track_with(&[(0, 0, 0xB0, 1, 10), (10, 0, 0xB0, 1, 20)]);
        let m = ControllerKind::MODULATION;
        let clip = EventClip::copy(&t, m, 0, &[0, 1]);
        assert_eq!(clip.points, vec![(0, 10), (10, 20)]);
        let e =
            prepare_points(&t, m, 0, &clip.placed(100).unwrap(), WritePolicy::Merge, 10).unwrap();
        apply(&mut t, e);
        assert_eq!(collect_points(&t.events, m, 0).len(), 4);
        let e = prepare_rechannel(&t, &[0, 1], 5).unwrap();
        apply(&mut t, e);
        assert_eq!(collect_points(&t.events, m, 5).len(), 2);
        assert_eq!(collect_points(&t.events, m, 0).len(), 2);
        assert!(clip.placed(u64::MAX).is_err());
    }

    #[test]
    fn renaming_edits_the_track_name_event_in_place() {
        let mut t = track_with(&[(0, 0, 0xB0, 1, 10)]);
        assert_eq!(t.name(), None);
        let e = prepare_rename(&t, "  Lead ♪ ").unwrap();
        apply(&mut t, e.clone());
        assert_eq!(t.name(), Some("Lead ♪"));
        assert_eq!(t.events.track_name(), Some("Lead ♪".as_bytes()));
        assert!(matches!(
            t.events.events()[0].kind,
            RawKind::Meta {
                meta_type: meta::TRACK_NAME,
                ..
            }
        ));
        assert_eq!(t.events.len(), 2);
        // Undo / redo of the patch (arena bytes stay valid).
        assert!(t.events.apply_patch(&e.patch, true));
        t.refresh_name();
        assert_eq!(t.name(), None);
        assert!(t.events.apply_patch(&e.patch, false));
        t.refresh_name();
        assert_eq!(t.name(), Some("Lead ♪"));
        // Rename again: still one Track Name event.
        let e = prepare_rename(&t, "Bass").unwrap();
        apply(&mut t, e);
        assert_eq!(t.name(), Some("Bass"));
        assert_eq!(t.events.len(), 2);
        // Clearing removes it.
        let e = prepare_rename(&t, "").unwrap();
        apply(&mut t, e);
        assert_eq!(t.name(), None);
        assert_eq!(t.events.len(), 1);
        assert!(prepare_rename(&t, "a\u{7}b").is_err());
        assert!(prepare_rename(&t, &"x".repeat(201)).is_err());
    }
}
