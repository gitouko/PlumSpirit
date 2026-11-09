//! Object incarnations (v1.0-beta1 B1-0022, `docs/LUA_API.md` "Handles").
//!
//! Several kinds of ids can be handed out again within one open project:
//! clip and clip content ids (Undo of a creation lowers the next id), folder
//! ids (Undo restores the folder table with its counter) and subchannel ids
//! (a new subchannel takes the lowest free id). Track ids only grow, but are
//! tracked too so that every kind follows one rule.
//!
//! [`Incarnations`] remembers how often each id **vanished** from the
//! document. A holder of `(id, incarnation)` — a Lua handle — can then tell
//! "my object" from "a different object that now has the same id": the
//! object is alive only while the id exists **and** its incarnation is
//! unchanged. Once an object disappears (deleted, or its creation undone),
//! everything that held it is expired for good, even if Undo brings the id
//! back (a conservative rule that never lets a holder silently point at a
//! different object).
//!
//! The [`History`](super::History) observes the document after every step
//! (apply, apply without undo, undo, redo) and when asked; a new or opened
//! project starts a new [`ProjectSerial`]. Nothing here is saved: ids and
//! incarnations are meaningful only within one open project in one run.

use crate::model::PlumDocument;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identity of one open project in this run (a fresh number for every new or
/// opened project, never 0).
pub type ProjectSerial = u64;

fn fresh_project_serial() -> ProjectSerial {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The kinds of ids the ledger follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdKind {
    /// A track or subtrack (`TrackId`).
    Track,
    /// A placed clip (`ClipId::Placed`); a primary clip is its track.
    Clip,
    /// A clip content the document owns (`ContentId`).
    Content,
    /// A Track Folder.
    Folder,
    /// A subchannel of a track (`owner` is the track id).
    Subchannel,
    /// A project-wide note group.
    Group,
}

/// One followed id: kind, owner (the track of a subchannel, else 0) and id.
pub type IdKey = (IdKind, u32, u32);

/// The ids that exist, grouped by what changes them, with the stamps that
/// tell when a group must be listed again. Packed integers, sorted: the
/// lists come out of the document already (nearly) in order, so listing and
/// comparing them is linear.
#[derive(Clone, Debug, Default)]
struct Seen {
    /// Tracks and subchannels: (structure revision, track count, sum of the
    /// tracks' subchannel revisions).
    tracks_stamp: (u64, usize, u64),
    /// Track ids.
    tracks: Vec<u32>,
    /// `track << 32 | subchannel`.
    subs: Vec<u64>,
    /// `kind << 32 | id` of clips, contents and folders (listed on every
    /// observation: a few thousand ids at most).
    other: Vec<u64>,
}

/// How often each id vanished (see the module documentation).
#[derive(Clone, Debug)]
pub struct Incarnations {
    project: ProjectSerial,
    seen: Option<Seen>,
    /// Ids that vanished at least once → how many times. Shared so that a
    /// snapshot for a script run is a pointer copy.
    gone: Arc<BTreeMap<IdKey, u32>>,
}

impl Default for Incarnations {
    fn default() -> Self {
        Incarnations::new()
    }
}

impl Incarnations {
    /// A ledger for a new project (nothing observed yet).
    pub fn new() -> Incarnations {
        Incarnations {
            project: fresh_project_serial(),
            seen: None,
            gone: Arc::new(BTreeMap::new()),
        }
    }

    /// A ledger that continues `snapshot` (same project, same incarnations)
    /// for a working copy `doc` of that project's document — a script
    /// transaction applies its edits to one and must expire handles exactly
    /// as the history will.
    pub fn resume(snapshot: &IncarnationSnapshot, doc: &PlumDocument) -> Incarnations {
        let mut l = Incarnations {
            project: snapshot.project,
            seen: None,
            gone: Arc::clone(&snapshot.gone),
        };
        l.seed(doc);
        l
    }

    /// The open project's serial.
    pub fn project(&self) -> ProjectSerial {
        self.project
    }

    /// Starts over for another project: a new serial, nothing remembered.
    pub fn reset(&mut self) {
        *self = Incarnations::new();
    }

    /// The current incarnation of `key` (0 until it first vanishes).
    pub fn of(&self, key: IdKey) -> u32 {
        self.gone.get(&key).copied().unwrap_or(0)
    }

    /// How many ids vanished at least once (diagnostics and tests).
    pub fn vanished_ids(&self) -> usize {
        self.gone.len()
    }

    /// Records the ids `doc` has if nothing was observed yet (before the
    /// first step changes anything). O(1) afterwards.
    pub fn seed(&mut self, doc: &PlumDocument) {
        if self.seen.is_none() {
            let (tracks, subs) = list_tracks(doc);
            self.seen = Some(Seen {
                tracks_stamp: tracks_stamp(doc),
                tracks,
                subs,
                other: list_other(doc),
            });
        }
    }

    /// Compares `doc` with what was seen last and gives every id that
    /// disappeared a new incarnation. Tracks and subchannels are listed again
    /// only when their stamp changed (O(tracks) to check); clips, contents
    /// and folders every time.
    pub fn observe(&mut self, doc: &PlumDocument) {
        let Some(seen) = self.seen.as_mut() else {
            self.seed(doc);
            return;
        };
        let mut vanished: Vec<IdKey> = Vec::new();
        let stamp = tracks_stamp(doc);
        if stamp != seen.tracks_stamp {
            let (tracks, subs) = list_tracks(doc);
            missing(&seen.tracks, &tracks, |t| {
                vanished.push((IdKind::Track, 0, t));
            });
            missing(&seen.subs, &subs, |k| {
                vanished.push((IdKind::Subchannel, (k >> 32) as u32, k as u32));
            });
            seen.tracks = tracks;
            seen.subs = subs;
            seen.tracks_stamp = stamp;
        }
        let now = list_other(doc);
        if now != seen.other {
            missing(&seen.other, &now, |k| vanished.push(unpack_other(k)));
            seen.other = now;
        }
        if !vanished.is_empty() {
            let gone = Arc::make_mut(&mut self.gone);
            for k in vanished {
                let n = gone.entry(k).or_insert(0);
                *n = n.saturating_add(1);
            }
        }
    }

    /// A read-only copy for a script run (cheap: the map is shared).
    pub fn snapshot(&self) -> IncarnationSnapshot {
        IncarnationSnapshot {
            project: self.project,
            gone: Arc::clone(&self.gone),
        }
    }
}

/// What a script run reads: the project serial and the incarnations at the
/// moment the run's document snapshot was taken.
#[derive(Clone, Debug)]
pub struct IncarnationSnapshot {
    project: ProjectSerial,
    gone: Arc<BTreeMap<IdKey, u32>>,
}

impl IncarnationSnapshot {
    pub fn project(&self) -> ProjectSerial {
        self.project
    }

    pub fn of(&self, key: IdKey) -> u32 {
        self.gone.get(&key).copied().unwrap_or(0)
    }
}

fn tracks_stamp(doc: &PlumDocument) -> (u64, usize, u64) {
    let subs = doc
        .tracks()
        .iter()
        .fold(0u64, |a, t| a.wrapping_add(t.subchannels_revision()));
    (doc.structure_revision(), doc.track_count(), subs)
}

/// Every track id, and every subchannel as `track << 32 | id`, sorted.
fn list_tracks(doc: &PlumDocument) -> (Vec<u32>, Vec<u64>) {
    let mut tracks = Vec::with_capacity(doc.track_count());
    let mut subs = Vec::with_capacity(doc.track_count());
    for t in doc.tracks() {
        let id = t.id().0;
        tracks.push(id);
        for s in t.subchannels().entries() {
            subs.push(u64::from(id) << 32 | u64::from(s.id.0));
        }
    }
    tracks.sort_unstable();
    subs.sort_unstable();
    (tracks, subs)
}

fn kind_code(k: IdKind) -> u64 {
    match k {
        IdKind::Track => 0,
        IdKind::Clip => 1,
        IdKind::Content => 2,
        IdKind::Folder => 3,
        IdKind::Subchannel => 4,
        IdKind::Group => 5,
    }
}

fn unpack_other(k: u64) -> IdKey {
    let kind = match k >> 32 {
        1 => IdKind::Clip,
        2 => IdKind::Content,
        5 => IdKind::Group,
        _ => IdKind::Folder,
    };
    (kind, 0, k as u32)
}

/// Every placed clip, owned content and folder id (`kind << 32 | id`),
/// sorted.
fn list_other(doc: &PlumDocument) -> Vec<u64> {
    let clips = doc.clips();
    let pack = |k: IdKind, id: u32| kind_code(k) << 32 | u64::from(id);
    let mut v: Vec<u64> = Vec::new();
    for p in clips.explicit_placements() {
        if let crate::model::ClipId::Placed(n) = p.id {
            v.push(pack(IdKind::Clip, n));
        }
    }
    for c in clips.contents() {
        v.push(pack(IdKind::Content, c.id.0));
    }
    for f in doc.project().folders.folders() {
        v.push(pack(IdKind::Folder, f.id));
    }
    for g in doc.project().groups.entries() {
        v.push(pack(IdKind::Group, g.id));
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Calls `out` with every key of sorted `old` that sorted `new` lacks.
fn missing<T: Ord + Copy>(old: &[T], new: &[T], mut out: impl FnMut(T)) {
    let mut j = 0;
    for &k in old {
        while j < new.len() && new[j] < k {
            j += 1;
        }
        if j >= new.len() || new[j] != k {
            out(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_lists_exactly_the_keys_that_disappeared() {
        let mut out = Vec::new();
        missing(&[1u64, 2, 5, 9], &[0, 2, 9, 10], |k| out.push(k));
        assert_eq!(out, vec![1, 5]);
        let packed = kind_code(IdKind::Content) << 32 | 7;
        assert_eq!(unpack_other(packed), (IdKind::Content, 0, 7));
    }

    #[test]
    fn every_ledger_gets_its_own_project_serial() {
        let a = Incarnations::new();
        let mut b = Incarnations::new();
        assert_ne!(a.project(), b.project());
        let before = b.project();
        b.reset();
        assert_ne!(b.project(), before);
        assert_ne!(b.project(), 0);
    }

    /// B1-0022 cost of the ledger per history step on a large project
    /// (65,000 tracks, 10,000 placed clips): an unchanged step, a step that
    /// changed clips, and one that changed the track list.
    /// `cargo test --release -p plumspirit_engine ledger_step_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ledger_step_cost() {
        use crate::model::{
            ClipPlacement, ContentRef, Division, OwnedContent, SmfFormat, Track, TrackId,
        };
        use std::time::Instant;
        let tracks: Vec<Track> = (0..65_000u32).map(|i| Track::new(TrackId(i))).collect();
        let mut doc =
            PlumDocument::from_parts(SmfFormat::Parallel, Division::ppqn(960), tracks, vec![]);
        let mut layout = doc.clips().layout();
        let mut contents = Vec::new();
        for k in 0..10_000u32 {
            let c = crate::model::ContentId(k);
            layout.contents.push(c);
            contents.push(OwnedContent::new(c));
            layout.placements.push(ClipPlacement {
                id: crate::model::ClipId::Placed(k),
                track: TrackId(k % 65_000),
                content: ContentRef::Owned(c),
                start: u64::from(k) * 960,
                offset: 0,
                length: Some(960),
                muted: false,
                name: None,
                color: None,
                revision: 0,
            });
        }
        layout.next_clip = 10_000;
        layout.next_content = 10_000;
        let mut h = crate::history::History::default();
        h.apply(
            &mut doc,
            crate::history::Operation::SetClips {
                layout: Box::new(layout),
                contents,
                label: "clips",
            },
        )
        .unwrap();
        let mut ids = Incarnations::new();
        let t = Instant::now();
        ids.seed(&doc);
        let seed = t.elapsed();
        let best = |f: &mut dyn FnMut()| {
            (0..5)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed()
                })
                .min()
                .unwrap()
        };
        let unchanged = best(&mut || ids.observe(&doc));
        // The ledger's own share after a step: `ids` is a second ledger that
        // diffs what the history's step changed.
        let mut clip_step = std::time::Duration::MAX;
        for k in 1..=5u32 {
            let op = crate::editor::clips::delete_clips(&doc, &[crate::model::ClipId::Placed(k)])
                .unwrap();
            h.apply(&mut doc, op).unwrap();
            let t = Instant::now();
            ids.observe(&doc);
            clip_step = clip_step.min(t.elapsed());
        }
        let mut track_step = std::time::Duration::MAX;
        let mut add_track = std::time::Duration::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            h.apply(&mut doc, crate::history::Operation::AddTrack)
                .unwrap();
            add_track = add_track.min(t.elapsed());
            let t = Instant::now();
            ids.observe(&doc);
            track_step = track_step.min(t.elapsed());
        }
        println!(
            "history apply(AddTrack) with its own ledger: {:.3} ms",
            add_track.as_secs_f64() * 1e3
        );
        let clips_changed = clip_step;
        let tracks_changed = track_step;
        println!(
            "ledger at 65,000 tracks / 10,000 clips: seed {:.2} ms; unchanged step {:.3} ms; step that deleted a clip {:.3} ms; step that added a track {:.3} ms; vanished ids {}",
            seed.as_secs_f64() * 1e3,
            unchanged.as_secs_f64() * 1e3,
            clips_changed.as_secs_f64() * 1e3,
            tracks_changed.as_secs_f64() * 1e3,
            ids.vanished_ids()
        );
    }
}
