//! Pasting generated colour artwork into the project (v0.8 built-in plugin
//! foundation, contract §027, §031–032, §039–040).
//!
//! A generator (Image to MIDI, Spiderweb) produces [`Generated`] notes, each
//! with a **colour index** (0, 1, 2, … into its palette). A [`PasteSpec`] says
//! where the colours go:
//!
//! * **Subtracks** — every colour used becomes a new Subtrack of the
//!   destination Track (its Onion colour = the source colour);
//! * **Subchannels** — every colour goes to a logical Subchannel of the
//!   destination Track: colour `c` → Subchannel id `c + offset` (the signed
//!   *Subchannel Paste Offset*), created when missing, reused when present;
//! * **Existing** — an explicit choice per colour: an existing Track /
//!   Subtrack, a Subchannel of the destination, or Skip.
//!
//! [`plan`] shows the mapping before anything changes (the plugin windows list
//! it); [`build`] turns it into **one** undo operation. Logical Subchannels are
//! not MIDI channels: a new Subchannel's MIDI channel comes from the
//! [`ChannelRule`]; notes take their Subchannel's (or Track's) channel.
//!
//! Negative offsets ([`BelowZero`]): a colour whose Subchannel would be below
//! the first one (id < 0) is refused (the default: Apply explains the smallest
//! valid offset), clamped onto the first Subchannel, or dropped.

use super::TransformError;
use crate::history::Operation;
use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
use crate::model::track::{Subchannel, SubchannelTable, Track, TrackId, next_midi_channel};
use crate::model::{NoteStore, PlumDocument};

/// Most colours one paste may map (new Subtracks / Subchannels).
pub const MAX_COLORS: usize = 4096;
/// Largest Subchannel id a paste may create.
pub const MAX_SUB_ID: i64 = 1 << 24;

/// One generated note: ticks relative to the artwork's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColorNote {
    pub start: u64,
    pub dur: u64,
    pub pitch: u8,
    pub vel: u8,
    pub color: u32,
}

/// A palette entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub rgb: [u8; 3],
    /// Shown in the mapping and used for new Subtrack / Subchannel names.
    pub name: String,
}

impl ColorInfo {
    pub fn new(rgb: [u8; 3]) -> ColorInfo {
        ColorInfo {
            rgb,
            name: format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]),
        }
    }
}

/// Generated artwork.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Generated {
    /// Sorted by (start, pitch, colour).
    pub notes: Vec<ColorNote>,
    pub colors: Vec<ColorInfo>,
}

impl Generated {
    pub fn sort(&mut self) {
        self.notes.sort_unstable();
    }

    /// Notes per colour index.
    pub fn counts(&self) -> Vec<u64> {
        let mut c = vec![0u64; self.colors.len()];
        for n in &self.notes {
            if let Some(x) = c.get_mut(n.color as usize) {
                *x += 1;
            }
        }
        c
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    Subtracks,
    Subchannels,
    Existing,
}

pub const PASTE_MODES: &[&str] = &[
    "Colors → new Subtracks",
    "Colors → Subchannels of one Track",
    "Colors → chosen Tracks / Subchannels",
];

/// What happens to colours whose Subchannel id would be below 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BelowZero {
    Refuse,
    Clamp,
    Drop,
}

pub const BELOW_ZERO: &[&str] = &[
    "Refuse (Apply explains the smallest offset)",
    "Put them on the first Subchannel",
    "Leave those colors out",
];

/// MIDI channel of the notes of a new Subtrack / Subchannel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelRule {
    /// After the destination's last Subchannel, like Add Subchannel (cycling
    /// 16 → 1; optionally skipping channel 10).
    Next {
        skip_10: bool,
    },
    /// Colour index mod 16 (Image to MIDI's original rule).
    ByColor,
    /// The destination Track's default channel.
    TrackChannel,
    Fixed(u8),
}

pub const CHANNEL_RULES: &[&str] = &[
    "Next channel (like Add Subchannel)",
    "Color number (color mod 16)",
    "The Track's channel",
    "One channel for all",
];

/// One colour's explicit destination (Existing mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingDest {
    Skip,
    /// An existing Track or Subtrack (its first Subchannel).
    Track(TrackId),
    /// A Subchannel of the destination Track.
    Subchannel(SubchannelId),
}

/// Where and how to paste.
#[derive(Clone, Debug, PartialEq)]
pub struct PasteSpec {
    pub mode: PasteMode,
    /// The destination Track (Subchannels; parent of new Subtracks).
    pub dest: TrackId,
    /// Insertion tick of the artwork's tick 0.
    pub at: u64,
    /// Subchannel Paste Offset (Subchannels mode): colour c → id c + offset.
    pub sub_offset: i64,
    pub below_zero: BelowZero,
    pub channel: ChannelRule,
    /// Existing mode: per colour index (missing = Skip).
    pub existing: Vec<ExistingDest>,
    /// Name prefix of new Subtracks / Subchannels ("Image", "Spiderweb").
    pub prefix: String,
}

