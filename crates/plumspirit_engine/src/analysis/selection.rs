//! Selection analytics (v0.7, contract §132–134): the v0.6 metrics restricted
//! to the **selected** notes, plus the selection's own totals and peaks.
//!
//! Nothing here is a second analyzer, and no metric is defined here: the
//! definitions and their primitives live in [`super::canonical`]. Everything
//! is derived from an ordinary [`AnalysisIndex`] whose pieces hold only the
//! selected notes ([`TrackAnalysis::build_selected`]), answered by the same
//! code as the Progressor, so the definitions are identical: notes count as
//! they play (Track Properties corrections applied, muted notes skipped),
//! active over `[start, end)`, NPS / PPS over the trailing window of musical
//! time.
//!
//! * Position metrics (NPS, PPS, poly at the transport) — [`AnalysisIndex::metrics`].
//! * **Peak Poly**, **Peak NPS**, **Peak PPS** (v0.9 V09-S19: the highest
//!   value at any tick of the selection, never by playing) and **Poly Area**
//!   in note·ns — the selection index's [`PeakProfile`] with
//!   [`Peaks::WithPps`] (`docs/SELECTION_PEAKS.md`), each with the first tick
//!   reaching it.
//! * **Poly Area** in note·ticks (Σ lengths = Σ end ticks − Σ start ticks),
//!   the counted notes, first start and last end — the index's sums.
//! * **Unique Pitch Poly** — the most distinct pitches sounding at once (pitches
//!   are not in the index): one sweep over the selected notes in start order,
//!   kept as blocks of distinct start ticks.
//!
//! * **Gate range** (B1-0105) — the shortest and longest gate (note length,
//!   end − start in ticks, as the notes play) of the counted notes, their
//!   average, the musical time of both when the tempo is constant over the
//!   selection, and short / medium / long counts at any threshold: every
//!   selection piece keeps its notes' gates as a [`SortedTicks`]
//!   multiset, so the counts are rank queries and an update reuses the
//!   gates of unchanged pieces with the pieces. A gate is a note's length,
//!   not how densely notes are packed.
//!
//! Memory is bounded by the polyphony and the index, not by the selection
//! size: the pitch sweep keeps only the sounding notes' ends (a min-heap) and
//! 256 per-pitch counts.

use super::canonical::PitchPoly;
use super::profile::{PeakProfile, Peaks};
use super::ticks::SortedTicks;
use super::{AnalysisIndex, BuildStats, DEFAULT_WINDOW_NS, Metrics, Scope, Timing, TrackAnalysis};
use crate::model::correction::{Corrected, Correction};
use crate::model::note::{NoteRecord, SubchannelId};
use crate::model::note_store::NoteStore;
use crate::model::track::TrackId;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One track's selection for a build.
#[derive(Clone, Debug)]
pub struct SelectionInput {
    pub id: TrackId,
    /// The track's notes with their selection bits (cheap clone: shared chunks).
    pub notes: NoteStore,
    pub correction: Option<Correction>,
    /// Song tick of tick 0 of `notes` (an opened clip's `start − offset`;
    /// 0 for a track's own notes; V09-S11).
    pub shift: i128,
}

/// The analysis of a selection (see the module documentation).
#[derive(Debug)]
pub struct SelectionAnalysis {
    /// Selected notes as stored.
    pub selected: u64,
    /// Selected notes as they play (muted notes and notes a correction drops
    /// are not counted).
    pub counted: u64,
    /// First start and last end of the counted notes (ticks).
    pub first_tick: u64,
    pub last_tick: u64,
    /// Musical time from the first start to the last end, nanoseconds.
    pub span_ns: u64,
    /// Most simultaneous note instances, and the first tick where it occurs.
    pub peak_poly: u64,
    pub peak_poly_tick: u64,
    /// Most distinct sounding pitches, and the first tick where it occurs.
    pub peak_unique: u32,
    pub peak_unique_tick: u64,
    /// Poly Area in note·ticks (Σ lengths).
    pub area_ticks: u128,
    /// Poly Area in note·nanoseconds (Σ musical lengths).
    pub area_ns: u128,
    /// Highest NPS at any tick (a selected start) and the first tick
    /// reaching it (v0.9 V09-S19; window `window_ns`).
    pub peak_nps: u64,
    pub peak_nps_tick: u64,
    /// Highest PPS at any tick and the first tick reaching it (exact maximum
    /// converted once to f64; v0.9 V09-S19).
    pub peak_pps: f64,
    pub peak_pps_tick: u64,
    /// The NPS / PPS window the peaks are for (the Progressor's).
    pub window_ns: u64,
    /// B1-0105: the counted notes' gate range (`None`: no counted notes).
    pub gate: Option<GateRange>,
    /// Shared (v1.0-Launch L-E03 … L-E08): the selection charts read the
    /// same index and peak profile (`index()`, `profile()`).
    index: Arc<AnalysisIndex>,
    profile: Arc<PeakProfile>,
    unique: UniqueBlocks,
    /// The inputs' tracks, shifts and corrections ([`SelectionAnalysis::update`]
    /// reuses only for the same).
    key: Vec<InputKey>,
    /// What building this analysis re-did.
    pub update_stats: UpdateStats,
}

