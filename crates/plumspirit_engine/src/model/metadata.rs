//! Sparse plugin-owned project attributes. No note IDs or MIDI representation.
use super::track::TrackId;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_NAMESPACES: usize = 128;
pub const MAX_RECORDS: usize = 4096;
const TARGET_LAYOUT: u32 = u32::from_le_bytes(*b"XMD2");
pub const MAX_NODES: usize = 2048;
pub const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    /// IEEE bits; validated finite on admission. Preserves negative zero.
    Number(u64),
    Text(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub version: u32,
    pub value: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Project,
    Track(TrackId),
    Group(u32),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata(BTreeMap<(String, Target), Record>);

fn id_ok(s: &str) -> bool {
    (3..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// Native read-only review in canonical namespace/target order; no copies.
    pub fn records(&self) -> impl ExactSizeIterator<Item = (&str, Target, &Record)> {
        self.0
            .iter()
            .map(|((owner, target), record)| (owner.as_str(), *target, record))
    }
    pub fn get(&self, owner: &str) -> Option<&Record> {
        self.get_at(owner, Target::Project)
    }
    pub fn get_at(&self, owner: &str, target: Target) -> Option<&Record> {
        self.0.get(&(owner.into(), target))
    }
    pub fn set(&mut self, owner: &str, record: Option<Record>) -> Result<(), String> {
        self.set_at(owner, Target::Project, record)
    }
    /// Full prospective validation before replacement; owner cannot cross namespaces.
    pub fn set_at(
        &mut self,
        owner: &str,
        target: Target,
        record: Option<Record>,
    ) -> Result<(), String> {
        if !id_ok(owner) {
            return Err("Invalid metadata owner namespace.".into());
        }
        let mut next = self.clone();
        let key = (owner.into(), target);
        match record {
            Some(v) => {
                next.0.insert(key, v);
            }
            None => {
                next.0.remove(&key);
            }
        }
        next.validate()?;
        *self = next;
        Ok(())
    }
    /// Referenced IDs only, at most4096; no per-note scan or full track-ID copy.
    pub fn validate_targets(&self, doc: &super::PlumDocument) -> Result<(), String> {
        for (_, target) in self.0.keys() {
            if let Target::Group(id) = target
                && doc.project().groups.get(*id).is_none_or(|g| g.temporary)
            {
                return Err("Metadata requires an existing permanent Group.".into());
            }
        }
        let mut needed: BTreeSet<_> = self
            .0
            .keys()
            .filter_map(|(_, t)| match t {
                Target::Track(id) => Some(*id),
                _ => None,
            })
            .collect();
        if needed.is_empty() {
            return Ok(());
        }
        for t in doc.tracks() {
            needed.remove(&t.id());
            if needed.is_empty() {
                return Ok(());
            }
        }
        Err("Metadata refers to a missing Track.".into())
    }
    /// Native deletion uses this in its own history boundary; foreign owners included.
    pub fn removing_tracks(&self, ids: &[TrackId]) -> Option<Self> {
        if self.is_empty() {
            return None;
        }
        let ids: BTreeSet<_> = ids.iter().copied().collect();
        if !self
            .0
            .keys()
            .any(|(_, t)| matches!(t,Target::Track(id) if ids.contains(id)))
        {
            return None;
        }
        let mut next = self.clone();
        next.0
            .retain(|(_, t), _| !matches!(t,Target::Track(id) if ids.contains(id)));
        Some(next)
    }
    /// Deleted or newly temporary Groups lose associations with the same Undo step.
    pub fn removing_groups(&self, table: &super::GroupTable) -> Option<Self> {
        let gone = |target: &Target| matches!(target,Target::Group(id) if table.get(*id).is_none_or(|g|g.temporary));
        if !self.0.keys().any(|(_, t)| gone(t)) {
            return None;
        }
        let mut next = self.clone();
        next.0.retain(|(_, t), _| !gone(t));
        Some(next)
    }
    pub fn heap_bytes(&self) -> usize {
        fn value(v: &Value) -> usize {
            match v {
                Value::Text(s) => s.capacity(),
                Value::Array(a) => {
                    a.capacity() * std::mem::size_of::<Value>() + a.iter().map(value).sum::<usize>()
                }
                Value::Object(a) => a.iter().map(|(k, v)| 128 + k.capacity() + value(v)).sum(),
                _ => 0,
            }
        }
        self.0
            .iter()
            .map(|((k, _), r)| 144 + k.capacity() + value(&r.value))
            .sum()
    }
    pub fn validate(&self) -> Result<(), String> {
        self.encode().map(|_| ())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.0.len() > MAX_RECORDS {
            return Err("Metadata record limit exceeded.".into());
        }
        let mut owners = BTreeSet::new();
        let targeted = self.0.keys().any(|(_, t)| *t != Target::Project);
        let mut out = Vec::new();
        if targeted {
            out.extend(TARGET_LAYOUT.to_le_bytes());
        }
        put_u32(&mut out, self.0.len())?;
        for ((id, target), r) in &self.0 {
            owners.insert(id.as_str());
            if owners.len() > MAX_NAMESPACES {
                return Err("Metadata namespace limit exceeded.".into());
            }
            if !id_ok(id) || r.version == 0 {
                return Err("Invalid metadata owner/version.".into());
            }
            put_text(&mut out, id)?;
            if targeted {
                match target {
                    Target::Project => out.push(0),
                    Target::Track(id) => {
                        out.push(1);
                        out.extend(id.0.to_le_bytes());
                    }
                    Target::Group(id) => {
                        out.push(2);
                        out.extend(id.to_le_bytes());
                    }
                }
            }
            out.extend(r.version.to_le_bytes());
            let start = out.len();
            let mut nodes = 0;
            encode_value(&mut out, &r.value, 0, &mut nodes)?;
            if out.len() - start > MAX_RECORD_BYTES || out.len() > MAX_BYTES {
                return Err("Metadata byte budget exceeded.".into());
            }
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("Metadata byte budget exceeded.".into());
        }
        let mut d = Reader { bytes, pos: 0 };
        let head = d.u32()?;
        let targeted = head == TARGET_LAYOUT;
        let n = if targeted {
            d.count(MAX_RECORDS)?
        } else {
            if head as usize > MAX_NAMESPACES {
                return Err("Metadata count exceeds budget.".into());
            }
            head as usize
        };
        let mut map = BTreeMap::new();
        let mut last = None;
        let mut owners = BTreeSet::new();
        for _ in 0..n {
            let owner = d.text(64)?;
            if !id_ok(&owner) {
                return Err("Invalid metadata namespace.".into());
            }
            owners.insert(owner.clone());
            if owners.len() > MAX_NAMESPACES {
                return Err("Metadata namespace limit exceeded.".into());
            }
            let target = if targeted {
                match d.take(1)?[0] {
                    0 => Target::Project,
                    1 => Target::Track(TrackId(d.u32()?)),
                    2 => Target::Group(d.u32()?),
                    _ => return Err("Unknown metadata target.".into()),
                }
            } else {
                Target::Project
            };
            let key = (owner, target);
            if last.as_ref().is_some_and(|k| k >= &key) {
                return Err("Invalid metadata namespace/target order.".into());
            }
            last = Some(key.clone());
            let version = d.u32()?;
            if version == 0 {
                return Err("Invalid metadata schema version.".into());
            }
            let start = d.pos;
            let mut nodes = 0;
            let value = d.value(0, &mut nodes)?;
            if d.pos - start > MAX_RECORD_BYTES {
                return Err("Metadata record byte budget exceeded.".into());
            }
            map.insert(key, Record { version, value });
        }
        if d.pos != bytes.len() {
            return Err("Trailing metadata bytes.".into());
        }
        Ok(Self(map))
    }
}
fn put_u32(out: &mut Vec<u8>, n: usize) -> Result<(), String> {
    out.extend(
        u32::try_from(n)
            .map_err(|_| "Metadata length overflow.")?
            .to_le_bytes(),
    );
    Ok(())
}
fn put_text(out: &mut Vec<u8>, s: &str) -> Result<(), String> {
    put_u32(out, s.len())?;
    out.extend(s.as_bytes());
    Ok(())
}
fn encode_value(
    out: &mut Vec<u8>,
    v: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    *nodes += 1;
    if depth > MAX_DEPTH || *nodes > MAX_NODES || out.len() > MAX_BYTES {
        return Err("Metadata structure budget exceeded.".into());
    }
    match v {
        Value::Null => out.push(0),
        Value::Bool(b) => out.push(if *b { 2 } else { 1 }),
        Value::Integer(n) => {
            out.push(3);
            out.extend(n.to_le_bytes());
        }
        Value::Number(n) => {
            if !f64::from_bits(*n).is_finite() {
                return Err("Metadata numbers must be finite.".into());
            }
            out.push(4);
            out.extend(n.to_le_bytes());
        }
        Value::Text(s) => {
            if s.len() > 16384 {
                return Err("Metadata text exceeds16KiB.".into());
            }
            out.push(5);
            put_text(out, s)?;
        }
        Value::Array(a) => {
            if a.len() > MAX_NODES {
                return Err("Metadata array too large.".into());
            }
            out.push(6);
            put_u32(out, a.len())?;
            for v in a {
                encode_value(out, v, depth + 1, nodes)?;
            }
        }
        Value::Object(a) => {
            if a.len() > MAX_NODES {
                return Err("Metadata object too large.".into());
            }
            out.push(7);
            put_u32(out, a.len())?;
            for (k, v) in a {
                if k.len() > 128 {
                    return Err("Metadata key exceeds128bytes.".into());
                }
                put_text(out, k)?;
                encode_value(out, v, depth + 1, nodes)?;
            }
        }
    }
    Ok(())
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or("Truncated metadata.")?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn count(&mut self, max: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n > max {
            return Err("Metadata count exceeds budget.".into());
        }
        Ok(n)
    }
    fn text(&mut self, max: usize) -> Result<String, String> {
        let n = self.count(max)?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "Metadata text is not UTF-8.".into())
    }
    fn value(&mut self, depth: usize, nodes: &mut usize) -> Result<Value, String> {
        *nodes += 1;
        if depth > MAX_DEPTH || *nodes > MAX_NODES {
            return Err("Metadata structure budget exceeded.".into());
        }
        Ok(match self.take(1)?[0] {
            0 => Value::Null,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::Integer(i64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            4 => {
                let b = u64::from_le_bytes(self.take(8)?.try_into().unwrap());
                if !f64::from_bits(b).is_finite() {
                    return Err("Metadata numbers must be finite.".into());
                }
                Value::Number(b)
            }
            5 => Value::Text(self.text(16384)?),
            6 => {
                let n = self.count(MAX_NODES.saturating_sub(*nodes))?;
                let mut a = Vec::new();
                for _ in 0..n {
                    a.push(self.value(depth + 1, nodes)?);
                }
                Value::Array(a)
            }
            7 => {
                let n = self.count(MAX_NODES.saturating_sub(*nodes))?;
                let mut a = BTreeMap::new();
                let mut last = None;
                for _ in 0..n {
                    let key = self.text(128)?;
                    if last.as_ref().is_some_and(|k| k >= &key) {
                        return Err("Metadata keys are not sorted.".into());
                    }
                    last = Some(key.clone());
                    a.insert(key, self.value(depth + 1, nodes)?);
                }
                Value::Object(a)
            }
            _ => return Err("Unknown metadata value type.".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn targeted_codec_keeps_legacy_project_bytes_and_bounds4096_sparse_records() {
        let mut old = Metadata::default();
        old.set(
            "author.demo",
            Some(Record {
                version: 1,
                value: Value::Null,
            }),
        )
        .unwrap();
        let legacy = old.encode().unwrap();
        assert_eq!(&legacy[..4], &1u32.to_le_bytes());
        assert_eq!(Metadata::decode(&legacy).unwrap(), old);
        old.set_at(
            "author.demo",
            Target::Track(TrackId(7)),
            Some(Record {
                version: 2,
                value: Value::Integer(i64::MAX),
            }),
        )
        .unwrap();
        let b = old.encode().unwrap();
        assert_eq!(&b[..4], b"XMD2");
        assert_eq!(Metadata::decode(&b).unwrap(), old);
        for end in 0..b.len() {
            assert!(Metadata::decode(&b[..end]).is_err());
        }
        let mut bytes = Vec::new();
        bytes.extend(TARGET_LAYOUT.to_le_bytes());
        put_u32(&mut bytes, MAX_RECORDS).unwrap();
        for i in 0..MAX_RECORDS {
            put_text(&mut bytes, "author.demo").unwrap();
            bytes.push(1);
            bytes.extend((i as u32).to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            bytes.push(0);
        }
        let mut full = Metadata::decode(&bytes).unwrap();
        assert_eq!(full.len(), MAX_RECORDS);
        let saved = full.clone();
        assert!(
            full.set_at(
                "author.demo",
                Target::Track(TrackId(4096)),
                Some(Record {
                    version: 1,
                    value: Value::Null
                })
            )
            .is_err()
        );
        assert_eq!(full, saved);
        bytes[4..8].copy_from_slice(&4097u32.to_le_bytes());
        assert!(Metadata::decode(&bytes).is_err());
    }
    #[test]
    fn typed_sparse_roundtrip_exact_numbers_and_atomic_invalid_replacement() {
        let mut m = Metadata::default();
        assert_eq!(m.heap_bytes(), 0);
        let value = Value::Array(vec![
            Value::Integer(9_007_199_254_740_993),
            Value::Number((-0.0f64).to_bits()),
            Value::Text("音🎵".into()),
            Value::Bool(true),
            Value::Null,
            Value::Object(BTreeMap::new()),
        ]);
        m.set("author.demo", Some(Record { version: 7, value }))
            .unwrap();
        let bytes = m.encode().unwrap();
        assert_eq!(Metadata::decode(&bytes).unwrap(), m);
        let old = m.clone();
        assert!(
            m.set(
                "author.demo",
                Some(Record {
                    version: 0,
                    value: Value::Null
                })
            )
            .is_err()
        );
        assert_eq!(m, old);
        assert!(
            m.set(
                "other",
                Some(Record {
                    version: 1,
                    value: Value::Number(f64::NAN.to_bits())
                })
            )
            .is_err()
        );
        assert_eq!(m, old);
        m.set("author.demo", None).unwrap();
        assert!(m.is_empty());
    }
    #[test]
    fn malformed_truncation_depth_counts_and_bytes_are_bounded() {
        let mut m = Metadata::default();
        m.set(
            "author.demo",
            Some(Record {
                version: 1,
                value: Value::Integer(i64::MIN),
            }),
        )
        .unwrap();
        let b = m.encode().unwrap();
        for end in 0..b.len() {
            assert!(Metadata::decode(&b[..end]).is_err());
        }
        let mut tail = b.clone();
        tail.push(0);
        assert!(Metadata::decode(&tail).is_err());
        assert!(Metadata::decode(&u32::MAX.to_le_bytes()).is_err());
        assert!(Metadata::decode(&vec![0; MAX_BYTES + 1]).is_err());
        let mut v = Value::Null;
        for _ in 0..10 {
            v = Value::Array(vec![v]);
        }
        assert!(
            m.set(
                "author.demo",
                Some(Record {
                    version: 1,
                    value: v
                })
            )
            .is_err()
        );
        assert!(
            m.set(
                "author.demo",
                Some(Record {
                    version: 1,
                    value: Value::Array(vec![Value::Null; MAX_NODES])
                })
            )
            .is_err()
        );
        assert!(
            m.set(
                "author.demo",
                Some(Record {
                    version: 1,
                    value: Value::Text("x".repeat(16385))
                })
            )
            .is_err()
        );
    }
}
