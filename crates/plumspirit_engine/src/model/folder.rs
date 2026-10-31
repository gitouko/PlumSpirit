//! Track Folders (v0.8 §077–§080): organizational metadata of the track list.
//!
//! A folder is **not** a track: it has no notes, no MIDI channel, no playback
//! and is never exported. It groups top-level tracks (each with its whole
//! subtree) under a header in the track list. Membership is kept here, keyed
//! by the stable [`TrackId`] of top-level tracks — the musical Track / Subtrack
//! hierarchy is untouched (subtracks follow their top-level track). The
//! Conductor is not a track and can never be in a folder.
//!
//! * Display order is the document's track order: a folder's header is drawn
//!   before its first member. The app keeps members contiguous (moves into /
//!   out of / with folders are track moves plus a membership change in one
//!   undo step); if they ever are not, the header is simply drawn again.
//! * An empty folder is shown before its `anchor` track (or at the end).
//! * Ids are never reused while the table lives (`next`).
//! * Stale members (deleted tracks) are ignored, and dropped when saved.
//! * Collapse is view state outside undo: [`ProjectInfo::folders_collapsed`].
//!
//! [`ProjectInfo::folders_collapsed`]: super::ProjectInfo::folders_collapsed

use super::track::TrackId;
use std::collections::BTreeMap;

pub const MAX_FOLDERS: usize = 4096;
pub const MAX_FOLDER_NAME_CHARS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderInfo {
    /// ≥ 1.
    pub id: u32,
    pub name: String,
    /// Where the folder is shown while it is empty: before this top-level
    /// track (None or a missing track: at the end of the list).
    pub anchor: Option<TrackId>,
}

/// The project's folders and which top-level tracks they hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackFolders {
    /// Sorted by id.
    folders: Vec<FolderInfo>,
    /// Top-level track → folder id.
    members: BTreeMap<TrackId, u32>,
    /// Next id to hand out (≥ every id ever used + 1).
    next: u32,
}

impl TrackFolders {
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn folders(&self) -> &[FolderInfo] {
        &self.folders
    }

    pub fn members(&self) -> &BTreeMap<TrackId, u32> {
        &self.members
    }

    pub fn get(&self, id: u32) -> Option<&FolderInfo> {
        self.folders
            .binary_search_by_key(&id, |f| f.id)
            .ok()
            .map(|i| &self.folders[i])
    }

    /// The folder holding top-level track `t` (0: none).
    pub fn folder_of(&self, t: TrackId) -> u32 {
        self.members.get(&t).copied().unwrap_or(0)
    }

    /// The id the next [`TrackFolders::create`] returns.
    pub fn next_id(&self) -> u32 {
        self.next.max(1)
    }

    /// Adds a folder; returns its id. Names are trimmed and limited.
    pub fn create(&mut self, name: &str, anchor: Option<TrackId>) -> u32 {
        let id = self.next_id();
        self.next = id + 1;
        self.folders.push(FolderInfo {
            id,
            name: clean_name(name, id),
            anchor,
        });
        id
    }

    pub fn rename(&mut self, id: u32, name: &str) -> bool {
        match self.folders.iter_mut().find(|f| f.id == id) {
            Some(f) => {
                f.name = clean_name(name, id);
                true
            }
            None => false,
        }
    }

    /// Removes a folder; its tracks stay where they are, outside folders.
    pub fn delete(&mut self, id: u32) -> bool {
        let before = self.folders.len();
        self.folders.retain(|f| f.id != id);
        self.members.retain(|_, f| *f != id);
        self.folders.len() != before
    }

    /// Puts top-level track `t` into folder `id` (0: out of every folder).
    pub fn set_member(&mut self, t: TrackId, id: u32) {
        if id == 0 || self.get(id).is_none() {
            self.members.remove(&t);
        } else {
            self.members.insert(t, id);
        }
    }

    pub fn set_anchor(&mut self, id: u32, anchor: Option<TrackId>) {
        if let Some(f) = self.folders.iter_mut().find(|f| f.id == id) {
            f.anchor = anchor;
        }
    }