/// What [`SelectionAnalysis::update`] re-did (tests, measurements, the
/// developer overlay).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    /// A full build (no previous analysis, other inputs).
    pub full: bool,
    pub pieces_built: usize,
    pub pieces_reused: usize,
    pub notes_built: u64,
    /// Peak profile blocks swept again (`None`: the profile was rebuilt).
    pub profile_blocks: Option<usize>,
    /// Unique Pitch Poly blocks swept again (`None`: rebuilt).
    pub unique_blocks: Option<usize>,
}

/// B1-0105: the gates (note lengths in ticks, end − start as the notes
/// play: Track Properties corrections applied, muted notes left out) of a
/// selection's counted notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateRange {
    /// The shortest and the longest gate, ticks.
    pub min: u64,
    pub max: u64,
    /// How many gates, and their sum (the exact average is `sum / count`).
    pub count: u64,
    pub sum: u128,
    /// Musical time of the shortest and the longest gate, nanoseconds —
    /// only when one constant tempo covers the whole selection (else a
    /// gate's time depends on where the note is, and no single value is
    /// exact).
    pub nanos: Option<(u64, u64)>,
}

impl GateRange {
    /// The exact average gate in ticks, as f64 for display.
    pub fn average(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }
}

/// B1-0105: how many gates are short, medium and long.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateCounts {
    /// Gates shorter than the short limit.
    pub short: u64,
    /// The rest below the long limit.
    pub medium: u64,
    /// Gates of at least the long limit.
    pub long: u64,
}

/// B1-0105: PlumSpirit's gate classes, in note values so they mean the same
/// at every PPQN: **short** = shorter than a 1/16 note (PPQN / 4 ticks),
/// **long** = a quarter note (one beat, PPQN ticks) or longer, **medium** =
/// in between. Returns (short limit, long limit) in ticks.
pub fn gate_limits(ppqn: u64) -> (u64, u64) {
    let ppqn = ppqn.max(1);
    ((ppqn / 4).max(1), ppqn)
}

type InputKey = (TrackId, i128, Option<Correction>);

fn input_key(inputs: &[SelectionInput]) -> Vec<InputKey> {
    inputs
        .iter()
        .map(|t| (t.id, t.shift, t.correction))
        .collect()
}

