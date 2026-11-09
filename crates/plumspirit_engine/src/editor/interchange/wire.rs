//! Version 1 wire format. See docs/MUSICAL_CLIPBOARD.md.

use super::{ClipBlock, Music, Origin, Result, add_tick, valid_division};
use crate::codec::{Crc32, get_uvarint, uvarint_len};
use crate::editor::EditControl;
use crate::editor::clipboard::Clipboard;
use crate::editor::clips::{ClipClipboard, ClipEntry};
use crate::editor::controllers::{ControllerKind, EventClip};
use crate::model::clip::{ContentId, MAX_CLIP_NAME_CHARS};
use crate::model::conductor::MASTER_VOLUME_MAX;
use crate::model::events::{EventStore, RawEvent, RawKind, channel_data_len};
use crate::model::group::MAX_GROUP_NAME_CHARS;
use crate::model::note::NoteAttrs;
use crate::model::project::KeyRange;
use crate::model::timing::Division;
use crate::model::track::{MAX_SUBCHANNEL_NAME_CHARS, Subchannel};
use crate::model::{GroupInfo, GroupTable, NoteRecord, OwnedContent, SubchannelId};

const MAGIC: &[u8; 8] = b"PLUMCLIP";
const VERSION: u16 = 1;
const HEADER: usize = 28;
const TRAILER: usize = 4;
pub const MAX_WIRE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NOTES: usize = 4_000_000;
const MAX_DESCRIPTORS: usize = 65_536;
const MAX_CLIPS: usize = 16_384;
const MAX_EVENTS: usize = 1_000_000;
const INVALID: &str = "Invalid or damaged PlumSpirit musical clipboard.";
const TOO_LARGE: &str =
    "This selection is too large for the musical clipboard. Copy it in smaller parts.";
const MEMORY: &str = "There is not enough memory for this musical clipboard.";

pub fn wire_len(origin: Origin, music: &Music) -> Result<usize> {
    measure(origin, music, EditControl::default())
}

fn measure(origin: Origin, music: &Music, control: EditControl<'_>) -> Result<usize> {
    let mut writer = Writer {
        data: None,
        len: HEADER,
        control,
        next_check: 0,
    };
    write_body(&mut writer, origin, music)?;
    Ok(writer.len + TRAILER)
}

pub fn encode(origin: Origin, music: &Music) -> Result<Vec<u8>> {
    encode_controlled(origin, music, EditControl::default())
}

pub fn encode_controlled(
    origin: Origin,
    music: &Music,
    control: EditControl<'_>,
) -> Result<Vec<u8>> {
    let len = measure(origin, music, control)?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| MEMORY)?;
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&VERSION.to_le_bytes());
    data.push(match music {
        Music::Notes(_) => 1,
        Music::Clips(_) => 2,
        Music::Controller { .. } => 3,
        Music::Master(_) => 4,
    });
    data.push(0);
    data.extend_from_slice(&(len as u32).to_le_bytes());
    data.extend_from_slice(&origin.division.encoded().to_le_bytes());
    data.extend_from_slice(&origin.anchor.to_le_bytes());
    let mut writer = Writer {
        data: Some(data),
        len: HEADER,
        control,
        next_check: 0,
    };
    write_body(&mut writer, origin, music)?;
    let mut data = writer.data.expect("materializing writer");
    data.extend_from_slice(&checksum(&data, control)?.to_le_bytes());
    Ok(data)
}

pub fn decode(bytes: &[u8]) -> Result<(Origin, Music)> {
    decode_controlled(bytes, EditControl::default())
}

