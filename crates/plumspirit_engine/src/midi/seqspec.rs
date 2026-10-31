//! PlumSpirit's own Sequencer-Specific meta events (`FF 7F`).
//!
//! Standard MIDI has no chapter spans, and a delta time cannot exceed
//! `0x0FFF_FFFF` ticks. PlumSpirit stores what it needs in Sequencer-Specific meta
//! events, which every other MIDI program ignores. Every PlumSpirit record starts
//! with a fixed namespace, so data from other programs is never mistaken for it:
//!
//! ```text
//! FF 7F len  7D 'P' 'l' 'u' 'm' 'S' 'p' 'i' 'r' 'i' 't'  type  version  body…
//! ```
//!
//! `7D` is the MIDI manufacturer id reserved for non-commercial use, followed by
//! the ASCII name. Records:
//!
//! | type | name | body |
//! |---|---|---|
//! | `01` | Chapter | `uvar id`, `uvar length in ticks`, title bytes (the rest) |
//! | `02` | Gap filler | empty — written only to split a delta time longer than a MIDI file allows; dropped on import |
//! | `03` | Track node | the chunk's PlumSpirit track, parent, layer, channel → subchannel map and (layer 0) subchannel table; see [`TrackNodeRecord`] |
//!
//! The only version is `01`; records with another version or a truncated body are
//! treated like any other unknown event (kept verbatim).

use crate::codec::{get_uvarint, put_uvarint};

/// Namespace every PlumSpirit record starts with.
pub const NAMESPACE: &[u8; 11] = b"\x7DPlumSpirit";
pub const VERSION: u8 = 1;
pub const TYPE_CHAPTER: u8 = 0x01;
pub const TYPE_GAP_FILLER: u8 = 0x02;
pub const TYPE_TRACK_NODE: u8 = 0x03;
/// Meta event type of Sequencer-Specific events.
pub const META_SEQUENCER_SPECIFIC: u8 = 0x7F;

/// The complete payload of a gap filler.
pub const GAP_FILLER: [u8; 13] = [
    0x7D,
    b'P',
    b'l',
    b'u',
    b'm',
    b'S',
    b'p',
    b'i',
    b'r',
    b'i',
    b't',
    TYPE_GAP_FILLER,
    VERSION,
];

/// `(type, body)` of a PlumSpirit record, or `None` for any other payload.
pub fn parse(payload: &[u8]) -> Option<(u8, &[u8])> {
    let rest = payload.strip_prefix(&NAMESPACE[..])?;
    match rest {
        [ty, VERSION, body @ ..] => Some((*ty, body)),
        _ => None,
    }
}

pub fn is_gap_filler(meta_type: u8, payload: &[u8]) -> bool {
    meta_type == META_SEQUENCER_SPECIFIC && payload == GAP_FILLER
}

/// A chapter record: id, length and title.
pub fn chapter_record(id: u32, length: u64, title: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(NAMESPACE.len() + 2 + 12 + title.len());
    out.extend_from_slice(NAMESPACE);
    out.push(TYPE_CHAPTER);
    out.push(VERSION);
    put_uvarint(&mut out, u64::from(id));
    put_uvarint(&mut out, length);
    out.extend_from_slice(title);
    out
}

/// Decodes a chapter record body.
pub fn parse_chapter(body: &[u8]) -> Option<(u32, u64, &[u8])> {
    let mut pos = 0;
    let id = u32::try_from(get_uvarint(body, &mut pos)?).ok()?;
    let length = get_uvarint(body, &mut pos)?;
    Some((id, length, &body[pos..]))
}

/// Text of the Marker written at a chapter's end (readable in other programs).
pub fn end_label(title: &[u8]) -> Vec<u8> {
    let mut v = title.to_vec();
    v.extend_from_slice(b" (end)");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_foreign_data_is_ignored() {
        let r = chapter_record(42, 123_456_789_012, "Verse ✨".as_bytes());
        let (ty, body) = parse(&r).unwrap();
        assert_eq!(ty, TYPE_CHAPTER);
        assert_eq!(
            parse_chapter(body),
            Some((42, 123_456_789_012, "Verse ✨".as_bytes()))
        );
        assert_eq!(parse(&GAP_FILLER), Some((TYPE_GAP_FILLER, &[][..])));
        assert!(is_gap_filler(0x7F, &GAP_FILLER));
        assert!(!is_gap_filler(0x01, &GAP_FILLER));
        // Other vendors' data, wrong versions and truncated records are not ours.
        assert_eq!(parse(&[0x00, 0x00, 0x41, 1, 2, 3]), None);
        assert_eq!(parse(b"\x7DPlumSpiri"), None);
        let mut v2 = GAP_FILLER.to_vec();
        v2[12] = 2;
        assert_eq!(parse(&v2), None);
        assert_eq!(parse_chapter(&[0x80]), None);
        assert_eq!(end_label(b"Intro"), b"Intro (end)".to_vec());
    }
}

