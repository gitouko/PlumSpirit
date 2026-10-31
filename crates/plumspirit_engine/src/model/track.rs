//! A Standard MIDI File track: independent notes plus preserved events, and the
//! track's own properties (default channel, type, output route, playback
//! corrections, key range, Onion Skin colour, mute/solo, explicit end).
//!
//! Tracks are *not* MIDI channels. A track may contain notes on any of the 16
//! channels, and every note keeps its own channel; the track's default channel
//! only decides the channel of newly drawn notes and events.

use super::events::{EventStore, RawEvent, RawKind, display_text, meta};
use super::note::SubchannelId;
use super::note_store::NoteStore;
use crate::editor::controllers::EventsEdit;
use std::fmt;

/// Stable identity of a track within one document. Unlike the track's index, the id
/// never changes when other tracks are added or deleted, so jobs and UI state can
/// detect that "their" track disappeared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u32);

/// One PlumSpirit subchannel of a track: an application-level note group with its
/// own display colour (derived from its id) and the physical MIDI channel (0–15)
/// that new notes drawn into it receive. A subchannel is **not** a MIDI channel:
/// a track may have any number of subchannels, several may use the same MIDI
/// channel, and each note keeps its own MIDI channel.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Subchannel {
    pub id: SubchannelId,
    /// Physical MIDI channel (0–15, shown as 1–16) for new notes.
    pub midi_channel: u8,
    /// Optional name ("Ch 5", "Strings · Ch 1"); `None` shows "Subchannel N".
    pub name: Option<String>,
}

/// Longest subchannel name, in characters.
pub const MAX_SUBCHANNEL_NAME_CHARS: usize = 200;

impl Subchannel {
    pub fn new(id: SubchannelId, midi_channel: u8) -> Self {
        Subchannel {
            id,
            midi_channel,
            name: None,
        }
    }

    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => format!("Subchannel {}", self.id.number()),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.midi_channel > 15 {
            return Err(format!(
                "The MIDI channel must be 1 to 16 ({} is not).",
                u16::from(self.midi_channel) + 1
            ));
        }
        if let Some(n) = &self.name
            && (n.trim().is_empty() || n.chars().count() > MAX_SUBCHANNEL_NAME_CHARS)
        {
            return Err(format!(
                "A subchannel name must have 1 to {MAX_SUBCHANNEL_NAME_CHARS} characters."
            ));
        }
        Ok(())
    }
}

/// The MIDI channel (0–15) after `prev`: the next one, cycling 16 → 1; with
/// `skip_10`, channel 10 (index 9) is skipped.
pub fn next_midi_channel(prev: u8, skip_10: bool) -> u8 {
    let mut c = (prev.min(15) + 1) % 16;
    if skip_10 && c == 9 {
        c = 10;
    }
    c
}

/// A track's subchannels, sorted by id; never empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubchannelTable {
    entries: Vec<Subchannel>,
}

impl SubchannelTable {
    /// One main subchannel on MIDI channel `midi_channel`.
    pub fn single(midi_channel: u8) -> Self {
        SubchannelTable {
            entries: vec![Subchannel::new(SubchannelId::MAIN, midi_channel.min(15))],
        }
    }

    /// Validates `entries` (non-empty, sorted by id, unique ids, valid fields).
    pub fn from_entries(entries: Vec<Subchannel>) -> Result<Self, String> {
        if entries.is_empty() {
            return Err("A track needs at least one subchannel.".into());
        }
        for e in &entries {
            e.validate()?;
        }
        if entries.windows(2).any(|w| w[0].id >= w[1].id) {
            return Err("Subchannel ids must be unique and in order.".into());
        }
        Ok(SubchannelTable { entries })
    }

