//! Seek-aware project analytics (v0.6): how many notes have started, how many
//! sound, notes per second and polyphony per second — at **any** tick, without
//! playing up to it.
//!
//! See `docs/ANALYTICS_ARCHITECTURE.md` for the metric definitions. In short,
//! every authoritative note is counted once as it plays (Track Properties
//! corrections applied exactly as playback applies them), active over the
//! half-open interval `[start, end)`:
//!
//! * **Notes passed** `N(T)` = notes with `start <= T`.
//! * **Poly** `P(T)` = notes with `start <= T < end` (identical pitches count
//!   separately; zero-length notes never sound).
//! * **NPS** = note starts whose time lies in the trailing window
//!   `(t − W, t]`, `t` = time of `T` from the tempo map, `W` = 1 s by default.
//! * **PPS** = time-weighted average poly over the same window:
//!   `(1 / |window|) ∫ P(x) dx`, using the available window at the start of
//!   the song.
//!
//! # Index
//!
//! [`AnalysisIndex`] is derived data only. For every chunk of every track (a
//! "piece"; a corrected track splits its chunks at the correction start) it keeps
//! the piece's start and end ticks as two compressed sorted multisets
//! ([`ticks::SortedTicks`], about 1.2 bytes per value), plus counts, sums and
//! extremes. Queries binary-search pieces by first start, use running prefix
//! counts for the pieces before, and only touch pieces that are sounding across
//! the query tick, so their cost depends on the music around `T`, not on the
//! song's length. Building is per chunk; a rebuild reuses every piece whose
//! chunk is unchanged (same shared allocation) or whose timing is unchanged
//! (same fingerprint), so editing velocities or a few notes rebuilds almost
//! nothing.
//!
//! The index holds only weak references to chunks (for identity), never the
//! notes themselves, so it neither duplicates notes nor makes edits copy chunks.

pub mod canonical;
pub mod profile;
pub mod selection;
pub mod ticks;
pub mod timeline;

use crate::model::bitset::BitSet;
use crate::model::correction::{Corrected, Correction};
use crate::model::document::PlumDocument;
use crate::model::note::{NoteRecord, SubchannelId};
use crate::model::note_store::{ChunkData, NoteSnapshot, NoteStore};
use crate::model::tempo::{Speed, TempoMap};
use crate::model::track::TrackId;
use crate::view::grid::MeterMap;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use ticks::SortedTicks;
use timeline::Timeline;

/// Notes per window of a full timeline build (bounds the temporary buffers).
const BUILD_WINDOW_NOTES: u64 = 1 << 18;

/// Default NPS / PPS window: one second, in nanoseconds.
pub const DEFAULT_WINDOW_NS: u64 = 1_000_000_000;

/// The correction each track plays with (the same resolution playback uses).
pub fn document_corrections(doc: &PlumDocument) -> Vec<Option<Correction>> {
    let meter = MeterMap::new(doc.division(), &doc.meter_events(0));
    doc.tracks()
        .iter()
        .map(|t| Correction::resolve(t.props(), &meter, doc.project().keys))
        .collect()
}

// ------------------------------------------------------------------ pieces

/// Timing summary of one piece. Shared between index versions.
#[derive(Debug)]
pub struct PieceData {
    pub n: usize,
    pub first_start: u64,
    pub last_start: u64,
    pub min_end: u64,
    pub max_end: u64,
    pub starts: SortedTicks,
    pub ends: SortedTicks,
    /// B1-0105: selection pieces only — the gates (end − start in ticks, as
    /// the notes play) of its notes, for the selection's gate range and the
    /// short / long counts at any threshold. `None` in project indexes.
    pub gates: Option<SortedTicks>,
}

impl PieceData {
    fn from_sorted_starts(
        starts: &[u64],
        mut ends: Vec<u64>,
        gates: Option<Vec<u64>>,
    ) -> Option<PieceData> {
        if starts.is_empty() {
            return None;
        }
        ends.sort_unstable();
        Some(PieceData {
            n: starts.len(),
            first_start: starts[0],
            last_start: *starts.last().expect("non-empty"),
            min_end: ends[0],
            max_end: *ends.last().expect("non-empty"),
            starts: SortedTicks::from_sorted(starts),
            ends: SortedTicks::from_sorted(&ends),
            gates: gates.map(SortedTicks::from_unsorted),
        })
    }

    pub fn heap_bytes(&self) -> usize {
        self.starts.heap_bytes()
            + self.ends.heap_bytes()
            + self.gates.as_ref().map_or(0, SortedTicks::heap_bytes)
    }
}

/// Content signature of a chunk range (timing, and pitch when corrected).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Sig {
    fp: u64,
    n: u32,
    first: u64,
    last: u64,
}

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(27)
}

fn signature(c: &ChunkData, a: usize, b: usize, correction: Option<&Correction>) -> Sig {
    let (st, du, at) = (&c.starts()[a..b], &c.durs()[a..b], &c.attrs_col()[a..b]);
    let mut h = 0x243F_6A88_85A3_08D3u64;
    if let Some(cr) = correction {
        h = mix(h, cr.from_tick);
        h = mix(h, cr.ticks as u64);
        h = mix(h, u64::from(cr.keys as u8));
        h = mix(h, u64::from(cr.gate.hundredths()));
        h = mix(h, u64::from(cr.range.0) << 8 | u64::from(cr.range.1));
        for ((s, d), x) in st.iter().zip(du).zip(at) {
            h = mix(mix(mix(h, *s), *d), u64::from(x.pitch()));
        }
    } else {
        for (s, d) in st.iter().zip(du) {
            h = mix(mix(h, *s), *d);
        }
    }
    // Muted notes (v0.7) do not count: a mute toggle changes the piece.
    for (i, x) in at.iter().enumerate() {
        if x.muted() {
            h = mix(h, 0xA5A5_0000_0000_0000 | i as u64);
        }
    }
    Sig {
        fp: h,
        n: (b - a) as u32,
        first: st.first().copied().unwrap_or(0),
        last: st.last().copied().unwrap_or(0),
    }
}

/// Content signature of the selected notes of a chunk range as a selection
/// piece plays them (v0.9 V09-S19): start, length, pitch and mute of each
/// selected note (pitch too: Unique Pitch Poly reads it), the shift, the
/// correction and the run.
fn selected_signature(
    c: &ChunkData,
    a: usize,
    b: usize,
    sel: &BitSet,
    shift: i128,
    correction: Option<&Correction>,
    run: usize,
) -> (Sig, u64) {
    let mut h = 0x1319_8A2E_0370_7344u64 ^ run as u64;
    h = mix(h, shift as u64);
    h = mix(h, (shift >> 64) as u64);
    if let Some(cr) = correction {
        h = mix(h, cr.from_tick);
        h = mix(h, cr.ticks as u64);
        h = mix(h, u64::from(cr.keys as u16));
        h = mix(h, u64::from(cr.gate.hundredths()));
        h = mix(
            h,
            u64::from(cr.range.0) << 16 | u64::from(cr.range.1) << 8 | u64::from(cr.top),
        );
    }
    // The timing signature: the same without pitches, unless a correction
    // reads them (its key range drops notes).
    let mut tm = mix(h, 0x7457_4D49_4E47);
    let (st, du, at) = (c.starts(), c.durs(), c.attrs_col());
    let (mut n, mut first, mut last) = (0u32, 0u64, 0u64);
    for i in sel.iter_ones().filter(|&i| i >= a && i < b) {
        if n == 0 {
            first = st[i];
        }
        last = st[i];
        n += 1;
        let x = at[i];
        h = mix(mix(h, st[i]), du[i]);
        h = mix(h, u64::from(x.pitch()) | u64::from(x.muted()) << 8);
        tm = mix(mix(tm, st[i]), du[i]);
        tm = if correction.is_some() {
            mix(tm, u64::from(x.pitch()) | u64::from(x.muted()) << 8)
        } else {
            mix(tm, u64::from(x.muted()))
        };
    }
    let sig = Sig {
        fp: h,
        n,
        first,
        last,
    };
    (sig, mix(tm, u64::from(n) << 32 | u64::from(n > 0)))
}

/// [`build_piece`] for a clip (v0.9): the notes start `shift` ticks later in
/// the song than in their content; the correction applies in song time. A
/// separate function so the identity path's code stays exactly v0.8's.
fn build_piece_shifted(
    c: &ChunkData,
    a: usize,
    b: usize,
    shift: i128,
    correction: Option<&Correction>,
    sel: Option<&BitSet>,
) -> Option<PieceData> {
    if shift == 0 {
        return build_piece(c, a, b, correction, sel);
    }
    {
        let song = |t: u64| (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64;
        let mut starts = Vec::with_capacity(b - a);
        let mut ends = Vec::with_capacity(b - a);
        let mut gates = sel.map(|_| Vec::with_capacity(b - a));
        for i in a..b {
            if c.attrs_col()[i].muted() || sel.is_some_and(|s| !s.get(i)) {
                continue;
            }
            let r = NoteRecord {
                start_tick: song(c.starts()[i]),
                duration_ticks: c.durs()[i],
                attrs: c.attrs_col()[i],
                sub: SubchannelId(0),
                group: 0,
            };
            let (s, e) = match correction {
                None => (r.start_tick, r.start_tick.saturating_add(r.duration_ticks)),
                Some(cr) => match cr.note(r) {
                    Corrected::Note { note, .. } => {
                        (note.start_tick, note.start_tick + note.duration_ticks)
                    }
                    _ => continue,
                },
            };
            starts.push(s);
            ends.push(e);
            if let Some(g) = gates.as_mut() {
                g.push(e - s);
            }
        }
        if !starts.windows(2).all(|w| w[0] <= w[1]) {
            starts.sort_unstable();
        }
        PieceData::from_sorted_starts(&starts, ends, gates)
    }
}

/// The piece of notes `a..b` of a chunk as they play. `sel`: only the notes
/// whose bit is set (selection analytics, v0.7).
fn build_piece(
    c: &ChunkData,
    a: usize,
    b: usize,
    correction: Option<&Correction>,
    sel: Option<&BitSet>,
) -> Option<PieceData> {
    let (st, du) = (&c.starts()[a..b], &c.durs()[a..b]);
    let at = &c.attrs_col()[a..b];
    let any_muted = at.iter().any(|x| x.muted());
    let keep = |i: usize| sel.is_none_or(|s| s.get(i));
    match correction {
        None if !any_muted && sel.is_none() => {
            let ends: Vec<u64> = st.iter().zip(du).map(|(s, d)| s + d).collect();
            PieceData::from_sorted_starts(st, ends, None)
        }
        None => {
            let mut starts = Vec::with_capacity(b - a);
            let mut ends = Vec::with_capacity(b - a);
            let mut gates = sel.map(|_| Vec::with_capacity(b - a));
            for (k, ((s, d), x)) in st.iter().zip(du).zip(at).enumerate() {
                if !x.muted() && keep(a + k) {
                    starts.push(*s);
                    ends.push(s + d);
                    if let Some(g) = gates.as_mut() {
                        g.push(*d);
                    }
                }
            }
            PieceData::from_sorted_starts(&starts, ends, gates)
        }
        Some(cr) => {
            let mut starts = Vec::with_capacity(b - a);
            let mut ends = Vec::with_capacity(b - a);
            let mut gates = sel.map(|_| Vec::with_capacity(b - a));
            for i in a..b {
                if !keep(i) {
                    continue;
                }
                let r = NoteRecord {
                    start_tick: c.starts()[i],
                    duration_ticks: c.durs()[i],
                    attrs: c.attrs_col()[i],
                    sub: SubchannelId(0),
                    group: 0,
                };
                if r.attrs.muted() {
                    continue;
                }
                if let Corrected::Note { note, .. } = cr.note(r) {
                    starts.push(note.start_tick);
                    ends.push(note.start_tick + note.duration_ticks);
                    if let Some(g) = gates.as_mut() {
                        g.push(note.duration_ticks);
                    }
                }
            }
            // The two runs of a corrected track are each shifted uniformly, so
            // their starts stay sorted; sort defensively anyway (cheap if so).
            if !starts.windows(2).all(|w| w[0] <= w[1]) {
                starts.sort_unstable();
            }
            PieceData::from_sorted_starts(&starts, ends, gates)
        }
    }
}

#[derive(Clone, Debug)]
struct Piece {
    /// Address of the chunk allocation it was built from (identity only; the
    /// owning [`TrackAnalysis`] keeps a `Weak` to it, so the address cannot be
    /// reused by another chunk while this piece exists).
    chunk: usize,
    sig: Sig,
    /// Selection analytics: the fingerprint of the chunk's selection bits it
    /// was built from (0 for a whole-chunk piece).
    sel: u64,
    /// Selection analytics: the timing signature of its notes (start,
    /// length, mute — and pitch when corrected, which can drop notes); `sig`
    /// holds pitches too. 0 for a whole-chunk piece.
    timing: u64,
    data: Arc<PieceData>,
}

/// Pieces whose start ranges are ordered and do not overlap (one per chunk of
/// an uncorrected track; a corrected track has two runs: before and from the
/// correction start).
#[derive(Clone, Debug, Default)]
struct Run {
    pieces: Vec<Piece>,
    /// `cum_n[i]` / `cum_sum[i]`: notes and start-tick sum of pieces `< i`.
    cum_n: Vec<u64>,
    cum_sum: Vec<u128>,
    /// `prefix_max_end[i]` = max end of pieces `0..=i` (monotone).
    prefix_max_end: Vec<u64>,
}

impl Run {
    fn new(pieces: Vec<Piece>) -> Run {
        let mut cum_n = Vec::with_capacity(pieces.len() + 1);
        let mut cum_sum = Vec::with_capacity(pieces.len() + 1);
        let mut prefix_max_end = Vec::with_capacity(pieces.len());
        let (mut n, mut s, mut m) = (0u64, 0u128, 0u64);
        for p in &pieces {
            cum_n.push(n);
            cum_sum.push(s);
            n += p.data.n as u64;
            s += p.data.starts.total();
            m = m.max(p.data.max_end);
            prefix_max_end.push(m);
        }
        cum_n.push(n);
        cum_sum.push(s);
        Run {
            pieces,
            cum_n,
            cum_sum,
            prefix_max_end,
        }
    }

    fn notes(&self) -> u64 {
        self.cum_n.last().copied().unwrap_or(0)
    }

    /// Number and start-tick sum of notes starting at or before `k`.
    fn starts_le(&self, k: u64) -> (u64, u128) {
        let p = self.pieces.partition_point(|x| x.data.first_start <= k);
        if p == 0 {
            return (0, 0);
        }
        let (c, s) = self.pieces[p - 1].data.starts.prefix_le(k);
        (self.cum_n[p - 1] + c as u64, self.cum_sum[p - 1] + s)
    }

    fn starts_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        let (ch, sh) = self.starts_le(hi);
        match lo {
            Some(l) if l < hi => {
                let (cl, sl) = self.starts_le(l);
                (ch - cl, sh - sl)
            }
            Some(_) => (0, 0),
            None => (ch, sh),
        }
    }

    /// Pieces starting at or before `hi` that can hold a note ending after
    /// `after` (index range; members may still need their `max_end` checked).
    fn live_range(&self, after: Option<u64>, hi: u64) -> std::ops::Range<usize> {
        let p = self.pieces.partition_point(|x| x.data.first_start <= hi);
        let first = match after {
            Some(k) => self.prefix_max_end.partition_point(|&m| m <= k).min(p),
            None => 0,
        };
        first..p
    }

    /// Notes sounding at `k`: start <= k < end.
    fn poly(&self, k: u64) -> u64 {
        let mut n = 0u64;
        for piece in &self.pieces[self.live_range(Some(k), k)] {
            let d = &piece.data;
            if d.max_end > k {
                n += (d.starts.count_le(k) - d.ends.count_le(k)) as u64;
            }
        }
        n
    }

    /// Number and end-tick sum of notes ending in `(lo, hi]`.
    fn ends_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        let (mut c, mut s) = (0u64, 0u128);
        for piece in &self.pieces[self.live_range(lo, hi)] {
            let d = &piece.data;
            if lo.is_some_and(|l| d.max_end <= l) || d.min_end > hi {
                continue;
            }
            if lo.is_none_or(|l| d.min_end > l) && d.max_end <= hi {
                c += d.n as u64;
                s += d.ends.total();
            } else {
                let (pc, ps) = d.ends.range(lo, hi);
                c += pc as u64;
                s += ps;
            }
        }
        (c, s)
    }

    fn for_each_start_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64)) {
        let first = match lo {
            Some(l) => self.pieces.partition_point(|x| x.data.last_start <= l),
            None => 0,
        };
        for piece in &self.pieces[first..] {
            if piece.data.first_start > hi {
                break;
            }
            piece.data.starts.for_each_in(lo, hi, &mut *f);
        }
    }

    fn for_each_end_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64)) {
        for piece in &self.pieces[self.live_range(lo, hi)] {
            let d = &piece.data;
            if lo.is_some_and(|l| d.max_end <= l) || d.min_end > hi {
                continue;
            }
            d.ends.for_each_in(lo, hi, &mut *f);
        }
    }
}

