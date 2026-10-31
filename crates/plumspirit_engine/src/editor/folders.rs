//! Track Folder edits (v0.8 §077–§080) planned as undoable operations.
//!
//! Every edit that changes where tracks are is a group of
//! [`Operation::MoveTracks`] followed by one [`Operation::SetFolders`] — one
//! undo step — so a folder's members stay contiguous in the track list and
//! the list always shows what export will write (folders never reach MIDI).
//! Folders hold top-level tracks; a subtrack given to a folder brings its
//! top-level track (with every subtrack).

use crate::history::{Operation, TrackPlace, plan_track_moves};
use crate::model::{PlumDocument, Track, TrackFolders, TrackId};

/// Index of the top-level track whose subtree holds track `i`.
pub fn top_root(doc: &PlumDocument, i: usize) -> usize {
    let mut i = i;
    let mut guard = doc.track_count();
    while let Some(p) = doc.track(i).and_then(|t| t.parent()) {
        match doc.index_of(p) {
            Some(pi) if guard > 0 => {
                i = pi;
                guard -= 1;
            }
            _ => break,
        }
    }
    i
}

/// The distinct top-level tracks of `tracks`, in document order.
pub fn top_roots(doc: &PlumDocument, tracks: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = tracks
        .iter()
        .filter(|&&i| i < doc.track_count())
        .map(|&i| top_root(doc, i))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The top-level tracks in folder `id`, in document order.
pub fn folder_roots(doc: &PlumDocument, id: u32) -> Vec<usize> {
    let f = &doc.project().folders;
    if id == 0 || f.get(id).is_none() {
        return Vec::new();
    }
    doc.tracks()
        .iter()
        .enumerate()
        .filter(|(_, t)| t.parent().is_none() && f.folder_of(t.id()) == id)
        .map(|(i, _)| i)
        .collect()
}

fn with_folders(moves: Option<Operation>, folders: TrackFolders, label: &'static str) -> Operation {
    let set = Operation::SetFolders {
        folders: Box::new(folders),
        label,
    };
    match moves {
        Some(Operation::Group { mut ops, .. }) => {
            ops.push(set);
            Operation::Group { ops, label }
        }
        Some(op) => Operation::Group {
            ops: vec![op, set],
            label,
        },
        None => set,
    }
}

/// Applies the track moves of `op` to a working copy.
fn apply_moves(doc: &mut PlumDocument, op: &Operation) -> Result<(), String> {
    match op {
        Operation::MoveTracks { from, to, parent } => doc
            .move_tracks(*from, *to, *parent)
            .map(|_| ())
            .map_err(|e| format!("{e:?}")),
        Operation::Group { ops, .. } => ops.iter().try_for_each(|o| apply_moves(doc, o)),
        _ => Ok(()),
    }
}

fn exists(doc: &PlumDocument, id: u32) -> Result<(), String> {
    if doc.project().folders.get(id).is_some() {
        Ok(())
    } else {
        Err("That folder no longer exists.".into())
    }
}

/// New Folder: holding the top-level tracks of `tracks` (gathered right after
/// the first one, order kept), or empty and shown before `anchor`. Returns the
/// operation and the new folder's id.
pub fn plan_create(
    doc: &PlumDocument,
    tracks: &[usize],
    name: &str,
    anchor: Option<TrackId>,
) -> Result<(Operation, u32), String> {
    let mut f = doc.project().folders.clone();
    if f.folders().len() >= crate::model::folder::MAX_FOLDERS {
        return Err("This project already has the maximum number of folders.".into());
    }
    let roots = top_roots(doc, tracks);
    let id = f.create(name, anchor);
    let moves = if roots.len() > 1 {
        Some(plan_track_moves(
            doc,
            &roots[1..],
            roots[0],
            TrackPlace::After,
        )?)
    } else {
        None
    };
    for &r in &roots {
        if let Some(t) = doc.track(r) {
            f.set_member(t.id(), id);
        }
    }
    Ok((with_folders(moves, f, "New Folder"), id))
}

pub fn plan_rename(doc: &PlumDocument, id: u32, name: &str) -> Result<Operation, String> {
    exists(doc, id)?;
    let mut f = doc.project().folders.clone();
    f.rename(id, name);
    Ok(with_folders(None, f, "Rename Folder"))
}

/// Delete Folder: the folder goes, its tracks stay where they are.
pub fn plan_delete(doc: &PlumDocument, id: u32) -> Result<Operation, String> {
    exists(doc, id)?;
    let mut f = doc.project().folders.clone();
    f.delete(id);
    Ok(with_folders(None, f, "Delete Folder"))
}

/// Move to Folder / drop on a folder: the top-level tracks of `tracks` go to
/// the end of folder `id` (or where an empty folder is shown) and join it.
pub fn plan_into(doc: &PlumDocument, tracks: &[usize], id: u32) -> Result<Operation, String> {
    exists(doc, id)?;
    let roots = top_roots(doc, tracks);
    if roots.is_empty() {
        return Err("Select the tracks to move.".into());
    }
    let mut f = doc.project().folders.clone();
    let members: Vec<usize> = folder_roots(doc, id)
        .into_iter()
        .filter(|r| !roots.contains(r))
        .collect();
    let moves = match members.last() {
        Some(&last) => Some(plan_track_moves(doc, &roots, last, TrackPlace::After)?),
        None => {
            let anchor = f
                .get(id)
                .and_then(|x| x.anchor)
                .and_then(|a| doc.index_of(a))
                .filter(|&a| doc.track(a).is_some_and(|t| t.parent().is_none()));
            match anchor {
                Some(a) if !roots.contains(&a) => {
                    Some(plan_track_moves(doc, &roots, a, TrackPlace::Before)?)
                }
                Some(_) => None,
                None => {
                    // At the end of the list.
                    let last_root = top_root(doc, doc.track_count().saturating_sub(1));
                    (!roots.contains(&last_root))
                        .then(|| plan_track_moves(doc, &roots, last_root, TrackPlace::After))
                        .transpose()?
                }
            }
        }
    };
    for &r in &roots {
        if let Some(t) = doc.track(r) {
            f.set_member(t.id(), id);
        }
    }
    Ok(with_folders(moves, f, "Move to Folder"))
}

/// Remove from Folder: the top-level tracks of `tracks` leave their folders;
/// those inside a folder's run move right after it (so members stay together).
pub fn plan_out(doc: &PlumDocument, tracks: &[usize]) -> Result<Operation, String> {
    let roots = top_roots(doc, tracks);
    let mut f = doc.project().folders.clone();
    let leaving: Vec<usize> = roots
        .iter()
        .copied()
        .filter(|&r| doc.track(r).is_some_and(|t| f.folder_of(t.id()) != 0))
        .collect();
    if leaving.is_empty() {
        return Err("These tracks are not in a folder.".into());
    }
    let mut ops: Vec<Operation> = Vec::new();
    // Per folder: move the leaving tracks after the last staying member.
    let mut folders: Vec<u32> = leaving
        .iter()
        .filter_map(|&r| doc.track(r).map(|t| f.folder_of(t.id())))
        .collect();
    folders.sort_unstable();
    folders.dedup();
    // Planned on a working copy (chunks are shared, only the list is copied):
    // each folder's moves are planned after the previous ones.
    let mut shape = doc.clone();
    for id in folders {
        let staying: Vec<usize> = folder_roots(doc, id)
            .into_iter()
            .filter(|r| !leaving.contains(r))
            .collect();
        let these: Vec<TrackId> = leaving
            .iter()
            .filter(|&&r| doc.track(r).is_some_and(|t| f.folder_of(t.id()) == id))
            .filter_map(|&r| doc.track(r).map(|t| t.id()))
            .collect();
        match staying.last() {
            Some(&last) => {
                let last_id = doc
                    .track(last)
                    .map(|t| t.id())
                    .ok_or("the folder changed")?;
                let idx: Vec<usize> = these.iter().filter_map(|&t| shape.index_of(t)).collect();
                let target = shape.index_of(last_id).ok_or("the folder changed")?;
                let op = plan_track_moves(&shape, &idx, target, TrackPlace::After)?;
                apply_moves(&mut shape, &op)?;
                match op {
                    Operation::Group { ops: g, .. } => ops.extend(g),
                    other => ops.push(other),
                }
            }
            None => {
                // The folder becomes empty: it stays shown where it was.
                f.set_anchor(id, these.first().copied());
            }
        }
        for t in these {
            f.set_member(t, 0);
        }
    }
    let moves = (!ops.is_empty()).then_some(Operation::Group {
        ops,
        label: "Remove from Folder",
    });
    Ok(with_folders(moves, f, "Remove from Folder"))
}

/// A track drag next to / into `target` that keeps folders consistent: moved
/// top-level tracks take the folder of a top-level target (none for a target
/// outside folders); tracks that become subtracks leave their folders.
pub fn plan_move_next_to(
    doc: &PlumDocument,
    tracks: &[usize],
    target: usize,
    place: TrackPlace,
) -> Result<Operation, String> {
    let moves = plan_track_moves(doc, tracks, target, place)?;
    let f0 = &doc.project().folders;
    if f0.is_empty() {
        return Ok(moves);
    }
    let mut f = f0.clone();
    let target_top = doc.track(target).is_some_and(|t| t.parent().is_none());
    let folder = match (place, target_top) {
        (TrackPlace::Into, _) | (_, false) => 0,
        _ => doc.track(target).map_or(0, |t| f.folder_of(t.id())),
    };
    // The top-most moved tracks (not those moving inside a moved subtree).
    let moved: std::collections::HashSet<TrackId> = tracks
        .iter()
        .filter_map(|&i| doc.track(i).map(|t| t.id()))
        .collect();
    for &i in tracks {
        let Some(t) = doc.track(i) else {
            continue;
        };
        let mut p = t.parent();
        let mut inside = false;
        let mut guard = doc.track_count();
        while let Some(pid) = p {
            if moved.contains(&pid) || guard == 0 {
                inside = true;
                break;
            }
            guard -= 1;
            p = doc
                .index_of(pid)
                .and_then(|k| doc.track(k))
                .and_then(|x| x.parent());
        }
        if !inside {
            f.set_member(t.id(), folder);
        }
    }
    if f == *f0 {
        return Ok(moves);
    }
    Ok(with_folders(Some(moves), f, "Move Tracks"))
}

/// Moves folder `id` with all its tracks before / after `target` (another
/// folder's whole run when `target` is in one).
pub fn plan_move_folder(
    doc: &PlumDocument,
    id: u32,
    target: usize,
    place: TrackPlace,
) -> Result<Operation, String> {
    exists(doc, id)?;
    if target >= doc.track_count() {
        return Err("That track no longer exists.".into());
    }
    let f = &doc.project().folders;
    let mut t = top_root(doc, target);
    let tf = doc.track(t).map_or(0, |x| f.folder_of(x.id()));
    if tf == id {
        return Err("A folder cannot be moved onto itself.".into());
    }
    let place = match place {
        TrackPlace::Into => TrackPlace::After,
        p => p,
    };
    if tf != 0 {
        let run = folder_roots(doc, tf);
        t = match place {
            TrackPlace::Before => run.first().copied().unwrap_or(t),
            _ => run.last().copied().unwrap_or(t),
        };
    }
    let roots = folder_roots(doc, id);
    if roots.is_empty() {
        // An empty folder moves its anchor.
        let mut f = f.clone();
        let anchor = match place {
            TrackPlace::Before => doc.track(t).map(|x| x.id()),
            _ => {
                let next = doc.subtree_end(t);
                doc.track(next).map(|x| x.id())
            }
        };
        f.set_anchor(id, anchor);
        return Ok(with_folders(None, f, "Move Folder"));
    }
    let op = plan_track_moves(doc, &roots, t, place)?;
    Ok(match op {
        Operation::Group { ops, .. } => Operation::Group {
            ops,
            label: "Move Folder",
        },
        other => other,
    })
}

/// Tracks dropped just before (`before`) or after folder `id`, outside it:
/// moved next to the folder's run (an empty folder: before / at its anchor)
/// and out of every folder.
pub fn plan_next_to_folder(
    doc: &PlumDocument,
    tracks: &[usize],
    id: u32,
    before: bool,
) -> Result<Operation, String> {
    exists(doc, id)?;
    let roots = top_roots(doc, tracks);
    if roots.is_empty() {
        return Err("Select the tracks to move.".into());
    }
    let run: Vec<usize> = folder_roots(doc, id)
        .into_iter()
        .filter(|r| !roots.contains(r))
        .collect();
    let (target, place) = match (run.first(), run.last()) {
        (Some(&first), _) if before => (Some(first), TrackPlace::Before),
        (_, Some(&last)) => (Some(last), TrackPlace::After),
        _ => {
            let anchor = doc
                .project()
                .folders
                .get(id)
                .and_then(|f| f.anchor)
                .and_then(|a| doc.index_of(a))
                .filter(|a| !roots.contains(a));
            (anchor, TrackPlace::Before)
        }
    };
    let moves = match target {
        Some(t) => Some(plan_track_moves(doc, &roots, t, place)?),
        None => None,
    };
    let mut f = doc.project().folders.clone();
    for &r in &roots {
        if let Some(t) = doc.track(r) {
            f.set_member(t.id(), 0);
        }
    }
    if f == doc.project().folders {
        return moves.ok_or_else(|| "Nothing to move.".to_owned());
    }
    Ok(with_folders(moves, f, "Move Tracks"))
}

/// Add Track Below (v0.8 §203–§204): an empty track right after `i`'s
/// subtree, at `i`'s level. Beside a folder member it joins that folder, so
/// the folder stays one run in the list (a new track between two members
/// used to split it). One undo step.
pub fn plan_add_track_below(doc: &PlumDocument, i: usize) -> Result<Operation, String> {
    let t = doc.track(i).ok_or("That track no longer exists.")?;
    let parent = t.parent();
    let mut ops = vec![
        Operation::AddTrack,
        Operation::MoveTracks {
            from: doc.track_count(),
            to: doc.subtree_end(i),
            parent,
        },
    ];
    let folders = &doc.project().folders;
    let folder = if parent.is_none() {
        folders.folder_of(t.id())
    } else {
        0
    };
    if folder != 0 {
        let mut f = folders.clone();
        f.set_member(TrackId(doc.next_track_id()), folder);
        ops.push(Operation::SetFolders {
            folders: Box::new(f),
            label: "Add Track",
        });
    }
    Ok(Operation::Group {
        ops,
        label: "Add Track",
    })
}

/// Add Track to Folder (the folder header's context): an empty top-level
/// track after the folder's last track (an empty folder: where it is shown),
/// made a member. One undo step.
pub fn plan_add_track_to_folder(doc: &PlumDocument, id: u32) -> Result<Operation, String> {
    exists(doc, id)?;
    let count = doc.track_count();
    let to = folder_insert_index(doc, id);
    let mut f = doc.project().folders.clone();
    f.set_member(TrackId(doc.next_track_id()), id);
    Ok(Operation::Group {
        ops: vec![
            Operation::AddTrack,
            Operation::MoveTracks {
                from: count,
                to,
                parent: None,
            },
            Operation::SetFolders {
                folders: Box::new(f),
                label: "Add Track to Folder",
            },
        ],
        label: "Add Track to Folder",
    })
}

/// Where a track added to folder `id` goes: after the folder's last track
/// (an empty folder: where it is shown, else the end).
fn folder_insert_index(doc: &PlumDocument, id: u32) -> usize {
    match folder_roots(doc, id).last() {
        Some(&last) => doc.subtree_end(last),
        None => doc
            .project()
            .folders
            .get(id)
            .and_then(|f| f.anchor)
            .and_then(|a| doc.index_of(a))
            .filter(|&a| doc.track(a).is_some_and(|t| t.parent().is_none()))
            .unwrap_or(doc.track_count()),
    }
}

/// The most tracks and subtracks a project holds: a MIDI file counts its
/// tracks in 16 bits.
pub const MAX_TRACKS: usize = u16::MAX as usize;

/// Where Add Tracks… (B2-F03, contract §007) puts its tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddTracksAt {
    /// At the end of the track list, top level (the Conductor's context and
    /// the list's empty space — like Add Track).
    End,
    /// Right after track `i` and its subtracks, at `i`'s level: siblings of
    /// a subtrack under the same parent; beside a folder member, in that
    /// folder (like Add Track Below).
    Below(usize),
    /// Inside folder `id`, after its last track (like Add Track to Folder).
    Folder(u32),
}

