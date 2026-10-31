//! Selection analytics in the Piano Roll (contract §132–134; View → Selection
//! Analytics, F6, default on): a compact readout of the active track's selected
//! notes — count, NPS / PPS / poly at the transport position (the v0.6
//! definitions and the Progressor's window), Peak Poly (instances), Unique
//! Pitch Poly, Poly Area (note·s and note·ticks), averages.
//!
//! v0.8 (§147–149, §202): the text is a template on the Progressor's template
//! system ([`SelField`]s, `ProgressorSettings::selection_template`, View →
//! Selection Analytics Settings…); the default reproduces the v0.7 readout. A
//! broken template shows its text as typed, never fails. Formatting is cached
//! apart from the analysis: a new template, number format or position only
//! re-renders the text, never re-analyses the selection.
//!
//! The engine's [`SelectionAnalysis`] is built once per selection (keyed by the
//! track's revision, which every selection change bumps, its corrections, the
//! tempo map and the NPS / PPS window): inline for small selections, on a
//! worker with a cancel flag for large ones (a newer selection cancels the
//! running build). Position queries are binary searches, so playing, pausing
//! and seeking cost nothing extra; no playback is needed.
//!
//! v0.9 (V09-S19, `docs/SELECTION_PEAKS.md`): the readout adds the
//! selection's Peak NPS and Peak PPS over its whole region (`{peak_nps}`,
//! `{peak_pps}` and the song times reaching them), computed with the
//! analysis — never by playing. After an edit, a selection change or Undo
//! the next analysis is an **update** of the last one
//! ([`SelectionAnalysis::update`]: only the chunks, profile blocks and pitch
//! blocks the change reaches are analysed again), as long as the note host,
//! its correction and the tempo map are the same.
//!
//! B1-0105 (§015-A): the **gate range** — the shortest ~ longest note length
//! of the selection ("dense ~ long"), in ticks, exact beats and, at one
//! constant tempo, time — with short / medium / long counts by PlumSpirit's
//! note-value classes (`gate_limits`: short < 1/16 note, long ≥ one beat).
//! A gate is a note's length, not how densely notes are packed.

use super::PlumSpiritApp;
use super::progressor::{self, Field, ProgressorSettings, Template};
use crate::ui::theme::Palette;
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput, UpdateStats};
use plumspirit_engine::analysis::{Timing, document_corrections};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::view::track_list::ClipRect;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// Selected notes analysed on the UI thread at most (≈ 10 ms in a release
/// build); more go to a worker.
pub const SYNC_NOTES: usize = 50_000;

/// The analysis inputs, made where the analysis runs (L-S46: marking a clip
/// selection's notes is O(notes); never on the UI thread when large).
type Prepare = Box<dyn FnOnce() -> Vec<SelectionInput> + Send>;

/// The gate line (B1-0105).
pub const GATE_LINE: &str =
    "Gate (length): {gate_range} · {gate_short} short / {gate_medium} medium / {gate_long} long";

/// The readout's default text (the v0.7 readout with the v0.9 peaks line
/// and the beta1 gate line).
pub const DEFAULT_TEMPLATE: &str = "Selection: {selected} notes{not_played_note} · {duration}\n\
NPS: {nps}   PPS: {pps}   Poly: {poly}\n\
Peak NPS: {peak_nps}   Peak PPS: {peak_pps}\n\
Peak Poly: {peak_poly}   Unique Pitch Poly: {unique_poly}\n\
Gate (length): {gate_range} · {gate_short} short / {gate_medium} medium / {gate_long} long\n\
Poly Area: {poly_area} note·s ({poly_area_ticks} note·ticks)\n\
Avg NPS: {avg_nps}   Avg Poly: {avg_poly}";

/// The v0.9 default text: settings that still hold it unchanged get
/// [`DEFAULT_TEMPLATE`] (B1-0105: the gate line).
pub const V09_DEFAULT_TEMPLATE: &str = "Selection: {selected} notes{not_played_note} · {duration}\n\
NPS: {nps}   PPS: {pps}   Poly: {poly}\n\
Peak NPS: {peak_nps}   Peak PPS: {peak_pps}\n\
Peak Poly: {peak_poly}   Unique Pitch Poly: {unique_poly}\n\
Poly Area: {poly_area} note·s ({poly_area_ticks} note·ticks)\n\
Avg NPS: {avg_nps}   Avg Poly: {avg_poly}";

/// The default text up to v0.8: settings that still hold it unchanged get
/// [`DEFAULT_TEMPLATE`] (an edited text is kept as typed).
pub const V08_DEFAULT_TEMPLATE: &str = "Selection: {selected} notes{not_played_note} · {duration}\n\
NPS: {nps}   PPS: {pps}   Poly: {poly}\n\
Peak Poly: {peak_poly}   Unique Pitch Poly: {unique_poly}\n\
Poly Area: {poly_area} note·s ({poly_area_ticks} note·ticks)\n\
Avg NPS: {avg_nps}   Avg Poly: {avg_poly}";

/// The standard lines the field checkboxes add / remove.
pub const FIELDS: [(&str, &str); 11] = [
    (
        "Count",
        "Selection: {selected} notes{not_played_note} · {duration}",
    ),
    ("NPS / PPS / Poly", "NPS: {nps}   PPS: {pps}   Poly: {poly}"),
    (
        "Peak NPS / PPS",
        "Peak NPS: {peak_nps}   Peak PPS: {peak_pps}",
    ),
    (
        "Peaks",
        "Peak Poly: {peak_poly}   Unique Pitch Poly: {unique_poly}",
    ),
    (
        "Poly Area",
        "Poly Area: {poly_area} note·s ({poly_area_ticks} note·ticks)",
    ),
    ("Averages", "Avg NPS: {avg_nps}   Avg Poly: {avg_poly}"),
    ("Gate range", GATE_LINE),
    (
        "Gate units",
        "Gate: {gate_beats} · {gate_time} · average {gate_avg} ticks",
    ),
    ("Start / End", "From {start} to {end}"),
    ("Ticks", "Ticks: {start_tick} – {end_tick}"),
    ("Position", "At {time} (tick {tick})"),
];

/// A value the Selection Analytics text can show (definitions:
/// `plumspirit_engine::analysis::canonical`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelField {
    Selected,
    Notes,
    NotPlayed,
    NotPlayedNote,
    Nps,
    Pps,
    Poly,
    PeakNps,
    PeakPps,
    PeakNpsAt,
    PeakPpsAt,
    PeakPoly,
    UniquePoly,
    PolyArea,
    PolyAreaTicks,
    Duration,
    Start,
    End,
    StartTick,
    EndTick,
    AvgNps,
    AvgPoly,
    Time,
    Tick,
    GateRange,
    GateMin,
    GateMax,
    GateBeats,
    GateTime,
    GateAvg,
    GateShort,
    GateMedium,
    GateLong,
}

impl SelField {
    const ALL: [SelField; 33] = [
        SelField::Selected,
        SelField::Notes,
        SelField::NotPlayed,
        SelField::NotPlayedNote,
        SelField::Nps,
        SelField::Pps,
        SelField::Poly,
        SelField::PeakNps,
        SelField::PeakPps,
        SelField::PeakNpsAt,
        SelField::PeakPpsAt,
        SelField::PeakPoly,
        SelField::UniquePoly,
        SelField::PolyArea,
        SelField::PolyAreaTicks,
        SelField::Duration,
        SelField::Start,
        SelField::End,
        SelField::StartTick,
        SelField::EndTick,
        SelField::AvgNps,
        SelField::AvgPoly,
        SelField::Time,
        SelField::Tick,
        SelField::GateRange,
        SelField::GateMin,
        SelField::GateMax,
        SelField::GateBeats,
        SelField::GateTime,
        SelField::GateAvg,
        SelField::GateShort,
        SelField::GateMedium,
        SelField::GateLong,
    ];
}