// ------------------------------------------------------------------ tracks

/// What one build did (diagnostics, tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildStats {
    pub tracks_reused: usize,
    pub pieces_reused: usize,
    pub pieces_built: usize,
    pub notes_built: u64,
    /// The project timeline was built from scratch (no usable previous index,
    /// or the change was too large for a delta).
    pub timeline_full: bool,
    /// Timeline segments rewritten by a delta.
    pub segments_rewritten: usize,
    /// Start / end ticks in the applied delta (after cancelling).
    pub delta_events: u64,
}

/// A track's build between [`TrackAnalysis::plan`] and
/// [`TrackPlan::finish`] (L-S38): the pieces to compute are independent, so
/// the pieces of every track of a large build are computed on the analysis
/// workers together (`par_map`), then each track is assembled in chunk order
/// — exactly the analysis the one-thread build made.
struct TrackPlan<'a> {
    id: TrackId,
    chunks: &'a [Arc<ChunkData>],
    correction: Option<Correction>,
    runs: usize,
    by_sig: HashMap<Sig, Arc<PieceData>>,
    entries: Vec<PlanEntry>,
}

enum PlanEntry {
    /// Reused by identity (run, piece).
    Ready(usize, Piece),
    /// To compute: notes `a..b` of chunk `chunk` for `run`.
    Job {
        run: usize,
        chunk: usize,
        a: usize,
        b: usize,
    },
}

/// A computed piece: reused by its signature, or built (`None`: every note
/// of it muted or corrected away).
enum PieceOut {
    Reused(Sig, Arc<PieceData>),
    Built(Sig, Option<Arc<PieceData>>),
}

impl TrackPlan<'_> {
    /// Notes the plan's jobs read.
    fn job_notes(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| match e {
                PlanEntry::Job { a, b, .. } => (b - a) as u64,
                PlanEntry::Ready(..) => 0,
            })
            .sum()
    }

    /// Answers the job `entry` (any thread).
    fn compute(&self, entry: usize) -> PieceOut {
        let PlanEntry::Job { chunk, a, b, .. } = self.entries[entry] else {
            unreachable!("only jobs are computed")
        };
        let c = &self.chunks[chunk];
        let corr = self.correction.as_ref();
        let sig = signature(c, a, b, corr);
        match self.by_sig.get(&sig) {
            Some(d) => PieceOut::Reused(sig, Arc::clone(d)),
            None => PieceOut::Built(sig, build_piece(c, a, b, corr, None).map(Arc::new)),
        }
    }

    /// The track's analysis, `out` answering its jobs in entry order.
    fn finish(
        self,
        out: &mut impl Iterator<Item = PieceOut>,
        stats: &mut BuildStats,
    ) -> TrackAnalysis {
        let mut runs: Vec<Vec<Piece>> = vec![Vec::new(); self.runs];
        for e in self.entries {
            match e {
                PlanEntry::Ready(run, p) => runs[run].push(p),
                PlanEntry::Job { run, chunk, a, b } => {
                    let chunk_addr = Arc::as_ptr(&self.chunks[chunk]) as usize;
                    match out.next().expect("one answer per job") {
                        PieceOut::Reused(sig, data) => {
                            stats.pieces_reused += 1;
                            runs[run].push(Piece {
                                chunk: chunk_addr,
                                sig,
                                sel: 0,
                                timing: 0,
                                data,
                            });
                        }
                        PieceOut::Built(sig, d) => {
                            stats.pieces_built += 1;
                            stats.notes_built += (b - a) as u64;
                            if let Some(d) = d {
                                runs[run].push(Piece {
                                    chunk: chunk_addr,
                                    sig,
                                    sel: 0,
                                    timing: 0,
                                    data: d,
                                });
                            }
                        }
                    }
                }
            }
        }
        let runs: Vec<Run> = runs.into_iter().map(Run::new).collect();
        let notes = runs.iter().map(Run::notes).sum();
        TrackAnalysis {
            id: self.id,
            correction: self.correction,
            lanes: Vec::new(),
            chunks: self.chunks.iter().map(Arc::downgrade).collect(),
            runs,
            notes,
        }
    }
}

/// Worker threads for analysing about `notes` notes (L-S38): the MIDI
/// workers' count (`midi::workers::threads_for`) less one hardware thread,
/// which stays free for the UI (the app builds the index in the background
/// while the user works: on 2 hardware threads the build stays in place);
/// 0 (in place) for a small build, so an incremental update after an edit
/// stays on the calling thread. `PLUMSPIRIT_MIDI_THREADS` overrides it
/// exactly (A/B measurements).
pub(crate) fn analysis_threads(notes: u64) -> usize {
    let t = crate::midi::workers::threads_for(Some(notes.saturating_mul(16)));
    if std::env::var_os("PLUMSPIRIT_MIDI_THREADS").is_some() {
        return t;
    }
    let hw = std::thread::available_parallelism().map_or(1, |n| n.get());
    match t.min(hw.saturating_sub(1)) {
        n if n >= 2 => n,
        _ => 0,
    }
}

