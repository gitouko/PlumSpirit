//! Keeps the analysis engine's index in step with the document and answers
//! position queries for the Progressor (and other consumers).
//!
//! * The index is derived data, rebuilt incrementally: small edits (a few
//!   chunks) synchronously, large ones (opening a project, huge pastes) on a
//!   worker thread with a cancel flag. One build runs at a time; edits made
//!   meanwhile are picked up when it finishes (its result becomes the base of
//!   the next incremental build), so builds never pile up.
//! * A result is only *used* when it describes the current document exactly
//!   (same chunks, same corrections); until then the last values are shown as
//!   "updating", never as current.
//! * Position queries are synchronous and cheap (binary searches over the
//!   index), so seeking and scrubbing never queue work.
//! * Peaks (optional template fields) come from a [`PeakProfile`] built on a
//!   worker when first needed, per scope and tempo map.

use plumspirit_engine::analysis::profile::{PeakProfile, window_peak_poly};
use plumspirit_engine::analysis::{
    AnalysisIndex, Metrics, Scope, Timing, TrackInput, document_corrections,
};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::correction::Correction;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Chunks rebuilt synchronously at most (≈ 4096 notes each, ~50 µs per chunk);
/// more go to a worker.
pub const SYNC_CHUNKS: usize = 48;

/// Cheap per-frame key of everything the index depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotesKey {
    doc_generation: u64,
    structure: u64,
    notes: u64,
    props: u64,
    meter: u64,
    /// v0.9: placements and owned contents' notes (0 without clips).
    clips: u64,
}

impl NotesKey {
    pub fn of(doc: &PlumDocument, doc_generation: u64) -> NotesKey {
        let (mut notes, mut props) = (0u64, 0u64);
        for t in doc.tracks() {
            notes = notes.wrapping_add(t.notes.content_revision());
            props = props.wrapping_add(t.props_revision());
        }
        NotesKey {
            doc_generation,
            structure: doc.structure_revision(),
            notes,
            props,
            meter: doc.conductor().meter_revision(),
            clips: if doc.clips_are_identity() {
                0
            } else {
                doc.clips().contents().iter().fold(
                    doc.clips().revision().wrapping_add(1),
                    |a, c| {
                        a.wrapping_mul(0x100_0000_01B3)
                            .wrapping_add(c.notes.content_revision())
                    },
                )
            },
        }
    }
}

/// Key of the time map (tempo; Format 2 files keep tempo in their tracks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimingKey {
    doc_generation: u64,
    structure: u64,
    tempo: u64,
    events: u64,
}

impl TimingKey {
    fn of(doc: &PlumDocument, doc_generation: u64) -> TimingKey {
        TimingKey {
            doc_generation,
            structure: doc.structure_revision(),
            tempo: doc.tempo_revision(),
            events: doc
                .tracks()
                .iter()
                .fold(0u64, |a, t| a.wrapping_add(t.events_revision())),
        }
    }
}

struct BuildJob {
    rx: Receiver<Option<AnalysisIndex>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    notes: u64,
}

struct ProfileJob {
    key: ProfileKey,
    /// The index the profile is built for (kept with the result).
    index: Arc<AnalysisIndex>,
    rx: Receiver<Option<PeakProfile>>,
    cancel: Arc<AtomicBool>,
}

/// Profile updates that re-sweep at most this many blocks (≈ 256 event
/// ticks each, well under a millisecond in all) run at once, so peaks stay
/// current through small edits; larger ones go to a worker.
const SYNC_UPDATE_BLOCKS: usize = 64;
/// Starts and ends of changed pieces examined at once for such an update
/// (a few chunks); more: the delta is taken on the worker.
const SYNC_DELTA_EVENTS: u64 = 1 << 17;

#[derive(Clone, Debug, PartialEq)]
struct ProfileKey {
    index: usize,
    timing: TimingKey,
    scope: Scope,
    window_ns: u64,
}

/// Which peak fields a query computes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peaks {
    /// Highest poly in the trailing window (exact sweep of the window).
    pub window: bool,
    /// Highest poly / NPS so far (needs the background peak profile).
    pub so_far: bool,
}

#[cfg(test)]
impl Peaks {
    pub const NONE: Peaks = Peaks {
        window: false,
        so_far: false,
    };
    pub const ALL: Peaks = Peaks {
        window: true,
        so_far: true,
    };
}

/// One analysed position (what consumers display).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub metrics: Metrics,
    pub peak_nps: Option<u64>,
    pub peak_poly: Option<u64>,
    pub peak_poly_1s: Option<u64>,
    /// v0.9 V09-S17: notes that have ended at or before the position — the
    /// MIDI Player's Passed, as Kiva counts (`docs/PLAYER.md` P4); the
    /// Progressor's Notes count started notes (`metrics.notes_passed`).
    pub ended: u64,
    /// The values describe an earlier version of the document.
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct QueryKey {
    tick: u64,
    notes: NotesKey,
    timing: TimingKey,
    scope: Scope,
    window_ns: u64,
    peaks: Peaks,
    profile_ready: bool,
}