/// The tick spans `[first start, last end]` (merged, inclusive) of the
/// pieces the two versions of the tracks do not share — by timing data and
/// by content signature (a pitch-only edit shares the timing data): where
/// Unique Pitch Poly can differ.
fn changed_spans(old: &[Arc<TrackAnalysis>], new: &[Arc<TrackAnalysis>]) -> Vec<(u64, u64)> {
    let mut net: std::collections::HashMap<(usize, u64), ((u64, u64), i64)> = Default::default();
    for (ts, w) in [(old, -1i64), (new, 1)] {
        for p in ts
            .iter()
            .flat_map(|t| t.runs.iter())
            .flat_map(|r| r.pieces.iter())
        {
            net.entry((Arc::as_ptr(&p.data) as usize, p.sig.fp))
                .or_insert(((p.data.first_start, p.data.max_end), 0))
                .1 += w;
        }
    }
    let mut spans: Vec<(u64, u64)> = net
        .into_values()
        .filter(|(_, n)| *n != 0)
        .map(|(s, _)| s)
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match merged.last_mut() {
            Some(m) if a <= m.1.saturating_add(1) => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Every selection piece's gates.
fn gate_sets(index: &AnalysisIndex) -> impl Iterator<Item = &SortedTicks> {
    index
        .tracks
        .iter()
        .flat_map(|t| t.runs.iter())
        .flat_map(|r| r.pieces.iter())
        .filter_map(|p| p.data.gates.as_ref())
        .filter(|g| !g.is_empty())
}

/// B1-0105: the gate range of the selection index (one pass over its pieces).
fn gate_range(
    index: &AnalysisIndex,
    timing: &Timing,
    first_tick: u64,
    last_tick: u64,
) -> Option<GateRange> {
    let mut r: Option<GateRange> = None;
    for g in gate_sets(index) {
        let x = r.get_or_insert(GateRange {
            min: u64::MAX,
            max: 0,
            count: 0,
            sum: 0,
            nanos: None,
        });
        x.min = x.min.min(g.min());
        x.max = x.max.max(g.max());
        x.count += g.len() as u64;
        x.sum += g.total();
    }
    let mut r = r?;
    let spans = timing.map().spans(first_tick, last_tick);
    if let [one] = spans.as_slice()
        && one.linear.is_some()
    {
        // One constant tempo from the first start to the last end: every
        // note's length in time is its gate at that rate.
        let ns =
            |g: u64| timing.nanos_at(first_tick.saturating_add(g)) - timing.nanos_at(first_tick);
        r.nanos = Some((ns(r.min), ns(r.max)));
    }
    Some(r)
}

/// Notes visited between two cancel checks.
const CANCEL_EVERY: u64 = 1 << 16;

/// The selected notes of one track run as they play: (start, end, pitch),
/// sorted by start. `run`: `None` = every note (no correction), `Some(false)`
/// = before the correction start, `Some(true)` = from it.
fn played<'a>(
    store: &'a NoteStore,
    correction: Option<&'a Correction>,
    run: Option<bool>,
    shift: i128,
    skip_chunks: usize,
) -> impl Iterator<Item = (u64, u64, u8)> + 'a {
    store
        .selected_chunks()
        .skip(skip_chunks)
        .flat_map(move |(c, sel)| {
            sel.iter_ones().filter_map(move |i| {
                let attrs = c.attrs_col()[i];
                if attrs.muted() {
                    return None;
                }
                let start =
                    (i128::from(c.starts()[i]) + shift).clamp(0, i128::from(u64::MAX)) as u64;
                if let (Some(after), Some(cr)) = (run, correction)
                    && (start >= cr.from_tick) != after
                {
                    return None;
                }
                let r = NoteRecord {
                    start_tick: start,
                    duration_ticks: c.durs()[i],
                    attrs,
                    sub: SubchannelId(0),
                    group: 0,
                };
                match correction {
                    None => Some((start, start + r.duration_ticks, attrs.pitch())),
                    Some(cr) => match cr.note(r) {
                        Corrected::Note { note, .. } => Some((
                            note.start_tick,
                            note.start_tick + note.duration_ticks,
                            note.attrs.pitch(),
                        )),
                        _ => None,
                    },
                }
            })
        })
}

type Stream<'a> = std::iter::Peekable<Box<dyn Iterator<Item = (u64, u64, u8)> + 'a>>;

/// The selected notes of every input as they play, merged in start order
/// (a k-way merge of the runs: one per track, two for a corrected one).
struct Merged<'a> {
    streams: Vec<Stream<'a>>,
    heads: BinaryHeap<Reverse<(u64, usize)>>,
}