/// `f` over `items` on up to `threads` scoped threads (0 or 1: in place),
/// results in item order; each worker owns one `init()` state (scratch
/// buffers). `None` as soon as any call returns `None` (cancelled). A
/// worker's panic is resumed on the caller, as in the one-thread loop.
pub(crate) fn par_map<T, S, R>(
    items: &[T],
    threads: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, &T) -> Option<R> + Sync,
) -> Option<Vec<R>>
where
    T: Sync,
    R: Send,
{
    let threads = threads.min(items.len());
    if threads < 2 {
        let mut state = init();
        return items.iter().map(|t| f(&mut state, t)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let parts: Vec<Option<Vec<(usize, R)>>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut state = init();
                    let mut done = Vec::new();
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            return None;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(t) = items.get(i) else {
                            return Some(done);
                        };
                        match f(&mut state, t) {
                            Some(r) => done.push((i, r)),
                            None => {
                                stop.store(true, Ordering::Relaxed);
                                return None;
                            }
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for part in parts {
        for (i, r) in part? {
            out[i] = Some(r);
        }
    }
    out.into_iter().collect()
}

/// Appends a clip piece (no signature reuse: its timing depends on the
/// placement).
fn runs_push(run: &mut Vec<Piece>, chunk: usize, d: PieceData) {
    let sig = Sig {
        fp: 0,
        n: d.n as u32,
        first: d.first_start,
        last: d.last_start,
    };
    run.push(Piece {
        chunk,
        sig,
        sel: 0,
        timing: 0,
        data: Arc::new(d),
    });
}

/// The analysis of one track (or subtrack).
#[derive(Debug)]
pub struct TrackAnalysis {
    pub id: TrackId,
    correction: Option<Correction>,
    /// v0.9 clips: the placements it was built from (window and shift per
    /// lane); empty for a track that is its own identity clip.
    lanes: Vec<(u64, u64, i128)>,
    /// Identity of the chunks this was built from (never upgraded): keeps their
    /// addresses reserved, so pointer equality means "same, unchanged chunk".
    chunks: Vec<Weak<ChunkData>>,
    runs: Vec<Run>,
    notes: u64,
}

impl TrackAnalysis {
    /// Notes of the track as it plays (corrections applied).
    pub fn notes(&self) -> u64 {
        self.notes
    }

    /// True when this analysis describes exactly these chunks and correction.
    fn matches<'a>(
        &self,
        mut chunks: impl ExactSizeIterator<Item = &'a Arc<ChunkData>>,
        correction: Option<Correction>,
    ) -> bool {
        self.correction == correction
            && self.chunks.len() == chunks.len()
            && self.chunks.iter().all(|w| {
                chunks
                    .next()
                    .is_some_and(|a| std::ptr::eq(w.as_ptr(), Arc::as_ptr(a)))
            })
    }

    /// Chunks of `chunks` not described by this analysis (work estimate).
    fn changed_chunks<'a>(&self, chunks: impl Iterator<Item = &'a Arc<ChunkData>>) -> usize {
        let known: std::collections::HashSet<usize> =
            self.chunks.iter().map(|w| w.as_ptr() as usize).collect();
        chunks
            .filter(|a| !known.contains(&(Arc::as_ptr(a) as usize)))
            .count()
    }

    /// True when this analysis describes exactly `input` (chunks, correction
    /// and, for clips, the placements).
    fn matches_input(&self, input: &TrackInput) -> bool {
        match &input.lanes {
            None => {
                self.lanes.is_empty()
                    && self.matches(input.notes.chunk_arcs().iter(), input.correction)
            }
            Some(lanes) => {
                self.correction == input.correction
                    && self.lanes.len() == lanes.len()
                    && self
                        .lanes
                        .iter()
                        .zip(lanes)
                        .all(|(a, l)| *a == (l.lo, l.hi, l.shift))
                    && {
                        let mut mine = self.chunks.iter();
                        lanes
                            .iter()
                            .flat_map(|l| l.notes.chunk_arcs().iter())
                            .all(|c| {
                                mine.next()
                                    .is_some_and(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(c)))
                            })
                            && mine.next().is_none()
                    }
            }
        }
    }

    /// The analysis of a track's placements (v0.9 clips): per placement one
    /// run (two when corrected) of the notes starting in its window, moved to
    /// song time, corrected there. Rebuilt whole when anything changed.
    /// A track that plays through its clips (`lanes`), built whole. `None`
    /// once `cancel` is raised (checked per chunk, as [`TrackAnalysis::plan`]).
    fn build_lanes(
        id: TrackId,
        lanes: &[InputLane],
        correction: Option<Correction>,
        stats: &mut BuildStats,
        cancel: Option<&AtomicBool>,
    ) -> Option<TrackAnalysis> {
        let mut runs: Vec<Run> = Vec::new();
        let mut chunks_seen = Vec::new();
        for lane in lanes {
            let content =
                |t: u64| (i128::from(t) - lane.shift).clamp(0, i128::from(u64::MAX)) as u64;
            let split = correction.map(|c| content(c.from_tick).max(lane.lo));
            let mut parts: [Vec<Piece>; 2] = [Vec::new(), Vec::new()];
            for c in lane.notes.chunk_arcs() {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return None;
                }
                chunks_seen.push(Arc::downgrade(c));
                let starts = c.starts();
                let a0 = starts.partition_point(|&s| s < lane.lo);
                let b0 = starts.partition_point(|&s| s < lane.hi);
                if a0 >= b0 {
                    continue;
                }
                let cut = split.map_or(b0, |t| starts.partition_point(|&s| s < t).clamp(a0, b0));
                for (run, a, b) in [(0usize, a0, cut), (1, cut, b0)] {
                    if a >= b {
                        continue;
                    }
                    stats.pieces_built += 1;
                    stats.notes_built += (b - a) as u64;
                    if let Some(d) =
                        build_piece_shifted(c, a, b, lane.shift, correction.as_ref(), None)
                    {
                        runs_push(&mut parts[run], Arc::as_ptr(c) as usize, d);
                    }
                }
            }
            let [p0, p1] = parts;
            runs.push(Run::new(p0));
            if split.is_some() {
                runs.push(Run::new(p1));
            }
        }
        let notes = runs.iter().map(Run::notes).sum();
        Some(TrackAnalysis {
            id,
            correction,
            lanes: lanes.iter().map(|l| (l.lo, l.hi, l.shift)).collect(),
            chunks: chunks_seen,
            runs,
            notes,
        })
    }

    /// The first half of a track's build (L-S38): every piece reused by
    /// identity is taken at once; the rest become [`PlanEntry::Job`]s that
    /// [`TrackPlan::compute`] answers (on any thread) and
    /// [`TrackPlan::finish`] puts in place. `None` once `cancel` is raised
    /// (checked per chunk, L-H03: a closed project's build stops within one
    /// chunk, not one whole track).
    fn plan<'a>(
        id: TrackId,
        chunks: &'a [Arc<ChunkData>],
        correction: Option<Correction>,
        previous: Option<&TrackAnalysis>,
        stats: &mut BuildStats,
        cancel: Option<&AtomicBool>,
    ) -> Option<TrackPlan<'a>> {
        // Reuse by identity (same allocation, therefore unchanged), then by
        // content signature (same timing after a velocity / subchannel edit).
        let mut by_ptr: HashMap<(usize, usize), Piece> = HashMap::new();
        let mut by_sig: HashMap<Sig, Arc<PieceData>> = HashMap::new();
        if let Some(prev) = previous.filter(|p| p.correction == correction && p.lanes.is_empty()) {
            for (ri, run) in prev.runs.iter().enumerate() {
                for piece in &run.pieces {
                    by_ptr.insert((piece.chunk, ri), piece.clone());
                    by_sig.insert(piece.sig, Arc::clone(&piece.data));
                }
            }
        }
        let split = correction.map(|c| c.from_tick);
        let mut entries = Vec::new();
        for (ci, c) in chunks.iter().enumerate() {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let addr = Arc::as_ptr(c) as usize;
            let starts = c.starts();
            let cut = match split {
                Some(t) => starts.partition_point(|&s| s < t),
                None => starts.len(),
            };
            for (run, a, b) in [(0usize, 0usize, cut), (1, cut, starts.len())] {
                if a >= b {
                    continue;
                }
                if let Some(p) = by_ptr.get(&(addr, run)) {
                    stats.pieces_reused += 1;
                    entries.push(PlanEntry::Ready(run, p.clone()));
                    continue;
                }
                entries.push(PlanEntry::Job {
                    run,
                    chunk: ci,
                    a,
                    b,
                });
            }
        }
        Some(TrackPlan {
            id,
            chunks,
            correction,
            runs: if split.is_some() { 2 } else { 1 },
            by_sig,
            entries,
        })
    }

    /// The analysis of the **selected** notes of `store` as they play
    /// (selection analytics, v0.7): the same pieces and runs as
    /// [`TrackAnalysis::build`], restricted to the selection bits, `shift`
    /// ticks later (an opened clip's content tick 0 is song tick `shift`;
    /// V09-S11).
    ///
    /// v0.9 V09-S19 (`docs/SELECTION_PEAKS.md`): with `previous` — the
    /// analysis of the same track's selection with the same correction and
    /// shift — a piece is reused when its chunk is the same allocation with
    /// the same selection bits, else when the selected notes' content
    /// (start, length, pitch, mute) is that of a previous piece; only the
    /// chunks an edit or a selection change touched are built.
    pub(crate) fn build_selected(
        id: TrackId,
        store: &NoteStore,
        correction: Option<Correction>,
        shift: i128,
        previous: Option<&TrackAnalysis>,
        stats: &mut BuildStats,
        cancel: Option<&AtomicBool>,
    ) -> Option<TrackAnalysis> {
        let song = |t: u64| (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64;
        let split = correction.map(|c| c.from_tick);
        let mut by_key: HashMap<(usize, usize, u64), &Piece> = HashMap::new();
        let mut by_sig: HashMap<Sig, &Arc<PieceData>> = HashMap::new();
        let mut by_timing: HashMap<u64, &Arc<PieceData>> = HashMap::new();
        if let Some(prev) = previous.filter(|p| p.correction == correction) {
            for (ri, run) in prev.runs.iter().enumerate() {
                for p in &run.pieces {
                    by_key.insert((p.chunk, ri, p.sel), p);
                    by_sig.insert(p.sig, &p.data);
                    by_timing.insert(p.timing, &p.data);
                }
            }
        }
        let mut runs: Vec<Vec<Piece>> = vec![Vec::new(); if split.is_some() { 2 } else { 1 }];
        let mut chunks = Vec::new();
        for (c, sel) in store.selected_chunks() {
            if cancel.is_some_and(|x| x.load(Ordering::Relaxed)) {
                return None;
            }
            let addr = Arc::as_ptr(c) as usize;
            chunks.push(Arc::downgrade(c));
            let fp = sel.fingerprint();
            let starts = c.starts();
            let cut = match split {
                Some(t) => starts.partition_point(|&s| song(s) < t),
                None => starts.len(),
            };
            for (run, a, b) in [(0usize, 0usize, cut), (1, cut, starts.len())] {
                if a >= b {
                    continue;
                }
                if let Some(&p) = by_key.get(&(addr, run, fp)) {
                    stats.pieces_reused += 1;
                    runs[run].push(p.clone());
                    continue;
                }
                let (sig, timing) =
                    selected_signature(c, a, b, sel, shift, correction.as_ref(), run);
                // Same content; else same timing (a pitch-only edit: the
                // index and the peak profile stay, `sig` tells Unique Pitch
                // Poly that the pitches changed).
                let data = match by_sig.get(&sig).or_else(|| by_timing.get(&timing)) {
                    Some(&d) => {
                        stats.pieces_reused += 1;
                        Some(Arc::clone(d))
                    }
                    None => {
                        stats.pieces_built += 1;
                        stats.notes_built += (b - a) as u64;
                        build_piece_shifted(c, a, b, shift, correction.as_ref(), Some(sel))
                            .map(Arc::new)
                    }
                };
                if let Some(data) = data {
                    runs[run].push(Piece {
                        chunk: addr,
                        sig,
                        sel: fp,
                        timing,
                        data,
                    });
                }
            }
        }
        let runs: Vec<Run> = runs.into_iter().map(Run::new).collect();
        let notes = runs.iter().map(Run::notes).sum();
        Some(TrackAnalysis {
            id,
            correction,
            lanes: Vec::new(),
            chunks,
            runs,
            notes,
        })
    }

    fn heap_bytes(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut b = self.chunks.capacity() * 8;
        for r in &self.runs {
            b += r.pieces.capacity() * std::mem::size_of::<Piece>()
                + r.cum_n.capacity() * 8
                + r.cum_sum.capacity() * 16
                + r.prefix_max_end.capacity() * 8;
            for p in &r.pieces {
                if seen.insert(Arc::as_ptr(&p.data) as usize) {
                    b += p.data.heap_bytes() + std::mem::size_of::<PieceData>();
                }
            }
        }
        b
    }
}

// ------------------------------------------------------------------ index

/// One placement of a track (v0.9 clips): the content's notes, the content
/// ticks `[lo, hi)` whose starts the clip plays, and `shift` = song tick −
/// content tick.
#[derive(Clone, Debug)]
pub struct InputLane {
    pub notes: NoteSnapshot,
    pub lo: u64,
    pub hi: u64,
    pub shift: i128,
}

/// One track's input for a build.
#[derive(Clone, Debug)]
pub struct TrackInput {
    pub id: TrackId,
    /// The track's own notes (what plays while the clips are the identity).
    pub notes: NoteSnapshot,
    pub correction: Option<Correction>,
    /// v0.9: the track's unmuted placements when the clips are not the
    /// identity (then they, not `notes`, are what plays).
    pub lanes: Option<Vec<InputLane>>,
}

impl TrackInput {
    /// Every track of `doc` (cheap: shares chunks).
    pub fn all(doc: &PlumDocument) -> Vec<TrackInput> {
        let corr = document_corrections(doc);
        TrackInput::with_corrections(doc, corr)
    }

    /// Every track of `doc` with the given corrections.
    pub fn with_corrections(doc: &PlumDocument, corr: Vec<Option<Correction>>) -> Vec<TrackInput> {
        // B2-S27: every track's placements in one pass (a per-track
        // `placed_contents` looked each content's track up by id: O(tracks²)
        // for a project with thousands of tracks and any clip).
        let mut placed = if doc.clips_are_identity() {
            None
        } else {
            Some(doc.placed_contents_by_track().into_iter())
        };
        doc.tracks()
            .iter()
            .zip(corr)
            .map(|(t, correction)| TrackInput {
                id: t.id(),
                notes: t.notes.snapshot(),
                correction,
                lanes: placed.as_mut().map(|all| {
                    all.next()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|pc| !pc.placement.muted)
                        .map(|pc| {
                            let (lo, hi) = pc.window();
                            InputLane {
                                notes: pc.notes.snapshot(),
                                lo,
                                hi,
                                shift: pc.shift(),
                            }
                        })
                        .collect()
                }),
            })
            .collect()
    }
}

/// Which tracks a query covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every note of the project (default).
    #[default]
    Project,
    /// Only these tracks (audible tracks, or a track and its subtracks).
    Tracks(std::collections::BTreeSet<TrackId>),
}

impl Scope {
    #[inline]
    pub fn includes(&self, id: TrackId) -> bool {
        match self {
            Scope::Project => true,
            Scope::Tracks(set) => set.contains(&id),
        }
    }
}

/// Derived, revision-bound analytical index of a whole project.
#[derive(Debug, Default)]
pub struct AnalysisIndex {
    tracks: Vec<Arc<TrackAnalysis>>,
    /// Every start and end of the project merged (Project-scope queries).
    timeline: Timeline,
    total: u64,
    pub stats: BuildStats,
}

/// How a scope is answered: the merged timeline, a sum over a few tracks, or
/// the timeline minus a few excluded tracks.
enum Plan<'a> {
    All(&'a Timeline),
    Sum(Vec<&'a Run>),
    Diff(&'a Timeline, Vec<&'a Run>),
}

impl Plan<'_> {
    fn starts_le(&self, k: u64) -> (u64, u128) {
        let sum = |runs: &[&Run]| {
            runs.iter()
                .map(|r| r.starts_le(k))
                .fold((0u64, 0u128), |a, b| (a.0 + b.0, a.1 + b.1))
        };
        match self {
            Plan::All(t) => t.starts_le(k),
            Plan::Sum(r) => sum(r),
            Plan::Diff(t, r) => {
                let (a, b) = (t.starts_le(k), sum(r));
                (a.0 - b.0, a.1 - b.1)
            }
        }
    }

    fn poly(&self, k: u64) -> u64 {
        match self {
            Plan::All(t) => t.poly(k),
            Plan::Sum(r) => r.iter().map(|r| r.poly(k)).sum(),
            Plan::Diff(t, r) => t.poly(k) - r.iter().map(|r| r.poly(k)).sum::<u64>(),
        }
    }

    fn starts_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        let sum = |runs: &[&Run]| {
            runs.iter()
                .map(|r| r.starts_range(lo, hi))
                .fold((0u64, 0u128), |a, b| (a.0 + b.0, a.1 + b.1))
        };
        match self {
            Plan::All(t) => t.starts_range(lo, hi),
            Plan::Sum(r) => sum(r),
            Plan::Diff(t, r) => {
                let (a, b) = (t.starts_range(lo, hi), sum(r));
                (a.0 - b.0, a.1 - b.1)
            }
        }
    }

    fn ends_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        let sum = |runs: &[&Run]| {
            runs.iter()
                .map(|r| r.ends_range(lo, hi))
                .fold((0u64, 0u128), |a, b| (a.0 + b.0, a.1 + b.1))
        };
        match self {
            Plan::All(t) => t.ends_range(lo, hi),
            Plan::Sum(r) => sum(r),
            Plan::Diff(t, r) => {
                let (a, b) = (t.ends_range(lo, hi), sum(r));
                (a.0 - b.0, a.1 - b.1)
            }
        }
    }

    /// Every start in `(lo, hi]` with its weight (+1, or −1 for an excluded
    /// track's start that the timeline also reported).
    fn for_each_start_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64, i64)) {
        match self {
            Plan::All(t) => t.for_each_start_in(lo, hi, &mut |x| f(x, 1)),
            Plan::Sum(r) => {
                for run in r {
                    run.for_each_start_in(lo, hi, &mut |x| f(x, 1));
                }
            }
            Plan::Diff(t, r) => {
                t.for_each_start_in(lo, hi, &mut |x| f(x, 1));
                for run in r {
                    run.for_each_start_in(lo, hi, &mut |x| f(x, -1));
                }
            }
        }
    }

    fn for_each_end_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64, i64)) {
        match self {
            Plan::All(t) => t.for_each_end_in(lo, hi, &mut |x| f(x, 1)),
            Plan::Sum(r) => {
                for run in r {
                    run.for_each_end_in(lo, hi, &mut |x| f(x, 1));
                }
            }
            Plan::Diff(t, r) => {
                t.for_each_end_in(lo, hi, &mut |x| f(x, 1));
                for run in r {
                    run.for_each_end_in(lo, hi, &mut |x| f(x, -1));
                }
            }
        }
    }
}