impl PasteSpec {
    pub fn new(dest: TrackId, at: u64) -> PasteSpec {
        PasteSpec {
            mode: PasteMode::Subchannels,
            dest,
            at,
            sub_offset: 0,
            below_zero: BelowZero::Refuse,
            channel: ChannelRule::Next { skip_10: false },
            existing: Vec::new(),
            prefix: "Color".into(),
        }
    }
}

/// Where one colour goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    NewSubtrack {
        name: String,
        rgb: [u8; 3],
        channel: u8,
    },
    Subchannel {
        id: SubchannelId,
        channel: u8,
        /// Created by this paste.
        new: bool,
    },
    Track {
        id: TrackId,
        sub: SubchannelId,
        channel: u8,
    },
    /// Left out (Skip, Drop policy, or no notes).
    Skipped(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorPlan {
    pub color: u32,
    pub notes: u64,
    pub dest: Dest,
}

/// The mapping a paste would make (shown before Apply).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PastePlan {
    pub colors: Vec<ColorPlan>,
    /// Notes that would be written / left out.
    pub notes: u64,
    pub skipped: u64,
    /// Why Apply is impossible (full sentence), if it is.
    pub error: Option<String>,
}

impl PastePlan {
    pub fn summary(&self) -> String {
        let g = crate::midi::group_digits;
        let new_tracks = self
            .colors
            .iter()
            .filter(|c| matches!(c.dest, Dest::NewSubtrack { .. }))
            .count();
        let new_subs = self
            .colors
            .iter()
            .filter(|c| matches!(c.dest, Dest::Subchannel { new: true, .. }))
            .count();
        let mut s = format!("{} note(s) to paste", g(self.notes));
        if new_tracks > 0 {
            s.push_str(&format!(", {new_tracks} new Subtrack(s)"));
        }
        if new_subs > 0 {
            s.push_str(&format!(", {new_subs} new Subchannel(s)"));
        }
        if self.skipped > 0 {
            s.push_str(&format!(", {} left out", g(self.skipped)));
        }
        s.push('.');
        s
    }
}