impl<'a> Merged<'a> {
    /// From the first chunk of each run that can hold a note sounding after
    /// `lo` (the run's first piece in `index` whose running maximum end
    /// passes `lo`); every earlier note has ended by `lo`. `lo = None`:
    /// from the start.
    fn from(
        inputs: &'a [SelectionInput],
        index: Option<&AnalysisIndex>,
        lo: Option<u64>,
    ) -> Merged<'a> {
        let mut streams: Vec<Stream<'a>> = Vec::new();
        for t in inputs.iter().filter(|t| t.notes.selected_count() > 0) {
            let runs: &[Option<bool>] = if t.correction.is_some() {
                &[Some(false), Some(true)]
            } else {
                &[None]
            };
            let ta = index.and_then(|i| i.tracks.iter().find(|a| a.id == t.id));
            let positions: std::collections::HashMap<usize, usize> = match (lo, ta) {
                (Some(_), Some(_)) => t
                    .notes
                    .selected_chunks()
                    .enumerate()
                    .map(|(k, (c, _))| (Arc::as_ptr(c) as usize, k))
                    .collect(),
                _ => Default::default(),
            };
            for (ri, &run) in runs.iter().enumerate() {
                let skip = match (lo, ta.and_then(|a| a.runs.get(ri))) {
                    (Some(l), Some(r)) => {
                        let p0 = r.prefix_max_end.partition_point(|&m| m <= l);
                        r.pieces
                            .get(p0)
                            .and_then(|p| positions.get(&p.chunk).copied())
                            .unwrap_or(usize::MAX)
                    }
                    _ => 0,
                };
                let it: Box<dyn Iterator<Item = (u64, u64, u8)> + 'a> =
                    Box::new(played(&t.notes, t.correction.as_ref(), run, t.shift, skip));
                streams.push(it.peekable());
            }
        }
        let mut heads = BinaryHeap::new();
        for (k, s) in streams.iter_mut().enumerate() {
            if let Some(&(start, ..)) = s.peek() {
                heads.push(Reverse((start, k)));
            }
        }
        Merged { streams, heads }
    }

    fn peek(&self) -> Option<u64> {
        self.heads.peek().map(|h| h.0.0)
    }

    fn next(&mut self) -> Option<(u64, u64, u8)> {
        let Reverse((_, k)) = self.heads.pop()?;
        let note = self.streams[k].next().expect("peeked");
        if let Some(&(next, ..)) = self.streams[k].peek() {
            self.heads.push(Reverse((next, k)));
        }
        Some(note)
    }
}

/// Distinct start ticks per Unique Pitch Poly block.
const UNIQUE_BLOCK_TICKS: usize = 256;

/// The most distinct pitches sounding at once over a block of distinct start
/// ticks `[first, next block's first)`, and the first tick reaching it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UniqueBlock {
    first: u64,
    max: u32,
    at: u64,
}

/// Unique Pitch Poly per block (v0.9 V09-S19, `docs/SELECTION_PEAKS.md`):
/// the distinct-pitch count only rises at a start, so it is settled once
/// per distinct start tick, after the notes ending by then stop.
#[derive(Clone, Debug, Default)]
struct UniqueBlocks {
    blocks: Vec<UniqueBlock>,
}

impl UniqueBlocks {
    /// One sweep over the selected notes in start order. `None` when
    /// cancelled.
    fn build(inputs: &[SelectionInput], cancel: Option<&AtomicBool>) -> Option<UniqueBlocks> {
        let mut out = UniqueBlocks::default();
        Self::sweep(inputs, None, None, None, cancel, &mut out.blocks)?;
        Some(out)
    }