/// L-S48: whether patching `prev`'s timeline with `events` start / end ticks
/// inside `spans` costs less than building the timeline of `after` notes
/// from the pieces on `threads` threads. Per-tick costs measured on one
/// thread (release, `timeline_patch_or_build`): a rewritten segment ~15 ns a
/// note it holds, a delta tick ~40 ns (read, sort, merge), a build ~62 ns a
/// note.
fn patch_is_cheaper(
    prev: &AnalysisIndex,
    spans: &mut [(u64, u64)],
    events: u64,
    after: u64,
    threads: Option<usize>,
) -> bool {
    let segs = prev.timeline.segments().max(1) as u128;
    let touched = prev.timeline.touched(spans) as u128;
    let patch = 15 * u128::from(prev.total) * touched / segs + 40 * u128::from(events);
    let threads = threads.unwrap_or_else(|| analysis_threads(after)).max(1) as u128;
    let build = 62 * u128::from(after) / threads;
    patch <= build
}

/// Sorted `add` minus sorted `del` as multisets, both ways: returns
/// `(del − add, add − del)`.
fn net_delta(del: &[u64], add: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let (mut d, mut a) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < del.len() || j < add.len() {
        match (del.get(i), add.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                d.push(*x);
                i += 1;
            }
            (Some(_), Some(y)) => {
                a.push(*y);
                j += 1;
            }
            (Some(x), None) => {
                d.push(*x);
                i += 1;
            }
            (None, Some(y)) => {
                a.push(*y);
                j += 1;
            }
            (None, None) => break,
        }
    }
    (d, a)
}

/// Start / end ticks removed and added between two versions of the tracks,
/// from the pieces that are not shared (multiset-exact). L-S46: [`Delta::track`]
/// only lists the pieces and counts their ticks; [`Delta::expand`] reads the
/// ticks, so a caller over its budget never walks the notes (the Undo of a
/// clip edit on 224M notes walked ~900M ticks before giving up).
#[derive(Default)]
struct Delta<'a> {
    pending: Vec<(&'a PieceData, i64)>,
    planned: u64,
    del_s: Vec<u64>,
    add_s: Vec<u64>,
    del_e: Vec<u64>,
    add_e: Vec<u64>,
}

impl<'a> Delta<'a> {
    fn piece(&mut self, d: &PieceData, times: i64) {
        let (s, e) = if times > 0 {
            (&mut self.add_s, &mut self.add_e)
        } else {
            (&mut self.del_s, &mut self.del_e)
        };
        for _ in 0..times.unsigned_abs() {
            d.starts.for_each_in(None, u64::MAX, |x| s.push(x));
            d.ends.for_each_in(None, u64::MAX, |x| e.push(x));
        }
    }

    /// Lists the pieces `old` and `new` do not share (net count each) and
    /// counts their ticks; reads none.
    fn track(&mut self, old: Option<&'a TrackAnalysis>, new: Option<&'a TrackAnalysis>) {
        let mut net: HashMap<usize, (&'a Arc<PieceData>, i64)> = HashMap::new();
        for (t, w) in [(old, -1i64), (new, 1)] {
            for run in t.iter().flat_map(|t| t.runs.iter()) {
                for p in &run.pieces {
                    net.entry(Arc::as_ptr(&p.data) as usize)
                        .or_insert((&p.data, 0))
                        .1 += w;
                }
            }
        }
        for (d, n) in net.into_values() {
            if n != 0 {
                self.planned += (d.starts.len() + d.ends.len()) as u64 * n.unsigned_abs();
                self.pending.push((d.as_ref(), n));
            }
        }
    }

    /// Ticks the listed pieces hold (each start and end once per count):
    /// what [`Delta::expand`] would push.
    fn events(&self) -> u64 {
        self.planned
    }

    /// L-S48: the tick span (first start to last end) of every listed piece.
    fn spans(&self) -> Vec<(u64, u64)> {
        self.pending
            .iter()
            .map(|(d, _)| (d.first_start.min(d.min_end), d.last_start.max(d.max_end)))
            .collect()
    }

    /// Reads the listed pieces' ticks.
    fn expand(&mut self) {
        for (d, n) in std::mem::take(&mut self.pending) {
            self.piece(d, n);
        }
        debug_assert_eq!(
            (self.del_s.len() + self.add_s.len() + self.del_e.len() + self.add_e.len()) as u64,
            self.planned
        );
    }
}

/// What a scope's note starts and ends gained and lost between two indexes
/// (net multisets, each sorted): the change a peak profile re-sweeps
/// (v0.9 V09-S18, `docs/INSPECTOR.md` I6; [`AnalysisIndex::delta_since`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickDelta {
    pub removed_starts: Vec<u64>,
    pub added_starts: Vec<u64>,
    pub removed_ends: Vec<u64>,
    pub added_ends: Vec<u64>,
}

impl TickDelta {
    /// Nothing that counts changed (e.g. only pitches, velocities or the
    /// selection did).
    pub fn is_empty(&self) -> bool {
        self.removed_starts.is_empty()
            && self.added_starts.is_empty()
            && self.removed_ends.is_empty()
            && self.added_ends.is_empty()
    }

    /// Ticks in the delta (each start and end once).
    pub fn len(&self) -> usize {
        self.removed_starts.len()
            + self.added_starts.len()
            + self.removed_ends.len()
            + self.added_ends.len()
    }
}

impl AnalysisIndex {
    /// Builds the index of `inputs`, reusing what `previous` already describes.
    /// Returns `None` when `cancel` is raised.
    pub fn build(
        inputs: &[TrackInput],
        previous: Option<&AnalysisIndex>,
        cancel: Option<&AtomicBool>,
    ) -> Option<AnalysisIndex> {
        Self::build_on(inputs, previous, cancel, None)
    }