/// A finished background build, for the task log.
pub struct Finished {
    pub elapsed: Duration,
    pub notes: u64,
}

/// Readers of the analysis at once: the Progressor (or Selection
/// Analytics) and the MIDI Player.
const CONSUMERS: usize = 2;

#[derive(Default)]
pub struct AnalysisService {
    index: Option<Arc<AnalysisIndex>>,
    /// The key `index` was verified to describe (None: stale or none).
    current: Option<NotesKey>,
    seen: Option<NotesKey>,
    corrections: Vec<Option<Correction>>,
    timing: Option<(TimingKey, Arc<Timing>)>,
    job: Option<BuildJob>,
    /// Peak profiles, most recently used first, at most [`CONSUMERS`]: the
    /// Progressor and the MIDI Player query different scopes at once
    /// (v0.9 V09-S17, `docs/PLAYER.md` P4) and must not cancel each other.
    /// Each keeps the index it describes: the base of its ranged update
    /// after an edit (v0.9 V09-S18, `docs/INSPECTOR.md` I6), and its address
    /// stays reserved while the key names it.
    profiles: Vec<(ProfileKey, Arc<PeakProfile>, Arc<AnalysisIndex>)>,
    /// Running profile builds, at most [`CONSUMERS`].
    profile_jobs: Vec<ProfileJob>,
    /// The last answers, most recently used first, at most [`CONSUMERS`].
    last: Vec<(QueryKey, Snapshot)>,
    /// Background builds started (diagnostics, tests): edits during a build
    /// are coalesced into one follow-up build, never queued.
    pub builds_started: u64,
    /// Position queries answered from the index (not from the last-query
    /// cache): formatting the readout never adds to it (§202).
    pub queries_computed: u64,
    /// Peak profiles brought up to date at once by a ranged update
    /// (diagnostics, tests).
    pub profiles_updated: u64,
}

impl AnalysisService {
    /// Close Project: called only by the reaper. Cancel and drain workers
    /// before releasing their shared indexes and note inputs.
    pub fn release(mut self) {
        if let Some(j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
            drop(j.rx.into_iter().last());
        }
        for j in self.profile_jobs.drain(..) {
            j.cancel.store(true, Ordering::Relaxed);
            drop(j.rx.into_iter().last());
        }
    }

