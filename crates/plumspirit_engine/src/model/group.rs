//! Note groups (v0.7): a per-note group id (the note store's group column,
//! 0 = no group) plus a small project-wide table describing each group.
//!
//! * Ids are project-wide (a group may hold notes of several tracks) and never
//!   reused while the table lives (`next`).
//! * Groups nest through `parent` (validated acyclic, iteratively).
//! * Locked groups refuse note edits in the group tools (lock is enforced by the
//!   platform tools, not by the store).
//! * Temporary groups are not saved: `.lup` writes their notes with group 0.
//! * Linked groups share a `link` id (instance clones); unlinking clears it.

/// One group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupInfo {
    /// ≥ 1.
    pub id: u32,
    /// Enclosing group (0 = top level).
    pub parent: u32,
    pub name: Option<String>,
    /// Outline colour.
    pub color: Option<[u8; 3]>,
    pub locked: bool,
    /// Not saved with the project.
    pub temporary: bool,
    /// Link set (0 = not linked): linked groups are instance clones.
    pub link: u32,
}

/// Maximum nesting depth (validation bound; deeper tables are refused).
pub const MAX_GROUP_DEPTH: usize = 64;
/// Maximum number of groups in a project.
pub const MAX_GROUPS: usize = 1 << 20;
pub const MAX_GROUP_NAME_CHARS: usize = 64;

/// The project's groups, sorted by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupTable {
    entries: Vec<GroupInfo>,
    /// Next id to hand out (≥ every id ever used + 1).
    next: u32,
}

impl GroupTable {
    pub fn new() -> Self {
        GroupTable {
            entries: Vec::new(),
            next: 1,
        }
    }

    /// Builds a table from saved entries (validated).
    pub fn from_entries(mut entries: Vec<GroupInfo>, next: u32) -> Result<Self, String> {
        entries.sort_by_key(|e| e.id);
        let t = GroupTable {
            next: next.max(entries.last().map_or(1, |e| e.id.saturating_add(1))),
            entries,
        };
        t.validate()?;
        Ok(t)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.entries.len() > MAX_GROUPS {
            return Err("too many groups".into());
        }
        for w in self.entries.windows(2) {
            if w[0].id >= w[1].id {
                return Err("duplicate group id".into());
            }
        }
        for e in &self.entries {
            if e.id == 0 {
                return Err("group id 0".into());
            }
            if e.parent == e.id {
                return Err("a group cannot contain itself".into());
            }
            if e.parent != 0 && self.get(e.parent).is_none() {
                return Err("a group refers to a missing parent".into());
            }
            if e.name
                .as_ref()
                .is_some_and(|n| n.chars().count() > MAX_GROUP_NAME_CHARS)
            {
                return Err("group name too long".into());
            }
            // Iterative cycle / depth check.
            let mut p = e.parent;
            let mut depth = 0;
            while p != 0 {
                depth += 1;
                if depth > MAX_GROUP_DEPTH {
                    return Err("groups nest too deeply (or form a cycle)".into());
                }
                p = self.get(p).map_or(0, |g| g.parent);
            }
        }
        Ok(())
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[GroupInfo] {
        &self.entries
    }

    pub fn next_id(&self) -> u32 {
        self.next.max(1)
    }

    pub fn get(&self, id: u32) -> Option<&GroupInfo> {
        self.entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut GroupInfo> {
        match self.entries.binary_search_by_key(&id, |e| e.id) {
            Ok(i) => Some(&mut self.entries[i]),
            Err(_) => None,
        }
    }

    /// Adds a group with a fresh id and returns the id.
    pub fn add(&mut self, mut info: GroupInfo) -> u32 {
        let id = self.next_id();
        self.next = id.saturating_add(1);
        info.id = id;
        self.entries.push(info);
        id
    }

    /// Reserves `n` consecutive fresh ids (paste remapping) and returns the first.
    pub fn reserve(&mut self, n: u32) -> u32 {
        let first = self.next_id();
        self.next = first.saturating_add(n);
        first
    }

    /// Inserts an entry with a given id (paste remapping).
    pub fn insert(&mut self, info: GroupInfo) {
        match self.entries.binary_search_by_key(&info.id, |e| e.id) {
            Ok(i) => self.entries[i] = info,
            Err(i) => {
                self.next = self.next.max(info.id.saturating_add(1));
                self.entries.insert(i, info);
            }
        }
    }

    /// Removes groups; children of removed groups move to the removed group's
    /// parent.
    pub fn remove(&mut self, ids: &[u32]) {
        if ids.is_empty() {
            return;
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        let parent_of = |t: &GroupTable, id: u32| t.get(id).map_or(0, |g| g.parent);
        // Resolve the new parent of every surviving entry first.
        let mut new_parents = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            let mut p = e.parent;
            let mut guard = 0;
            while p != 0 && sorted.binary_search(&p).is_ok() && guard <= MAX_GROUP_DEPTH {
                p = parent_of(self, p);
                guard += 1;
            }
            new_parents.push(p);
        }
        for (e, p) in self.entries.iter_mut().zip(new_parents) {
            e.parent = p;
        }
        self.entries
            .retain(|e| sorted.binary_search(&e.id).is_err());
    }

    /// Ids whose notes are written to disk (every non-temporary group).
    pub fn saved(&self, id: u32) -> bool {
        id != 0 && self.get(id).is_some_and(|g| !g.temporary)
    }

    /// The top-most ancestor of `id` (itself when top level).
    pub fn root_of(&self, id: u32) -> u32 {
        let mut cur = id;
        for _ in 0..=MAX_GROUP_DEPTH {
            match self.get(cur) {
                Some(g) if g.parent != 0 => cur = g.parent,
                _ => break,
            }
        }
        cur
    }

    /// Whether `id` or one of its ancestors is locked.
    pub fn locked(&self, id: u32) -> bool {
        let mut cur = id;
        for _ in 0..=MAX_GROUP_DEPTH {
            match self.get(cur) {
                Some(g) if g.locked => return true,
                Some(g) if g.parent != 0 => cur = g.parent,
                _ => return false,
            }
        }
        false
    }

    /// `id` and all its descendants (sorted).
    pub fn subtree(&self, id: u32) -> Vec<u32> {
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            let p = out[i];
            out.extend(
                self.entries
                    .iter()
                    .filter(|e| e.parent == p && e.id != p)
                    .map(|e| e.id),
            );
            i += 1;
            if out.len() > self.entries.len() + 1 {
                break;
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<GroupInfo>()
            + self
                .entries
                .iter()
                .map(|e| e.name.as_ref().map_or(0, String::capacity))
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_nest_validate_and_remove_reparents_children() {
        let mut t = GroupTable::new();
        let a = t.add(GroupInfo::default());
        let b = t.add(GroupInfo {
            parent: a,
            ..Default::default()
        });
        let c = t.add(GroupInfo {
            parent: b,
            locked: false,
            ..Default::default()
        });
        assert!(t.validate().is_ok());
        assert_eq!(t.root_of(c), a);
        assert_eq!(t.subtree(a), vec![a, b, c]);
        t.get_mut(a).unwrap().locked = true;
        assert!(t.locked(c));
        t.remove(&[b]);
        assert_eq!(t.get(c).unwrap().parent, a);
        assert_eq!(t.next_id(), 4, "ids are never reused");
        let mut cyc = t.clone();
        cyc.get_mut(a).unwrap().parent = c;
        assert!(cyc.validate().is_err());
    }
}