    /// [`AnalysisIndex::build`] on `threads` workers (`None`: as many as
    /// [`analysis_threads`] gives the build's size).
    pub(crate) fn build_on(
        inputs: &[TrackInput],
        previous: Option<&AnalysisIndex>,
        cancel: Option<&AtomicBool>,
        threads: Option<usize>,
    ) -> Option<AnalysisIndex> {
        let mut stats = BuildStats::default();
        let mut prev: HashMap<TrackId, &Arc<TrackAnalysis>> = HashMap::new();
        if let Some(p) = previous {
            for t in &p.tracks {
                prev.insert(t.id, t);
            }
        }
        enum Slot<'a> {
            Done(Arc<TrackAnalysis>),
            Plan(TrackPlan<'a>),
            /// L-S46: a track that plays through clips, built whole on the
            /// analysis workers (one thread: ~20 ns a note — 11 s for the 448M
            /// notes a linked duplicate of a 224M-note project plays).
            Lanes(&'a TrackInput, &'a [InputLane]),
        }
        let mut slots = Vec::with_capacity(inputs.len());
        for input in inputs {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let chunks = input.notes.chunk_arcs();
            let old = prev.get(&input.id).copied();
            if let Some(o) = old
                && o.matches_input(input)
            {
                stats.tracks_reused += 1;
                slots.push(Slot::Done(Arc::clone(o)));
                continue;
            }
            if let Some(lanes) = &input.lanes {
                slots.push(Slot::Lanes(input, lanes));
                continue;
            }
            slots.push(Slot::Plan(TrackAnalysis::plan(
                input.id,
                chunks,
                input.correction,
                old.map(|a| a.as_ref()),
                &mut stats,
                cancel,
            )?));
        }
        // L-S46: the tracks that play through clips, each built whole, on
        // the analysis workers; exactly the one-thread result (each track is
        // independent; the statistics are sums).
        let lane_slots: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| matches!(s, Slot::Lanes(..)).then_some(i))
            .collect();
        if !lane_slots.is_empty() {
            let lane_notes: u64 = lane_slots
                .iter()
                .map(|&i| match &slots[i] {
                    Slot::Lanes(_, lanes) => lanes.iter().map(|l| l.notes.len() as u64).sum(),
                    _ => 0,
                })
                .sum();
            let built = par_map(
                &lane_slots,
                threads.unwrap_or_else(|| analysis_threads(lane_notes)),
                || (),
                |_, &si| {
                    let Slot::Lanes(input, lanes) = &slots[si] else {
                        unreachable!("lane slots only")
                    };
                    let mut own = BuildStats::default();
                    let t = TrackAnalysis::build_lanes(
                        input.id,
                        lanes,
                        input.correction,
                        &mut own,
                        cancel,
                    )?;
                    Some((t, own.pieces_built, own.notes_built))
                },
            )?;
            for (si, (t, pieces, notes)) in lane_slots.into_iter().zip(built) {
                stats.pieces_built += pieces;
                stats.notes_built += notes;
                slots[si] = Slot::Done(Arc::new(t));
            }
        }
        // L-S38: the pieces of every track, computed together on the
        // analysis workers when there are many (one thread: 87-94 ns a note,
        // 8.7 s for 100M), then each track assembled in chunk order.
        let mut jobs: Vec<(usize, usize)> = Vec::new();
        let mut job_notes = 0u64;
        for (si, slot) in slots.iter().enumerate() {
            if let Slot::Plan(p) = slot {
                job_notes += p.job_notes();
                for (ei, e) in p.entries.iter().enumerate() {
                    if matches!(e, PlanEntry::Job { .. }) {
                        jobs.push((si, ei));
                    }
                }
            }
        }
        let out = par_map(
            &jobs,
            threads.unwrap_or_else(|| analysis_threads(job_notes)),
            || (),
            |_, &(si, ei)| {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return None;
                }
                let Slot::Plan(p) = &slots[si] else {
                    unreachable!("jobs belong to plans")
                };
                Some(p.compute(ei))
            },
        )?;
        let mut out = out.into_iter();
        let tracks: Vec<Arc<TrackAnalysis>> = slots
            .into_iter()
            .map(|s| match s {
                Slot::Done(t) => t,
                Slot::Plan(p) => Arc::new(p.finish(&mut out, &mut stats)),
                Slot::Lanes(..) => unreachable!("lane tracks were built above"),
            })
            .collect();
        let total = tracks.iter().map(|t| t.notes).sum();
        let timeline = Self::timeline(&tracks, previous, &mut stats, cancel, threads)?;
        debug_assert_eq!(timeline.notes(), total);
        Some(AnalysisIndex {
            tracks,
            timeline,
            total,
            stats,
        })
    }

    /// The merged timeline of `tracks`: `previous`'s with the exact delta of
    /// the changed pieces when that is small, else built from the pieces.
    fn timeline(
        tracks: &[Arc<TrackAnalysis>],
        previous: Option<&AnalysisIndex>,
        stats: &mut BuildStats,
        cancel: Option<&AtomicBool>,
        threads: Option<usize>,
    ) -> Option<Timeline> {
        if let Some(prev) = previous {
            let mut delta = Delta::default();
            let old: HashMap<TrackId, &Arc<TrackAnalysis>> =
                prev.tracks.iter().map(|t| (t.id, t)).collect();
            let mut seen = HashSet::with_capacity(tracks.len());
            for t in tracks {
                seen.insert(t.id);
                match old.get(&t.id) {
                    Some(o) if Arc::ptr_eq(o, t) => {}
                    o => delta.track(o.map(|o| o.as_ref()), Some(t)),
                }
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return None;
                }
                if delta.events() > prev.total.max(1) {
                    break;
                }
            }
            for o in prev.tracks.iter().filter(|o| !seen.contains(&o.id)) {
                delta.track(Some(o), None);
            }
            // Patching rewrites every touched segment on this thread; a build
            // reads every piece on `threads` threads. L-S48: the Undo of a
            // 224M-note Import Into (448M ticks out of a 562M-note timeline)
            // patched for 12.8 s on the owner's PC where the build of the
            // rest takes ~2.6 s.
            if delta.events() <= prev.total.max(1)
                && patch_is_cheaper(
                    prev,
                    &mut delta.spans(),
                    delta.events(),
                    tracks.iter().map(|t| t.notes).sum(),
                    threads,
                )
            {
                delta.expand();
                for v in [
                    &mut delta.del_s,
                    &mut delta.add_s,
                    &mut delta.del_e,
                    &mut delta.add_e,
                ] {
                    v.sort_unstable();
                }
                let (ds, as_) = net_delta(&delta.del_s, &delta.add_s);
                let (de, ae) = net_delta(&delta.del_e, &delta.add_e);
                stats.delta_events = (ds.len() + as_.len() + de.len() + ae.len()) as u64;
                let (t, n) = prev.timeline.apply(&ds, &as_, &de, &ae);
                stats.segments_rewritten = n;
                return Some(t);
            }
        }
        stats.timeline_full = true;
        let runs: Vec<&Run> = tracks.iter().flat_map(|t| t.runs.iter()).collect();
        let samples: Vec<(u64, u64)> = runs
            .iter()
            .flat_map(|r| r.pieces.iter())
            .map(|p| (p.data.first_start, p.data.n as u64))
            .collect();
        let bounds = timeline::windows(samples, BUILD_WINDOW_NOTES);
        let notes: u64 = tracks.iter().map(|t| t.notes).sum();
        let threads = threads.unwrap_or_else(|| analysis_threads(notes));
        Timeline::build(&bounds, cancel, threads, |lo, hi, s, e| {
            for r in &runs {
                r.for_each_start_in(lo, hi, &mut |x| s.push(x));
                r.for_each_end_in(lo, hi, &mut |x| e.push(x));
            }
        })
    }

    /// An index over already analysed tracks (selection analytics: tracks
    /// built by [`TrackAnalysis::build_selected`]). `None` when cancelled.
    pub(crate) fn from_tracks(
        tracks: Vec<Arc<TrackAnalysis>>,
        cancel: Option<&AtomicBool>,
    ) -> Option<AnalysisIndex> {
        let mut stats = BuildStats::default();
        let total = tracks.iter().map(|t| t.notes).sum();
        let timeline = Self::timeline(&tracks, None, &mut stats, cancel, None)?;
        Some(AnalysisIndex {
            tracks,
            timeline,
            total,
            stats,
        })
    }

    /// [`AnalysisIndex::from_tracks`] for the next version of an index of
    /// analysed tracks (v0.9 V09-S19: selection analytics): the timeline is
    /// `prev`'s patched with the exact delta of the pieces the two do not
    /// share (a fresh build past about half of the events).
    pub(crate) fn from_tracks_after(
        tracks: Vec<Arc<TrackAnalysis>>,
        prev: &AnalysisIndex,
        cancel: Option<&AtomicBool>,
    ) -> Option<AnalysisIndex> {
        let mut stats = BuildStats::default();
        let total = tracks.iter().map(|t| t.notes).sum();
        let timeline = Self::timeline(&tracks, Some(prev), &mut stats, cancel, None)?;
        debug_assert_eq!(timeline.notes(), total);
        Some(AnalysisIndex {
            tracks,
            timeline,
            total,
            stats,
        })
    }

    /// The starts and ends `scope` gained and lost since `prev` (this index
    /// built from `prev`, or any two indexes of the same project), from the
    /// pieces the two do not share — exact, net (a note whose chunk changed
    /// but whose ticks did not cancels out). `None` when the changed pieces
    /// hold more than twice the scope's notes (and more than 2^16 events): a
    /// full rebuild of what depends on it is as cheap. Costs O(events of the
    /// changed pieces).
    pub fn delta_since(&self, prev: &AnalysisIndex, scope: &Scope) -> Option<TickDelta> {
        // Gross events of the changed pieces (both versions); small songs
        // live in one chunk, so a few chunks' worth is always allowed.
        let limit = (self.notes_in(scope).max(prev.notes_in(scope)) * 2).max(1 << 16);
        self.delta_since_within(prev, scope, limit)
    }

    /// [`AnalysisIndex::delta_since`] that gives up (`None`) once the changed
    /// pieces hold more than `max_events` starts and ends — for callers that
    /// must stay cheap (the UI thread) and hand larger changes to a worker.
    pub fn delta_since_within(
        &self,
        prev: &AnalysisIndex,
        scope: &Scope,
        max_events: u64,
    ) -> Option<TickDelta> {
        let limit = max_events;
        let old: HashMap<TrackId, &Arc<TrackAnalysis>> = prev
            .tracks
            .iter()
            .filter(|t| scope.includes(t.id))
            .map(|t| (t.id, t))
            .collect();
        let mut delta = Delta::default();
        let mut seen = HashSet::new();
        for t in self.tracks.iter().filter(|t| scope.includes(t.id)) {
            seen.insert(t.id);
            match old.get(&t.id) {
                Some(o) if Arc::ptr_eq(o, t) => {}
                o => delta.track(o.map(|o| o.as_ref()), Some(t)),
            }
            if delta.events() > limit {
                return None;
            }
        }
        for o in old.values().filter(|o| !seen.contains(&o.id)) {
            delta.track(Some(o), None);
            if delta.events() > limit {
                return None;
            }
        }
        delta.expand();
        for v in [
            &mut delta.del_s,
            &mut delta.add_s,
            &mut delta.del_e,
            &mut delta.add_e,
        ] {
            v.sort_unstable();
        }
        let (removed_starts, added_starts) = net_delta(&delta.del_s, &delta.add_s);
        let (removed_ends, added_ends) = net_delta(&delta.del_e, &delta.add_e);
        Some(TickDelta {
            removed_starts,
            added_starts,
            removed_ends,
            added_ends,
        })
    }

    /// Convenience: the index of a document from scratch.
    pub fn for_document(doc: &PlumDocument) -> AnalysisIndex {
        AnalysisIndex::build(&TrackInput::all(doc), None, None).expect("not canceled")
    }

    /// True when the index describes exactly the document's notes as they play
    /// (same chunks, same corrections). O(chunks).
    pub fn describes(&self, doc: &PlumDocument, corrections: &[Option<Correction>]) -> bool {
        if !doc.clips_are_identity() {
            let inputs = TrackInput::with_corrections(doc, corrections.to_vec());
            return self.tracks.len() == inputs.len()
                && self
                    .tracks
                    .iter()
                    .zip(&inputs)
                    .all(|(a, i)| a.id == i.id && a.matches_input(i));
        }
        self.tracks.len() == doc.track_count()
            && self
                .tracks
                .iter()
                .zip(doc.tracks())
                .zip(corrections)
                .all(|((a, t), c)| a.id == t.id() && a.matches(t.notes.chunk_data(), *c))
    }

    /// Chunks of `doc` the index does not describe yet (how much an update
    /// would rebuild at most; signature reuse may make it less), plus the
    /// chunks of tracks the index describes that `doc` no longer has: their
    /// notes leave the merged timeline (L-S48: the Undo of a 224M-note Import
    /// Into counted 0 and patched 448M ticks out of the timeline on the
    /// window's thread, 12.2 s).
    pub fn changed_chunks(&self, doc: &PlumDocument, corrections: &[Option<Correction>]) -> usize {
        let prev: HashMap<TrackId, &Arc<TrackAnalysis>> =
            self.tracks.iter().map(|t| (t.id, t)).collect();
        let present: HashSet<TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
        let removed: usize = self
            .tracks
            .iter()
            .filter(|a| !present.contains(&a.id))
            .map(|a| a.chunks.len().max(a.runs.len()))
            .sum();
        if !doc.clips_are_identity() {
            // A track with clips is rebuilt whole when anything changed.
            return removed
                + TrackInput::with_corrections(doc, corrections.to_vec())
                    .iter()
                    .filter(|i| prev.get(&i.id).is_none_or(|a| !a.matches_input(i)))
                    .map(|i| match &i.lanes {
                        Some(l) => l.iter().map(|l| l.notes.chunk_arcs().len()).sum(),
                        None => i.notes.chunk_arcs().len(),
                    })
                    .sum::<usize>();
        }
        removed
            + doc
                .tracks()
                .iter()
                .zip(corrections)
                .map(|(t, c)| match prev.get(&t.id()) {
                    // L-S46: built from clips (now removed): the track and its
                    // part of the timeline are rebuilt whole, even when its
                    // chunks are the same ones (Undo of a linked duplicate).
                    Some(a) if !a.lanes.is_empty() => t.notes.chunk_count() + a.chunks.len(),
                    Some(a) if a.matches(t.notes.chunk_data(), *c) => 0,
                    Some(a) if a.correction == *c => a.changed_chunks(t.notes.chunk_data()),
                    _ => t.notes.chunk_count(),
                })
                .sum::<usize>()
    }

    pub fn tracks(&self) -> &[Arc<TrackAnalysis>] {
        &self.tracks
    }

    /// Notes of the whole project as it plays.
    pub fn total_notes(&self) -> u64 {
        self.total
    }

    /// The earliest start of every note (`None` without notes): the first
    /// piece of each run holds its run's earliest start.
    pub fn first_start(&self) -> Option<u64> {
        self.tracks
            .iter()
            .flat_map(|t| t.runs.iter())
            .filter_map(|r| r.pieces.first().map(|p| p.data.first_start))
            .min()
    }

    /// Heap bytes of the index (shared pieces counted once per track).
    pub fn heap_bytes(&self) -> usize {
        self.tracks.iter().map(|t| t.heap_bytes()).sum::<usize>()
            + self.tracks.capacity() * std::mem::size_of::<Arc<TrackAnalysis>>()
            + self.timeline.heap_bytes()
    }

    /// Heap bytes of the merged project timeline alone.
    pub fn timeline_heap_bytes(&self) -> usize {
        self.timeline.heap_bytes()
    }

    /// Segments of the project timeline.
    pub fn timeline_segments(&self) -> usize {
        self.timeline.segments()
    }

    fn plan(&self, scope: &Scope) -> Plan<'_> {
        match scope {
            Scope::Project => Plan::All(&self.timeline),
            Scope::Tracks(set) => {
                let inside = self.tracks.iter().filter(|t| set.contains(&t.id)).count();
                let runs = |want: bool| {
                    self.tracks
                        .iter()
                        .filter(move |t| set.contains(&t.id) == want)
                        .flat_map(|t| t.runs.iter())
                        .collect::<Vec<&Run>>()
                };
                if inside == self.tracks.len() {
                    Plan::All(&self.timeline)
                } else if inside * 2 > self.tracks.len() {
                    Plan::Diff(&self.timeline, runs(false))
                } else {
                    Plan::Sum(runs(true))
                }
            }
        }
    }

    /// Largest event tick of the scope (an upper bound for subsets).
    pub fn last_tick(&self, scope: &Scope) -> u64 {
        match self.plan(scope) {
            Plan::All(t) | Plan::Diff(t, _) => t.max_tick(),
            Plan::Sum(r) => r
                .iter()
                .filter_map(|r| r.prefix_max_end.last().copied())
                .max()
                .unwrap_or(0),
        }
    }

    /// Notes in scope.
    pub fn notes_in(&self, scope: &Scope) -> u64 {
        match self.plan(scope) {
            Plan::All(_) => self.total,
            Plan::Sum(r) => r.iter().map(|r| r.notes()).sum(),
            Plan::Diff(_, r) => self.total - r.iter().map(|r| r.notes()).sum::<u64>(),
        }
    }

    /// Notes starting at or before `tick`.
    pub fn notes_passed(&self, scope: &Scope, tick: u64) -> u64 {
        self.plan(scope).starts_le(tick).0
    }

    /// Notes sounding at `tick` (start <= tick < end).
    pub fn poly(&self, scope: &Scope, tick: u64) -> u64 {
        self.plan(scope).poly(tick)
    }

    /// Number and tick sum of note starts in `(lo, hi]`.
    pub fn starts_in(&self, scope: &Scope, lo: Option<u64>, hi: u64) -> (u64, u128) {
        self.plan(scope).starts_range(lo, hi)
    }

    /// Number and tick sum of note ends in `(lo, hi]`.
    pub fn ends_in(&self, scope: &Scope, lo: Option<u64>, hi: u64) -> (u64, u128) {
        self.plan(scope).ends_range(lo, hi)
    }

    /// Every note start in `(lo, hi]` with a weight: +1, or −1 for a start
    /// that an excluded track contributes to an earlier +1 (summing the
    /// weights per tick gives the scope's exact multiset). Unsorted.
    pub fn for_each_start_in(
        &self,
        scope: &Scope,
        lo: Option<u64>,
        hi: u64,
        mut f: impl FnMut(u64, i64),
    ) {
        self.plan(scope).for_each_start_in(lo, hi, &mut f);
    }

    /// Every note end in `(lo, hi]` with a weight (see
    /// [`AnalysisIndex::for_each_start_in`]).
    pub fn for_each_end_in(
        &self,
        scope: &Scope,
        lo: Option<u64>,
        hi: u64,
        mut f: impl FnMut(u64, i64),
    ) {
        self.plan(scope).for_each_end_in(lo, hi, &mut f);
    }

    /// Analyzes position `tick` (see [`Metrics`]).
    pub fn metrics(&self, scope: &Scope, timing: &Timing, tick: u64, window_ns: u64) -> Metrics {
        let plan = self.plan(scope);
        let window_ns = window_ns.max(1);
        let t = timing.nanos_at(tick);
        let k_a = canonical::window_lo(timing, t, window_ns);
        let notes = plan.starts_le(tick).0;
        let poly = plan.poly(tick);
        let before = k_a.map_or(0, |k| plan.starts_le(k).0);
        let pps = Self::pps_with(&plan, timing, tick, k_a, window_ns, poly);
        Metrics {
            tick,
            nanos: t,
            notes_passed: notes,
            total_notes: self.notes_in(scope),
            poly,
            nps: notes - before,
            pps,
            window_ns,
            window_start_tick: k_a,
        }
    }

    /// PPS at `tick`: the time-weighted average poly over the trailing window.
    fn pps_with(
        plan: &Plan<'_>,
        timing: &Timing,
        tick: u64,
        k_a: Option<u64>,
        window_ns: u64,
        poly_now: u64,
    ) -> f64 {
        // Exact (fractional) time of the position; the window is W long, or
        // reaches back to the song start.
        let wb = timing.map.frames_at_tick_f64(tick);
        let w = if k_a.is_some() { window_ns as f64 } else { wb };
        if w <= 0.0 {
            // Tick 0 (no time has passed): the average over an empty window is
            // taken as its limit, the poly at the position.
            return poly_now as f64;
        }
        let p_a = k_a.map_or(0, |k| plan.poly(k));
        let mut area = p_a as f64 * w;
        let first = k_a.map_or(0, |k| k + 1);
        if first > tick {
            return area / w;
        }
        for span in timing.map.spans(first, tick) {
            let lo = span.lo.checked_sub(1);
            match span.linear {
                Some((base_tick, base_f, fpt)) => {
                    // τ(x) = τ(lo) + fpt · (x − lo) for every tick x of the span.
                    let tau_lo = base_f + fpt * (span.lo - base_tick) as f64;
                    let rel = |n: u64, sum: u128| -> f64 {
                        if n == 0 {
                            return 0.0;
                        }
                        let off = sum - u128::from(n) * u128::from(span.lo);
                        n as f64 * (wb - tau_lo) - fpt * off as f64
                    };
                    let (ns, ss) = plan.starts_range(lo, span.hi);
                    let (ne, se) = plan.ends_range(lo, span.hi);
                    area += rel(ns, ss) - rel(ne, se);
                }
                None => {
                    let map = &timing.map;
                    plan.for_each_start_in(lo, span.hi, &mut |x, k| {
                        area += k as f64 * (wb - map.frames_at_tick_f64(x));
                    });
                    plan.for_each_end_in(lo, span.hi, &mut |x, k| {
                        area -= k as f64 * (wb - map.frames_at_tick_f64(x));
                    });
                }
            }
        }
        (area / w).max(0.0)
    }
}