    /// Appends the blocks of the start ticks in `(lo, hi]` to `out` (`lo`
    /// `None`: from the song start; `hi` `None`: to the end). With `lo`, the
    /// notes sounding at `lo` are found first (streams start at the first
    /// chunk whose notes can still sound there, by the pieces of `index`).
    fn sweep(
        inputs: &[SelectionInput],
        index: Option<&AnalysisIndex>,
        lo: Option<u64>,
        hi: Option<u64>,
        cancel: Option<&AtomicBool>,
        out: &mut Vec<UniqueBlock>,
    ) -> Option<()> {
        let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        let mut merged = Merged::from(inputs, index, lo);
        let mut pitches = PitchPoly::default();
        let mut active: BinaryHeap<Reverse<(u64, u8)>> = BinaryHeap::new();
        let mut seen = 0u64;
        if let Some(l) = lo {
            // Seed: the notes started by lo that still sound at lo.
            while merged.peek().is_some_and(|t| t <= l) {
                let (_, end, pitch) = merged.next().expect("peeked");
                seen += 1;
                if seen.is_multiple_of(CANCEL_EVERY) && cancelled() {
                    return None;
                }
                if end > l {
                    active.push(Reverse((end, pitch)));
                    pitches.start(pitch);
                }
            }
        }
        let mut in_block = 0usize;
        while let Some(t) = merged.peek() {
            if hi.is_some_and(|h| t > h) {
                break;
            }
            // Half-open: the notes ended by t stop first, then t's starts.
            while let Some(&Reverse((e, p))) = active.peek() {
                if e > t {
                    break;
                }
                active.pop();
                pitches.end(p);
            }
            while merged.peek() == Some(t) {
                let (start, end, pitch) = merged.next().expect("peeked");
                seen += 1;
                if seen.is_multiple_of(CANCEL_EVERY) && cancelled() {
                    return None;
                }
                if end > start {
                    active.push(Reverse((end, pitch)));
                    pitches.start(pitch);
                } // a zero-length note never sounds
            }
            let d = pitches.distinct();
            if in_block == 0 {
                out.push(UniqueBlock {
                    first: t,
                    max: d,
                    at: t,
                });
            } else {
                let b = out.last_mut().expect("open block");
                if d > b.max {
                    b.max = d;
                    b.at = t;
                }
            }
            in_block = (in_block + 1) % UNIQUE_BLOCK_TICKS;
        }
        Some(())
    }

    /// The blocks after a change whose notes all lie in `spans` (merged
    /// inclusive tick spans: the pieces the two analyses do not share):
    /// only the blocks a span reaches are swept again; past half of them a
    /// full build. `index` is the new selection index.
    fn update(
        &self,
        inputs: &[SelectionInput],
        index: &AnalysisIndex,
        spans: &[(u64, u64)],
        cancel: Option<&AtomicBool>,
    ) -> Option<(UniqueBlocks, Option<usize>)> {
        if spans.is_empty() {
            return Some((self.clone(), Some(0)));
        }
        if self.blocks.is_empty() {
            return Some((Self::build(inputs, cancel)?, None));
        }
        let block_of = |t: u64| {
            self.blocks
                .partition_point(|b| b.first <= t)
                .saturating_sub(1)
        };
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for &(a, b) in spans {
            let (j0, j1) = (block_of(a), block_of(b));
            match ranges.last_mut() {
                Some(r) if j0 <= r.1 + 1 => r.1 = r.1.max(j1),
                _ => ranges.push((j0, j1)),
            }
        }
        let swept: usize = ranges.iter().map(|r| r.1 - r.0 + 1).sum();
        if swept * 2 > self.blocks.len() {
            return Some((Self::build(inputs, cancel)?, None));
        }
        let mut blocks = Vec::with_capacity(self.blocks.len() + ranges.len());
        let mut next = 0;
        for (j0, j1) in ranges {
            blocks.extend_from_slice(&self.blocks[next..j0]);
            next = j1 + 1;
            let lo = (j0 > 0).then(|| self.blocks[j0].first - 1);
            let hi = self.blocks.get(j1 + 1).map(|b| b.first - 1);
            Self::sweep(inputs, Some(index), lo, hi, cancel, &mut blocks)?;
        }
        blocks.extend_from_slice(&self.blocks[next..]);
        Some((UniqueBlocks { blocks }, Some(swept)))
    }

    /// The highest count and the first tick reaching it (0, 0 if none).
    fn peak(&self) -> (u32, u64) {
        let mut best = (0, 0);
        for b in &self.blocks {
            if b.max > best.0 {
                best = (b.max, b.at);
            }
        }
        best
    }

    fn heap_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<UniqueBlock>()
    }
}

impl SelectionAnalysis {
    /// Analyzes the selected notes of `inputs` with the default one-second
    /// window. `None` when `cancel` is raised.
    pub fn build(
        inputs: &[SelectionInput],
        timing: &Timing,
        cancel: Option<&AtomicBool>,
    ) -> Option<SelectionAnalysis> {
        SelectionAnalysis::build_for(inputs, timing, DEFAULT_WINDOW_NS, cancel)
    }