impl Field for SelField {
    fn all() -> &'static [SelField] {
        &SelField::ALL
    }
    fn key(self) -> &'static str {
        match self {
            SelField::Selected => "selected",
            SelField::Notes => "notes",
            SelField::NotPlayed => "not_played",
            SelField::NotPlayedNote => "not_played_note",
            SelField::Nps => "nps",
            SelField::Pps => "pps",
            SelField::Poly => "poly",
            SelField::PeakNps => "peak_nps",
            SelField::PeakPps => "peak_pps",
            SelField::PeakNpsAt => "peak_nps_at",
            SelField::PeakPpsAt => "peak_pps_at",
            SelField::PeakPoly => "peak_poly",
            SelField::UniquePoly => "unique_poly",
            SelField::PolyArea => "poly_area",
            SelField::PolyAreaTicks => "poly_area_ticks",
            SelField::Duration => "duration",
            SelField::Start => "start",
            SelField::End => "end",
            SelField::StartTick => "start_tick",
            SelField::EndTick => "end_tick",
            SelField::AvgNps => "avg_nps",
            SelField::AvgPoly => "avg_poly",
            SelField::Time => "time",
            SelField::Tick => "tick",
            SelField::GateRange => "gate_range",
            SelField::GateMin => "gate_min",
            SelField::GateMax => "gate_max",
            SelField::GateBeats => "gate_beats",
            SelField::GateTime => "gate_time",
            SelField::GateAvg => "gate_avg",
            SelField::GateShort => "gate_short",
            SelField::GateMedium => "gate_medium",
            SelField::GateLong => "gate_long",
        }
    }
    fn describe(self) -> &'static str {
        match self {
            SelField::Selected => "selected notes (as stored)",
            SelField::Notes => "selected notes that play (muted / filtered ones left out)",
            SelField::NotPlayed => "selected notes that do not play",
            SelField::NotPlayedNote => "“ (N not played)” when some do not play, else nothing",
            SelField::Nps => "selected note starts in the last second at the position",
            SelField::Pps => "average poly of the selection over the last second",
            SelField::Poly => "selected notes sounding at the position",
            SelField::PeakNps => "highest NPS of the selection anywhere (no playback needed)",
            SelField::PeakPps => "highest PPS of the selection anywhere (no playback needed)",
            SelField::PeakNpsAt => "song time where Peak NPS is first reached",
            SelField::PeakPpsAt => "song time where Peak PPS is first reached",
            SelField::PeakPoly => "most selected notes sounding at once (instances)",
            SelField::UniquePoly => "most distinct pitches sounding at once",
            SelField::PolyArea => "Poly Area (Σ lengths) in note·seconds",
            SelField::PolyAreaTicks => "Poly Area in note·ticks",
            SelField::Duration => "song time from the first start to the last end",
            SelField::Start => "song time of the first start",
            SelField::End => "song time of the last end",
            SelField::StartTick => "tick of the first start",
            SelField::EndTick => "tick of the last end",
            SelField::AvgNps => "notes per second over the selection's duration",
            SelField::AvgPoly => "Poly Area / duration",
            SelField::Time => "song time at the position",
            SelField::Tick => "position in ticks",
            SelField::GateRange => {
                "shortest ~ longest gate (note length) in ticks, e.g. 1 ~ 960 ticks; a length, not how dense the notes are"
            }
            SelField::GateMin => "shortest gate (note length) in ticks",
            SelField::GateMax => "longest gate (note length) in ticks",
            SelField::GateBeats => {
                "shortest ~ longest gate in beats (quarter notes), as exact fractions"
            }
            SelField::GateTime => {
                "shortest ~ longest gate in time when one tempo covers the selection, else “time varies with tempo”"
            }
            SelField::GateAvg => "average gate in ticks",
            SelField::GateShort => "notes shorter than a 1/16 note (PPQN / 4 ticks)",
            SelField::GateMedium => "notes from a 1/16 note up to (not including) one beat",
            SelField::GateLong => "notes of one beat (PPQN ticks) or longer",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    doc_generation: u64,
    /// The note host (`render::host_key`) and its shift.
    track: u64,
    shift: i128,
    revision: u64,
    props: u64,
    tempo: u64,
    structure: u64,
    /// Corrections start at a measure: the meter moves them.
    meter: u64,
    /// Format 2 sequences keep tempo in their tracks.
    events: u64,
    /// The NPS / PPS window of the peaks (the Progressor's).
    window: u64,
}

impl Key {
    /// Same note host, correction inputs and tempo map: the analysis for
    /// `self` can be updated into one for `other` (only the notes, the
    /// selection or the window differ).
    fn same_source(&self, other: &Key) -> bool {
        Key {
            revision: other.revision,
            window: other.window,
            ..*self
        } == *other
    }
}

/// (generation, structure, tempo, Format 2 track events) revisions.
type TimingKey = (u64, u64, u64, u64);

/// What the rendered text depends on (never the analysis' inputs: §202).
#[derive(Clone, Debug, PartialEq)]
struct TextKey {
    analysis: usize,
    tick: u64,
    template: String,
    window_ns: u64,
    number_format: progressor::NumberFormat,
    pps_decimals: u8,
    ppqn: Option<u64>,
}

struct Job {
    key: Key,
    rx: Receiver<Option<SelectionAnalysis>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct SelectionStats {
    /// The last result, the key it describes, and its timing.
    result: Option<(Key, Arc<SelectionAnalysis>, Arc<Timing>)>,
    timing: Option<(TimingKey, Arc<Timing>)>,
    job: Option<Job>,
    /// The parsed template and the last text (formatting only).
    template: RefCell<Option<(String, Template<SelField>)>>,
    text_cache: RefCell<Option<(TextKey, String)>>,
    /// Texts rendered (tests: formatting is cached).
    renders: std::cell::Cell<u64>,
    /// Analyses that were updates of the previous one (tests).
    updates: u64,
    /// The current selection is being analysed (the readout is stale).
    pub pending: bool,
    /// B1-0105: the document's PPQN (the gate classes and beats; `None`:
    /// SMPTE timing, which has no beat).
    ppqn: Option<u64>,
}

impl SelectionStats {
    /// Close Project, on the reaper rather than the UI thread.
    pub fn release(mut self) {
        if let Some(j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
            drop(j.rx.into_iter().last());
        }
    }

    pub fn reset(&mut self) {
        self.cancel_job();
        *self = SelectionStats::default();
    }

    /// v1.0-Launch L-E03 … L-E08: the current analysis and its timing (the
    /// selection charts read it; `pending`: a newer one is being built).
    pub fn current(&self) -> Option<(&Arc<SelectionAnalysis>, &Arc<Timing>)> {
        self.result.as_ref().map(|(_, a, t)| (a, t))
    }