/// Bytes one new empty track takes in the document (its record and what
/// it allocates), for Add Tracks…' memory preflight.
pub fn new_track_bytes() -> usize {
    std::mem::size_of::<Track>() + Track::new(TrackId(1)).heap_bytes()
}

/// Add Tracks…: `count` empty tracks at `at` in one undo step (one
/// insertion, plus the folder membership when they join a folder). They
/// have no clips, so the Piano Roll is locked on them until one is added;
/// their ids follow the project's next id in order (colours cycle with
/// the ids). Refused with a sentence, before anything changes, for a count
/// of 0, more tracks than the project can hold, or a place that no longer
/// exists.
pub fn plan_add_tracks(
    doc: &PlumDocument,
    at: AddTracksAt,
    count: usize,
) -> Result<Operation, String> {
    if count == 0 {
        return Err("Enter a whole number of 1 or more.".into());
    }
    let room = MAX_TRACKS.saturating_sub(doc.track_count());
    if count > room {
        return Err(format!(
            "A project can hold at most 65,535 tracks and subtracks (the limit of a MIDI file); {} more can be added.",
            crate::midi::error::group_digits(room as u64)
        ));
    }
    let first = doc.next_track_id();
    let last = u32::try_from(count - 1)
        .ok()
        .and_then(|n| first.checked_add(n))
        .ok_or("The project has no free track numbers left for that many tracks.")?;
    let folders = &doc.project().folders;
    let (index, parent, folder) = match at {
        AddTracksAt::End => (doc.track_count(), None, 0),
        AddTracksAt::Below(i) => {
            let t = doc.track(i).ok_or("That track no longer exists.")?;
            let parent = t.parent();
            let folder = if parent.is_none() {
                folders.folder_of(t.id())
            } else {
                0
            };
            (doc.subtree_end(i), parent, folder)
        }
        AddTracksAt::Folder(id) => {
            exists(doc, id)?;
            (folder_insert_index(doc, id), None, id)
        }
    };
    let tracks: Vec<Track> = (first..=last)
        .map(|id| {
            let mut t = Track::new(TrackId(id));
            t.set_parent(parent);
            t
        })
        .collect();
    let insert = Operation::InsertTracks {
        index,
        tracks,
        label: "Add Tracks",
    };
    if folder == 0 {
        return Ok(insert);
    }
    let mut f = folders.clone();
    for id in first..=last {
        f.set_member(TrackId(id), folder);
    }
    Ok(Operation::Group {
        ops: vec![
            insert,
            Operation::SetFolders {
                folders: Box::new(f),
                label: "Add Tracks",
            },
        ],
        label: "Add Tracks",
    })
}

/// Delete Folder and Its Tracks: deletes every track of folder `id` (with
/// subtracks) and the folder itself in one undo step. `roots` are the
/// folder's top-level tracks (from [`folder_roots`]); refused when they are
/// all the tracks of the project (a MIDI file needs one).
pub fn plan_delete_with_tracks(doc: &PlumDocument, id: u32) -> Result<Operation, String> {
    exists(doc, id)?;
    let roots = folder_roots(doc, id);
    let covered: usize = roots.iter().map(|&r| doc.subtree_end(r) - r).sum();
    if covered >= doc.track_count() {
        return Err("A MIDI file needs at least one track: this folder holds all the tracks of the project.".into());
    }
    let mut f = doc.project().folders.clone();
    f.delete(id);
    // From the last to the first: earlier indices stay valid.
    let mut ops: Vec<Operation> = roots
        .iter()
        .rev()
        .map(|&index| Operation::DeleteTrack { index })
        .collect();
    ops.push(Operation::SetFolders {
        folders: Box::new(f),
        label: "Delete Folder and Tracks",
    });
    Ok(Operation::Group {
        ops,
        label: "Delete Folder and Tracks",
    })
}
