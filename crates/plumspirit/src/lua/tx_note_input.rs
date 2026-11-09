//! Typed source adapters share validation without allocating a Lua table per
//! column row. Keep existing 0.1 get/raw_get and integer conversion semantics.
use super::*;

pub(super) struct Columns {
    starts: Table,
    gates: Option<Table>,
    keys: Option<Table>,
    velocities: Option<Table>,
    channels: Option<Table>,
    subs: Option<Table>,
    muted: Option<Table>,
    off_kind: Option<Table>,
    off_velocity: Option<Table>,
    group: Option<Table>,
}
impl Columns {
    pub(super) fn raw(notes: &Table, starts: Table) -> mlua::Result<Self> {
        Ok(Self {
            starts,
            gates: notes.raw_get("gate")?,
            keys: notes.raw_get("key")?,
            velocities: notes.raw_get("velocity")?,
            channels: notes.raw_get("channel")?,
            subs: notes.raw_get("subchannel")?,
            muted: notes.raw_get("muted")?,
            off_kind: notes.raw_get("off_kind")?,
            off_velocity: notes.raw_get("off_velocity")?,
            group: notes.raw_get("group")?,
        })
    }
    pub(super) fn value(&self, key: &str, index: usize) -> mlua::Result<Value> {
        match self.column(key) {
            Some(table) => table.raw_get(index),
            None => Ok(Value::Nil),
        }
    }
    pub(super) fn new(notes: &Table, starts: Table) -> mlua::Result<Self> {
        Ok(Self {
            starts,
            gates: notes.get("gate")?,
            keys: notes.get("key")?,
            velocities: notes.get("velocity")?,
            channels: notes.get("channel")?,
            subs: notes.get("subchannel")?,
            muted: notes.raw_get("muted")?,
            off_kind: notes.raw_get("off_kind")?,
            off_velocity: notes.raw_get("off_velocity")?,
            group: notes.raw_get("group")?,
        })
    }
    fn column(&self, key: &str) -> Option<&Table> {
        match key {
            "start" => Some(&self.starts),
            "gate" => self.gates.as_ref(),
            "key" => self.keys.as_ref(),
            "velocity" => self.velocities.as_ref(),
            "channel" => self.channels.as_ref(),
            "subchannel" => self.subs.as_ref(),
            "muted" => self.muted.as_ref(),
            "off_kind" => self.off_kind.as_ref(),
            "off_velocity" => self.off_velocity.as_ref(),
            "group" => self.group.as_ref(),
            _ => None,
        }
    }
}

pub(super) enum Input<'a> {
    Row(&'a Table),
    Column(&'a Columns, usize),
}
impl Input<'_> {
    pub(super) fn value(&self, key: &str) -> mlua::Result<Value> {
        match self {
            Self::Row(t) => t.raw_get(key),
            Self::Column(c, i) => c.value(key, *i),
        }
    }

    pub(super) fn tick(&self, key: &str) -> mlua::Result<Option<u64>> {
        let value = match self {
            Self::Row(table) => table.get(key)?,
            Self::Column(columns, index) => match columns.column(key) {
                Some(table) => table.raw_get(*index)?,
                None => Value::Nil,
            },
        };
        match value {
            Value::Nil => Ok(None),
            value => super::super::ticks::from_lua(&value, key).map(Some),
        }
    }
    pub(super) fn integer(&self, key: &str) -> mlua::Result<Option<i64>> {
        match self {
            Self::Row(table) => table.get(key),
            Self::Column(columns, index) => match columns.column(key) {
                Some(table) => table.raw_get(*index),
                None => Ok(None),
            },
        }
    }

    /// New optional fields never invoke a legacy row's __index handler.
    fn raw_integer(&self, key: &str) -> mlua::Result<Option<i64>> {
        match self {
            Self::Row(t) => t.raw_get(key),
            Self::Column(columns, index) => match columns.column(key) {
                Some(t) => t.raw_get(*index),
                None => Ok(None),
            },
        }
    }
}

/// Optional packed fields: no separate per-note allocations or identity.
#[derive(Clone, Debug, Default)]
pub(super) struct Metadata {
    muted: Option<bool>,
    off_kind: Option<NoteOffKind>,
    off_velocity: Option<u8>,
    group: Option<u32>,
}
impl Metadata {
    pub(super) fn parse(input: &Input<'_>) -> mlua::Result<Self> {
        let muted = match input.value("muted")? {
            Value::Nil => None,
            Value::Boolean(b) => Some(b),
            _ => return Err(runtime("muted must be a boolean.")),
        };
        let off_kind = match input.value("off_kind")? {
            Value::Nil => None,
            Value::String(s) if s.as_bytes().as_ref() == b"note_off" => Some(NoteOffKind::NoteOff),
            Value::String(s) if s.as_bytes().as_ref() == b"note_on_zero" => {
                Some(NoteOffKind::NoteOnZero)
            }
            _ => return Err(runtime("off_kind must be note_off or note_on_zero.")),
        };
        let off_velocity = input
            .raw_integer("off_velocity")?
            .map(|v| {
                u8::try_from(v)
                    .ok()
                    .filter(|v| *v <= 127)
                    .ok_or_else(|| runtime("off_velocity must be 0 - 127."))
            })
            .transpose()?;
        let group = input
            .raw_integer("group")?
            .map(|v| {
                u32::try_from(v)
                    .map_err(|_| runtime("group must be a group ID from 0 through 4294967295."))
            })
            .transpose()?;
        Ok(Self {
            muted,
            off_kind,
            off_velocity,
            group,
        })
    }
    pub(super) fn apply(&self, mut r: NoteRecord) -> mlua::Result<NoteRecord> {
        let kind = self.off_kind.unwrap_or(r.attrs.off_kind());
        if kind == NoteOffKind::NoteOnZero && self.off_velocity.is_some_and(|v| v != 0) {
            return Err(runtime("note_on_zero cannot have a nonzero off_velocity."));
        }
        r.attrs = r
            .attrs
            .with_note_off(self.off_velocity.unwrap_or(r.attrs.off_velocity()), kind)
            .map_err(|e| runtime(e.to_string()))?;
        if let Some(muted) = self.muted {
            r.attrs = r.attrs.with_muted(muted);
        }
        if let Some(group) = self.group {
            r.group = group;
        }
        Ok(r)
    }
}