/// PlumSpirit structure of one exported `MTrk` chunk (v0.5). Written at the end
/// of the chunk (just before End of Track) so it never changes the order of the
/// chunk's other events. Other programs ignore it; PlumSpirit uses it to rebuild
/// the track tree and the subchannels when the file is opened again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackNodeRecord {
    /// The PlumSpirit track (its id in the exporting project).
    pub node: u32,
    /// Parent track id, `None` for a top-level track.
    pub parent: Option<u32>,
    /// 0 = the track's own chunk; ≥ 1 = an extra chunk holding only notes of
    /// subchannels that share a MIDI channel with another subchannel.
    pub layer: u32,
    /// Which subchannel the notes of each MIDI channel in this chunk belong to,
    /// sorted by channel.
    pub map: Vec<(u8, u32)>,
    /// Layer 0 only: the track's subchannels as (id, MIDI channel, name).
    pub subs: Vec<(u32, u8, Option<Vec<u8>>)>,
}

impl TrackNodeRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(NAMESPACE.len() + 16 + self.map.len() * 3);
        out.extend_from_slice(NAMESPACE);
        out.push(TYPE_TRACK_NODE);
        out.push(VERSION);
        put_uvarint(&mut out, u64::from(self.node));
        put_uvarint(&mut out, self.parent.map_or(0, |p| u64::from(p) + 1));
        put_uvarint(&mut out, u64::from(self.layer));
        put_uvarint(&mut out, self.map.len() as u64);
        for &(ch, sub) in &self.map {
            out.push(ch);
            put_uvarint(&mut out, u64::from(sub));
        }
        put_uvarint(&mut out, self.subs.len() as u64);
        for (id, ch, name) in &self.subs {
            put_uvarint(&mut out, u64::from(*id));
            out.push(*ch);
            match name {
                Some(n) => {
                    put_uvarint(&mut out, n.len() as u64 + 1);
                    out.extend_from_slice(n);
                }
                None => put_uvarint(&mut out, 0),
            }
        }
        out
    }

    /// Decodes a record body; `None` if it is malformed in any way.
    pub fn decode(body: &[u8]) -> Option<TrackNodeRecord> {
        let mut pos = 0;
        let node = u32::try_from(get_uvarint(body, &mut pos)?).ok()?;
        let parent = match get_uvarint(body, &mut pos)? {
            0 => None,
            v => Some(u32::try_from(v - 1).ok()?),
        };
        let layer = u32::try_from(get_uvarint(body, &mut pos)?).ok()?;
        let n = usize::try_from(get_uvarint(body, &mut pos)?).ok()?;
        if n > 16 {
            return None;
        }
        let mut map = Vec::with_capacity(n);
        for _ in 0..n {
            let ch = *body.get(pos)?;
            pos += 1;
            if ch > 15 {
                return None;
            }
            let sub = u32::try_from(get_uvarint(body, &mut pos)?).ok()?;
            map.push((ch, sub));
        }
        if map.windows(2).any(|w| w[0].0 >= w[1].0) {
            return None;
        }
        let k = usize::try_from(get_uvarint(body, &mut pos)?).ok()?;
        if k > body.len() {
            return None;
        }
        let mut subs = Vec::with_capacity(k);
        for _ in 0..k {
            let id = u32::try_from(get_uvarint(body, &mut pos)?).ok()?;
            let ch = *body.get(pos)?;
            pos += 1;
            if ch > 15 {
                return None;
            }
            let name = match usize::try_from(get_uvarint(body, &mut pos)?).ok()? {
                0 => None,
                l => {
                    let bytes = body.get(pos..pos.checked_add(l - 1)?)?;
                    pos += l - 1;
                    Some(bytes.to_vec())
                }
            };
            subs.push((id, ch, name));
        }
        if pos != body.len() || subs.windows(2).any(|w| w[0].0 >= w[1].0) {
            return None;
        }
        Some(TrackNodeRecord {
            node,
            parent,
            layer,
            map,
            subs,
        })
    }
}

#[cfg(test)]
mod node_tests {
    use super::*;

    #[test]
    fn track_node_records_round_trip_and_reject_damage() {
        let r = TrackNodeRecord {
            node: 7,
            parent: Some(3),
            layer: 0,
            map: vec![(0, 0), (4, 12), (15, 1_000_000)],
            subs: vec![
                (0, 0, None),
                (12, 4, Some(b"Strings".to_vec())),
                (1_000_000, 15, None),
            ],
        };
        let bytes = r.encode();
        let (ty, body) = parse(&bytes).unwrap();
        assert_eq!(ty, TYPE_TRACK_NODE);
        assert_eq!(TrackNodeRecord::decode(body), Some(r));
        for cut in 0..body.len() {
            assert!(TrackNodeRecord::decode(&body[..cut]).is_none(), "cut {cut}");
        }
        let mut extra = body.to_vec();
        extra.push(0);
        assert!(TrackNodeRecord::decode(&extra).is_none());
    }
}