/// The mapping of `art` for `spec` on `doc` (nothing changes).
pub fn plan(doc: &PlumDocument, art: &Generated, spec: &PasteSpec) -> PastePlan {
    let mut out = PastePlan {
        colors: Vec::new(),
        notes: 0,
        skipped: 0,
        error: None,
    };
    let Some(dest) = doc.track_by_id(spec.dest) else {
        out.error = Some("The destination track no longer exists.".into());
        return out;
    };
    if art.colors.len() > MAX_COLORS {
        out.error = Some(format!(
            "The artwork has {} colors; at most {MAX_COLORS} can be pasted at once.",
            art.colors.len()
        ));
        return out;
    }
    // v0.9 256-Key Mode: artwork above the project's highest key is refused,
    // never folded onto it.
    let keys = doc.project().keys;
    if let Some(n) = art.notes.iter().find(|n| !keys.contains(n.pitch)) {
        out.error = Some(format!(
            "The artwork has notes up to key {}, above this project's highest key ({}).",
            art.notes.iter().map(|n| n.pitch).max().unwrap_or(n.pitch),
            keys.max_pitch()
        ));
        return out;
    }
    if let Some(n) = art
        .notes
        .iter()
        .find(|n| n.color as usize >= art.colors.len())
    {
        out.error = Some(format!(
            "Internal error: a note uses color {} outside the palette.",
            n.color
        ));
        return out;
    }
    let counts = art.counts();
    let table = dest.subchannels();
    let mut last_channel = table.entries().last().map_or(0, |e| e.midi_channel);
    let track_channel = dest.props().default_channel.min(15);
    let mut channel_for = |c: u32| -> u8 {
        match spec.channel {
            ChannelRule::Next { skip_10 } => {
                last_channel = next_midi_channel(last_channel, skip_10);
                last_channel
            }
            ChannelRule::ByColor => (c % 16) as u8,
            ChannelRule::TrackChannel => track_channel,
            ChannelRule::Fixed(ch) => ch.min(15),
        }
    };
    let mut below: Vec<u32> = Vec::new();
    for (c, &count) in counts.iter().enumerate() {
        let c = c as u32;
        if count == 0 {
            continue;
        }
        let info = &art.colors[c as usize];
        let dest_of = match spec.mode {
            PasteMode::Subtracks => Dest::NewSubtrack {
                name: format!("{} {}", spec.prefix, info.name),
                rgb: info.rgb,
                channel: channel_for(c),
            },
            PasteMode::Subchannels => {
                let raw = i64::from(c) + spec.sub_offset;
                let id = if raw < 0 {
                    match spec.below_zero {
                        BelowZero::Refuse => {
                            below.push(c);
                            Some(0)
                        }
                        BelowZero::Clamp => Some(0),
                        BelowZero::Drop => None,
                    }
                } else {
                    Some(raw)
                };
                match id {
                    None => Dest::Skipped("below the first Subchannel"),
                    Some(id) if id > MAX_SUB_ID => {
                        out.error = Some(format!(
                            "Subchannel ids above {MAX_SUB_ID} are not supported; lower the offset."
                        ));
                        Dest::Skipped("offset too large")
                    }
                    Some(id) => {
                        let sid = SubchannelId(id as u32);
                        match table.get(sid) {
                            Some(e) => Dest::Subchannel {
                                id: sid,
                                channel: e.midi_channel,
                                new: false,
                            },
                            None => {
                                // Several colours may land on one new id
                                // (Clamp): one Subchannel, made once.
                                let made = out.colors.iter().find_map(|p| match p.dest {
                                    Dest::Subchannel {
                                        id,
                                        channel,
                                        new: true,
                                    } if id == sid => Some(channel),
                                    _ => None,
                                });
                                Dest::Subchannel {
                                    id: sid,
                                    channel: made.unwrap_or_else(|| channel_for(c)),
                                    new: true,
                                }
                            }
                        }
                    }
                }
            }
            PasteMode::Existing => match spec
                .existing
                .get(c as usize)
                .copied()
                .unwrap_or(ExistingDest::Skip)
            {
                ExistingDest::Skip => Dest::Skipped("skipped"),
                ExistingDest::Track(id) => match doc.track_by_id(id) {
                    Some(t) => {
                        let e = t.subchannels().first();
                        Dest::Track {
                            id,
                            sub: e.id,
                            channel: e.midi_channel,
                        }
                    }
                    None => {
                        out.error = Some(format!(
                            "The track chosen for color {} no longer exists.",
                            info.name
                        ));
                        Dest::Skipped("missing track")
                    }
                },
                ExistingDest::Subchannel(id) => match table.get(id) {
                    Some(e) => Dest::Subchannel {
                        id,
                        channel: e.midi_channel,
                        new: false,
                    },
                    None => {
                        out.error = Some(format!(
                            "Subchannel {} chosen for color {} is not in the destination track.",
                            id.number(),
                            info.name
                        ));
                        Dest::Skipped("missing subchannel")
                    }
                },
            },
        };
        if matches!(dest_of, Dest::Skipped(_)) {
            out.skipped += count;
        } else {
            out.notes += count;
        }
        out.colors.push(ColorPlan {
            color: c,
            notes: count,
            dest: dest_of,
        });
    }
    if !below.is_empty() && out.error.is_none() {
        let need = -i64::from(*below.iter().min().expect("non-empty"));
        out.error = Some(format!(
            "With offset {} color(s) {} would land below the first Subchannel. Use an offset of at least {need}, or choose to clamp or leave them out.",
            spec.sub_offset,
            colour_list(&below)
        ));
    }
    if let Some(n) = art.notes.last()
        && n.start.checked_add(spec.at).is_none()
    {
        out.error = Some("The artwork would end beyond the last possible tick.".into());
    }
    if art
        .notes
        .iter()
        .any(|n| n.dur == 0 || n.start.saturating_add(spec.at).checked_add(n.dur).is_none())
        && out.error.is_none()
    {
        out.error = Some(
            "The artwork has notes without length or ending beyond the last possible tick.".into(),
        );
    }
    out
}

fn colour_list(v: &[u32]) -> String {
    let shown: Vec<String> = v.iter().take(8).map(|c| c.to_string()).collect();
    let mut s = shown.join(", ");
    if v.len() > 8 {
        s.push_str(&format!(" and {} more", v.len() - 8));
    }
    s
}

fn record(n: &ColorNote, at: u64, ch: u8, sub: SubchannelId) -> NoteRecord {
    NoteRecord {
        start_tick: n.start + at,
        duration_ticks: n.dur.max(1),
        // The plan checked the pitch against the project's key range.
        attrs: NoteAttrs::new_wide(n.pitch, n.vel.clamp(1, 127), ch, 64, NoteOffKind::NoteOff)
            .expect("clamped attributes are valid"),
        sub,
        group: 0,
    }
}