    #[inline]
    pub fn entries(&self) -> &[Subchannel] {
        &self.entries
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Always false (a table is never empty); for API symmetry.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Position of `id` in the sorted table (O(log n)).
    #[inline]
    pub fn position(&self, id: SubchannelId) -> Option<usize> {
        self.entries.binary_search_by_key(&id, |e| e.id).ok()
    }

    #[inline]
    pub fn get(&self, id: SubchannelId) -> Option<&Subchannel> {
        self.position(id).map(|i| &self.entries[i])
    }

    #[inline]
    pub fn contains(&self, id: SubchannelId) -> bool {
        self.position(id).is_some()
    }

    pub fn first(&self) -> &Subchannel {
        &self.entries[0]
    }

    pub fn max_id(&self) -> SubchannelId {
        self.entries.last().map_or(SubchannelId::MAIN, |e| e.id)
    }

    /// MIDI channels (0–15) for `count` new subchannels (contract §125): each
    /// gets the previous one's channel + 1, starting after the channel of the
    /// highest-numbered existing subchannel, cycling 1 → 16 → 1; with `skip_10`
    /// channel 10 (the GM drum channel) is skipped (9 → 11).
    pub fn auto_channels(&self, count: usize, skip_10: bool) -> Vec<u8> {
        let mut prev = self.entries.last().map_or(15, |e| e.midi_channel);
        (0..count)
            .map(|_| {
                prev = next_midi_channel(prev, skip_10);
                prev
            })
            .collect()
    }

    /// `count` fresh ids after the highest one (then any free ids from the
    /// start), or `None` when fewer than `count` ids are free.
    pub fn free_ids(&self, count: usize) -> Option<Vec<SubchannelId>> {
        let total = u64::from(u32::MAX) + 1;
        if (count as u64) > total - self.entries.len() as u64 {
            return None;
        }
        let mut out = Vec::new();
        out.try_reserve_exact(count).ok()?;
        let mut next = u64::from(self.max_id().0) + 1;
        while out.len() < count && next < total {
            out.push(SubchannelId(next as u32));
            next += 1;
        }
        let mut cand = 0u32;
        let mut i = 0usize;
        while out.len() < count {
            while i < self.entries.len() && self.entries[i].id.0 < cand {
                i += 1;
            }
            if i < self.entries.len() && self.entries[i].id.0 == cand {
                cand = cand.checked_add(1)?;
                continue;
            }
            out.push(SubchannelId(cand));
            cand = cand.checked_add(1)?;
        }
        out.sort();
        Some(out)
    }

    /// Inserts entries whose ids are all absent. All-or-nothing.
    pub(crate) fn insert_all(&mut self, add: &[Subchannel]) -> Result<(), String> {
        for a in add {
            a.validate()?;
            if self.contains(a.id) {
                return Err(format!("Subchannel {} already exists.", a.id.number()));
            }
        }
        let mut ids: Vec<SubchannelId> = add.iter().map(|a| a.id).collect();
        ids.sort();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err("Duplicate subchannel ids.".into());
        }
        self.entries
            .try_reserve(add.len())
            .map_err(|_| "Not enough memory for the new subchannels.".to_string())?;
        self.entries.extend_from_slice(add);
        self.entries.sort_by_key(|e| e.id);
        Ok(())
    }

    /// Removes the entries with these ids (all must exist, and at least one entry
    /// must remain). All-or-nothing; returns the removed entries in id order.
    pub(crate) fn remove_all(&mut self, ids: &[SubchannelId]) -> Result<Vec<Subchannel>, String> {
        let mut sorted = ids.to_vec();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != ids.len() || sorted.iter().any(|id| !self.contains(*id)) {
            return Err("That subchannel does not exist.".into());
        }
        if sorted.len() >= self.entries.len() {
            return Err("A track keeps at least one subchannel.".into());
        }
        let mut removed = Vec::with_capacity(sorted.len());
        let mut kept = Vec::with_capacity(self.entries.len() - sorted.len());
        for e in self.entries.drain(..) {
            if sorted.binary_search(&e.id).is_ok() {
                removed.push(e);
            } else {
                kept.push(e);
            }
        }
        self.entries = kept;
        Ok(removed)
    }

    /// A copy without the `remove` entries (which must all be present and
    /// equal) and with the `add` entries. Undo/redo of subchannel changes.
    pub(crate) fn swapped(
        &self,
        remove: &[Subchannel],
        add: &[Subchannel],
    ) -> Result<SubchannelTable, String> {
        for r in remove {
            if self.get(r.id) != Some(r) {
                return Err("the subchannels differ from the history".into());
            }
        }
        let mut ids: Vec<SubchannelId> = remove.iter().map(|r| r.id).collect();
        ids.sort();
        let mut entries: Vec<Subchannel> = self
            .entries
            .iter()
            .filter(|e| ids.binary_search(&e.id).is_err())
            .cloned()
            .collect();
        entries.extend_from_slice(add);
        entries.sort_by_key(|e| e.id);
        SubchannelTable::from_entries(entries)
    }