/// Tick ↔ nanoseconds of musical (not audition) time, from the Conductor.
#[derive(Clone, Debug)]
pub struct Timing {
    map: Arc<TempoMap>,
}

impl Timing {
    pub fn for_document(doc: &PlumDocument) -> Timing {
        Timing {
            map: Arc::new(TempoMap::for_document_at(
                doc,
                crate::model::duration::NANOS_PER_SECOND,
                Speed::NORMAL,
            )),
        }
    }

    /// Nanoseconds from the start to `tick` (floor).
    #[inline]
    pub fn nanos_at(&self, tick: u64) -> u64 {
        self.map.frame_at_tick(tick)
    }

    /// The last tick that has started by `nanos`.
    #[inline]
    pub fn tick_at_nanos(&self, nanos: u64) -> u64 {
        self.map.tick_at_frame(nanos)
    }

    pub fn map(&self) -> &TempoMap {
        &self.map
    }
}

/// The analysis of one position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub tick: u64,
    /// Musical time of `tick` in nanoseconds.
    pub nanos: u64,
    pub notes_passed: u64,
    pub total_notes: u64,
    pub poly: u64,
    pub nps: u64,
    pub pps: f64,
    pub window_ns: u64,
    /// Last tick before the window (`None`: the window reaches the song start).
    pub window_start_tick: Option<u64>,
}

// --------------------------------------------------- sounding notes (doc)

/// Calls `f` with every note of `store` sounding at `tick` as it plays
/// (`correction` applied): `start <= tick < end`. Visits only chunks and
/// notes that can reach `tick` (chunk maxima bound the scan).
pub fn for_each_sounding(
    store: &NoteStore,
    correction: Option<&Correction>,
    tick: u64,
    mut f: impl FnMut(NoteRecord),
) {
    let (shift_pos, shift_neg) = match correction {
        Some(c) if c.ticks > 0 => (c.ticks as u64, 0u64),
        Some(c) => (0, c.ticks.unsigned_abs()),
        None => (0, 0),
    };
    // Chunks ending (raw) before `tick − (longest possible extension)` cannot
    // sound at `tick`.
    let extend = match correction {
        Some(cr) if cr.gate.hundredths() > 10_000 => {
            let d = store.max_duration();
            cr.gate.apply(d).unwrap_or(u64::MAX).saturating_sub(d)
        }
        _ => 0,
    };
    let from = tick.saturating_sub(shift_pos.saturating_add(extend));
    for c in store.chunk_data_ending_after(from) {
        let st = c.starts();
        if st[0] > tick.saturating_add(shift_neg) {
            break;
        }
        let max_dur = match correction {
            Some(cr) => cr
                .gate
                .apply(c.chunk_max_dur())
                .unwrap_or(u64::MAX)
                .max(c.chunk_max_dur()),
            None => c.chunk_max_dur(),
        };
        if c.chunk_max_end()
            .saturating_add(shift_pos)
            .saturating_add(max_dur - c.chunk_max_dur())
            <= tick
        {
            continue;
        }
        let lo = tick.saturating_sub(shift_pos).saturating_sub(max_dur);
        let hi = tick.saturating_add(shift_neg);
        let a = st.partition_point(|&s| s < lo);
        let b = st.partition_point(|&s| s <= hi);
        for (i, &start) in st.iter().enumerate().take(b).skip(a) {
            let r = NoteRecord {
                start_tick: start,
                duration_ticks: c.durs()[i],
                attrs: c.attrs_col()[i],
                sub: SubchannelId(c.sub_at(i)),
                group: c.group_at(i),
            };
            if r.attrs.muted() {
                continue;
            }
            let n = match correction {
                None => r,
                Some(cr) => match cr.note(r) {
                    Corrected::Note { note, .. } => note,
                    _ => continue,
                },
            };
            if n.start_tick <= tick && n.start_tick + n.duration_ticks > tick {
                f(n);
            }
        }
    }
}

/// The shifts a correction can apply to a start: (later, earlier).
fn correction_reach(correction: Option<&Correction>) -> (u64, u64) {
    match correction {
        Some(c) if c.ticks > 0 => (c.ticks as u64, 0u64),
        Some(c) => (0, c.ticks.unsigned_abs()),
        None => (0, 0),
    }
}