/// The undo operation that pastes `art` as `plan` says (one step). Refuses
/// with the plan's error, or [`TransformError::Stale`] when the destination
/// changed since planning.
pub fn build(
    doc: &PlumDocument,
    art: &Generated,
    spec: &PasteSpec,
    plan: &PastePlan,
    label: &'static str,
) -> Result<Operation, TransformError> {
    if let Some(e) = &plan.error {
        return Err(TransformError::Invalid(e.clone()));
    }
    if plan.notes == 0 {
        return Err(TransformError::Invalid("There is nothing to paste.".into()));
    }
    // The plan must still describe the document.
    if *plan != self::plan(doc, art, spec) {
        return Err(TransformError::Stale);
    }
    let dest_index = doc.index_of(spec.dest).ok_or(TransformError::Stale)?;
    let dest = doc.track(dest_index).ok_or(TransformError::Stale)?;
    let at = spec.at;
    let by_color: Vec<Option<&Dest>> = {
        let mut v = vec![None; art.colors.len()];
        for p in &plan.colors {
            if !matches!(p.dest, Dest::Skipped(_)) {
                v[p.color as usize] = Some(&p.dest);
            }
        }
        v
    };
    let mut ops: Vec<Operation> = Vec::new();
    // Notes into the destination's Subchannels (new ones first).
    let mut new_subs: Vec<Subchannel> = Vec::new();
    for p in &plan.colors {
        if let Dest::Subchannel {
            id,
            channel,
            new: true,
        } = p.dest
            && !new_subs.iter().any(|s| s.id == id)
        {
            let mut s = Subchannel::new(id, channel);
            s.name = Some(format!(
                "{} {}",
                spec.prefix, art.colors[p.color as usize].name
            ));
            new_subs.push(s);
        }
    }
    new_subs.sort_by_key(|s| s.id);
    let mut dest_records: Vec<NoteRecord> = Vec::new();
    let mut other: std::collections::BTreeMap<TrackId, Vec<NoteRecord>> = Default::default();
    let mut subtracks: std::collections::BTreeMap<u32, Vec<NoteRecord>> = Default::default();
    for n in &art.notes {
        match by_color[n.color as usize] {
            Some(Dest::Subchannel { id, channel, .. }) => {
                dest_records.push(record(n, at, *channel, *id));
            }
            Some(Dest::Track { id, sub, channel }) => {
                let r = record(n, at, *channel, *sub);
                if *id == spec.dest {
                    dest_records.push(r);
                } else {
                    other.entry(*id).or_default().push(r);
                }
            }
            Some(Dest::NewSubtrack { channel, .. }) => {
                subtracks.entry(n.color).or_default().push(record(
                    n,
                    at,
                    *channel,
                    SubchannelId::MAIN,
                ));
            }
            Some(Dest::Skipped(_)) | None => {}
        }
    }
    let label_ops = |track: TrackId,
                     base: u64,
                     mut records: Vec<NoteRecord>,
                     subs: Vec<Subchannel>|
     -> Operation {
        records.sort_by_key(|r| r.start_tick);
        Operation::Prepared {
            edit: crate::editor::PreparedEdit::InsertSelected {
                host: crate::model::ContentRef::Track(track),
                base_revision: base,
                records,
                subs_track: track,
                new_subs: subs,
            },
            label,
        }
    };
    if !dest_records.is_empty() || !new_subs.is_empty() {
        ops.push(label_ops(
            spec.dest,
            dest.notes.revision(),
            dest_records,
            new_subs,
        ));
    }
    for (id, recs) in other {
        let t = doc.track_by_id(id).ok_or(TransformError::Stale)?;
        ops.push(label_ops(id, t.notes.revision(), recs, Vec::new()));
    }
    if !subtracks.is_empty() {
        let mut tracks = Vec::with_capacity(subtracks.len());
        let mut next = doc.next_track_id();
        for (color, mut recs) in subtracks {
            let Some(Dest::NewSubtrack { name, rgb, channel }) = by_color[color as usize] else {
                continue;
            };
            recs.sort_by_key(|r| r.start_tick);
            let mut store = NoteStore::new();
            store
                .insert_sorted_batch(&recs, false)
                .map_err(|e| TransformError::Invalid(e.to_string()))?;
            let mut t = Track::named(TrackId(next), name);
            next = next.checked_add(1).ok_or_else(|| {
                TransformError::Invalid("The project has too many tracks.".into())
            })?;
            t.notes = store;
            t.set_parent(Some(spec.dest));
            t.set_subchannels(SubchannelTable::single(*channel));
            let mut props = t.props().clone();
            props.default_channel = *channel;
            props.onion_color = Some(*rgb);
            t.set_props(props, None);
            tracks.push(t);
        }
        ops.push(Operation::InsertTracks {
            index: doc.subtree_end(dest_index),
            tracks,
            label,
        });
    }
    Ok(match ops.len() {
        0 => return Err(TransformError::Invalid("There is nothing to paste.".into())),
        1 => ops.pop().expect("one"),
        _ => Operation::Group { ops, label },
    })
}