    /// Replaces the entry with the same id.
    pub(crate) fn replace(&mut self, e: Subchannel) -> Result<Subchannel, String> {
        e.validate()?;
        let i = self
            .position(e.id)
            .ok_or_else(|| "That subchannel does not exist.".to_string())?;
        Ok(std::mem::replace(&mut self.entries[i], e))
    }

    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Subchannel>()
            + self
                .entries
                .iter()
                .map(|e| e.name.as_ref().map_or(0, String::capacity))
                .sum::<usize>()
    }
}

/// Melody or percussion (General MIDI puts drums on channel 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TrackKind {
    #[default]
    Melody,
    /// Rhythm / GM drums: new notes default to channel 10 and the keyboard shows
    /// General MIDI drum names.
    Rhythm,
}

impl TrackKind {
    pub fn label(self) -> &'static str {
        match self {
            TrackKind::Melody => "Melody",
            TrackKind::Rhythm => "Rhythm (GM drums)",
        }
    }
}

/// Zero-based MIDI channel of General MIDI percussion (channel 10).
pub const GM_DRUM_CHANNEL: u8 = 9;

/// Where a track's playback goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum OutputRoute {
    /// PlumSpirit's SoundFont synthesizer.
    #[default]
    Internal,
    /// An external MIDI output device, by its system name. The name is kept in
    /// the project even when the device is not connected; playback then uses
    /// the internal synthesizer for the track and says so (never both).
    External(String),
}

impl OutputRoute {
    pub fn label(&self) -> String {
        match self {
            OutputRoute::Internal => "Internal SoundFont Synthesizer".into(),
            OutputRoute::External(n) => n.clone(),
        }
    }
}

/// A gate (length) correction in hundredths of a percent: 10,000 = 100 % (no
/// change), 5,000 = half as long, 20,000 = twice as long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GatePercent(u32);

/// Smallest gate correction: 0.01 %.
pub const MIN_GATE_HUNDREDTHS: u32 = 1;
/// Largest gate correction: 1,000,000 % (10,000×).
pub const MAX_GATE_HUNDREDTHS: u32 = 100_000_000;

impl GatePercent {
    pub const IDENTITY: GatePercent = GatePercent(10_000);

    pub fn from_hundredths(h: u32) -> Option<GatePercent> {
        (MIN_GATE_HUNDREDTHS..=MAX_GATE_HUNDREDTHS)
            .contains(&h)
            .then_some(GatePercent(h))
    }

    pub fn hundredths(self) -> u32 {
        self.0
    }

    pub fn is_identity(self) -> bool {
        self.0 == 10_000
    }

    /// `round_half_up(duration × percent / 100)`, at least 1 for a note that had
    /// a length (zero-length imported notes stay zero). `None` on overflow.
    pub fn apply(self, duration: u64) -> Option<u64> {
        if self.is_identity() || duration == 0 {
            return Some(duration);
        }
        let v = (u128::from(duration) * u128::from(self.0) * 2 + 10_000) / 20_000;
        u64::try_from(v.max(1)).ok()
    }

    /// Parses "100", "50%", "87.5", "200 %" (up to two decimals).
    pub fn parse(input: &str) -> Result<GatePercent, String> {
        let t = input.trim().trim_end_matches('%').trim();
        let err = || {
            format!(
                "Enter the gate correction as a percentage from 0.01 to 1,000,000 (100 = unchanged); \"{}\" is not one.",
                input.trim().chars().take(30).collect::<String>()
            )
        };
        if t.is_empty() {
            return Err(err());
        }
        let (int, frac) = match t.split_once('.') {
            Some((a, b)) => (a, b),
            None => (t, ""),
        };
        if frac.len() > 2
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
            || (int.is_empty() && frac.is_empty())
        {
            return Err(err());
        }
        let i: u64 = if int.is_empty() {
            0
        } else {
            int.parse().map_err(|_| err())?
        };
        let f: u64 = format!("{frac:0<2}").parse().map_err(|_| err())?;
        let h = i
            .checked_mul(100)
            .and_then(|v| v.checked_add(f))
            .ok_or_else(err)?;
        u32::try_from(h)
            .ok()
            .and_then(GatePercent::from_hundredths)
            .ok_or_else(err)
    }
}