pub fn decode_controlled<'a>(bytes: &'a [u8], control: EditControl<'a>) -> Result<(Origin, Music)> {
    if !(HEADER + 1 + TRAILER..=MAX_WIRE_BYTES).contains(&bytes.len()) || &bytes[..8] != MAGIC {
        return Err(INVALID.into());
    }
    let mut r = Reader {
        bytes,
        pos: MAGIC.len(),
        control,
        next_check: 0,
    };
    if r.u16()? != VERSION {
        return Err("This musical clipboard uses an unsupported version.".into());
    }
    let tag = r.byte()?;
    if r.byte()? != 0 || r.u32()? as usize != bytes.len() {
        return Err(INVALID.into());
    }
    let division = Division::from_encoded(r.u32()?)
        .filter(|d| valid_division(*d))
        .ok_or(INVALID)?;
    let anchor = r.u64()?;
    let (body, crc) = bytes.split_at(bytes.len() - TRAILER);
    if checksum(body, control)?.to_le_bytes() != crc {
        return Err(INVALID.into());
    }
    r.bytes = body;
    let keys = if r.flag()? {
        KeyRange::Extended256
    } else {
        KeyRange::Standard
    };
    let mut remaining_notes = MAX_NOTES;
    let mut remaining_subs = MAX_DESCRIPTORS;
    let mut remaining_events = MAX_EVENTS;
    let music = match tag {
        1 => {
            let subs = r.subs(&mut remaining_subs)?;
            let groups = r.groups()?;
            let records = r.notes(keys, &subs, &groups, &mut remaining_notes)?;
            if records.first().is_none_or(|r| r.start_tick != 0) {
                return Err(INVALID.into());
            }
            Music::Notes(Clipboard {
                records,
                subs,
                groups,
            })
        }
        2 => {
            let groups = r.groups()?;
            let count = r.count(MAX_CLIPS, 12)?;
            if count == 0 {
                return Err(INVALID.into());
            }
            let mut entries = reserved(count)?;
            let mut all_subs = reserved(count)?;
            for _ in 0..count {
                let rows_below = r.var32()? as usize;
                let after = r.var()?;
                let offset = r.var()?;
                let length = if r.flag()? { Some(r.var()?) } else { None };
                if let Some(len) = length {
                    if len == 0 {
                        return Err(INVALID.into());
                    }
                    add_tick(offset, len)?;
                    add_tick(after, len)?;
                }
                let muted = r.flag()?;
                let name = r.text(MAX_CLIP_NAME_CHARS)?;
                let color = r.rgb()?;
                let mut content = OwnedContent::new(ContentId(0));
                content.name = r.text(MAX_CLIP_NAME_CHARS)?;
                content.color = r.rgb()?;
                let subs = r.subs(&mut remaining_subs)?;
                let records = r.notes(keys, &subs, &groups, &mut remaining_notes)?;
                content
                    .notes
                    .insert_sorted_batch(&records, false)
                    .map_err(|e| e.to_string())?;
                drop(records);
                content.events = r.events(&mut remaining_events)?;
                entries.push(ClipEntry {
                    rows_below,
                    after,
                    offset,
                    length,
                    muted,
                    name,
                    color,
                    content,
                });
                all_subs.push(subs);
            }
            if !entries.iter().any(|e| e.after == 0) || !entries.iter().any(|e| e.rows_below == 0) {
                return Err(INVALID.into());
            }
            Music::Clips(ClipBlock {
                clips: ClipClipboard { entries },
                subs: all_subs,
                groups,
            })
        }
        3 => {
            let channel = r.byte()?;
            if channel >= 16 {
                return Err(INVALID.into());
            }
            let kind = match r.byte()? {
                n @ 0..=127 => ControllerKind::Cc(n),
                128 => ControllerKind::PitchBend,
                _ => return Err(INVALID.into()),
            };
            Music::Controller {
                clip: EventClip {
                    kind: Some(kind),
                    points: r.points(kind.max())?,
                },
                channel,
            }
        }
        4 => Music::Master(r.points(MASTER_VOLUME_MAX)?),
        _ => return Err(INVALID.into()),
    };
    if r.pos != r.bytes.len() {
        return Err(INVALID.into());
    }
    Ok((
        Origin {
            division,
            keys,
            anchor,
        },
        music,
    ))
}