    /// Analyzes the selected notes of `inputs`; the peaks use the NPS / PPS
    /// window `window_ns` (the Progressor's). `None` when `cancel` is raised.
    pub fn build_for(
        inputs: &[SelectionInput],
        timing: &Timing,
        window_ns: u64,
        cancel: Option<&AtomicBool>,
    ) -> Option<SelectionAnalysis> {
        let mut tracks = Vec::with_capacity(inputs.len());
        let mut stats = BuildStats::default();
        for t in inputs {
            if t.notes.selected_count() == 0 {
                continue;
            }
            tracks.push(Arc::new(TrackAnalysis::build_selected(
                t.id,
                &t.notes,
                t.correction,
                t.shift,
                None,
                &mut stats,
                cancel,
            )?));
        }
        let index = AnalysisIndex::from_tracks(tracks, cancel)?;
        let window_ns = window_ns.max(1);
        let profile = PeakProfile::build_with(
            &index,
            &Scope::Project,
            timing,
            window_ns,
            Peaks::WithPps,
            cancel,
        )?;
        let unique = UniqueBlocks::build(inputs, cancel)?;
        let mut a = SelectionAnalysis::assemble(inputs, timing, window_ns, index, profile, unique);
        a.update_stats = UpdateStats {
            full: true,
            pieces_built: stats.pieces_built,
            pieces_reused: 0,
            notes_built: stats.notes_built,
            profile_blocks: None,
            unique_blocks: None,
        };
        Some(a)
    }

    /// The analysis of `inputs` from this one (v0.9 V09-S19, V09-0056,
    /// `docs/SELECTION_PEAKS.md`) — after an edit, a selection change or
    /// Undo, at the **same tempo map**: with the same tracks, shifts and
    /// corrections, every piece whose chunk and selection bits (or selected
    /// content) are unchanged is reused, the index's timeline is patched,
    /// and the peak profile and Unique Pitch Poly blocks are swept again
    /// only where the change reaches (a new window rebuilds the profile
    /// alone). Otherwise a full build. Equal to [`SelectionAnalysis::build_for`]
    /// in every value. `None` when `cancel` is raised.
    pub fn update(
        &self,
        inputs: &[SelectionInput],
        timing: &Timing,
        window_ns: u64,
        cancel: Option<&AtomicBool>,
    ) -> Option<SelectionAnalysis> {
        let window_ns = window_ns.max(1);
        if input_key(inputs) != self.key {
            return SelectionAnalysis::build_for(inputs, timing, window_ns, cancel);
        }
        let mut tracks = Vec::with_capacity(inputs.len());
        let mut stats = BuildStats::default();
        for t in inputs.iter().filter(|t| t.notes.selected_count() > 0) {
            let prev = self.index.tracks.iter().find(|a| a.id == t.id);
            tracks.push(Arc::new(TrackAnalysis::build_selected(
                t.id,
                &t.notes,
                t.correction,
                t.shift,
                prev.map(|a| a.as_ref()),
                &mut stats,
                cancel,
            )?));
        }
        let spans = changed_spans(&self.index.tracks, &tracks);
        let index = AnalysisIndex::from_tracks_after(tracks, &self.index, cancel)?;
        let scope = Scope::Project;
        let delta = (window_ns == self.window_ns)
            .then(|| index.delta_since(&self.index, &scope))
            .flatten();
        let (profile, profile_blocks) = match delta {
            Some(d) => {
                let n = self.profile.update_blocks(&d, timing);
                (
                    self.profile.update(&index, &scope, timing, &d, cancel)?,
                    (n != usize::MAX).then_some(n),
                )
            }
            None => (
                PeakProfile::build_with(&index, &scope, timing, window_ns, Peaks::WithPps, cancel)?,
                None,
            ),
        };
        let (unique, unique_blocks) = self.unique.update(inputs, &index, &spans, cancel)?;
        let mut a = SelectionAnalysis::assemble(inputs, timing, window_ns, index, profile, unique);
        a.update_stats = UpdateStats {
            full: false,
            pieces_built: stats.pieces_built,
            pieces_reused: stats.pieces_reused,
            notes_built: stats.notes_built,
            profile_blocks,
            unique_blocks,
        };
        Some(a)
    }