impl fmt::Display for GatePercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (i, d) = (self.0 / 100, self.0 % 100);
        if d == 0 {
            write!(f, "{i}")
        } else if d % 10 == 0 {
            write!(f, "{i}.{}", d / 10)
        } else {
            write!(f, "{i}.{d:02}")
        }
    }
}

impl Default for GatePercent {
    fn default() -> Self {
        GatePercent::IDENTITY
    }
}

/// Largest tick correction magnitude (about ±9.2 × 10^18 is representable; the
/// limit keeps every sum within 64 bits).
pub const MAX_TICK_CORRECTION: i64 = i64::MAX / 2;

/// A track's editable properties (Track Properties dialog). Everything here is
/// part of the project (saved in `.lup`) and changes through the undo history.
/// The corrections are **non-destructive**: the notes stay as they are; playback
/// and (by default) MIDI export apply them once, on the fly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrackProps {
    /// Channel (0–15) of newly drawn notes and controller events.
    pub default_channel: u8,
    pub kind: TrackKind,
    pub route: OutputRoute,
    /// Signed tick offset for notes and channel events at or after the
    /// correction start.
    pub tick_correction: i64,
    /// First measure (1-based, by the Conductor's time signatures) the tick, key
    /// and gate corrections apply to; 1 = the whole track.
    pub correction_from_measure: u32,
    /// Signed transposition in semitones (−127 … +127; −255 … +255 in a
    /// 256-Key Mode project, v0.9).
    pub key_correction: i16,
    pub gate: GatePercent,
    /// Inclusive pitch range the track plays (a filter; notes outside it stay in
    /// the project but are not played or, with corrections applied, exported).
    /// The default (0, 127) is the whole keyboard (0–255 in 256-Key Mode).
    pub key_range: (u8, u8),
    /// Onion Skin colour; `None` = the automatic colour for the track.
    pub onion_color: Option<[u8; 3]>,
}

impl Default for TrackProps {
    fn default() -> Self {
        TrackProps {
            default_channel: 0,
            kind: TrackKind::Melody,
            route: OutputRoute::Internal,
            tick_correction: 0,
            correction_from_measure: 1,
            key_correction: 0,
            gate: GatePercent::IDENTITY,
            key_range: (0, 127),
            onion_color: None,
        }
    }
}

impl TrackProps {
    /// Any correction or filter is active (playback/export must apply it).
    pub fn corrects(&self) -> bool {
        self.tick_correction != 0
            || self.key_correction != 0
            || !self.gate.is_identity()
            || self.key_range != (0, 127)
    }