/// Calls `f` with every raw note (content ticks) of placement `pc` whose song
/// start (before corrections) lies in `[a, b]`, moved to song time.
fn placed_raw_starts(
    pc: &crate::model::PlacedContent<'_>,
    a: i128,
    b: i128,
    f: &mut impl FnMut(NoteRecord),
) {
    let (lo, hi) = pc.window();
    let shift = pc.shift();
    let ca = (a - shift).max(i128::from(lo));
    let cb = (b - shift).min(i128::from(hi) - 1);
    if ca > cb || cb < 0 {
        return;
    }
    let ca = ca.clamp(0, i128::from(u64::MAX)) as u64;
    let cb = cb.clamp(0, i128::from(u64::MAX)) as u64;
    let song = |t: u64| (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64;
    for_each_starting(pc.notes, None, ca.checked_sub(1), cb, |mut r| {
        r.start_tick = song(r.start_tick);
        f(r);
    });
}

/// [`for_each_sounding`] for track `index` of `doc` as it plays (v0.9: its
/// unmuted placements; the track's own notes while the clips are the
/// identity).
pub fn for_each_sounding_placed(
    doc: &PlumDocument,
    index: usize,
    correction: Option<&Correction>,
    tick: u64,
    mut f: impl FnMut(NoteRecord),
) {
    if doc.clips_are_identity() {
        if let Some(t) = doc.track(index) {
            for_each_sounding(&t.notes, correction, tick, f);
        }
        return;
    }
    let (later, earlier) = correction_reach(correction);
    for pc in doc.placed_contents(index) {
        if pc.placement.muted {
            continue;
        }
        let d = pc.notes.max_duration();
        let longest = match correction {
            Some(cr) => cr.gate.apply(d).unwrap_or(u64::MAX).max(d),
            None => d,
        };
        let a = i128::from(tick) - i128::from(later) - i128::from(longest);
        let b = i128::from(tick) + i128::from(earlier);
        placed_raw_starts(&pc, a, b, &mut |r| {
            let n = match correction {
                None => r,
                Some(cr) => match cr.note(r) {
                    Corrected::Note { note, .. } => note,
                    _ => return,
                },
            };
            if n.start_tick <= tick && n.start_tick + n.duration_ticks > tick {
                f(n);
            }
        });
    }
}

/// [`for_each_starting`] for track `index` of `doc` as it plays (see
/// [`for_each_sounding_placed`]).
pub fn for_each_starting_placed(
    doc: &PlumDocument,
    index: usize,
    correction: Option<&Correction>,
    lo: Option<u64>,
    hi: u64,
    mut f: impl FnMut(NoteRecord),
) {
    if doc.clips_are_identity() {
        if let Some(t) = doc.track(index) {
            for_each_starting(&t.notes, correction, lo, hi, f);
        }
        return;
    }
    let (later, earlier) = correction_reach(correction);
    for pc in doc.placed_contents(index) {
        if pc.placement.muted {
            continue;
        }
        let a = lo.map_or(i128::MIN / 4, |l| i128::from(l) + 1 - i128::from(later));
        let b = i128::from(hi) + i128::from(earlier);
        placed_raw_starts(&pc, a, b, &mut |r| {
            let n = match correction {
                None => r,
                Some(cr) => match cr.note(r) {
                    Corrected::Note { note, .. } => note,
                    _ => return,
                },
            };
            if lo.is_none_or(|l| n.start_tick > l) && n.start_tick <= hi {
                f(n);
            }
        });
    }
}

/// [`next_start_bound`] for track `index` of `doc` as it plays (see
/// [`for_each_sounding_placed`]): no note starts in `(t, bound)`.
pub fn next_start_bound_placed(
    doc: &PlumDocument,
    index: usize,
    correction: Option<&Correction>,
    t: u64,
) -> Option<u64> {
    if doc.clips_are_identity() {
        return doc
            .track(index)
            .and_then(|tr| next_start_bound(&tr.notes, correction, t));
    }
    let (later, earlier) = correction_reach(correction);
    let mut best: Option<u64> = None;
    for pc in doc.placed_contents(index) {
        if pc.placement.muted {
            continue;
        }
        let (lo, hi) = pc.window();
        let shift = pc.shift();
        // A start s' > t needs a raw song start > t − later, i.e. a content
        // start > t − later − shift, inside the window.
        let raw = i128::from(t) - i128::from(later) - shift;
        let from = if raw < i128::from(lo) {
            lo.checked_sub(1)
        } else {
            Some(raw.clamp(0, i128::from(u64::MAX)) as u64)
        };
        let next = match from {
            None => pc
                .notes
                .chunk_data_after_start(None)
                .next()
                .map(|c| c.starts()[0]),
            Some(f) => next_start_bound(pc.notes, None, f),
        };
        let Some(c) = next else {
            continue;
        };
        if c >= hi {
            continue;
        }
        let song =
            (i128::from(c) + shift - i128::from(earlier)).clamp(0, i128::from(u64::MAX)) as u64;
        let b = song.max(t.saturating_add(1));
        best = Some(best.map_or(b, |x| x.min(b)));
    }
    best
}

/// A lower bound of the first start, as the notes play, after `t`: no note of
/// `store` starts in `(t, bound)`. `None`: no note can start after `t`.
/// Exact without a correction; `O(log chunks + log chunk)`.
pub fn next_start_bound(store: &NoteStore, correction: Option<&Correction>, t: u64) -> Option<u64> {
    let (shift_pos, shift_neg) = match correction {
        Some(c) if c.ticks > 0 => (c.ticks as u64, 0u64),
        Some(c) => (0, c.ticks.unsigned_abs()),
        None => (0, 0),
    };
    // A corrected start s' > t needs a raw start s > t − shift_pos, and then
    // s' >= s − shift_neg.
    let raw = t.checked_sub(shift_pos);
    let c = store.chunk_data_after_start(raw).next()?;
    let st = c.starts();
    let i = raw.map_or(0, |r| st.partition_point(|&s| s <= r));
    let s = *st.get(i)?;
    Some(s.saturating_sub(shift_neg).max(t.saturating_add(1)))
}

/// Calls `f` with every note of `store` whose start as it plays lies in
/// `(lo, hi]` (`lo = None`: from tick 0 inclusive).
pub fn for_each_starting(
    store: &NoteStore,
    correction: Option<&Correction>,
    lo: Option<u64>,
    hi: u64,
    mut f: impl FnMut(NoteRecord),
) {
    let (shift_pos, shift_neg) = match correction {
        Some(c) if c.ticks > 0 => (c.ticks as u64, 0u64),
        Some(c) => (0, c.ticks.unsigned_abs()),
        None => (0, 0),
    };
    // Raw starts that can land in (lo, hi]: corrected ones move by the shift.
    let raw_lo = lo.map(|l| l.saturating_sub(shift_pos));
    let raw_hi = hi.saturating_add(shift_neg);
    for c in store.chunk_data_after_start(raw_lo) {
        let st = c.starts();
        if st[0] > raw_hi {
            break;
        }
        let a = match raw_lo {
            Some(l) => st.partition_point(|&s| s <= l),
            None => 0,
        };
        let b = st.partition_point(|&s| s <= raw_hi);
        for (i, &start) in st.iter().enumerate().take(b).skip(a) {
            let r = NoteRecord {
                start_tick: start,
                duration_ticks: c.durs()[i],
                attrs: c.attrs_col()[i],
                sub: SubchannelId(c.sub_at(i)),
                group: c.group_at(i),
            };
            if r.attrs.muted() {
                continue;
            }
            let n = match correction {
                None => r,
                Some(cr) => match cr.note(r) {
                    Corrected::Note { note, .. } => note,
                    _ => continue,
                },
            };
            if lo.is_none_or(|l| n.start_tick > l) && n.start_tick <= hi {
                f(n);
            }
        }
    }
}

/// V09-S07: the analysis of a document with clips equals the analysis of the
/// same music written into plain tracks (notes, poly, starts / ends, NPS
/// windows), fresh and incrementally after a clip changes.
#[cfg(test)]
mod clip_tests {
    use super::*;
    use crate::model::clip::test_support::{base, clipped, flatten};
    use crate::model::{ClipId, ClipPlacement};

    fn scopes(d: &PlumDocument) -> Vec<Scope> {
        let mut v = vec![Scope::Project];
        for t in d.tracks() {
            v.push(Scope::Tracks([t.id()].into_iter().collect()));
        }
        v
    }

    /// Every query the app makes, at many ticks, for every scope.
    fn answers(d: &PlumDocument, idx: &AnalysisIndex) -> Vec<String> {
        let timing = Timing::for_document(d);
        let mut out = vec![format!("total {}", idx.total_notes())];
        for sc in scopes(d) {
            out.push(format!(
                "in {} last {}",
                idx.notes_in(&sc),
                idx.last_tick(&sc)
            ));
            for k in 0..160u64 {
                let t = k * 97;
                let m = idx.metrics(&sc, &timing, t, DEFAULT_WINDOW_NS);
                out.push(format!(
                    "{t}: passed {} poly {} starts {:?} ends {:?} {m:?}",
                    idx.notes_passed(&sc, t),
                    idx.poly(&sc, t),
                    idx.starts_in(&sc, Some(t / 2), t),
                    idx.ends_in(&sc, Some(t / 2), t),
                ));
            }
        }
        out
    }

    #[test]
    fn clips_analyse_exactly_like_the_flattened_music() {
        let d = clipped();
        let f = flatten(&d);
        let (a, b) = (
            AnalysisIndex::for_document(&d),
            AnalysisIndex::for_document(&f),
        );
        assert_eq!(answers(&d, &a), answers(&f, &b));
        assert!(a.describes(&d, &document_corrections(&d)));
        assert!(!a.describes(&f, &document_corrections(&f)));
        assert_ne!(
            a.total_notes(),
            AnalysisIndex::for_document(&base()).total_notes(),
            "clips change what counts"
        );
    }

    #[test]
    fn moving_a_clip_updates_the_index_incrementally_and_exactly() {
        let mut d = clipped();
        let idx = AnalysisIndex::for_document(&d);
        let t0 = d.track(0).unwrap().id();
        let mut p = d.clips().placement(ClipId::Primary(t0)).unwrap();
        p.start += 333;
        d.clips_mut().set_placement(p);
        assert!(!idx.describes(&d, &document_corrections(&d)));
        assert!(idx.changed_chunks(&d, &document_corrections(&d)) > 0);
        let next = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx), None).unwrap();
        assert_eq!(
            next.stats.tracks_reused, 2,
            "only the changed track is rebuilt"
        );
        let f = flatten(&d);
        assert_eq!(
            answers(&d, &next),
            answers(&f, &AnalysisIndex::for_document(&f))
        );
        // Back to plain tracks: the identity build again, exact.
        let mut e = base();
        let idx_e = AnalysisIndex::build(&TrackInput::all(&e), Some(&next), None).unwrap();
        assert_eq!(
            answers(&e, &idx_e),
            answers(&e, &AnalysisIndex::for_document(&e))
        );
        // A named identity primary is a record but counts like v0.8.
        let t = e.track(0).unwrap().id();
        e.clips_mut().set_placement(ClipPlacement {
            name: Some("x".into()),
            ..ClipPlacement::primary(t)
        });
        assert_eq!(
            answers(&e, &AnalysisIndex::for_document(&e)),
            answers(&base(), &AnalysisIndex::for_document(&base()))
        );
    }

    /// The Live Keyboard's queries (sounding at a tick, starting in a range,
    /// next start bound) through placements equal the flattened twin's, also
    /// on a corrected host.
    #[test]
    fn keyboard_queries_through_placements_equal_the_flattened_music() {
        for corrected in [false, true] {
            let mut d = clipped();
            if corrected {
                let t = d.track_mut(2).unwrap();
                let end = t.end_of_track_tick;
                let props = crate::model::track::TrackProps {
                    tick_correction: -29,
                    correction_from_measure: 2,
                    gate: crate::model::track::GatePercent::from_hundredths(15_000).unwrap(),
                    ..t.props().clone()
                };
                t.set_props(props, end);
            }
            let f = flatten(&d);
            let corr = document_corrections(&d);
            for (i, c) in corr.iter().enumerate() {
                let c = c.as_ref();
                let flat = &f.track(i).unwrap().notes;
                for k in 0..400u64 {
                    let t = k * 37;
                    let mut a = Vec::new();
                    for_each_sounding_placed(&d, i, c, t, |n| a.push(n));
                    let mut b = Vec::new();
                    for_each_sounding(flat, c, t, |n| b.push(n));
                    let key = |n: &NoteRecord| (n.start_tick, n.duration_ticks, n.attrs.raw());
                    a.sort_by_key(key);
                    b.sort_by_key(key);
                    assert_eq!(a, b, "sounding at {t} on track {i}");
                    let (mut a, mut b) = (Vec::new(), Vec::new());
                    for_each_starting_placed(&d, i, c, Some(t), t + 211, |n| a.push(n));
                    for_each_starting(flat, c, Some(t), t + 211, |n| b.push(n));
                    a.sort_by_key(key);
                    b.sort_by_key(key);
                    assert_eq!(a, b, "starting in ({t}, {}] on track {i}", t + 211);
                    // A lower bound: nothing starts in (t, bound).
                    if let Some(bound) = next_start_bound_placed(&d, i, c, t) {
                        let mut early = 0;
                        for_each_starting(flat, c, Some(t), bound - 1, |_| early += 1);
                        assert_eq!(early, 0, "bound {bound} after {t} on track {i}");
                    } else {
                        let mut later = 0;
                        for_each_starting(flat, c, Some(t), u64::MAX, |_| later += 1);
                        assert_eq!(later, 0, "no bound after {t} on track {i}");
                    }
                }
            }
        }
    }

    /// L-S46 (Windows, 224M notes / 700 tracks: Undo of Duplicate Clips
    /// (Linked) held the UI 7.3 s): removing every clip of tracks whose notes
    /// did not change still rebuilds those tracks and their part of the
    /// timeline, so the work estimate counts them whole (the app then builds
    /// in the background, never on the UI thread); the result is exact.
    #[test]
    fn removing_the_clips_of_unchanged_tracks_counts_the_tracks_whole() {
        use crate::history::History;
        let mut d = base();
        let plain = AnalysisIndex::for_document(&d);
        let end = 48 * 120 + 480;
        let copies: Vec<(ClipId, usize, u64)> = (0..d.track_count())
            .map(|i| (ClipId::Primary(d.track(i).unwrap().id()), i, end))
            .collect();
        let op = crate::editor::clips::duplicate_clips(&d, &copies, true).unwrap();
        let mut h = History::default();
        h.apply(&mut d, op).unwrap();
        assert!(!d.clips_are_identity());
        let with_clips = AnalysisIndex::build(&TrackInput::all(&d), Some(&plain), None).unwrap();
        assert_eq!(with_clips.total_notes(), plain.total_notes() * 2);
        h.undo(&mut d).unwrap();
        assert!(d.clips_are_identity(), "Undo restores the plain tracks");
        let corr = document_corrections(&d);
        assert!(!with_clips.describes(&d, &corr));
        let chunks: usize = d.tracks().iter().map(|t| t.notes.chunk_count()).sum();
        assert!(
            with_clips.changed_chunks(&d, &corr) >= chunks,
            "{} < {chunks}",
            with_clips.changed_chunks(&d, &corr)
        );
        let back = AnalysisIndex::build(&TrackInput::all(&d), Some(&with_clips), None).unwrap();
        assert_eq!(answers(&d, &back), answers(&d, &plain));
        // Redo: the clips again, exact.
        h.redo(&mut d).unwrap();
        let again = AnalysisIndex::build(&TrackInput::all(&d), Some(&back), None).unwrap();
        assert_eq!(answers(&d, &again), answers(&d, &with_clips));
    }

    /// L-S48 (Windows, 562M notes: the Undo of a 224M-note / 700-track
    /// Import Into held the UI 12.2 s; Redo 41 ms): tracks the index
    /// describes but the document no longer has count with all their chunks
    /// (their notes leave the merged timeline), so the app builds in the
    /// background; the result is exact.
    #[test]
    fn removed_tracks_count_their_chunks() {
        let mut d = base();
        let idx = AnalysisIndex::for_document(&d);
        let gone = d.track(1).unwrap().notes.chunk_count();
        assert!(gone > 0);
        let (taken, _) = d.take_tracks(1, 1).unwrap();
        assert_eq!(taken.len(), 1);
        let corr = document_corrections(&d);
        assert!(!idx.describes(&d, &corr));
        assert!(
            idx.changed_chunks(&d, &corr) >= gone,
            "{} < {gone}",
            idx.changed_chunks(&d, &corr)
        );
        let next = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx), None).unwrap();
        assert_eq!(
            answers(&d, &next),
            answers(&d, &AnalysisIndex::for_document(&d))
        );
        // Nothing removed, nothing changed: no work.
        assert_eq!(next.changed_chunks(&d, &corr), 0);
    }

    /// L-S48: removing half the notes (the Undo of a large Import Into)
    /// builds the merged timeline from the pieces instead of patching every
    /// segment on one thread; a small edit is still patched; both exact.
    #[test]
    fn large_removals_build_the_timeline_small_edits_patch_it() {
        use crate::model::note::{NoteAttrs, NoteOffKind};
        use crate::model::project::Bpm;
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        for k in 0..16u64 {
            let i = d.add_track().unwrap();
            let recs: Vec<NoteRecord> = (0..20_000u64)
                .map(|j| {
                    let a = NoteAttrs::new(40 + (j % 30) as u8, 90, 0, 0, NoteOffKind::NoteOff)
                        .unwrap();
                    NoteRecord::new(j * 40 + k, 30, a)
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let full = AnalysisIndex::for_document(&d);
        let mut half = d.clone();
        half.take_tracks(8, 8).unwrap();
        let next = AnalysisIndex::build(&TrackInput::all(&half), Some(&full), None).unwrap();
        assert!(next.stats.timeline_full, "{:?}", next.stats);
        assert_eq!(
            answers(&half, &next),
            answers(&half, &AnalysisIndex::for_document(&half))
        );
        let mut one = d.clone();
        let a = NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        one.track_mut(3)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(400_000, 10, a)], false)
            .unwrap();
        let next = AnalysisIndex::build(&TrackInput::all(&one), Some(&full), None).unwrap();
        assert!(!next.stats.timeline_full, "{:?}", next.stats);
        assert_eq!(
            answers(&one, &next),
            answers(&one, &AnalysisIndex::for_document(&one))
        );
    }

    /// L-S48 timing probe: the merged timeline patched with the delta of a
    /// removed / added share of the tracks against a fresh build, as Undo /
    /// Redo of a large Import Into make them. Sizes from
    /// `PLUMSPIRIT_TIMELINE_PROBE` = "notes,tracks" (default 8M,64).
    #[test]
    #[ignore = "timing: cargo test --release -p plumspirit_engine --lib timeline_patch_or_build -- --ignored --nocapture"]
    fn timeline_patch_or_build() {
        use crate::model::note::{NoteAttrs, NoteOffKind};
        use crate::model::project::Bpm;
        let (n, tracks) = std::env::var("PLUMSPIRIT_TIMELINE_PROBE")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once(',')?;
                Some((
                    a.trim().parse::<u64>().ok()?,
                    b.trim().parse::<usize>().ok()?,
                ))
            })
            .unwrap_or((8_000_000, 64));
        let mut d = PlumDocument::new_project("probe".into(), Bpm::DEFAULT, 960);
        let per = n / tracks as u64;
        for k in 0..tracks {
            let i = d.add_track().unwrap();
            let recs: Vec<NoteRecord> = (0..per)
                .map(|j| {
                    let p = 30 + ((j * 7 + k as u64) % 60) as u8;
                    let a = NoteAttrs::new(p, 90, 0, 0, NoteOffKind::NoteOff).unwrap();
                    NoteRecord::new(j * 30 + k as u64 % 30, 45, a)
                })
                .collect();
            let t = d.track_mut(i).unwrap();
            t.notes.insert_sorted_batch(&recs, false).unwrap();
        }
        let full = AnalysisIndex::for_document(&d);
        for share in [2usize, 4, 8, 16, 32] {
            let k = tracks / share;
            let mut less = d.clone();
            less.take_tracks(tracks - k, k).unwrap();
            let t = std::time::Instant::now();
            let patched = AnalysisIndex::build(&TrackInput::all(&less), Some(&full), None).unwrap();
            let patch = t.elapsed();
            let fresh = AnalysisIndex::build(&TrackInput::all(&less), None, None).unwrap();
            assert_eq!(patched.total_notes(), fresh.total_notes());
            // The timeline alone: patched from `full` against built from the
            // (reused) pieces, on the threads a build uses.
            let mut st = BuildStats::default();
            let t = std::time::Instant::now();
            let tl_patch =
                AnalysisIndex::timeline(&patched.tracks, Some(&full), &mut st, None, None).unwrap();
            let tl_patch_t = t.elapsed();
            let t = std::time::Instant::now();
            let tl_full =
                AnalysisIndex::timeline(&patched.tracks, None, &mut st, None, None).unwrap();
            let build = t.elapsed();
            assert_eq!(tl_patch.notes(), tl_full.notes());
            eprintln!("  timeline only: patch {tl_patch_t:?} vs built {build:?}");
            // The other way: the tracks come back (Redo).
            let t = std::time::Instant::now();
            let back = AnalysisIndex::build(&TrackInput::all(&d), Some(&fresh), None).unwrap();
            let readd = t.elapsed();
            assert_eq!(back.total_notes(), full.total_notes());
            eprintln!(
                "timeline probe {n} notes / {tracks} tracks, 1/{share} removed: patch {patch:?} \
                 (full timeline: {}, delta events {}) vs fresh build {build:?}; re-added {readd:?} \
                 (full timeline: {})",
                patched.stats.timeline_full, patched.stats.delta_events, back.stats.timeline_full
            );
        }
    }

    #[test]
    fn corrected_hosts_analyse_like_the_flattened_music() {
        let mut d = clipped();
        let t = d.track_mut(2).unwrap();
        let end = t.end_of_track_tick;
        let props = crate::model::track::TrackProps {
            tick_correction: 41,
            correction_from_measure: 2,
            gate: crate::model::track::GatePercent::from_hundredths(5_000).unwrap(),
            ..t.props().clone()
        };
        t.set_props(props, end);
        let f = flatten(&d);
        assert_eq!(
            answers(&d, &AnalysisIndex::for_document(&d)),
            answers(&f, &AnalysisIndex::for_document(&f))
        );
    }
}