    /// The table as it is saved: members only for tracks in `alive` (and
    /// anchors only to them).
    pub fn pruned(&self, alive: impl Fn(TrackId) -> bool) -> TrackFolders {
        TrackFolders {
            folders: self
                .folders
                .iter()
                .map(|f| FolderInfo {
                    anchor: f.anchor.filter(|&a| alive(a)),
                    ..f.clone()
                })
                .collect(),
            members: self
                .members
                .iter()
                .filter(|(t, _)| alive(**t))
                .map(|(t, f)| (*t, *f))
                .collect(),
            next: self.next,
        }
    }

    /// Rebuilds a table read from a file (validated).
    pub fn from_parts(
        folders: Vec<FolderInfo>,
        members: Vec<(TrackId, u32)>,
        next: u32,
    ) -> Result<TrackFolders, String> {
        let mut t = TrackFolders {
            folders,
            members: BTreeMap::new(),
            next,
        };
        t.folders.sort_by_key(|f| f.id);
        for (track, f) in members {
            if t.members.insert(track, f).is_some() {
                return Err("a track is listed in two folders".into());
            }
        }
        t.validate()?;
        Ok(t)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.folders.len() > MAX_FOLDERS {
            return Err("too many folders".into());
        }
        let mut last = 0u32;
        for f in &self.folders {
            if f.id == 0 || f.id <= last {
                return Err("folder ids are not unique".into());
            }
            last = f.id;
            if f.id >= self.next {
                return Err("a folder id is beyond the next id".into());
            }
            if f.name.trim().is_empty() || f.name.chars().count() > MAX_FOLDER_NAME_CHARS {
                return Err("a folder name is empty or too long".into());
            }
            if f.name.chars().any(char::is_control) {
                return Err("a folder name contains control characters".into());
            }
        }
        if self.members.values().any(|&f| self.get(f).is_none()) {
            return Err("a track belongs to a folder that does not exist".into());
        }
        Ok(())
    }

    pub fn heap_bytes(&self) -> usize {
        self.folders
            .iter()
            .map(|f| f.name.capacity() + std::mem::size_of::<FolderInfo>())
            .sum::<usize>()
            + self.members.len() * 48
    }
}

fn clean_name(name: &str, id: u32) -> String {
    let n: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FOLDER_NAME_CHARS)
        .collect();
    if n.trim().is_empty() {
        format!("Folder {id}")
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_hold_tracks_by_id_and_deleting_keeps_the_tracks() {
        let mut f = TrackFolders::default();
        let a = f.create("  Drums ", None);
        let b = f.create("", Some(TrackId(7)));
        assert_eq!((a, b), (1, 2));
        assert_eq!(f.get(a).unwrap().name, "Drums");
        assert_eq!(f.get(b).unwrap().name, "Folder 2");
        f.set_member(TrackId(3), a);
        f.set_member(TrackId(4), a);
        f.set_member(TrackId(5), b);
        f.set_member(TrackId(6), 99); // unknown folder: not a member
        assert_eq!(f.folder_of(TrackId(4)), a);
        assert_eq!(f.folder_of(TrackId(6)), 0);
        assert!(f.delete(a));
        assert_eq!(f.folder_of(TrackId(3)), 0);
        assert_eq!(f.members().len(), 1);
        // Ids are never reused.
        assert_eq!(f.create("x", None), 3);
        f.validate().unwrap();
        let p = f.pruned(|t| t != TrackId(5) && t != TrackId(7));
        assert!(p.members().is_empty());
        assert_eq!(p.get(b).unwrap().anchor, None);
        let back = TrackFolders::from_parts(
            p.folders().to_vec(),
            p.members().iter().map(|(t, f)| (*t, *f)).collect(),
            p.next_id(),
        )
        .unwrap();
        assert_eq!(back, p);
        assert!(TrackFolders::from_parts(p.folders().to_vec(), vec![(TrackId(1), 42)], 9).is_err());
    }
}