    /// Checks value ranges in a project of key range `keys` (the dialog also
    /// checks the notes).
    pub fn validate(&self, keys: super::project::KeyRange) -> Result<(), String> {
        let top = keys.max_pitch();
        if self.default_channel > 15 {
            return Err("The channel must be 1 to 16.".into());
        }
        if self.tick_correction.unsigned_abs() > MAX_TICK_CORRECTION.unsigned_abs() {
            return Err("The tick correction is too large.".into());
        }
        if self.correction_from_measure == 0 {
            return Err("Measures are numbered from 1.".into());
        }
        if self.key_correction.unsigned_abs() > u16::from(top) {
            return Err(format!(
                "The key correction must be between −{top} and +{top} semitones."
            ));
        }
        if self.key_range.0 > self.key_range.1 || self.key_range.1 > top {
            return Err(format!(
                "The key range must go from a lower to a higher (or the same) note within 0–{top}."
            ));
        }
        if let OutputRoute::External(n) = &self.route
            && n.trim().is_empty()
        {
            return Err("Choose an output device.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    id: TrackId,
    /// Parent track for a subtrack; `None` for a top-level track. The document keeps
    /// tracks in depth-first order, so a track's subtracks follow it directly.
    parent: Option<TrackId>,
    subs: SubchannelTable,
    subs_revision: u64,
    pub notes: NoteStore,
    pub events: EventStore,
    /// The track's explicit end (End of Track): `None` = fit to content (End of
    /// Track right after the last note end or event); `Some(t)` keeps silence up
    /// to `t` (imported from a file's End-of-Track event, or set by the user).
    /// Export writes End of Track at `max(end, content end)`, so an explicit end
    /// never cuts content off.
    pub end_of_track_tick: Option<u64>,
    name: Option<String>,
    /// Incremented whenever the preserved events change (controller edits), so
    /// playback and graph caches know when to rebuild.
    events_revision: u64,
    props: TrackProps,
    /// Incremented whenever the properties or the explicit end change.
    props_revision: u64,
    /// Muted: plays nothing (still edited, saved and exported). Not an undoable
    /// edit; saved with the project.
    pub mute: bool,
    /// Soloed: while any track is soloed, only soloed tracks play.
    pub solo: bool,
}

impl Track {
    pub fn new(id: TrackId) -> Self {
        Track {
            id,
            parent: None,
            subs: SubchannelTable::single(0),
            subs_revision: 0,
            notes: NoteStore::new(),
            events: EventStore::new(),
            end_of_track_tick: None,
            name: None,
            events_revision: 0,
            props: TrackProps::default(),
            props_revision: 0,
            mute: false,
            solo: false,
        }
    }

    pub fn from_parts(
        id: TrackId,
        notes: NoteStore,
        events: EventStore,
        end_of_track_tick: Option<u64>,
    ) -> Self {
        let mut t = Track {
            id,
            parent: None,
            subs: SubchannelTable::single(0),
            subs_revision: 0,
            notes,
            events,
            end_of_track_tick,
            name: None,
            events_revision: 0,
            props: TrackProps::default(),
            props_revision: 0,
            mute: false,
            solo: false,
        };
        t.refresh_name();
        t.infer_subchannels_from_channels();
        t
    }

    /// Like [`Track::from_parts`], but keeps the notes' subchannels exactly as
    /// they are (native projects store them); the caller sets the table.
    pub fn from_parts_exact(
        id: TrackId,
        notes: NoteStore,
        events: EventStore,
        end_of_track_tick: Option<u64>,
    ) -> Self {
        let mut t = Track::new(id);
        t.notes = notes;
        t.events = events;
        t.end_of_track_tick = end_of_track_tick;
        t.refresh_name();
        t
    }

    /// A new empty track with a Track Name event (subtracks, imports).
    pub fn named(id: TrackId, name: &str) -> Self {
        let mut t = Track::new(id);
        if let Ok(range) = t.events.store_payload(name.as_bytes()) {
            let _ = t.events.push(RawEvent {
                tick: 0,
                anchor: 0,
                kind: RawKind::Meta {
                    meta_type: meta::TRACK_NAME,
                    payload: range,
                },
            });
        }
        t.refresh_name();
        t
    }

    /// The subchannel table as it is saved and exported: the track's table plus
    /// an entry for any subchannel a note uses that the table lacks (which only a
    /// bug or a hand-built document can produce). Keeps every saved file loadable
    /// and every exported structure record restorable. O(notes) once per save or
    /// export; borrows when nothing is missing.
    pub fn subchannels_for_saving(&self) -> std::borrow::Cow<'_, SubchannelTable> {
        let used = self.notes.used_subs();
        let missing: Vec<SubchannelId> = used
            .into_iter()
            .filter(|id| !self.subs.contains(*id))
            .collect();
        if missing.is_empty() {
            return std::borrow::Cow::Borrowed(&self.subs);
        }
        let mut entries = self.subs.entries.clone();
        for id in missing {
            let channel = self
                .notes
                .iter()
                .find(|v| v.sub == id)
                .map_or(self.props.default_channel, |v| v.attrs.channel());
            entries.push(Subchannel::new(id, channel));
        }
        entries.sort_by_key(|e| e.id);
        std::borrow::Cow::Owned(SubchannelTable { entries })
    }

    /// Gives the track one subchannel per MIDI channel its notes use (a single
    /// channel keeps just the main subchannel, on that channel), in channel order,
    /// and puts every note into the subchannel of its channel. Used when a track
    /// comes from a MIDI file or a project older than subchannels. O(notes) once;
    /// no memory is added for a single-channel track.
    pub fn infer_subchannels_from_channels(&mut self) {
        let used = self.notes.channels_used();
        let channels: Vec<u8> = (0..16u8).filter(|c| used & (1 << c) != 0).collect();
        let first = channels
            .first()
            .copied()
            .unwrap_or(self.props.default_channel);
        if channels.len() <= 1 {
            self.subs = SubchannelTable::single(first);
            self.props.default_channel = first;
            self.subs_revision = self.subs_revision.wrapping_add(1);
            return;
        }
        let mut map = [0u32; 16];
        let mut entries = Vec::with_capacity(channels.len());
        for (i, &ch) in channels.iter().enumerate() {
            map[usize::from(ch)] = i as u32;
            let mut sc = Subchannel::new(SubchannelId(i as u32), ch);
            sc.name = Some(format!("Ch {}", u16::from(ch) + 1));
            entries.push(sc);
        }
        self.notes.assign_subs_by_channel(&map);
        self.subs = SubchannelTable { entries };
        self.props.default_channel = first;
        self.subs_revision = self.subs_revision.wrapping_add(1);
    }

    #[inline]
    pub fn parent(&self) -> Option<TrackId> {
        self.parent
    }

    pub(crate) fn set_parent(&mut self, parent: Option<TrackId>) {
        self.parent = parent;
    }

    #[inline]
    pub fn subchannels(&self) -> &SubchannelTable {
        &self.subs
    }

    /// Changes whenever the subchannel table changes (UI caches).
    #[inline]
    pub fn subchannels_revision(&self) -> u64 {
        self.subs_revision
    }

    /// Replaces the table (loading, import mapping). The caller guarantees that
    /// every note's subchannel is in `subs`.
    pub(crate) fn set_subchannels(&mut self, subs: SubchannelTable) {
        self.subs = subs;
        self.subs_revision = self.subs_revision.wrapping_add(1);
    }

    pub(crate) fn subchannels_mut(&mut self) -> &mut SubchannelTable {
        self.subs_revision = self.subs_revision.wrapping_add(1);
        &mut self.subs
    }

    #[inline]
    pub fn id(&self) -> TrackId {
        self.id
    }

    /// The Track Name meta text (decoded for display), if present and non-blank.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Re-reads the name from the Track Name event (after an events edit).
    pub fn refresh_name(&mut self) {
        self.name = self
            .events
            .track_name()
            .map(display_text)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
    }

    /// Display label: the track name, or "Track N" using the 1-based `index`.
    /// Prefer [`PlumDocument::track_label`](crate::model::PlumDocument::track_label),
    /// which numbers unnamed tracks among their siblings.
    pub fn display_name(&self, index: usize) -> String {
        match &self.name {
            Some(n) => n.trim().to_owned(),
            None => format!("Track {}", index + 1),
        }
    }

    /// Display label given the track's 1-based position among its siblings
    /// ([`PlumDocument::track_ordinals`](crate::model::PlumDocument::track_ordinals)).
    pub fn label_with(&self, ordinal: u32) -> String {
        match (&self.name, self.parent) {
            (Some(n), _) => n.trim().to_owned(),
            (None, None) => format!("Track {ordinal}"),
            (None, Some(_)) => format!("Subtrack {ordinal}"),
        }
    }

    #[inline]
    pub fn props(&self) -> &TrackProps {
        &self.props
    }

    /// Changes whenever the properties or the explicit end change.
    #[inline]
    pub fn props_revision(&self) -> u64 {
        self.props_revision
    }

    /// Sets properties and explicit end (history-internal, and the `.lup` loader).
    pub(crate) fn set_props(&mut self, props: TrackProps, end: Option<u64>) {
        self.props = props;
        self.end_of_track_tick = end;
        self.props_revision = self.props_revision.wrapping_add(1);
    }

    /// Latest tick that holds content (note end or preserved event), ignoring the
    /// explicit end. O(1).
    pub fn content_only_end_tick(&self) -> u64 {
        self.notes
            .max_end()
            .unwrap_or(0)
            .max(self.events.last_tick().unwrap_or(0))
    }

    /// Latest tick that holds content (note end, preserved event, or explicit
    /// end). Used for scroll extents and the song length; O(1).
    pub fn content_end_tick(&self) -> u64 {
        self.content_only_end_tick()
            .max(self.end_of_track_tick.unwrap_or(0))
    }

    /// Tick at which the exporter writes End-of-Track (without corrections).
    pub fn export_end_of_track_tick(&self) -> u64 {
        self.content_end_tick()
    }

    /// The explicit end adds silence after the content.
    pub fn has_trailing_silence(&self) -> bool {
        self.end_of_track_tick
            .is_some_and(|e| e > self.content_only_end_tick())
    }

    pub fn heap_bytes(&self) -> usize {
        self.notes.heap_bytes() + self.events.heap_bytes() + self.subs.heap_bytes()
    }

    /// Changes whenever the preserved events change.
    #[inline]
    pub fn events_revision(&self) -> u64 {
        self.events_revision
    }

    pub(crate) fn bump_events_revision(&mut self) {
        self.events_revision = self.events_revision.wrapping_add(1);
    }

    /// Applies a prepared events edit: checks it was prepared from the current
    /// events, appends its payload (a renamed Track Name) and applies the patch.
    /// Returns false, changing nothing, when it does not fit.
    pub(crate) fn apply_events_edit(&mut self, edit: &EventsEdit) -> bool {
        if edit.host != crate::model::clip::ContentRef::Track(self.id)
            || edit.base_revision != self.events_revision
        {
            return false;
        }
        let arena_before = self.events.arena_len();
        if !edit.append.is_empty() && !self.events.append_payload_at(edit.append_at, &edit.append) {
            return false;
        }
        if !self.events.apply_patch(&edit.patch, false) {
            self.events.truncate_arena(arena_before);
            return false;
        }
        self.after_events_patch(&edit.patch);
        true
    }

    /// Reverts (`backwards`) or re-applies an events patch (undo / redo).
    pub(crate) fn patch_events(
        &mut self,
        patch: &crate::model::patch::ListPatch<crate::model::events::RawEvent>,
        backwards: bool,
    ) -> bool {
        if !self.events.apply_patch(patch, backwards) {
            return false;
        }
        self.after_events_patch(patch);
        true
    }

    fn after_events_patch(
        &mut self,
        patch: &crate::model::patch::ListPatch<crate::model::events::RawEvent>,
    ) {
        self.bump_events_revision();
        if patch.items().any(|e| {
            matches!(
                e.kind,
                RawKind::Meta {
                    meta_type: meta::TRACK_NAME,
                    ..
                }
            )
        }) {
            self.refresh_name();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_percent_parses_rounds_and_never_zeroes_a_note() {
        let g = |s: &str| GatePercent::parse(s).unwrap();
        assert_eq!(g("100"), GatePercent::IDENTITY);
        assert_eq!(g("50%").hundredths(), 5_000);
        assert_eq!(g("87.5").hundredths(), 8_750);
        assert_eq!(g(" 200 % ").hundredths(), 20_000);
        assert_eq!(g("0.01").hundredths(), 1);
        assert_eq!(g("1000000").hundredths(), MAX_GATE_HUNDREDTHS);
        for bad in ["", "0", "-5", "abc", "1.234", "1000001", "1e3", "50,5"] {
            assert!(GatePercent::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(g("50").apply(480), Some(240));
        assert_eq!(g("200").apply(480), Some(960));
        assert_eq!(g("33.33").apply(3), Some(1)); // 0.9999 → 1
        assert_eq!(g("50").apply(3), Some(2)); // 1.5 rounds half up
        assert_eq!(g("0.01").apply(1), Some(1)); // never below one tick
        assert_eq!(g("50").apply(0), Some(0)); // zero-length notes stay zero
        assert_eq!(g("200").apply(u64::MAX), None);
        assert_eq!(g("87.5").to_string(), "87.5");
        assert_eq!(g("12.34").to_string(), "12.34");
        assert_eq!(g("100").to_string(), "100");
    }

    #[test]
    fn properties_validate_their_ranges() {
        let mut p = TrackProps::default();
        assert!(p.validate(Default::default()).is_ok());
        assert!(!p.corrects());
        p.key_range = (60, 59);
        assert!(p.validate(Default::default()).is_err());
        p.key_range = (0, 127);
        p.correction_from_measure = 0;
        assert!(p.validate(Default::default()).is_err());
        p.correction_from_measure = 1;
        p.default_channel = 16;
        assert!(p.validate(Default::default()).is_err());
        p.default_channel = 9;
        p.key_correction = 12;
        assert!(p.validate(Default::default()).is_ok() && p.corrects());
        p.route = OutputRoute::External("  ".into());
        assert!(p.validate(Default::default()).is_err());
        // v0.9 256-Key Mode: corrections and ranges reach 255.
        use super::super::project::KeyRange;
        p.route = OutputRoute::Internal;
        p.key_correction = 200;
        p.key_range = (100, 255);
        assert!(p.validate(KeyRange::Standard).is_err());
        assert!(p.validate(KeyRange::Extended256).is_ok());
        p.key_correction = -256;
        assert!(p.validate(KeyRange::Extended256).is_err());
    }
}