    fn cancel_job(&mut self) {
        if let Some(j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Per frame: follows the selection of track `ti`. Returns true while a
    /// build runs (the caller polls).
    /// `target`: the notes the roll edits (an opened clip's content, whose
    /// tick 0 is song tick `shift`; V09-S11). `window_ns`: the Progressor's
    /// NPS / PPS window, which the peaks use.
    pub fn sync(
        &mut self,
        doc: &PlumDocument,
        generation: u64,
        target: plumspirit_engine::editor::NoteTarget,
        shift: i128,
        window_ns: u64,
    ) -> bool {
        let ti = target.host_track();
        let Ok(v) = plumspirit_engine::editor::note_view(doc, target) else {
            self.reset();
            return false;
        };
        let t = v.track;
        self.ppqn = doc.division().musical_ppqn();
        if v.notes.selected_count() == 0 {
            self.cancel_job();
            self.result = None;
            self.pending = false;
            return false;
        }
        let (timing, events) = self.timing_for(doc, generation);
        let key = Key {
            doc_generation: generation,
            track: crate::piano_roll::render::host_key(v.host()),
            shift,
            revision: v.notes.revision(),
            props: t.props_revision(),
            tempo: doc.tempo_revision(),
            structure: doc.structure_revision(),
            meter: doc.conductor().meter_revision(),
            events,
            window: window_ns.max(1),
        };
        if !self.needs_build(&key, &timing) {
            return self.job.is_some();
        }
        let correction = document_corrections(doc).get(ti).copied().flatten();
        let inputs = vec![SelectionInput {
            id: t.id(),
            notes: v.notes.clone(),
            correction,
            shift,
        }];
        let selected = v.notes.selected_count();
        self.request(key, selected, Box::new(move || inputs), timing)
    }

    /// B1-0107: per frame in the Track List — the selected clips' notes as
    /// they play, at their song positions (the union of the clips: two clips
    /// over the same ticks count their notes twice, as playback plays them
    /// twice), limited to the time selection `range` when there is one.
    /// Muted and empty clips play nothing and are left out. Returns true
    /// while a build runs.
    pub fn sync_clips(
        &mut self,
        doc: &PlumDocument,
        generation: u64,
        clips: &[ClipRect],
        range: Option<(u64, u64)>,
        window_ns: u64,
    ) -> bool {
        self.ppqn = doc.division().musical_ppqn();
        let windows = clip_windows(clips, range);
        if windows.is_empty() {
            self.cancel_job();
            self.result = None;
            self.pending = false;
            return false;
        }
        let (timing, events) = self.timing_for(doc, generation);
        let (mut shape, mut props) = (0xC11F_5E1E_C7ED_0001u64, 0u64);
        for (c, lo, hi) in &windows {
            for x in [
                clip_input_id(c.id).0.into(),
                c.track as u64,
                *lo,
                *hi,
                c.start,
                c.offset,
            ] {
                shape = shape.rotate_left(5) ^ x;
                shape = shape.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            props = props.wrapping_add(doc.track(c.track).map_or(0, |t| t.props_revision()));
        }
        let key = Key {
            doc_generation: generation,
            track: shape,
            shift: 0,
            revision: doc.analysis_revision(),
            props,
            tempo: doc.tempo_revision(),
            structure: doc.structure_revision(),
            meter: doc.conductor().meter_revision(),
            events,
            window: window_ns.max(1),
        };
        if !self.needs_build(&key, &timing) {
            return self.job.is_some();
        }
        let corrections = document_corrections(doc);
        // L-S46: here only the notes each clip plays are counted (binary
        // searches per chunk); marking them — one bit per note, 224M notes
        // held the UI 0.56 s after an Undo on Windows — happens where the
        // analysis runs (a worker when large).
        let mut selected = 0usize;
        let pending: Vec<(SelectionInput, u64, u64)> = windows
            .iter()
            .filter_map(|(c, lo, hi)| {
                let (store, _) = super::track_list::clip_store(doc, c)?;
                let n = if hi <= lo {
                    0
                } else {
                    store.count_starting_in(*lo, *hi - 1)
                };
                selected += n;
                (n > 0).then(|| {
                    (
                        SelectionInput {
                            id: clip_input_id(c.id),
                            notes: store.clone(),
                            correction: corrections.get(c.track).copied().flatten(),
                            shift: i128::from(c.start) - i128::from(c.offset),
                        },
                        *lo,
                        *hi,
                    )
                })
            })
            .collect();
        if pending.is_empty() {
            self.cancel_job();
            self.result = None;
            self.pending = false;
            return false;
        }
        let prepare = move || {
            pending
                .into_iter()
                .map(|(mut input, lo, hi)| {
                    input.notes.clear_selection();
                    input.notes.select_range(lo, hi, false);
                    input
                })
                .collect()
        };
        self.request(key, selected, Box::new(prepare), timing)
    }

    /// The tempo map for `doc` (kept while its inputs are the same) and the
    /// Format 2 events revision.
    fn timing_for(&mut self, doc: &PlumDocument, generation: u64) -> (Arc<Timing>, u64) {
        let events = doc
            .tracks()
            .iter()
            .fold(0u64, |a, t| a.wrapping_add(t.events_revision()));
        let tk = (
            generation,
            doc.structure_revision(),
            doc.tempo_revision(),
            events,
        );
        if self.timing.as_ref().is_none_or(|(k, _)| *k != tk) {
            self.timing = Some((tk, Arc::new(Timing::for_document(doc))));
        }
        (Arc::clone(&self.timing.as_ref().expect("set").1), events)
    }

    /// Collects a finished build; false when the result or the running job
    /// already answers `key`.
    fn needs_build(&mut self, key: &Key, timing: &Arc<Timing>) -> bool {
        // Collect a finished build.
        if let Some(j) = &self.job {
            match j.rx.try_recv() {
                Ok(r) => {
                    let j = self.job.take().expect("checked");
                    if let Some(a) = r {
                        self.result = Some((j.key, Arc::new(a), Arc::clone(timing)));
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.job = None,
            }
        }
        if self.result.as_ref().is_some_and(|r| r.0 == *key) {
            self.pending = false;
            return false;
        }
        if self.job.as_ref().is_some_and(|j| j.key == *key) {
            self.pending = true;
            return false;
        }
        true
    }

    /// Analyses `inputs` for `key`: an update of the last analysis of the
    /// same source, else a full build; inline when small, else on a worker
    /// (a newer request cancels the running one). True while a build runs.
    fn request(&mut self, key: Key, selected: usize, inputs: Prepare, timing: Arc<Timing>) -> bool {
        // A newer selection: cancel the running build, start this one.
        self.cancel_job();
        // The last analysis of the same host / correction / tempo map is
        // the base of an update (V09-S19).
        let base = self
            .result
            .as_ref()
            .filter(|(k, ..)| k.same_source(&key))
            .map(|(_, a, _)| Arc::clone(a));
        self.updates += u64::from(base.is_some());
        let window = key.window;
        let analyse = move |inputs: &[SelectionInput], tm: &Timing, cancel: Option<&AtomicBool>| {
            match &base {
                Some(b) => b.update(inputs, tm, window, cancel),
                None => SelectionAnalysis::build_for(inputs, tm, window, cancel),
            }
        };
        if selected <= SYNC_NOTES {
            let inputs = inputs();
            debug_assert_eq!(
                inputs
                    .iter()
                    .map(|i| i.notes.selected_count())
                    .sum::<usize>(),
                selected
            );
            if let Some(a) = analyse(&inputs, &timing, None) {
                self.result = Some((key, Arc::new(a), timing));
            }
            self.pending = false;
            return false;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let (tx, rx) = std::sync::mpsc::channel();
        let tm = Arc::clone(&timing);
        let spawned = std::thread::Builder::new()
            .name("plumspirit-selection".into())
            .spawn(move || {
                let inputs = inputs();
                let _ = tx.send(analyse(&inputs, &tm, Some(&c2)));
            });
        match spawned {
            Ok(_) => {
                self.job = Some(Job { key, rx, cancel });
                self.pending = true;
                true
            }
            Err(e) => {
                log::warn!("selection analysis thread could not start: {e}");
                false
            }
        }
    }

    /// The readout text at `tick` (None: nothing selected / analysed yet),
    /// from `ps.selection_template`. Cached: only a new analysis, position,
    /// template or number format renders it again.
    pub fn text(&self, tick: u64, ps: &ProgressorSettings) -> Option<String> {
        let (_, a, timing) = self.result.as_ref()?;
        let key = TextKey {
            analysis: Arc::as_ptr(a) as usize,
            tick,
            template: ps.selection_template.clone(),
            window_ns: ps.window_ns(),
            number_format: ps.number_format,
            pps_decimals: ps.pps_decimals,
            ppqn: self.ppqn,
        };
        if let Some((k, t)) = self.text_cache.borrow().as_ref()
            && *k == key
        {
            return Some(t.clone());
        }
        let mut tpl = self.template.borrow_mut();
        if tpl
            .as_ref()
            .is_none_or(|(src, _)| *src != ps.selection_template)
        {
            *tpl = Some((
                ps.selection_template.clone(),
                Template::parse(&ps.selection_template),
            ));
        }
        let t = &tpl.as_ref().expect("set").1;
        let text = render(t, a, timing, tick, ps, self.ppqn);
        self.renders.set(self.renders.get() + 1);
        *self.text_cache.borrow_mut() = Some((key, text.clone()));
        Some(text)
    }

    pub fn heap_bytes(&self) -> usize {
        self.result.as_ref().map_or(0, |r| r.1.heap_bytes())
    }

    /// What the current analysis re-did (`None`: nothing analysed).
    pub fn update_stats(&self) -> Option<UpdateStats> {
        self.result.as_ref().map(|r| r.1.update_stats)
    }
}

/// B1-0105: `ticks` in beats as an exact (mixed) fraction: "1/960",
/// "1 1/2", "4".
fn beats(ticks: u64, ppqn: u64) -> String {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let ppqn = ppqn.max(1);
    let (whole, rest) = (ticks / ppqn, ticks % ppqn);
    let g = gcd(rest, ppqn).max(1);
    match (whole, rest) {
        (w, 0) => w.to_string(),
        (0, r) => format!("{}/{}", r / g, ppqn / g),
        (w, r) => format!("{w} {}/{}", r / g, ppqn / g),
    }
}

/// B1-0105: a gate's time: milliseconds under a second, else seconds.
fn gate_time(nanos: u64) -> String {
    if nanos < 1_000_000_000 {
        format!("{:.2} ms", nanos as f64 / 1e6)
    } else {
        format_span(nanos as f64 / 1e9)
    }
}

fn format_span(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.2} s")
    } else {
        let m = (secs / 60.0).floor();
        format!("{}:{:05.2}", m as u64, secs - m * 60.0)
    }
}

/// B1-0107: the content windows `(clip, lo, hi)` the selected clips play
/// (limited to `range`, song ticks), skipping muted and empty clips.
fn clip_windows(clips: &[ClipRect], range: Option<(u64, u64)>) -> Vec<(&ClipRect, u64, u64)> {
    clips
        .iter()
        .filter(|c| !c.muted && !c.empty)
        .filter_map(|c| {
            let (s0, s1) = match range {
                Some((a, b)) => (c.start.max(a), c.end.min(b)),
                None => (c.start, c.end),
            };
            (s0 < s1).then(|| (c, c.offset + (s0 - c.start), c.offset + (s1 - c.start)))
        })
        .collect()
}

/// B1-0107: a distinct analysis id per clip (a track's primary clip keeps
/// its track id; other placements get ids above every track id), so an
/// update never reuses one clip's pieces for another.
fn clip_input_id(id: plumspirit_engine::model::ClipId) -> plumspirit_engine::model::TrackId {
    use plumspirit_engine::model::{ClipId, TrackId};
    match id {
        ClipId::Primary(t) => t,
        ClipId::Placed(n) => TrackId(0x8000_0000 | n),
    }
}

/// The text of `t` for analysis `a` at `tick` (bounded like the Progressor's).
fn render(
    t: &Template<SelField>,
    a: &SelectionAnalysis,
    timing: &Timing,
    tick: u64,
    ps: &ProgressorSettings,
    ppqn: Option<u64>,
) -> String {
    let f = ps.number_format;
    let gate = a.gate;
    let counts = if t.uses(|k| {
        matches!(
            k,
            SelField::GateShort | SelField::GateMedium | SelField::GateLong
        )
    }) {
        let (short, long) =
            plumspirit_engine::analysis::selection::gate_limits(ppqn.unwrap_or(480));
        a.gate_counts(short, long)
    } else {
        Default::default()
    };
    let ticks = |v: u64| format!("{} tick{}", f.count(v), if v == 1 { "" } else { "s" });
    let dec = ps.pps_decimals.min(4);
    let needs_position = t.uses(|k| matches!(k, SelField::Nps | SelField::Pps | SelField::Poly));
    // The peaks are for the analysis' window; a new window re-analyses
    // (the key holds it), so they match the position values.
    let m = needs_position.then(|| a.at(timing, tick, ps.window_ns()));
    let clock = |tk: u64| super::format_clock(timing.nanos_at(tk) as f64 / 1e9);
    let text = t.render(|k| match k {
        SelField::Selected => f.count(a.selected),
        SelField::Notes => f.count(a.counted),
        SelField::NotPlayed => f.count(a.selected - a.counted),
        SelField::NotPlayedNote => {
            if a.counted == a.selected {
                String::new()
            } else {
                format!(" ({} not played)", f.count(a.selected - a.counted))
            }
        }
        SelField::Nps => f.count(m.as_ref().map_or(0, |m| m.nps)),
        SelField::Pps => f.decimal(m.as_ref().map_or(0.0, |m| m.pps), dec),
        SelField::Poly => f.count(m.as_ref().map_or(0, |m| m.poly)),
        SelField::PeakNps => f.count(a.peak_nps),
        SelField::PeakPps => f.decimal(a.peak_pps, dec),
        SelField::PeakNpsAt => clock(a.peak_nps_tick),
        SelField::PeakPpsAt => clock(a.peak_pps_tick),
        SelField::PeakPoly => f.count(a.peak_poly),
        SelField::UniquePoly => a.peak_unique.to_string(),
        SelField::PolyArea => f.decimal(a.area_note_seconds(), 2),
        SelField::PolyAreaTicks => f.count(u64::try_from(a.area_ticks).unwrap_or(u64::MAX)),
        SelField::Duration => format_span(a.span_ns as f64 / 1e9),
        SelField::Start => clock(a.first_tick),
        SelField::End => clock(a.last_tick),
        SelField::StartTick => f.count(a.first_tick),
        SelField::EndTick => f.count(a.last_tick),
        SelField::AvgNps => f.decimal(a.avg_nps(), 1),
        SelField::AvgPoly => f.decimal(a.avg_poly(), dec.max(1)),
        SelField::Time => clock(tick),
        SelField::Tick => f.count(tick),
        SelField::GateRange => gate.map_or_else(
            || "—".into(),
            |g| {
                if g.min == g.max {
                    format!("{} (all the same)", ticks(g.min))
                } else {
                    format!("{} ~ {}", f.count(g.min), ticks(g.max))
                }
            },
        ),
        SelField::GateMin => gate.map_or_else(|| "—".into(), |g| f.count(g.min)),
        SelField::GateMax => gate.map_or_else(|| "—".into(), |g| f.count(g.max)),
        SelField::GateBeats => match (gate, ppqn) {
            (Some(g), Some(p)) => {
                let unit = if g.max == p { "beat" } else { "beats" };
                format!("{} ~ {} {unit}", beats(g.min, p), beats(g.max, p))
            }
            (Some(_), None) => "no beats (SMPTE timing)".into(),
            (None, _) => "—".into(),
        },
        SelField::GateTime => match gate {
            Some(g) => g.nanos.map_or_else(
                || "time varies with tempo".into(),
                |(lo, hi)| format!("{} ~ {}", gate_time(lo), gate_time(hi)),
            ),
            None => "—".into(),
        },
        SelField::GateAvg => gate.map_or_else(|| "—".into(), |g| f.decimal(g.average(), 1)),
        SelField::GateShort => f.count(counts.short),
        SelField::GateMedium => f.count(counts.medium),
        SelField::GateLong => f.count(counts.long),
    });
    text.lines()
        .take(progressor::MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the readout goes: the Progressor's side, the other vertical edge.
fn readout_settings(ps: &ProgressorSettings) -> ProgressorSettings {
    use progressor::Corner;
    let mut s = ps.clone();
    s.corner = match ps.corner {
        Corner::TopLeft => Corner::BottomLeft,
        Corner::TopRight => Corner::BottomRight,
        Corner::BottomLeft => Corner::TopLeft,
        Corner::BottomRight => Corner::TopRight,
    };
    s.offset = ps.selection_offset;
    s.collapsed = ps.selection_collapsed;
    s.font_size = (ps.font_size * 0.9).max(progressor::MIN_FONT);
    s.background = true;
    s.background_opacity = s.background_opacity.max(70);
    s
}

/// View → Selection Analytics Settings…: the readout's text on the shared
/// template editor (live in the piano roll; OK keeps, Cancel restores). The
/// style follows the Progressor's settings.
pub fn settings_window(
    ctx: &egui::Context,
    template: &mut String,
    enabled: &mut bool,
    compact: &mut bool,
) -> progressor::WindowOutcome {
    use crate::ui::theme::{self, space};
    use egui::RichText;
    let mut outcome = progressor::WindowOutcome::Open;
    let mut open = true;
    egui::Window::new("Selection Analytics Settings")
        .id(egui::Id::new("selection-analytics-settings"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(ctx.content_rect().left_top() + egui::Vec2::new(236.0, 96.0))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.checkbox(
                enabled,
                format!(
                    "Show Selection Analytics{}",
                    super::keybinds::hint(super::commands::Command::ToggleSelectionAnalytics)
                ),
            );
            // L-F01: compact by default over the notes; details on demand.
            ui.checkbox(compact, "Compact readout: only its first line")
                .on_hover_text("Over the notes the readout shows its first line. Hold Alt over it (Detailed Hover) or click its chip for every line; the caret opens the charts.");
            ui.add_space(space::MICRO);
            progressor::template_editor::<SelField>(ui, "sel", template, &FIELDS);
            ui.add_space(space::MICRO);
            ui.label(
                RichText::new(
                    "Font, colors, numbers and the NPS / PPS window follow the Progressor Settings.",
                )
                .size(theme::size::SMALL)
                .weak(),
            );
            ui.add_space(space::SECTION);
            ui.horizontal(|ui| {
                if ui.button("Reset to Default").clicked() {
                    *template = DEFAULT_TEMPLATE.into();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        outcome = progressor::WindowOutcome::Ok;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = progressor::WindowOutcome::Cancel;
                    }
                });
            });
        });
    if !open {
        outcome = progressor::WindowOutcome::Cancel;
    }
    outcome
}

/// Polling interval while a build runs.
const POLL: Duration = Duration::from_millis(50);

impl PlumSpiritApp {
    /// Per frame (before the panels): keeps the selection analysis current.
    pub(super) fn update_selection_stats(&mut self, ctx: &egui::Context) {
        if !self.settings.selection_analytics {
            if self.sel_stats.pending || self.sel_stats.heap_bytes() > 0 {
                self.sel_stats.reset();
            }
            return;
        }
        let Some(o) = &self.open else {
            return;
        };
        let doc = o.doc.clone();
        let generation = o.generation;
        let window = self.settings.progressor.window_ns();
        // B1-0107: in the Track List, the selected clips (and the time
        // selection, when there is one).
        let running = if self.main_view == super::track_list::MainView::TrackList {
            let clips = self.track_list.selection();
            self.sel_stats
                .sync_clips(&doc, generation, &clips, self.time_selection, window)
        } else {
            let (target, shift) = (self.edit_target(), self.edit_shift());
            self.sel_stats.sync(&doc, generation, target, shift, window)
        };
        if running {
            ctx.request_repaint_after(POLL);
        }
    }

    /// Paints the readout over the note area (B1-0065: fading under the
    /// pointer, with its collapse / move chip); returns where it is.
    pub(super) fn paint_selection_stats(
        &self,
        painter: &egui::Painter,
        area: egui::Rect,
        pal: &Palette,
        pointer: Option<egui::Pos2>,
    ) -> Option<progressor::OverlayView> {
        if !self.settings.selection_analytics {
            return None;
        }
        let ps = &self.settings.progressor;
        let text = self.sel_stats.text(self.transport.position(), ps)?;
        // L-F01 Detailed Hover: Alt held over the (compact) readout shows
        // every line, unfaded, while held — the text of the analysis already
        // made (hovering never analyzes).
        let detail = self.detailed_hover(painter.ctx(), pointer);
        progressor::paint_overlay(
            painter,
            area,
            &text,
            &readout_settings(ps),
            pal,
            self.sel_stats.pending,
            progressor::OverlayState {
                pointer: pointer.filter(|_| !detail),
                collapsed: ps.selection_collapsed && !detail,
                chip: true,
                advanced: Some(self.settings.selection_advanced),
            },
        )
    }

    /// L-F01: Alt is held while the pointer is over the readout (where it
    /// was last drawn).
    pub(super) fn detailed_hover(&self, ctx: &egui::Context, pointer: Option<egui::Pos2>) -> bool {
        ctx.input(|i| i.modifiers.alt)
            && pointer.is_some_and(|p| self.sel_readout_last.is_some_and(|r| r.contains(p)))
    }

    /// B1-0065: the Selection Analytics readout's corner (the Progressor's
    /// opposite one), for dragging its chip.
    pub(super) fn selection_corner(&self) -> progressor::Corner {
        readout_settings(&self.settings.progressor).corner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};

    /// The Progressor's default window.
    const W: u64 = 1_000_000_000;

    fn doc(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        let v: Vec<NoteRecord> = (0..n)
            .map(|i| NoteRecord {
                start_tick: i * 10,
                duration_ticks: 30,
                attrs: NoteAttrs::new((i % 100) as u8, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(0),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        d
    }

    /// B1-0107: the Track List's selected clips — the union of the notes
    /// they play at their song positions (two clips over the same ticks
    /// count twice), muted clips left out, a time selection limiting them —
    /// against a brute force over the clips' windows.
    #[test]
    fn selected_clips_count_the_union_of_their_notes_at_song_positions() {
        use plumspirit_engine::editor::clips::{duplicate_clips, set_muted};
        use plumspirit_engine::history::{History, HistoryLimits};
        use plumspirit_engine::model::ClipId;
        use plumspirit_engine::view::track_list::TrackListModel;
        let mut d = doc(100); // track 0: 100 notes, 30 ticks every 10
        d.add_track().unwrap();
        d.add_track().unwrap();
        let mut h = History::new(HistoryLimits::default());
        let t0 = ClipId::Primary(d.track(0).unwrap().id());
        // A linked instance of track 0 on track 1 at the same ticks, one on
        // track 2 later, both of the full window.
        let op = duplicate_clips(&d, &[(t0, 1, 0), (t0, 2, 5_000)], true).unwrap();
        h.apply(&mut d, op).unwrap();
        let all = |d: &PlumDocument| -> Vec<ClipRect> {
            let m = TrackListModel::build(d);
            (0..d.track_count())
                .flat_map(|i| m.row(i).to_vec())
                .filter(|c| !c.empty)
                .collect()
        };
        let clips = all(&d);
        assert_eq!(clips.len(), 3, "{clips:?}");
        // Brute force: (start, end) of every note each unmuted clip plays,
        // limited to song ticks [a, b).
        let brute = |d: &PlumDocument, clips: &[ClipRect], a: u64, b: u64| -> Vec<(u64, u64)> {
            let mut v = Vec::new();
            for c in clips.iter().filter(|c| !c.muted) {
                let notes = &d.track(0).unwrap().notes;
                for n in notes.iter() {
                    if n.start_tick < c.offset || n.start_tick >= c.offset + (c.end - c.start) {
                        continue;
                    }
                    let s = c.start + (n.start_tick - c.offset);
                    if s >= a && s < b {
                        v.push((s, s + n.duration_ticks));
                    }
                }
            }
            v
        };
        let check = |what: &str, s: &SelectionStats, want: &[(u64, u64)]| {
            let a = &s.result.as_ref().expect(what).1;
            assert_eq!(a.counted, want.len() as u64, "{what}: notes");
            assert_eq!(a.selected, want.len() as u64, "{what}: selected");
            let first = want.iter().map(|x| x.0).min().unwrap();
            let last = want.iter().map(|x| x.1).max().unwrap();
            assert_eq!((a.first_tick, a.last_tick), (first, last), "{what}: span");
            let mut poly = 0u64;
            for &(t, _) in want {
                poly = poly.max(want.iter().filter(|x| x.0 <= t && x.1 > t).count() as u64);
            }
            assert_eq!(a.peak_poly, poly, "{what}: Peak Poly");
            let g = a.gate.expect("gates");
            assert_eq!(
                (g.min, g.max, g.count),
                (30, 30, want.len() as u64),
                "{what}"
            );
        };
        let mut s = SelectionStats::default();
        assert!(!s.sync_clips(&d, 1, &clips, None, W));
        check("three clips", &s, &brute(&d, &clips, 0, u64::MAX));
        assert_eq!(s.result.as_ref().unwrap().1.counted, 300);
        // Two clips over the same ticks: their notes count twice, and the
        // poly doubles.
        let same: Vec<ClipRect> = clips.iter().filter(|c| c.start == 0).cloned().collect();
        assert_eq!(same.len(), 2);
        s.sync_clips(&d, 1, &same, None, W);
        check(
            "two clips at the same ticks",
            &s,
            &brute(&d, &same, 0, u64::MAX),
        );
        assert_eq!(s.result.as_ref().unwrap().1.peak_poly, 6);
        // A time selection limits them (by note start, song ticks).
        s.sync_clips(&d, 1, &clips, Some((500, 5_200)), W);
        check("range", &s, &brute(&d, &clips, 500, 5_200));
        // A muted clip plays nothing.
        let later = clips.iter().find(|c| c.start == 5_000).unwrap().id;
        let op = set_muted(&d, &[later], true).unwrap();
        h.apply(&mut d, op).unwrap();
        let clips = all(&d);
        s.sync_clips(&d, 1, &clips, None, W);
        check("one muted", &s, &brute(&d, &clips, 0, u64::MAX));
        assert_eq!(s.result.as_ref().unwrap().1.counted, 200);
        // Nothing playable selected: no readout.
        let muted: Vec<ClipRect> = clips.iter().filter(|c| c.muted).cloned().collect();
        s.sync_clips(&d, 1, &muted, None, W);
        assert!(s.result.is_none());
        assert_eq!(clip_input_id(ClipId::Placed(7)).0, 0x8000_0007);
    }

    #[test]
    fn small_selections_are_analysed_at_once_without_playback() {
        let mut d = doc(1000);
        let mut s = SelectionStats::default();
        assert!(!s.sync(&d, 1, 0.into(), 0, W));
        assert!(
            s.text(0, &ProgressorSettings::default()).is_none(),
            "nothing selected"
        );
        d.track_mut(0).unwrap().notes.select_range(0, 2000, false);
        assert!(!s.sync(&d, 1, 0.into(), 0, W));
        let t = s.text(1000, &ProgressorSettings::default()).unwrap();
        assert!(t.starts_with("Selection: 200 notes"), "{t}");
        assert!(t.contains("Peak Poly: 3"), "{t}");
        assert!(t.contains("note·ticks"), "{t}");
        // Same selection: nothing rebuilt; a different one: rebuilt.
        let before = Arc::as_ptr(&s.result.as_ref().unwrap().1);
        s.sync(&d, 1, 0.into(), 0, W);
        assert_eq!(before, Arc::as_ptr(&s.result.as_ref().unwrap().1));
        d.track_mut(0).unwrap().notes.select_all();
        s.sync(&d, 1, 0.into(), 0, W);
        let t = s.text(0, &ProgressorSettings::default()).unwrap();
        assert!(t.starts_with("Selection: 1,000 notes"), "{t}");
    }

    #[test]
    fn large_selections_are_analysed_in_the_background() {
        let mut d = doc(SYNC_NOTES as u64 + 5_000);
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        assert!(s.sync(&d, 1, 0.into(), 0, W), "a worker runs");
        assert!(s.pending);
        let t0 = std::time::Instant::now();
        while s.sync(&d, 1, 0.into(), 0, W) || s.pending {
            assert!(t0.elapsed() < Duration::from_secs(60));
            std::thread::sleep(Duration::from_millis(5));
        }
        let t = s.text(0, &ProgressorSettings::default()).unwrap();
        let want = format!(
            "Selection: {} notes",
            plumspirit_engine::midi::group_digits(SYNC_NOTES as u64 + 5_000)
        );
        assert!(t.starts_with(&want), "{t}");
    }

    #[test]
    fn the_default_template_is_the_v07_readout_with_the_peaks_and_gate_lines() {
        let mut d = doc(1000);
        d.track_mut(0).unwrap().notes.select_range(0, 2000, false);
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let t = s.text(1000, &ProgressorSettings::default()).unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 7, "{t}");
        assert!(lines[0].starts_with("Selection: 200 notes · "), "{t}");
        assert!(
            lines[1].starts_with("NPS: ") && lines[1].contains("   PPS: "),
            "{t}"
        );
        // 200 notes of 30 ticks every 10 ticks; one second at 120 BPM and
        // 960 PPQN is 1,920 ticks: at most 192 starts in one window.
        assert!(lines[2].starts_with("Peak NPS: 192   Peak PPS: "), "{t}");
        assert_eq!(lines[3], "Peak Poly: 3   Unique Pitch Poly: 3");
        // 30-tick notes at 960 PPQN: all shorter than a 1/16 note (240).
        assert_eq!(
            lines[4],
            "Gate (length): 30 ticks (all the same) · 200 short / 0 medium / 0 long"
        );
        assert!(lines[5].ends_with("note·s (6,000 note·ticks)"), "{t}");
        assert!(lines[6].starts_with("Avg NPS: "), "{t}");
    }

    /// B1-0105: the gate fields — "dense ~ long" range, exact beats, time at
    /// one tempo (else said to vary), average and the note-value classes;
    /// an untouched v0.9 default template gains the gate line.
    #[test]
    fn gate_fields_show_the_range_beats_time_and_classes() {
        let mut d = PlumDocument::new_project("g".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = [(0u64, 1u64), (10, 7), (20, 240), (40, 960), (100, 1_440)]
            .iter()
            .map(|&(s, len)| {
                NoteRecord::new(
                    s,
                    len,
                    NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let ps = ProgressorSettings {
            selection_template: "{gate_range}|{gate_min}|{gate_max}|{gate_beats}|{gate_time}|{gate_avg}|{gate_short}/{gate_medium}/{gate_long}".into(),
            ..ProgressorSettings::default()
        };
        let t = s.text(0, &ps).unwrap();
        // 120 BPM at 960 PPQN: 1 tick = 0.52 ms, 1,440 ticks = 0.75 s.
        assert_eq!(
            t,
            "1 ~ 1,440 ticks|1|1,440|1/960 ~ 1 1/2 beats|0.52 ms ~ 750.00 ms|529.6|2/1/2"
        );
        assert_eq!(beats(3_840, 960), "4");
        assert_eq!(beats(480, 960), "1/2");
        assert_eq!(beats(0, 960), "0");
        assert_eq!(beats(7, 0), "7");
        // A tempo change inside the selection: no single time.
        let mut d2 = d.clone();
        plumspirit_engine::fixtures::set_dense_tempo(&mut d2, 4, 50, 1);
        let mut s2 = SelectionStats::default();
        s2.sync(&d2, 1, 0.into(), 0, W);
        let t = s2.text(0, &ps).unwrap();
        assert!(t.contains("|time varies with tempo|"), "{t}");
        // Every field is documented, and the default template migration.
        for k in SelField::ALL {
            assert!(!k.describe().is_empty());
        }
        assert!(SelField::GateRange.describe().contains("not how dense"));
        assert!(DEFAULT_TEMPLATE.contains(GATE_LINE));
        assert!(!V09_DEFAULT_TEMPLATE.contains("gate"));
    }

    /// The values a fresh analysis of track 0's selection gives.
    fn fresh(d: &PlumDocument, window: u64) -> Vec<u128> {
        let inputs = vec![SelectionInput {
            id: d.track(0).unwrap().id(),
            notes: d.track(0).unwrap().notes.clone(),
            correction: document_corrections(d)[0],
            shift: 0,
        }];
        let a =
            SelectionAnalysis::build_for(&inputs, &Timing::for_document(d), window, None).unwrap();
        values(&a)
    }

    fn values(a: &SelectionAnalysis) -> Vec<u128> {
        vec![
            a.selected.into(),
            a.counted.into(),
            a.first_tick.into(),
            a.last_tick.into(),
            a.peak_poly.into(),
            a.peak_poly_tick.into(),
            a.peak_unique.into(),
            a.peak_unique_tick.into(),
            a.area_ticks,
            a.area_ns,
            a.peak_nps.into(),
            a.peak_nps_tick.into(),
            a.peak_pps.to_bits().into(),
            a.peak_pps_tick.into(),
        ]
    }

    fn shown(s: &SelectionStats) -> Vec<u128> {
        values(&s.result.as_ref().unwrap().1)
    }

    #[test]
    fn peaks_are_shown_without_playback_and_follow_edits_undo_and_redo() {
        use plumspirit_engine::history::{History, Operation};
        let mut d = doc(3000);
        d.track_mut(0)
            .unwrap()
            .notes
            .select_range(5_000, 15_000, false);
        let mut h = History::default();
        let mut s = SelectionStats::default();
        // No transport anywhere: the peaks come with the analysis.
        s.sync(&d, 1, 0.into(), 0, W);
        let ps = ProgressorSettings {
            selection_template: "{peak_nps} | {peak_pps} | {peak_nps_at} | {peak_pps_at}".into(),
            ..ProgressorSettings::default()
        };
        let a = s.result.as_ref().unwrap().1.clone();
        assert!(a.peak_nps > 0 && a.peak_pps > 0.0);
        let t = s.text(0, &ps).unwrap();
        assert!(t.starts_with(&format!("{} | ", a.peak_nps)), "{t}");
        let first = shown(&s);
        assert_eq!(first, fresh(&d, W));
        assert!(s.update_stats().unwrap().full);
        // Resize the selected notes: an update of the last analysis.
        h.apply(
            &mut d,
            Operation::Resize {
                track: 0.into(),
                delta: 45,
            },
        )
        .unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        let resized = shown(&s);
        assert_ne!(resized, first);
        assert_eq!(resized, fresh(&d, W));
        assert!(!s.update_stats().unwrap().full);
        // Undo / Redo: exactly the values before / after, by update.
        h.undo(&mut d).unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        assert_eq!(shown(&s), first);
        assert!(!s.update_stats().unwrap().full);
        h.redo(&mut d).unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        assert_eq!(shown(&s), resized);
        // Drawing a note selects it alone.
        let rec = NoteRecord::new(
            50_000,
            100,
            NoteAttrs::new(70, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
        );
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: rec,
            },
        )
        .unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        let one = &s.result.as_ref().unwrap().1;
        assert_eq!((one.counted, one.peak_nps, one.peak_poly), (1, 1, 1));
        assert_eq!(shown(&s), fresh(&d, W));
        // Undoing the draw leaves nothing selected: no readout.
        h.undo(&mut d).unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        assert!(s.text(0, &ps).is_none());
        assert!(s.updates >= 4, "{} updates", s.updates);
    }

    #[test]
    fn a_large_selection_updates_in_the_background_after_a_ctrl_click() {
        let mut d = doc(SYNC_NOTES as u64 + 30_000);
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        let wait = |s: &mut SelectionStats, d: &PlumDocument| {
            let t0 = std::time::Instant::now();
            while s.sync(d, 1, 0.into(), 0, W) || s.pending {
                assert!(t0.elapsed() < Duration::from_secs(60));
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        wait(&mut s, &d);
        assert!(s.update_stats().unwrap().full);
        let loc = plumspirit_engine::model::note_store::NoteLoc { chunk: 1, index: 7 };
        d.track_mut(0).unwrap().notes.set_selected(loc, false);
        assert!(s.sync(&d, 1, 0.into(), 0, W), "a worker updates");
        wait(&mut s, &d);
        let st = s.update_stats().unwrap();
        assert!(!st.full && st.pieces_built == 1, "{st:?}");
        assert_eq!(shown(&s), fresh(&d, W));
    }

    #[test]
    fn a_new_window_re_analyses_only_the_peaks() {
        let mut d = doc(2000);
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        s.sync(&d, 1, 0.into(), 0, 250_000_000);
        let st = s.update_stats().unwrap();
        assert!(
            !st.full && st.pieces_built == 0 && st.profile_blocks.is_none(),
            "{st:?}"
        );
        assert_eq!(shown(&s), fresh(&d, 250_000_000));
        assert_ne!(shown(&s), fresh(&d, W));
    }

    /// Measurement (release): `cargo test --release -p plumspirit --bin
    /// PlumSpirit selection_frame_cost -- --ignored --nocapture`. The app path
    /// (sync per frame, worker, channel) at 1 M / 10 M selected notes:
    /// full analysis, a Ctrl+click, an unselected note drawn, a selected note
    /// pasted, a new window — each until the readout is current — the
    /// per-frame cost when nothing changed, and the readout's text.
    #[test]
    #[ignore]
    fn selection_frame_cost() {
        use plumspirit_engine::fixtures::{FixtureShape, generate, set_dense_tempo};
        use std::time::Instant;
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        for n in [1_000_000usize, 10_000_000] {
            let mut d = generate(FixtureShape::Dense, n, 960, 42);
            set_dense_tempo(&mut d, 2_000, 960, 7);
            d.track_mut(1).unwrap().notes.select_all();
            let end = d
                .track(1)
                .unwrap()
                .notes
                .iter()
                .map(|v| v.start_tick)
                .max()
                .unwrap();
            let mut s = SelectionStats::default();
            let until_current = |s: &mut SelectionStats, d: &PlumDocument, w: u64| -> Duration {
                let t = Instant::now();
                while s.sync(d, 1, 1.into(), 0, w) || s.pending {
                    std::thread::sleep(Duration::from_micros(200));
                }
                t.elapsed()
            };
            println!("## {n} selected notes (Dense fixture, dense tempo map)");
            let t = until_current(&mut s, &d, W);
            println!(
                "selection readout current after selecting all: {:.2} ms",
                ms(t)
            );
            println!(
                "selection analysis heap: {:.2} MB",
                s.heap_bytes() as f64 / 1_048_576.0
            );
            // Per frame with nothing changed: the key check only.
            let t = Instant::now();
            for _ in 0..1000 {
                s.sync(&d, 1, 1.into(), 0, W);
            }
            println!(
                "selection sync per frame, unchanged: {:.4} ms",
                ms(t.elapsed()) / 1000.0
            );
            let ps = ProgressorSettings::default();
            let t = Instant::now();
            for k in 0..200u64 {
                std::hint::black_box(s.text(k * 997, &ps));
            }
            println!(
                "selection readout text, new position each frame: {:.4} ms",
                ms(t.elapsed()) / 200.0
            );
            let t = Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(s.text(5, &ps));
            }
            println!(
                "selection readout text, cached: {:.4} ms",
                ms(t.elapsed()) / 1000.0
            );
            let loc = plumspirit_engine::model::note_store::NoteLoc {
                chunk: (d.track(1).unwrap().notes.chunk_count() / 2) as u32,
                index: 5,
            };
            d.track_mut(1).unwrap().notes.set_selected(loc, false);
            let t = until_current(&mut s, &d, W);
            println!(
                "selection readout current after a Ctrl+click: {:.2} ms",
                ms(t)
            );
            let rec = |start: u64, pitch: u8| {
                NoteRecord::new(
                    start,
                    240,
                    NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
                )
            };
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(&[rec(end / 2, 61)], false)
                .unwrap();
            let t = until_current(&mut s, &d, W);
            println!(
                "selection readout current after an unselected note drawn: {:.2} ms",
                ms(t)
            );
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(&[rec(end / 3, 72)], true)
                .unwrap();
            let t = until_current(&mut s, &d, W);
            println!(
                "selection readout current after a selected note pasted: {:.2} ms",
                ms(t)
            );
            let t = until_current(&mut s, &d, 500_000_000);
            println!(
                "selection readout current after a new window: {:.2} ms",
                ms(t)
            );
            let st = s.update_stats().unwrap();
            assert!(!st.full && st.pieces_built == 0, "{st:?}");
        }
    }

    #[test]
    fn an_untouched_v08_default_text_gains_the_peaks_line_and_an_edited_one_is_kept() {
        let load = |t: &str| {
            let mut v = ProgressorSettings::default().to_json();
            v["selection_template"] = serde_json::Value::String(t.into());
            let mut problems = Vec::new();
            let s = ProgressorSettings::from_json(&v, &mut problems);
            assert!(problems.is_empty(), "{problems:?}");
            s.selection_template
        };
        assert_eq!(load(V08_DEFAULT_TEMPLATE), DEFAULT_TEMPLATE);
        assert_eq!(load(V09_DEFAULT_TEMPLATE), DEFAULT_TEMPLATE);
        let edited = format!("{V08_DEFAULT_TEMPLATE}\nMine");
        assert_eq!(load(&edited), edited);
        assert_eq!(load(DEFAULT_TEMPLATE), DEFAULT_TEMPLATE);
    }

    #[test]
    fn editing_the_text_re_renders_without_re_analysing() {
        let mut d = doc(1000);
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let analysis = Arc::as_ptr(&s.result.as_ref().unwrap().1);
        let mut ps = ProgressorSettings::default();
        let a = s.text(500, &ps).unwrap();
        let renders = s.renders.get();
        assert_eq!(s.text(500, &ps).unwrap(), a);
        assert_eq!(s.renders.get(), renders, "same text: cached");
        ps.selection_template = "{notes} | {peak_poly} | {unique_poly} | {poly_area_ticks}".into();
        assert_eq!(s.text(500, &ps).unwrap(), "1,000 | 3 | 3 | 30,000");
        ps.number_format = progressor::NumberFormat::Plain;
        assert_eq!(s.text(500, &ps).unwrap(), "1000 | 3 | 3 | 30000");
        assert_eq!(s.renders.get(), renders + 2);
        s.sync(&d, 1, 0.into(), 0, W);
        assert_eq!(
            Arc::as_ptr(&s.result.as_ref().unwrap().1),
            analysis,
            "formatting never re-analyzes (§202)"
        );
        assert!(!s.pending);
    }

    #[test]
    fn a_broken_template_shows_its_text_and_never_fails() {
        let mut d = doc(100);
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let ps = ProgressorSettings {
            selection_template: "{nope} {{x}} {notes} {start_tick".into(),
            ..ProgressorSettings::default()
        };
        assert_eq!(s.text(0, &ps).unwrap(), "{nope} {x} 100 {start_tick");
        let t = Template::<SelField>::parse(&ps.selection_template);
        assert_eq!(t.errors.len(), 2, "{:?}", t.errors);
        let ps = ProgressorSettings {
            selection_template: String::new(),
            ..ProgressorSettings::default()
        };
        assert_eq!(s.text(0, &ps).unwrap(), "");
        let many = ProgressorSettings {
            selection_template: "{notes}\n".repeat(100),
            ..ProgressorSettings::default()
        };
        assert_eq!(
            s.text(0, &many).unwrap().lines().count(),
            progressor::MAX_LINES
        );
    }

    #[test]
    fn notes_that_do_not_play_are_reported() {
        let mut d =
            PlumDocument::new_project("t".into(), plumspirit_engine::model::Bpm::DEFAULT, 960);
        let v: Vec<NoteRecord> = (0..10u64)
            .map(|i| NoteRecord {
                start_tick: i * 100,
                duration_ticks: 50,
                attrs: NoteAttrs::new(60, 90, 0, 64, NoteOffKind::NoteOff)
                    .unwrap()
                    .with_muted(i < 3),
                sub: SubchannelId(0),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, true)
            .unwrap();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let ps = ProgressorSettings {
            selection_template: "{selected}{not_played_note}; {notes} play, {not_played} not"
                .into(),
            ..ProgressorSettings::default()
        };
        assert_eq!(s.text(0, &ps).unwrap(), "10 (3 not played); 7 play, 3 not");
    }

    #[test]
    fn a_meter_edit_re_analyses_a_selection_with_a_measure_based_correction() {
        use plumspirit_engine::history::{History, Operation};
        let mut d = doc(2000);
        let mut h = History::default();
        let mut p = d.track(0).unwrap().props().clone();
        p.key_correction = 100; // from bar 2 on, pitches ≥ 28 overflow (not played)
        p.correction_from_measure = 2;
        let id = d.track(0).unwrap().id();
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
        d.track_mut(0).unwrap().notes.select_all();
        let mut s = SelectionStats::default();
        s.sync(&d, 1, 0.into(), 0, W);
        let before = s.result.as_ref().unwrap().1.counted;
        // 7/4 instead of 4/4: bar 2 starts later, more notes play unfiltered.
        let edit = d.conductor().prepare_set_signature(None, 0, 7, 2).unwrap();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Meter",
            },
        )
        .unwrap();
        s.sync(&d, 1, 0.into(), 0, W);
        let after = s.result.as_ref().unwrap().1.counted;
        assert!(after > before, "{before} → {after}");
        let corr = document_corrections(&d)[0];
        let want = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .filter(|v| {
                corr.is_none_or(|c| {
                    matches!(
                        c.note(v.record()),
                        plumspirit_engine::model::correction::Corrected::Note { .. }
                    )
                })
            })
            .count() as u64;
        assert_eq!(after, want);
    }
}