fn write_body(w: &mut Writer<'_>, origin: Origin, music: &Music) -> Result<()> {
    if !valid_division(origin.division) {
        return Err("This project does not have a supported MIDI timebase.".into());
    }
    w.flag(origin.keys == KeyRange::Extended256)?;
    let mut remaining_notes = MAX_NOTES;
    let mut remaining_subs = MAX_DESCRIPTORS;
    let mut remaining_events = MAX_EVENTS;
    match music {
        Music::Notes(clip) => {
            w.subs(&clip.subs, &mut remaining_subs)?;
            w.groups(&clip.groups)?;
            w.notes(
                clip.records.len(),
                clip.records.iter().copied(),
                origin.keys,
                &clip.subs,
                &clip.groups,
                &mut remaining_notes,
            )
        }
        Music::Clips(block) => {
            w.groups(&block.groups)?;
            w.count(block.clips.entries.len(), MAX_CLIPS)?;
            for (e, subs) in block.clips.entries.iter().zip(&block.subs) {
                w.var(e.rows_below as u64)?;
                w.var(e.after)?;
                w.var(e.offset)?;
                w.flag(e.length.is_some())?;
                if let Some(len) = e.length {
                    add_tick(e.offset, len)?;
                    add_tick(e.after, len)?;
                    w.var(len)?;
                }
                w.flag(e.muted)?;
                w.text(e.name.as_deref(), MAX_CLIP_NAME_CHARS)?;
                w.rgb(e.color)?;
                w.text(e.content.name.as_deref(), MAX_CLIP_NAME_CHARS)?;
                w.rgb(e.content.color)?;
                w.subs(subs, &mut remaining_subs)?;
                w.notes(
                    e.content.notes.len(),
                    e.content.notes.iter().map(|n| n.record()),
                    origin.keys,
                    subs,
                    &block.groups,
                    &mut remaining_notes,
                )?;
                w.events(&e.content.events, &mut remaining_events)?;
            }
            Ok(())
        }
        Music::Controller { clip, channel } => {
            let kind = clip.kind.ok_or(INVALID)?;
            if *channel >= 16 {
                return Err(INVALID.into());
            }
            w.bytes(&[
                *channel,
                match kind {
                    ControllerKind::Cc(n @ 0..=127) => n,
                    ControllerKind::PitchBend => 128,
                    _ => return Err(INVALID.into()),
                },
            ])?;
            w.points(&clip.points, kind.max())
        }
        Music::Master(points) => w.points(points, MASTER_VOLUME_MAX),
    }
}

fn checksum(bytes: &[u8], control: EditControl<'_>) -> Result<u32> {
    let mut crc = Crc32::new();
    for (i, part) in bytes.chunks(256 * 1024).enumerate() {
        control
            .tick((i * 256 * 1024) as u64)
            .map_err(|e| e.user_message())?;
        crc.update(part);
    }
    Ok(crc.finish())
}

struct Writer<'a> {
    data: Option<Vec<u8>>,
    len: usize,
    control: EditControl<'a>,
    next_check: usize,
}

impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if self.len + bytes.len() > MAX_WIRE_BYTES - TRAILER {
            return Err(TOO_LARGE.into());
        }
        self.len += bytes.len();
        if self.len >= self.next_check {
            self.control
                .tick(self.len as u64)
                .map_err(|e| e.user_message())?;
            self.next_check = self.len + 256 * 1024;
        }
        if let Some(data) = &mut self.data {
            data.extend_from_slice(bytes);
        }
        Ok(())
    }

    fn flag(&mut self, flag: bool) -> Result<()> {
        self.bytes(&[u8::from(flag)])
    }

    fn var(&mut self, mut value: u64) -> Result<()> {
        let mut bytes = [0; 10];
        let len = uvarint_len(value);
        for byte in &mut bytes[..len - 1] {
            *byte = (value as u8) | 0x80;
            value >>= 7;
        }
        bytes[len - 1] = value as u8;
        self.bytes(&bytes[..len])
    }

    fn count(&mut self, count: usize, limit: usize) -> Result<()> {
        if count > limit {
            return Err(TOO_LARGE.into());
        }
        self.var(count as u64)
    }

    fn text(&mut self, text: Option<&str>, max: usize) -> Result<()> {
        if let Some(text) = text {
            if text.chars().count() > max {
                return Err(INVALID.into());
            }
            self.var(text.len() as u64 + 1)?;
            self.bytes(text.as_bytes())
        } else {
            self.var(0)
        }
    }

    fn rgb(&mut self, color: Option<u32>) -> Result<()> {
        self.flag(color.is_some())?;
        if let Some(rgb) = color {
            if rgb > 0xFF_FF_FF {
                return Err(INVALID.into());
            }
            self.bytes(&rgb.to_le_bytes()[..3])?;
        }
        Ok(())
    }

    fn subs(&mut self, subs: &[Subchannel], remaining: &mut usize) -> Result<()> {
        self.count(subs.len(), *remaining)?;
        *remaining -= subs.len();
        for sub in subs {
            sub.validate()?;
            self.var(u64::from(sub.id.0))?;
            self.bytes(&[sub.midi_channel])?;
            self.text(sub.name.as_deref(), MAX_SUBCHANNEL_NAME_CHARS)?;
        }
        Ok(())
    }

    fn groups(&mut self, groups: &[GroupInfo]) -> Result<()> {
        self.count(groups.len(), MAX_DESCRIPTORS)?;
        for g in groups {
            self.var(u64::from(g.id))?;
            self.var(u64::from(g.parent))?;
            self.var(u64::from(g.link))?;
            self.flag(g.locked)?;
            self.flag(g.temporary)?;
            self.text(g.name.as_deref(), MAX_GROUP_NAME_CHARS)?;
            self.flag(g.color.is_some())?;
            if let Some(color) = g.color {
                self.bytes(&color)?;
            }
        }
        Ok(())
    }

    fn notes(
        &mut self,
        count: usize,
        notes: impl Iterator<Item = NoteRecord>,
        keys: KeyRange,
        subs: &[Subchannel],
        groups: &[GroupInfo],
        remaining: &mut usize,
    ) -> Result<()> {
        self.count(count, *remaining)?;
        *remaining -= count;
        let mut previous = 0;
        for n in notes {
            validate_note(n, keys, subs, groups)?;
            self.var(n.start_tick.checked_sub(previous).ok_or(INVALID)?)?;
            self.var(n.duration_ticks)?;
            self.bytes(&n.attrs.raw().to_le_bytes())?;
            self.var(u64::from(n.sub.0))?;
            self.var(u64::from(n.group))?;
            previous = n.start_tick;
        }
        Ok(())
    }

    fn events(&mut self, events: &EventStore, remaining: &mut usize) -> Result<()> {
        self.count(events.events().len(), *remaining)?;
        *remaining -= events.events().len();
        let mut previous = 0;
        for e in events.events() {
            let RawKind::Channel { status, data } = e.kind else {
                return Err(INVALID.into());
            };
            let len = event_data_len(status, data)?;
            self.var(e.tick.checked_sub(previous).ok_or(INVALID)?)?;
            self.var(u64::from(e.anchor))?;
            self.bytes(&[status])?;
            self.bytes(&data[..len])?;
            previous = e.tick;
        }
        Ok(())
    }

    fn points(&mut self, points: &[(u64, u16)], max: u16) -> Result<()> {
        if points.is_empty() || points[0].0 != 0 {
            return Err(INVALID.into());
        }
        self.count(points.len(), MAX_EVENTS)?;
        let mut previous = 0;
        for (i, &(tick, value)) in points.iter().enumerate() {
            if value > max || (i != 0 && tick <= previous) {
                return Err(INVALID.into());
            }
            self.var(tick - previous)?;
            self.var(u64::from(value))?;
            previous = tick;
        }
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    control: EditControl<'a>,
    next_check: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(size).ok_or(INVALID)?;
        let bytes = self.bytes.get(self.pos..end).ok_or(INVALID)?;
        self.pos = end;
        if end >= self.next_check {
            self.control
                .tick(end as u64)
                .map_err(|e| e.user_message())?;
            self.next_check = end + 256 * 1024;
        }
        Ok(bytes)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| INVALID.into())
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.fixed::<1>()?[0])
    }

    fn flag(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(INVALID.into()),
        }
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.fixed()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    fn var(&mut self) -> Result<u64> {
        let pos = self.pos;
        let v = get_uvarint(self.bytes, &mut self.pos).ok_or(INVALID)?;
        if self.pos - pos != uvarint_len(v) {
            return Err(INVALID.into());
        }
        Ok(v)
    }

    fn var32(&mut self) -> Result<u32> {
        u32::try_from(self.var()?).map_err(|_| INVALID.into())
    }

    fn count(&mut self, limit: usize, minimum: usize) -> Result<usize> {
        let n = self.var32()? as usize;
        if n > limit || n > (self.bytes.len() - self.pos) / minimum {
            return Err(INVALID.into());
        }
        Ok(n)
    }

    fn text(&mut self, max: usize) -> Result<Option<String>> {
        let Some(n) = self.var32()?.checked_sub(1) else {
            return Ok(None);
        };
        if n as usize > max * 4 {
            return Err(INVALID.into());
        }
        let text = std::str::from_utf8(self.take(n as usize)?).map_err(|_| INVALID)?;
        if text.chars().count() > max {
            return Err(INVALID.into());
        }
        Ok(Some(text.to_owned()))
    }

    fn rgb(&mut self) -> Result<Option<u32>> {
        if !self.flag()? {
            return Ok(None);
        }
        let [r, g, b] = self.fixed()?;
        Ok(Some(u32::from_le_bytes([r, g, b, 0])))
    }

    fn subs(&mut self, remaining: &mut usize) -> Result<Vec<Subchannel>> {
        let count = self.count(*remaining, 3)?;
        *remaining -= count;
        let mut subs = reserved::<Subchannel>(count)?;
        for _ in 0..count {
            let id = SubchannelId(self.var32()?);
            if subs.last().is_some_and(|s| s.id >= id) {
                return Err(INVALID.into());
            }
            let sub = Subchannel {
                id,
                midi_channel: self.byte()?,
                name: self.text(MAX_SUBCHANNEL_NAME_CHARS)?,
            };
            sub.validate()?;
            subs.push(sub);
        }
        Ok(subs)
    }

    fn groups(&mut self) -> Result<Vec<GroupInfo>> {
        let count = self.count(MAX_DESCRIPTORS, 7)?;
        let mut groups = reserved::<GroupInfo>(count)?;
        for _ in 0..count {
            let id = self.var32()?;
            if groups.last().is_some_and(|g| g.id >= id) {
                return Err(INVALID.into());
            }
            groups.push(GroupInfo {
                id,
                parent: self.var32()?,
                link: self.var32()?,
                locked: self.flag()?,
                temporary: self.flag()?,
                name: self.text(MAX_GROUP_NAME_CHARS)?,
                color: if self.flag()? {
                    Some(self.fixed()?)
                } else {
                    None
                },
            });
        }
        GroupTable::from_entries(groups.clone(), 1)?;
        Ok(groups)
    }

    fn notes(
        &mut self,
        keys: KeyRange,
        subs: &[Subchannel],
        groups: &[GroupInfo],
        remaining: &mut usize,
    ) -> Result<Vec<NoteRecord>> {
        let count = self.count(*remaining, 8)?;
        *remaining -= count;
        let mut records = reserved(count)?;
        let mut start = 0;
        for _ in 0..count {
            start = add_tick(start, self.var()?)?;
            let n = NoteRecord {
                start_tick: start,
                duration_ticks: self.var()?,
                attrs: NoteAttrs::from_raw(self.u32()?).ok_or(INVALID)?,
                sub: SubchannelId(self.var32()?),
                group: self.var32()?,
            };
            validate_note(n, keys, subs, groups)?;
            records.push(n);
        }
        Ok(records)
    }

    fn events(&mut self, remaining: &mut usize) -> Result<EventStore> {
        let count = self.count(*remaining, 4)?;
        *remaining -= count;
        let mut tick = 0;
        let mut store = EventStore::new();
        for _ in 0..count {
            tick = add_tick(tick, self.var()?)?;
            let anchor = self.var32()?;
            let status = self.byte()?;
            let mut data = [0; 2];
            let len = channel_data_len(status);
            data[..len].copy_from_slice(self.take(len)?);
            event_data_len(status, data)?;
            store
                .push(RawEvent {
                    tick,
                    anchor,
                    kind: RawKind::Channel { status, data },
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(store)
    }

    fn points(&mut self, max: u16) -> Result<Vec<(u64, u16)>> {
        let count = self.count(MAX_EVENTS, 2)?;
        if count == 0 {
            return Err(INVALID.into());
        }
        let mut points = reserved(count)?;
        let mut tick = 0;
        for i in 0..count {
            let delta = self.var()?;
            let value = u16::try_from(self.var()?).map_err(|_| INVALID)?;
            if value > max || (delta == 0) != (i == 0) {
                return Err(INVALID.into());
            }
            tick = add_tick(tick, delta)?;
            points.push((tick, value));
        }
        Ok(points)
    }
}

fn reserved<T>(count: usize) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| MEMORY)?;
    Ok(out)
}

fn validate_note(
    n: NoteRecord,
    keys: KeyRange,
    subs: &[Subchannel],
    groups: &[GroupInfo],
) -> Result<()> {
    if n.attrs.pitch() > keys.max_pitch()
        || n.end_tick().is_none()
        || subs.binary_search_by_key(&n.sub, |s| s.id).is_err()
        || (n.group != 0 && groups.binary_search_by_key(&n.group, |g| g.id).is_err())
    {
        return Err(INVALID.into());
    }
    Ok(())
}

fn event_data_len(status: u8, data: [u8; 2]) -> Result<usize> {
    let len = channel_data_len(status);
    if !(0x80..=0xEF).contains(&status) || data[..len].iter().any(|&v| v > 127) {
        return Err(INVALID.into());
    }
    Ok(len)
}