/// L-S38: an index built on several workers is exactly the one-thread build —
/// queries, per-track answers, segments and reuse statistics — fresh, after
/// edits (pieces reused by identity and by signature), with corrections (two
/// runs), muted notes, and when cancelled.
#[cfg(test)]
mod parallel_build_tests {
    use super::*;
    use crate::history::{History, Operation};
    use crate::model::project::Bpm;
    use crate::model::{NoteAttrs, NoteOffKind};

    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n.max(1)
        }
    }

    /// Four tracks of many chunks (more than two timeline windows), muted
    /// notes, track 2 corrected from measure 400 (two runs).
    fn project() -> PlumDocument {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 96);
        let mut h = History::default();
        for _ in 0..3 {
            h.apply(&mut d, Operation::AddTrack).unwrap();
        }
        let mut r = Lcg(9);
        for ti in 0..4usize {
            let n = 160_000 + ti as u64 * 7_000;
            let mut t = 0;
            let recs: Vec<NoteRecord> = (0..n)
                .map(|k| {
                    if k % 5 == 0 {
                        t += 1 + r.below(24);
                    }
                    let a = NoteAttrs::new(
                        20 + r.below(80) as u8,
                        1 + r.below(127) as u8,
                        0,
                        64,
                        NoteOffKind::NoteOff,
                    )
                    .unwrap()
                    .with_muted(r.below(50) == 0);
                    NoteRecord::new(t, r.below(200), a)
                })
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let t = d.track(2).unwrap();
        let mut p = t.props().clone();
        p.tick_correction = 30;
        p.key_correction = -2;
        p.correction_from_measure = 400;
        let id = t.id();
        h.apply(
            &mut d,
            Operation::TrackProps {
                track: id,
                props: p,
                end: None,
                label: "Props",
            },
        )
        .unwrap();
        d
    }

    fn answers(d: &PlumDocument, idx: &AnalysisIndex) -> Vec<String> {
        let timing = Timing::for_document(d);
        let mut scopes = vec![Scope::Project];
        for t in d.tracks() {
            scopes.push(Scope::Tracks([t.id()].into_iter().collect()));
        }
        let mut out = vec![format!("total {}", idx.total_notes())];
        for sc in &scopes {
            let last = idx.last_tick(sc);
            out.push(format!("in {} last {last}", idx.notes_in(sc)));
            for k in 0..120u64 {
                let t = last / 120 * k + k;
                out.push(format!(
                    "{t}: passed {} poly {} starts {:?} ends {:?} {:?}",
                    idx.notes_passed(sc, t),
                    idx.poly(sc, t),
                    idx.starts_in(sc, Some(t / 2), t),
                    idx.ends_in(sc, Some(t / 2), t),
                    idx.metrics(sc, &timing, t, DEFAULT_WINDOW_NS)
                ));
            }
        }
        out
    }

    /// (run, notes, first start, max end, starts, ends) of every piece.
    type PieceDump = (usize, usize, u64, u64, Vec<u64>, Vec<u64>);

    fn pieces(idx: &AnalysisIndex) -> Vec<PieceDump> {
        idx.tracks
            .iter()
            .flat_map(|t| t.runs.iter().enumerate())
            .flat_map(|(ri, r)| {
                r.pieces.iter().map(move |p| {
                    (
                        ri,
                        p.data.n,
                        p.data.first_start,
                        p.data.max_end,
                        p.data.starts.to_vec(),
                        p.data.ends.to_vec(),
                    )
                })
            })
            .collect()
    }

    fn same(d: &PlumDocument, a: &AnalysisIndex, b: &AnalysisIndex) {
        assert_eq!(a.stats, b.stats);
        assert_eq!(a.timeline.dump(), b.timeline.dump());
        assert_eq!(pieces(a), pieces(b));
        assert_eq!(answers(d, a), answers(d, b));
    }

    #[test]
    fn parallel_build_equals_the_one_thread_build() {
        let mut d = project();
        let inputs = TrackInput::all(&d);
        let one = AnalysisIndex::build_on(&inputs, None, None, Some(0)).unwrap();
        assert!(one.stats.timeline_full);
        assert!(one.timeline.segments() > 40, "several windows");
        assert_eq!(one.tracks[2].runs.len(), 2, "a corrected track: two runs");
        for threads in [2, 4, 7] {
            let many = AnalysisIndex::build_on(&inputs, None, None, Some(threads)).unwrap();
            same(&d, &one, &many);
        }
        assert_eq!(
            answers(&d, &one),
            answers(&d, &AnalysisIndex::for_document(&d)),
            "the public build (any worker count) answers the same"
        );

        // Edits: velocities of part of track 1 (same timing: reuse by
        // signature), a note added to track 3 (that chunk rebuilt).
        let tr = d.track_mut(1).unwrap();
        tr.notes.select_range(0, 40_000, false);
        tr.notes
            .map_selected_attrs(|a| a.with_velocity(7).unwrap_or(a));
        let a = NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap();
        d.track_mut(3)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(5_000, 77, a)], false)
            .unwrap();
        let inputs = TrackInput::all(&d);
        let one2 = AnalysisIndex::build_on(&inputs, Some(&one), None, Some(0)).unwrap();
        assert!(one2.stats.pieces_reused > 0 && one2.stats.pieces_built > 0);
        for threads in [2, 5] {
            let many2 = AnalysisIndex::build_on(&inputs, Some(&one), None, Some(threads)).unwrap();
            same(&d, &one2, &many2);
            // The same pieces are shared with the previous index, not copied.
            let was_old = |p: &Piece| {
                one.tracks
                    .iter()
                    .flat_map(|t| t.runs.iter().flat_map(|r| r.pieces.iter()))
                    .any(|o| Arc::ptr_eq(&o.data, &p.data))
            };
            let shared = |i: &AnalysisIndex| -> Vec<bool> {
                i.tracks
                    .iter()
                    .flat_map(|t| t.runs.iter().flat_map(|r| r.pieces.iter()))
                    .map(was_old)
                    .collect()
            };
            assert_eq!(shared(&one2), shared(&many2));
            assert!(shared(&many2).iter().any(|&x| x));
        }
        // A fresh build of the edited project agrees with the incremental one.
        assert_eq!(
            answers(&d, &one2),
            answers(
                &d,
                &AnalysisIndex::build_on(&inputs, None, None, Some(3)).unwrap()
            )
        );
    }

    /// L-S46: tracks that play through clips (a linked duplicate of every
    /// track, one windowed and moved, track 2 corrected) are built on the
    /// workers exactly as on one thread; Undo back to the plain tracks too;
    /// a raised cancel stops a clip build.
    #[test]
    fn parallel_clip_build_equals_the_one_thread_build() {
        use crate::model::{ClipId, ClipPlacement};
        let mut d = project();
        let plain = AnalysisIndex::build_on(&TrackInput::all(&d), None, None, Some(0)).unwrap();
        let end = d
            .tracks()
            .iter()
            .filter_map(|t| t.end_of_track_tick)
            .max()
            .unwrap_or(0)
            + 96;
        let copies: Vec<(ClipId, usize, u64)> = (0..d.track_count())
            .map(|i| (ClipId::Primary(d.track(i).unwrap().id()), i, end))
            .collect();
        let op = crate::editor::clips::duplicate_clips(&d, &copies, true).unwrap();
        let mut h = History::default();
        h.apply(&mut d, op).unwrap();
        let t1 = d.track(1).unwrap().id();
        let p = d.clips().placement(ClipId::Primary(t1)).unwrap();
        d.clips_mut().set_placement(ClipPlacement {
            start: 1_000,
            offset: 2_000,
            length: Some(50_000),
            ..p.clone()
        });
        let inputs = TrackInput::all(&d);
        assert!(inputs.iter().all(|i| i.lanes.is_some()));
        let one = AnalysisIndex::build_on(&inputs, Some(&plain), None, Some(0)).unwrap();
        assert!(one.stats.notes_built > 0);
        for threads in [2, 3, 6] {
            let many = AnalysisIndex::build_on(&inputs, Some(&plain), None, Some(threads)).unwrap();
            same(&d, &one, &many);
        }
        let cancel = AtomicBool::new(true);
        assert!(AnalysisIndex::build_on(&inputs, None, Some(&cancel), Some(3)).is_none());
        assert!(AnalysisIndex::build_on(&inputs, None, Some(&cancel), Some(0)).is_none());
        // Undo both steps: plain tracks again, from the clip index.
        d.clips_mut().set_placement(p);
        h.undo(&mut d).unwrap();
        assert!(d.clips_are_identity());
        let inputs = TrackInput::all(&d);
        let back = AnalysisIndex::build_on(&inputs, Some(&one), None, Some(0)).unwrap();
        for threads in [2, 5] {
            same(
                &d,
                &back,
                &AnalysisIndex::build_on(&inputs, Some(&one), None, Some(threads)).unwrap(),
            );
        }
        assert_eq!(answers(&d, &back), answers(&d, &plain));
    }

    #[test]
    fn a_raised_cancel_stops_the_parallel_build() {
        let d = project();
        let inputs = TrackInput::all(&d);
        let cancel = AtomicBool::new(true);
        assert!(AnalysisIndex::build_on(&inputs, None, Some(&cancel), Some(4)).is_none());
        let none = AtomicBool::new(false);
        assert!(AnalysisIndex::build_on(&inputs, None, Some(&none), Some(4)).is_some());
    }

    #[test]
    fn par_map_keeps_item_order_and_stops_on_none() {
        let items: Vec<u32> = (0..1_000).collect();
        for threads in [0, 1, 2, 9] {
            let out = par_map(
                &items,
                threads,
                || 0u32,
                |seen, &x| {
                    *seen += 1;
                    Some(u64::from(x) * 3)
                },
            )
            .unwrap();
            assert_eq!(
                out,
                items.iter().map(|&x| u64::from(x) * 3).collect::<Vec<_>>()
            );
            assert!(par_map(&items, threads, || (), |_, &x| (x != 700).then_some(x)).is_none());
        }
        assert_eq!(
            par_map(&[] as &[u8], 4, || (), |_, &x| Some(x)),
            Some(vec![])
        );
    }
}