    /// L-H03: raises every running build's cancel flag without waiting
    /// (Close Project, on the GUI thread, before [`AnalysisService::release`]
    /// drains them on the reaper).
    pub fn cancel_jobs(&self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        for j in &self.profile_jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets everything (another document was opened).
    pub fn reset(&mut self) {
        if let Some(j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
        for j in self.profile_jobs.drain(..) {
            j.cancel.store(true, Ordering::Relaxed);
        }
        *self = AnalysisService::default();
    }

    /// The index, when it describes the current document.
    pub fn index(&self) -> Option<&Arc<AnalysisIndex>> {
        self.current
            .is_some()
            .then_some(self.index.as_ref())
            .flatten()
    }

    /// A build is running.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn heap_bytes(&self) -> usize {
        self.index.as_ref().map_or(0, |i| i.heap_bytes())
            + self
                .profiles
                .iter()
                .map(|(_, p, _)| p.heap_bytes())
                .sum::<usize>()
    }

    /// Per frame: follows document changes, starts / collects builds. Returns
    /// a finished background build (for the status bar).
    pub fn sync(&mut self, doc: &PlumDocument, doc_generation: u64) -> Option<Finished> {
        let mut finished = None;
        let key = NotesKey::of(doc, doc_generation);
        let tkey = TimingKey::of(doc, doc_generation);
        if self.timing.as_ref().is_none_or(|(k, _)| *k != tkey) {
            self.timing = Some((tkey, Arc::new(Timing::for_document(doc))));
        }
        // Collect a finished build.
        if let Some(job) = &self.job {
            match job.rx.try_recv() {
                Ok(result) => {
                    let job = self.job.take().expect("checked");
                    if let Some(idx) = result {
                        finished = Some(Finished {
                            elapsed: job.started.elapsed(),
                            notes: job.notes,
                        });
                        // L-S48: the index it replaces is freed on the
                        // reaper (560 MB at 562M notes: a 78 ms frame on the
                        // owner's PC when the Undo of an Import Into settled).
                        if let Some(old) = self.index.replace(Arc::new(idx)) {
                            super::project::reap(old);
                        }
                        self.current = None;
                        self.seen = None; // re-verify below
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.job = None,
            }
        }
        if self.seen == Some(key) {
            return finished;
        }
        // The document changed (or a build finished): is the index current?
        self.seen = Some(key);
        self.corrections = document_corrections(doc);
        if let Some(idx) = &self.index
            && idx.describes(doc, &self.corrections)
        {
            self.current = Some(key);
            return finished;
        }
        self.current = None;
        if self.job.is_some() {
            // Picked up when the running build finishes (it clears `seen`).
            return finished;
        }
        let work = match &self.index {
            Some(idx) => idx.changed_chunks(doc, &self.corrections),
            None => doc.tracks().iter().map(|t| t.notes.chunk_count()).sum(),
        };
        let inputs = TrackInput::all(doc);
        if work <= SYNC_CHUNKS {
            let prev = self.index.clone();
            if let Some(idx) = AnalysisIndex::build(&inputs, prev.as_deref(), None) {
                self.index = Some(Arc::new(idx));
                self.current = Some(key);
            }
        } else {
            let prev = self.index.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let c2 = Arc::clone(&cancel);
            let (tx, rx) = std::sync::mpsc::channel();
            let notes = doc.note_count() as u64;
            let spawned = std::thread::Builder::new()
                .name("plumspirit-analysis".into())
                .spawn(move || {
                    let r = AnalysisIndex::build(&inputs, prev.as_deref(), Some(&c2));
                    let _ = tx.send(r);
                });
            match spawned {
                Ok(_) => {
                    self.builds_started += 1;
                    self.job = Some(BuildJob {
                        rx,
                        cancel,
                        started: Instant::now(),
                        notes,
                    });
                }
                Err(e) => log::warn!("analysis thread could not start: {e}"),
            }
        }
        finished
    }

    /// The scope's analysis at `tick`. `peaks`: also the peak fields (built in
    /// the background when first asked for; `None` until ready). Returns the
    /// last values (marked stale) while the index is being updated, and `None`
    /// before anything was ever analysed.
    pub fn query(
        &mut self,
        tick: u64,
        scope: &Scope,
        window_ns: u64,
        peaks: Peaks,
    ) -> Option<Snapshot> {
        let (Some(notes), Some(idx), Some((tkey, timing))) =
            (self.current, self.index.clone(), self.timing.clone())
        else {
            // The same consumer's last answer (its scope and window), else
            // the most recent one.
            let same = self
                .last
                .iter()
                .find(|(k, _)| k.scope == *scope && k.window_ns == window_ns);
            return same.or(self.last.first()).map(|(_, s)| Snapshot {
                stale: true,
                ..s.clone()
            });
        };
        let profile = if peaks.so_far {
            self.ensure_profile(&idx, tkey, &timing, scope, window_ns)
        } else {
            None
        };
        let qkey = QueryKey {
            tick,
            notes,
            timing: tkey,
            scope: scope.clone(),
            window_ns,
            peaks,
            profile_ready: profile.is_some(),
        };
        if let Some(i) = self.last.iter().position(|(k, _)| *k == qkey) {
            let hit = self.last.remove(i);
            let s = hit.1.clone();
            self.last.insert(0, hit);
            return Some(s);
        }
        self.queries_computed += 1;
        let metrics = idx.metrics(scope, &timing, tick, window_ns);
        let (mut peak_nps, mut peak_poly, mut peak_poly_1s) = (None, None, None);
        if peaks.window {
            peak_poly_1s = Some(match &profile {
                Some(p) => p.peak_poly_between(&idx, scope, metrics.window_start_tick, tick),
                None => window_peak_poly(&idx, scope, metrics.window_start_tick, tick),
            });
        }
        if let Some(p) = &profile {
            peak_poly = Some(p.peak_poly_so_far(&idx, scope, tick));
            peak_nps = Some(p.peak_nps_so_far(&idx, scope, &timing, tick));
        }
        let snap = Snapshot {
            metrics,
            peak_nps,
            peak_poly,
            peak_poly_1s,
            ended: idx.ends_in(scope, None, tick).0,
            stale: false,
        };
        self.last.insert(0, (qkey, snap.clone()));
        self.last.truncate(CONSUMERS);
        Some(snap)
    }

    fn ensure_profile(
        &mut self,
        idx: &Arc<AnalysisIndex>,
        tkey: TimingKey,
        timing: &Arc<Timing>,
        scope: &Scope,
        window_ns: u64,
    ) -> Option<Arc<PeakProfile>> {
        let key = ProfileKey {
            index: Arc::as_ptr(idx) as usize,
            timing: tkey,
            scope: scope.clone(),
            window_ns,
        };
        if let Some(i) = self.profiles.iter().position(|(k, _, _)| *k == key) {
            let hit = self.profiles.remove(i);
            let p = Arc::clone(&hit.1);
            self.profiles.insert(0, hit);
            return Some(p);
        }
        if let Some(i) = self.profile_jobs.iter().position(|j| j.key == key) {
            match self.profile_jobs[i].rx.try_recv() {
                Ok(Some(p)) => {
                    let job = self.profile_jobs.remove(i);
                    let p = Arc::new(p);
                    self.keep_profile(key, Arc::clone(&p), job.index);
                    return Some(p);
                }
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    self.profile_jobs.remove(i);
                }
                Err(TryRecvError::Empty) => return None,
            }
        }
        // v0.9 V09-S18 (`docs/INSPECTOR.md` I6): the same consumer's profile
        // of an earlier index (same tempo map, scope and window) is updated
        // where the notes changed instead of rebuilt — at once when small.
        let mut base = None;
        if let Some(b) = self.profiles.iter().position(|(k, _, _)| {
            k.timing == key.timing && k.scope == key.scope && k.window_ns == key.window_ns
        }) {
            let (prev_p, prev_i) = (
                Arc::clone(&self.profiles[b].1),
                Arc::clone(&self.profiles[b].2),
            );
            if let Some(delta) = idx.delta_since_within(&prev_i, scope, SYNC_DELTA_EVENTS)
                && prev_p.update_blocks(&delta, timing) <= SYNC_UPDATE_BLOCKS
                && let Some(p) = prev_p.update(idx, scope, timing, &delta, None)
            {
                let p = Arc::new(p);
                super::project::reap(self.profiles.remove(b));
                self.keep_profile(key, Arc::clone(&p), Arc::clone(idx));
                self.profiles_updated += 1;
                return Some(p);
            }
            base = Some((prev_p, prev_i));
        }
        // A build for the same consumer (scope and window) on an older index
        // or tempo map is outdated; beyond that, the oldest build gives way.
        self.profile_jobs.retain(|j| {
            let outdated = j.key.scope == key.scope && j.key.window_ns == key.window_ns;
            if outdated {
                j.cancel.store(true, Ordering::Relaxed);
            }
            !outdated
        });
        while self.profile_jobs.len() >= CONSUMERS {
            let j = self.profile_jobs.remove(0);
            j.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let (tx, rx) = std::sync::mpsc::channel();
        let (i2, t2, s2) = (Arc::clone(idx), Arc::clone(timing), scope.clone());
        if std::thread::Builder::new()
            .name("plumspirit-peaks".into())
            .spawn(move || {
                // A large change: the update (or, past half the song, a
                // build) on the worker; no earlier profile: a build.
                let r = match base.and_then(|(p, i)| Some((p, i2.delta_since(&i, &s2)?))) {
                    Some((p, delta)) => p.update(&i2, &s2, &t2, &delta, Some(&c2)),
                    None => PeakProfile::build(&i2, &s2, &t2, window_ns, Some(&c2)),
                };
                let _ = tx.send(r);
            })
            .is_ok()
        {
            self.profile_jobs.push(ProfileJob {
                key,
                index: Arc::clone(idx),
                rx,
                cancel,
            });
        }
        None
    }

    /// Keeps a finished profile (most recently used first, at most
    /// [`CONSUMERS`]).
    fn keep_profile(&mut self, key: ProfileKey, p: Arc<PeakProfile>, index: Arc<AnalysisIndex>) {
        // It supersedes the same consumer's (scope and window) older one.
        // L-S48: what leaves may hold the last reference to an old index
        // (hundreds of MB at 500M notes): freed on the reaper.
        let (mut gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.profiles)
            .into_iter()
            .partition(|(k, _, _)| k.scope == key.scope && k.window_ns == key.window_ns);
        self.profiles = kept;
        self.profiles.insert(0, (key, p, index));
        if self.profiles.len() > CONSUMERS {
            gone.extend(self.profiles.drain(CONSUMERS..));
        }
        if !gone.is_empty() {
            super::project::reap(gone);
        }
    }

    /// The tempo map the index is read with.
    pub fn timing(&self) -> Option<&Arc<Timing>> {
        self.timing.as_ref().map(|(_, t)| t)
    }

    /// The scope's peak profile of the current index when it is ready —
    /// started on a worker, or updated at once after a small edit, exactly as
    /// a peak query does. The MIDI Inspector's whole-song peaks and charts
    /// (v0.9 V09-S18, `docs/INSPECTOR.md` I9). `None` while the index or the
    /// profile is being brought up to date.
    pub fn profile(&mut self, scope: &Scope, window_ns: u64) -> Option<Arc<PeakProfile>> {
        let (Some(_), Some(idx), Some((tkey, timing))) =
            (self.current, self.index.clone(), self.timing.clone())
        else {
            return None;
        };
        self.ensure_profile(&idx, tkey, &timing, scope, window_ns)
    }

    /// A peak profile is being built (repaint until it is ready).
    pub fn profile_pending(&self) -> bool {
        !self.profile_jobs.is_empty()
    }

    /// Corrections of the document as last synced.
    pub fn corrections(&self) -> &[Option<Correction>] {
        &self.corrections
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::analysis::DEFAULT_WINDOW_NS;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn doc(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("a".into(), Bpm::DEFAULT, 960);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs: Vec<NoteRecord> = (0..n).map(|i| NoteRecord::new(i * 10, 30, a)).collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        d
    }

    fn wait(svc: &mut AnalysisService, d: &PlumDocument) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            svc.sync(d, 1);
            if svc.index().is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "analysis never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn small_projects_are_analysed_at_once_large_ones_in_the_background() {
        let d = doc(1000);
        let mut svc = AnalysisService::default();
        svc.sync(&d, 1);
        assert!(svc.index().is_some(), "small: synchronous");
        let s = svc
            .query(5000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        assert_eq!(s.metrics.notes_passed, 501);
        assert!(!s.stale);
        let big = doc(400_000);
        let mut svc = AnalysisService::default();
        assert!(svc.sync(&big, 1).is_none());
        assert!(svc.busy(), "large: on a worker");
        wait(&mut svc, &big);
        let s = svc
            .query(10, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        assert_eq!(s.metrics.total_notes, 400_000);
    }

    #[test]
    fn edits_update_and_stale_values_are_marked() {
        let mut d = doc(100_000);
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let before = svc
            .query(500_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        let mut h = History::default();
        let a = NoteAttrs::new(70, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: NoteRecord::new(499_990, 100, a),
            },
        )
        .unwrap();
        svc.sync(&d, 1);
        let after = svc
            .query(500_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        assert!(!after.stale, "one chunk: rebuilt at once");
        assert_eq!(after.metrics.total_notes, before.metrics.total_notes + 1);
        assert_eq!(after.metrics.poly, before.metrics.poly + 1);
        // A huge edit goes to the worker: meanwhile the old values are stale.
        d.track_mut(0).unwrap().notes.select_all();
        h.apply(&mut d, Operation::DeleteSelected { track: 0.into() })
            .unwrap();
        svc.sync(&d, 1);
        if svc.index().is_none() {
            let s = svc
                .query(500_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
                .unwrap();
            assert!(s.stale);
        }
        wait(&mut svc, &d);
        let s = svc
            .query(500_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        assert_eq!(s.metrics.total_notes, 0);
        h.undo(&mut d).unwrap();
        wait(&mut svc, &d);
        let s = svc
            .query(500_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        assert_eq!(s.metrics, after.metrics, "undo restores the exact values");
    }

    #[test]
    fn rapid_large_edits_coalesce_into_one_running_build() {
        let mut d = doc(400_000);
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let started = svc.builds_started;
        let mut h = History::default();
        d.track_mut(0).unwrap().notes.select_all();
        for i in 0..20 {
            let delta = if i % 2 == 0 { 7 } else { -3 };
            h.apply(
                &mut d,
                Operation::Resize {
                    track: 0.into(),
                    delta,
                },
            )
            .unwrap();
            svc.sync(&d, 1);
            assert!(svc.busy() || svc.index().is_some());
        }
        wait(&mut svc, &d);
        let builds = svc.builds_started - started;
        assert!(
            builds < 20,
            "{builds} builds for 20 edits: they must coalesce"
        );
        let fresh = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        for t in [0, 123_456, 2_000_000, 3_999_999] {
            let s = svc
                .query(t, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
                .unwrap();
            assert_eq!(
                s.metrics,
                fresh.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS)
            );
        }
    }

    #[test]
    fn peaks_arrive_from_the_background_profile() {
        let d = doc(20_000);
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let s = svc
            .query(100_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::ALL)
            .unwrap();
        assert!(
            s.peak_poly_1s.is_some(),
            "window peak is exact without the profile"
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let s = svc
                .query(100_000, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::ALL)
                .unwrap();
            if let (Some(p), Some(n)) = (s.peak_poly, s.peak_nps) {
                assert_eq!(
                    p, 3,
                    "notes of 30 ticks every 10 ticks overlap three at a time"
                );
                assert!(n > 0);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// v0.9 V09-S17 (`docs/PLAYER.md` P4): the Progressor (audible tracks)
    /// and the MIDI Player (the whole project) query different scopes every
    /// frame; each keeps its peak profile and its last answer instead of
    /// cancelling the other's.
    #[test]
    fn two_consumers_with_different_scopes_keep_their_peak_profiles() {
        let mut d = doc(20_000);
        d.add_track().unwrap();
        let a = NoteAttrs::new(72, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs: Vec<NoteRecord> = (0..5_000)
            .map(|i| NoteRecord::new(i * 40, 200, a))
            .collect();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        let first: Scope = Scope::Tracks([d.track(0).unwrap().id()].into_iter().collect());
        let project = Scope::Project;
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let deadline = Instant::now() + Duration::from_secs(60);
        let (a, b) = loop {
            // One frame: the Progressor's query, then the Player's.
            svc.sync(&d, 1);
            let a = svc.query(100_000, &first, DEFAULT_WINDOW_NS, Peaks::ALL);
            let b = svc.query(100_000, &project, DEFAULT_WINDOW_NS, Peaks::ALL);
            if let (Some(a), Some(b)) = (a, b)
                && a.peak_poly.is_some()
                && b.peak_poly.is_some()
            {
                break (a, b);
            }
            assert!(
                Instant::now() < deadline,
                "a profile was canceled by the other consumer"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(a, truth(&d, 100_000, &first));
        assert_eq!(b, truth(&d, 100_000, &project));
        assert_eq!(svc.profiles.len(), 2);
        // Alternating frames now answer from the two cached answers.
        let computed = svc.queries_computed;
        for _ in 0..10 {
            svc.query(100_000, &first, DEFAULT_WINDOW_NS, Peaks::ALL);
            svc.query(100_000, &project, DEFAULT_WINDOW_NS, Peaks::ALL);
        }
        assert_eq!(svc.queries_computed, computed);
    }

    // ------------------------------------------------- v0.8 §070–072, §228

    /// What a from-scratch analysis says at `tick` (index, timing and peak
    /// profile built anew from the document).
    /// v0.9 V09-S18 (`docs/INSPECTOR.md` I6): after small edits the peaks
    /// are current in the same frame (a ranged update of the earlier
    /// profile, no worker) and equal a fresh build; a large edit goes to the
    /// worker and still ends equal; a tempo edit rebuilds.
    #[test]
    fn small_edits_update_the_peak_profile_at_once_and_exactly() {
        let mut d = doc(60_000);
        let project = Scope::Project;
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        assert_eq!(
            settle(&mut svc, &d, 300_000, &project),
            truth(&d, 300_000, &project)
        );
        let a = NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        for (k, at) in [250_000u64, 10, 599_990, 300_000].into_iter().enumerate() {
            // A burst of 40 long notes: poly +40 over 20,000 ticks, NPS +40.
            let recs: Vec<NoteRecord> = (0..40).map(|_| NoteRecord::new(at, 20_000, a)).collect();
            d.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
            let updated = svc.profiles_updated;
            let jobs = svc.profile_jobs.len();
            svc.sync(&d, 1);
            for t in [0, at, at + 5_000, 300_000, 650_000] {
                let s = svc
                    .query(t, &project, DEFAULT_WINDOW_NS, Peaks::ALL)
                    .expect("analysed");
                assert!(
                    !s.stale && s.peak_poly.is_some(),
                    "edit {k}: peaks current at once"
                );
                assert_eq!(s, truth(&d, t, &project), "edit {k} at tick {t}");
            }
            assert_eq!(
                svc.profiles_updated,
                updated + 1,
                "edit {k}: one ranged update"
            );
            assert_eq!(svc.profile_jobs.len(), jobs, "edit {k}: no worker");
        }
        // A large edit (every note's length): the worker, then exact again.
        d.track_mut(0).unwrap().notes.select_all();
        d.track_mut(0)
            .unwrap()
            .notes
            .set_selected_durations(77)
            .unwrap();
        assert_eq!(
            settle(&mut svc, &d, 300_000, &project),
            truth(&d, 300_000, &project)
        );
        assert_eq!(svc.profiles.len(), 1, "the superseded profile is dropped");
    }

    fn truth(d: &PlumDocument, tick: u64, scope: &Scope) -> Snapshot {
        let idx = AnalysisIndex::for_document(d);
        let timing = Timing::for_document(d);
        let m = idx.metrics(scope, &timing, tick, DEFAULT_WINDOW_NS);
        let p = PeakProfile::build(&idx, scope, &timing, DEFAULT_WINDOW_NS, None).unwrap();
        Snapshot {
            peak_nps: Some(p.peak_nps_so_far(&idx, scope, &timing, tick)),
            peak_poly: Some(p.peak_poly_so_far(&idx, scope, tick)),
            peak_poly_1s: Some(p.peak_poly_between(&idx, scope, m.window_start_tick, tick)),
            ended: idx.ends_in(scope, None, tick).0,
            metrics: m,
            stale: false,
        }
    }

    /// The service's answer at `tick` once it has caught up (frames are
    /// simulated: sync + query until the values are current and the peaks
    /// arrived). Nothing is played.
    fn settle(svc: &mut AnalysisService, d: &PlumDocument, tick: u64, scope: &Scope) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            svc.sync(d, 1);
            if let Some(s) = svc.query(tick, scope, DEFAULT_WINDOW_NS, Peaks::ALL)
                && !s.stale
                && s.peak_poly.is_some()
                && s.peak_nps.is_some()
            {
                return s;
            }
            assert!(Instant::now() < deadline, "the analysis never caught up");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Paused at T: every kind of project change the contract lists updates
    /// Notes / NPS / Poly / PPS and the peaks without playback, to exactly what
    /// a fresh analysis of the changed document says (§070, §071, §228).
    #[test]
    fn paused_progressor_follows_every_kind_of_project_change() {
        use plumspirit_engine::model::{Interp, TempoValue, TrackProps};
        use plumspirit_engine::transform::paste::{self, ColorInfo, ColorNote, Generated};
        use plumspirit_engine::transform::registry::BuildCtx;
        use plumspirit_engine::transform::run::{collect_for, commit};
        use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TrackScope, run_kernel, tool};

        const T: u64 = 600_000; // the paused playhead, inside the notes
        let mut d = doc(120_000); // notes every 10 ticks up to 1.2M
        let mut h = History::default();
        let mut svc = AnalysisService::default();
        let scope = Scope::Project;
        let mut prev = settle(&mut svc, &d, T, &scope);
        assert_eq!(prev, truth(&d, T, &scope));

        // Each step: the change, then the frame loop — no Play, no seek.
        let mut check = |what: &str, d: &PlumDocument, changed: bool| {
            let s = settle(&mut svc, d, T, &scope);
            assert_eq!(s, truth(d, T, &scope), "{what}: equals a fresh analysis");
            if changed {
                assert_ne!(s.metrics, prev.metrics, "{what}: the readout changed");
            }
            prev = s;
        };

        // Add 100,000 notes before the playhead (pasted, as Spiderweb / Image
        // to MIDI do: new Subchannels of the track, one undo step).
        let art = Generated {
            notes: (0..100_000u64)
                .map(|i| ColorNote {
                    start: 100_000 + i * 5,
                    dur: 40,
                    pitch: 40 + (i % 40) as u8,
                    vel: 90,
                    color: (i % 3) as u32,
                })
                .collect(),
            colors: (0..3).map(|c| ColorInfo::new([c * 80, 40, 200])).collect(),
        };
        let spec = paste::PasteSpec::new(d.track(0).unwrap().id(), 0);
        let plan = paste::plan(&d, &art, &spec);
        let op = paste::build(&d, &art, &spec, &plan, "Spiderweb").unwrap();
        h.apply(&mut d, op).unwrap();
        check("paste 100,000 notes before T", &d, true);

        // One note at T.
        let a = NoteAttrs::new(90, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: NoteRecord::new(T, 10, a),
            },
        )
        .unwrap();
        check("insert one note at T", &d, true);

        // Delete notes before the playhead.
        d.track_mut(0)
            .unwrap()
            .notes
            .select_range(200_000, 400_000, false);
        h.apply(&mut d, Operation::DeleteSelected { track: 0.into() })
            .unwrap();
        check("delete notes before T", &d, true);

        // Undo / redo.
        h.undo(&mut d).unwrap();
        check("undo the delete", &d, true);
        h.redo(&mut d).unwrap();
        check("redo the delete", &d, true);

        // HZ-Chop and SAFOR (transform-platform plugins, committed like every
        // tool: one undo step).
        for id in ["hz_chop", "safor"] {
            d.track_mut(0)
                .unwrap()
                .notes
                .select_range(500_000, 650_000, false);
            let def = tool(id).unwrap();
            let values = ParamSet::defaults(&(def.spec)().params);
            let scopes = [TrackScope {
                index: 0,
                all: false,
                content: None,
            }];
            let src = collect_for(def, &d, &scopes, 10_000_000).unwrap();
            let ctx = Ctx::simple(960);
            let b = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let k = def.kernel(&values, &b).unwrap();
            let run = run_kernel(
                k.as_ref(),
                &src,
                def.boundary(&values),
                &Limits::default(),
                None,
            )
            .unwrap();
            commit(&mut h, &mut d, &run, "Plugin").unwrap();
            check(id, &d, id == "hz_chop");
        }

        // Add a track with notes around T, then delete it.
        let recs: Vec<NoteRecord> = (0..5000)
            .map(|i| NoteRecord::new(T - 25_000 + i * 10, 400, a))
            .collect();
        let mut store = plumspirit_engine::model::NoteStore::new();
        store.insert_sorted_batch(&recs, false).unwrap();
        let t = plumspirit_engine::model::Track::from_parts_exact(
            plumspirit_engine::model::TrackId(d.next_track_id()),
            store,
            plumspirit_engine::model::EventStore::new(),
            None,
        );
        h.apply(
            &mut d,
            Operation::InsertTracks {
                index: 1,
                tracks: vec![t],
                label: "Add Track",
            },
        )
        .unwrap();
        check("add a track with notes", &d, true);
        h.apply(&mut d, Operation::DeleteTrack { index: 1 })
            .unwrap();
        check("delete that track", &d, true);
        h.apply(&mut d, Operation::AddTrack).unwrap();
        check("add an empty track", &d, false);

        // Tempo automation before T: NPS / PPS windows are in seconds.
        let edit = d
            .conductor()
            .prepare_set_tempo(
                None,
                300_000,
                TempoValue::Bpm(Bpm::from_integer(240).unwrap()),
                Interp::Linear,
            )
            .unwrap();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
        check("tempo edit", &d, true);
        h.undo(&mut d).unwrap();
        check("undo the tempo edit", &d, true);

        // A track correction (Track Properties) shifts what plays.
        let mut props: TrackProps = d.track(0).unwrap().props().clone();
        props.tick_correction = 20_000;
        let id = d.track(0).unwrap().id();
        h.apply(
            &mut d,
            Operation::TrackProps {
                track: id,
                props,
                end: None,
                label: "Track Properties",
            },
        )
        .unwrap();
        check("track correction", &d, true);
    }

    /// §072: a one-note edit in a big project patches the index at once (no
    /// background rebuild of everything), and the result is exact.
    #[test]
    fn a_one_note_edit_in_a_big_project_is_patched_not_rescanned() {
        let mut d = doc(1_200_000);
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let builds = svc.builds_started;
        let mut h = History::default();
        let a = NoteAttrs::new(70, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let t0 = Instant::now();
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: NoteRecord::new(6_000_005, 50, a),
            },
        )
        .unwrap();
        svc.sync(&d, 1);
        let took = t0.elapsed();
        assert!(svc.index().is_some(), "patched in the same frame");
        assert_eq!(svc.builds_started, builds, "no background rebuild");
        let s = svc
            .query(6_000_010, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::NONE)
            .unwrap();
        let idx = AnalysisIndex::for_document(&d);
        let m = idx.metrics(
            &Scope::Project,
            &Timing::for_document(&d),
            6_000_010,
            DEFAULT_WINDOW_NS,
        );
        assert_eq!(s.metrics, m);
        eprintln!("one-note edit, 1.2M notes: edit + sync {took:?}");
    }

    /// §202: the Progressor's text is formatting only — another template
    /// with the same fields, number format or text never queries again; a
    /// field that needs other data (a peak) does.
    #[test]
    fn editing_the_progressor_text_never_recomputes_the_analysis() {
        use crate::app::progressor::{
            NumberFormat, ProgressorSettings, Readout, Template, Token, render_text,
        };
        let d = doc(5_000);
        let mut svc = AnalysisService::default();
        wait(&mut svc, &d);
        let mut ps = ProgressorSettings::default();
        let frame = |svc: &mut AnalysisService, ps: &ProgressorSettings| {
            let t: Template = Template::parse(&ps.template);
            let peaks = Peaks {
                window: t.uses(|k| k == Token::PeakPoly1s),
                so_far: t.uses(Token::needs_peaks),
            };
            let s = svc
                .query(20_000, &Scope::Project, ps.window_ns(), peaks)
                .unwrap();
            let r = Readout {
                notes: s.metrics.notes_passed,
                total: s.metrics.total_notes,
                nps: s.metrics.nps,
                poly: s.metrics.poly,
                pps: s.metrics.pps,
                ..Readout::default()
            };
            render_text(&t, &r, ps)
        };
        let a = frame(&mut svc, &ps);
        let n = svc.queries_computed;
        ps.template = "Σ {notes} of {total_notes} · {nps} NPS".into();
        let b = frame(&mut svc, &ps);
        ps.number_format = NumberFormat::Plain;
        let c = frame(&mut svc, &ps);
        assert_ne!(a, b);
        assert!(
            b.starts_with("Σ 2,001 of 5,000 · ") && b.ends_with(" NPS"),
            "{b}"
        );
        assert!(c.starts_with("Σ 2001 of 5000"), "{c}");
        assert_eq!(svc.queries_computed, n, "text edits: no new query");
        ps.template = "{peak_poly_1s}".into();
        frame(&mut svc, &ps);
        assert_eq!(
            svc.queries_computed,
            n + 1,
            "a field needing other data queries"
        );
    }
}