    /// The readable values from the index, the profile and the unique blocks.
    fn assemble(
        inputs: &[SelectionInput],
        timing: &Timing,
        window_ns: u64,
        index: AnalysisIndex,
        profile: PeakProfile,
        unique: UniqueBlocks,
    ) -> SelectionAnalysis {
        let scope = Scope::Project;
        let selected = inputs.iter().map(|t| t.notes.selected_count() as u64).sum();
        let counted = index.total_notes();
        let (first_tick, last_tick) = match index.first_start() {
            Some(f) if counted > 0 => (f, index.last_tick(&scope)),
            _ => (0, 0),
        };
        let (_, start_sum) = index.starts_in(&scope, None, last_tick);
        let (_, end_sum) = index.ends_in(&scope, None, last_tick);
        let (peak_poly, peak_poly_tick) = profile.peak_poly_first(&index, &scope);
        let (peak_nps, peak_nps_tick) = profile.peak_nps_first(&index, &scope, timing);
        let (peak_pps, peak_pps_tick) = profile
            .peak_pps()
            .map_or((0.0, 0), |(r, t)| (r.to_f64(), t));
        let (peak_unique, peak_unique_tick) = unique.peak();
        let gate = gate_range(&index, timing, first_tick, last_tick);
        SelectionAnalysis {
            selected,
            counted,
            first_tick,
            last_tick,
            span_ns: if counted > 0 {
                timing.nanos_at(last_tick) - timing.nanos_at(first_tick)
            } else {
                0
            },
            peak_poly,
            peak_poly_tick,
            peak_unique,
            peak_unique_tick,
            area_ticks: end_sum - start_sum,
            area_ns: profile.area_ns().unwrap_or(0),
            peak_nps,
            peak_nps_tick,
            peak_pps,
            peak_pps_tick,
            window_ns,
            gate,
            index: Arc::new(index),
            profile: Arc::new(profile),
            unique,
            key: input_key(inputs),
            update_stats: UpdateStats::default(),
        }
    }

    /// The selection-only analysis index (v1.0-Launch: the selection
    /// charts; `Scope::Project` of it is exactly the selected notes).
    pub fn index(&self) -> &Arc<AnalysisIndex> {
        &self.index
    }

    /// The selection's exact peak profile (its NPS window is `window_ns`).
    pub fn profile(&self) -> &Arc<PeakProfile> {
        &self.profile
    }

    /// NPS, PPS and poly of the selected notes at `tick` (v0.6 definitions,
    /// `window_ns` = the Progressor's window).
    pub fn at(&self, timing: &Timing, tick: u64, window_ns: u64) -> Metrics {
        self.index.metrics(&Scope::Project, timing, tick, window_ns)
    }

    /// Selected notes per second over the selection's span (0 for no span).
    pub fn avg_nps(&self) -> f64 {
        if self.span_ns == 0 {
            0.0
        } else {
            self.counted as f64 / (self.span_ns as f64 / 1e9)
        }
    }

    /// Average poly over the span: Poly Area / span.
    pub fn avg_poly(&self) -> f64 {
        if self.span_ns == 0 {
            0.0
        } else {
            self.area_ns as f64 / self.span_ns as f64
        }
    }

    /// Poly Area in note·seconds.
    pub fn area_note_seconds(&self) -> f64 {
        self.area_ns as f64 / 1e9
    }

    /// B1-0105: the counted notes' gates shorter than `short_below` ticks,
    /// at least `long_from` ticks, and in between (rank queries on every
    /// selection piece's gates: O(pieces · log), no note walk).
    pub fn gate_counts(&self, short_below: u64, long_from: u64) -> GateCounts {
        let long_from = long_from.max(short_below);
        let (mut short, mut below_long, mut all) = (0u64, 0u64, 0u64);
        for g in self.gate_sets() {
            all += g.len() as u64;
            if let Some(k) = short_below.checked_sub(1) {
                short += g.count_le(k) as u64;
            }
            if let Some(k) = long_from.checked_sub(1) {
                below_long += g.count_le(k) as u64;
            }
        }
        GateCounts {
            short,
            medium: below_long - short,
            long: all - below_long,
        }
    }

    fn gate_sets(&self) -> impl Iterator<Item = &SortedTicks> {
        gate_sets(&self.index)
    }

    pub fn heap_bytes(&self) -> usize {
        self.index.heap_bytes() + self.profile.heap_bytes() + self.unique.heap_bytes()
    }
}
