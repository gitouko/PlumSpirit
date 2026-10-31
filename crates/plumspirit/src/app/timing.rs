//! Select Range, the loop, playback speed and Playing Time.
//!
//! * **Select Range** selects by musical position (`Measure:Tick`, `Measure`
//!   or `Measure:Beat:Tick`, measures 1-based, offsets 0-based, through the
//!   Conductor's time signatures) over the half-open interval `[From, To)`:
//!   notes by onset (or, optionally, every note sounding in it) in the chosen
//!   tracks, or the events of an Event Graph.
//! * **Loop**: `[start, end)` in ticks; transport state saved with the project
//!   (`.lup`, outside undo). It never moves notes.
//! * **Speed**: an audition multiplier (user setting), never written to the
//!   project or the MIDI file.
//! * **Playing Time**: durations from the Conductor's tempo map (steps and
//!   ramps), as musical time or at the current playback speed.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use super::tasks::TaskOutcome;
use crate::ui::status_bar::StatusKind;
use crate::ui::theme::{self, space};
use egui::RichText;
use plumspirit_engine::editor::controllers::{self, collect_points, graph_controller};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::duration::{
    TimeBasis, TimeMap, format_duration, parse_minutes_seconds,
};
use plumspirit_engine::model::tempo::Speed;
use plumspirit_engine::model::{GraphTarget, LoopConfig, PlumDocument, TrackId};
use plumspirit_engine::view::grid::MeterMap;

/// What Select Range selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeTarget {
    /// Notes in the chosen tracks (Track List).
    Notes,
    /// The events of Event Graph 1 / 2 (their own type and channel).
    Graph(usize),
    /// The active graph's events, shown in the Event List.
    EventList,
}

/// Which tracks Select Range covers (for notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackScope {
    Current,
    /// The tracks Ctrl+clicked in the track list (and the active one).
    Selected,
    /// From one track until another (by position in the list).
    Between,
    All,
}

#[derive(Clone, Debug)]
pub struct RangeDraft {
    pub from: String,
    pub to: String,
    pub target: RangeTarget,
    pub scope: TrackScope,
    pub from_track: TrackId,
    pub until_track: TrackId,
    pub intersecting: bool,
    /// Add to the existing selection instead of replacing it.
    pub add: bool,
    pub set_loop: bool,
    /// (id, label) of every track, in order.
    pub tracks: Vec<(TrackId, String)>,
    pub selected_tracks: usize,
    pub graph_labels: [String; 2],
    pub error: Option<String>,
    pub focused: bool,
}

/// Which duration Playing Time shows.
#[derive(Clone, Debug)]
pub struct TimeDraft {
    pub audition: bool,
    pub minutes: String,
    pub seconds: String,
    pub position: String,
}

impl PlumSpiritApp {
    pub(super) fn open_select_range(&mut self) {
        let Some(doc) = self.doc() else {
            return;
        };
        let labels = doc.track_labels();
        let tracks: Vec<(TrackId, String)> = doc
            .tracks()
            .iter()
            .zip(labels)
            .enumerate()
            .map(|(i, (t, l))| (t.id(), format!("{}  {l}", i + 1)))
            .collect();
        let active = doc
            .track(self.active_track())
            .map_or(TrackId(0), |t| t.id());
        let (from, to) = match (self.time_selection, self.meter_map()) {
            (Some((a, b)), Some(m)) => (m.format_measure_tick(a), m.format_measure_tick(b)),
            (_, Some(m)) if m.is_musical() => ("1:0".into(), "2:0".into()),
            _ => ("0".into(), "1920".into()),
        };
        let labels = [0, 1].map(|s| {
            format!(
                "Event Graph {} ({})",
                s + 1,
                controllers::graph_label(self.shown_target(s))
            )
        });
        let last = tracks.last().map_or(active, |t| t.0);
        let selected_tracks = self.selected_tracks.len();
        let musical = self.meter_map().is_some_and(MeterMap::is_musical);
        self.dialog = Some(Dialog::SelectRange(
            Box::new(RangeDraft {
                from,
                to,
                target: RangeTarget::Notes,
                scope: TrackScope::Current,
                from_track: active,
                until_track: last,
                intersecting: false,
                add: false,
                set_loop: false,
                tracks,
                selected_tracks,
                graph_labels: labels,
                error: None,
                focused: false,
            }),
            musical,
        ));
    }

    /// Parses a range position: `M`, `M:T` or `M:B:T` (musical projects) or ticks.
    fn parse_range_position(meter: &MeterMap, text: &str) -> Result<u64, String> {
        let t = text.trim();
        if meter.is_musical() {
            meter.parse_position(t)
        } else {
            super::dialogs::parse_tick(t, "position")
        }
    }

    /// Apply in Select Range.
    pub(super) fn apply_select_range(&mut self, d: &RangeDraft) -> Result<(), String> {
        let meter = self.meter_map().cloned().ok_or("No project is open.")?;
        let t0 = Self::parse_range_position(&meter, &d.from).map_err(|e| format!("From: {e}"))?;
        let t1 = Self::parse_range_position(&meter, &d.to).map_err(|e| format!("To: {e}"))?;
        if t1 < t0 {
            return Err("“To” is before “From”.".into());
        }
        if t1 == t0 {
            return Err("The range is empty (“To” equals “From”); the end is not included.".into());
        }
        let span = format!(
            "{} – {} (ticks {} – {})",
            meter.format_position(t0),
            meter.format_position(t1),
            group_digits(t0),
            group_digits(t1)
        );
        // Select Range is a named user command: timed for the status bar (§157).
        let started = std::time::Instant::now();
        match d.target {
            RangeTarget::Notes => {
                let indices = self.scope_tracks(d)?;
                let intersecting = d.intersecting;
                let add = d.add;
                // V09-S11: the track with the opened clip selects in the
                // clip's content, at the range's content ticks.
                let session = self.session_view().map(|v| v.target);
                let (c0, c1) = self.range_to_content((t0, t1));
                let Some(doc) = self.doc_mut() else {
                    return Err(
                        "The project is busy; try again when the current job finishes.".into(),
                    );
                };
                let (mut total, mut tracks) = (0usize, 0usize);
                for &i in &indices {
                    let (target, a, b) = match session {
                        Some(t) if t.host_track() == i => (t, c0, c1),
                        _ => (plumspirit_engine::editor::NoteTarget::Track(i), t0, t1),
                    };
                    if let Ok(notes) = plumspirit_engine::editor::notes_mut(doc, target) {
                        if !add {
                            notes.clear_selection();
                        }
                        let n = notes.select_range(a, b, intersecting);
                        total += n;
                        tracks += usize::from(n > 0);
                    }
                }
                self.set_status(
                    format!(
                        "Selected {} note(s) {} in {} of {} track(s): {span}.",
                        group_digits(total as u64),
                        if intersecting { "sounding" } else { "starting" },
                        tracks,
                        indices.len()
                    ),
                    StatusKind::Info,
                );
                self.tasks.finish(
                    format!("Selected {} note(s)", group_digits(total as u64)),
                    started,
                    TaskOutcome::Done,
                );
                self.graph_focus = false;
            }
            RangeTarget::Graph(_) | RangeTarget::EventList => {
                let slot = match d.target {
                    RangeTarget::Graph(s) => s.min(1),
                    _ => self.active_graph,
                };
                let n = self.select_graph_range(slot, t0, t1, d.add)?;
                self.activate_graph(slot);
                if matches!(d.target, RangeTarget::EventList) {
                    self.event_list_open = true;
                }
                self.graph_focus = true;
                self.set_status(
                    format!(
                        "Selected {} event(s) in Graph {} ({}): {span}.",
                        group_digits(n as u64),
                        slot + 1,
                        controllers::graph_label(self.shown_target(slot))
                    ),
                    StatusKind::Info,
                );
                self.tasks.finish(
                    format!("Selected {} event(s)", group_digits(n as u64)),
                    started,
                    TaskOutcome::Done,
                );
            }
        }
        self.time_selection = Some((t0, t1));
        if d.set_loop {
            self.set_loop_range(Some((t0, t1)), true);
        }
        Ok(())
    }

    /// Track indices Select Range covers.
    fn scope_tracks(&self, d: &RangeDraft) -> Result<Vec<usize>, String> {
        let doc = self.doc().ok_or("No project is open.")?;
        let active = self.active_track();
        Ok(match d.scope {
            TrackScope::Current => {
                if self.conductor_selected() {
                    return Err(
                        "The Conductor has no notes; select a track or choose another scope."
                            .into(),
                    );
                }
                vec![active]
            }
            TrackScope::All => (0..doc.track_count()).collect(),
            TrackScope::Selected => {
                let mut v: Vec<usize> = self
                    .selected_tracks
                    .iter()
                    .filter_map(|id| doc.index_of(*id))
                    .collect();
                if !self.conductor_selected() && !v.contains(&active) {
                    v.push(active);
                }
                v.sort_unstable();
                if v.is_empty() {
                    return Err(
                        "No tracks are selected (Ctrl+click tracks in the track list).".into(),
                    );
                }
                v
            }
            TrackScope::Between => {
                let a = doc
                    .index_of(d.from_track)
                    .ok_or("The “From” track no longer exists.")?;
                let b = doc
                    .index_of(d.until_track)
                    .ok_or("The “Until” track no longer exists.")?;
                (a.min(b)..=a.max(b)).collect()
            }
        })
    }

    /// Selects graph `slot`'s events with ticks in `[t0, t1)`.
    fn select_graph_range(
        &mut self,
        slot: usize,
        t0: u64,
        t1: u64,
        add: bool,
    ) -> Result<usize, String> {
        let target = self.shown_target(slot);
        let channel = self.graph_slot(slot).channel;
        let doc = self
            .open
            .as_ref()
            .map(|o| o.doc.clone())
            .ok_or("No project is open.")?;
        // The roll's events and the range on its axis (an opened clip's
        // content; V09-S11).
        let edit = self.edit_target();
        let (c0, c1) = self.range_to_content((t0, t1));
        let view = &mut self.graphs.views[slot];
        let n = match target {
            GraphTarget::Bpm => {
                let ids: Vec<u32> = doc
                    .conductor()
                    .tempo_points()
                    .iter()
                    .filter(|p| p.tick >= t0 && p.tick < t1)
                    .map(|p| p.id)
                    .collect();
                let n = ids.len();
                view.select_tempo(ids, add);
                n
            }
            GraphTarget::MasterVolume => {
                let ids: Vec<u32> = doc
                    .conductor()
                    .master_volume()
                    .iter()
                    .filter(|p| p.tick >= t0 && p.tick < t1)
                    .map(|p| p.id)
                    .collect();
                let n = ids.len();
                view.select_master(ids, add);
                n
            }
            GraphTarget::Velocity => {
                return Err(
                    "Velocity belongs to the notes: choose “Notes” to select them by range.".into(),
                );
            }
            t => {
                let kind = graph_controller(t).ok_or("This graph has no events.")?;
                if self.open.as_ref().is_some_and(|o| o.conductor_selected) {
                    return Err(
                        "Select a track first: the graph shows the active track's events.".into(),
                    );
                }
                let track = plumspirit_engine::editor::note_view(&doc, edit)
                    .map_err(|_| "No track.".to_owned())?;
                let pts = collect_points(track.events(), kind, channel);
                let keys: Vec<usize> = pts
                    .iter()
                    .filter(|p| p.tick >= c0 && p.tick < c1)
                    .map(|p| p.index)
                    .collect();
                let n = keys.len();
                view.bind_controller_revision(
                    crate::piano_roll::render::host_key(track.host()),
                    track.events_revision(),
                    channel,
                );
                view.select_controllers(keys, add);
                n
            }
        };
        Ok(n)
    }

    // ------------------------------------------------------------ loop

    pub(super) fn loop_config(&self) -> LoopConfig {
        self.doc().map(|d| d.project().looping).unwrap_or_default()
    }

    /// Sets the loop range (and optionally turns the loop on). Project setting.
    pub(super) fn set_loop_range(&mut self, range: Option<(u64, u64)>, enable: bool) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        let l = &mut d.project_mut().looping;
        l.range = range.filter(|(a, b)| b > a);
        if enable {
            l.enabled = l.range.is_some();
        }
        if l.range.is_none() {
            l.enabled = false;
        }
        let l = *l;
        self.sync_loop();
        match l.range {
            Some((a, b)) => {
                let text = self
                    .meter_map()
                    .map(|m| format!("{} – {}", m.format_position(a), m.format_position(b)))
                    .unwrap_or_default();
                self.set_status(
                    format!("Loop {text}{}.", if l.enabled { " (on)" } else { " (off)" }),
                    StatusKind::Info,
                );
            }
            None => self.set_status("Loop cleared.", StatusKind::Info),
        }
    }

    pub(super) fn toggle_loop(&mut self) {
        let cfg = self.loop_config();
        if cfg.range.is_none() {
            // No range yet: use the selection.
            self.loop_to_selection();
            return;
        }
        if let Some(d) = self.doc_mut() {
            d.project_mut().looping.enabled = !cfg.enabled;
        }
        self.sync_loop();
        self.set_status(
            if cfg.enabled { "Loop off." } else { "Loop on." },
            StatusKind::Info,
        );
    }

    /// Loop Selected Time (§083): the loop becomes the time selection (or,
    /// without one, the span of the selected notes / chapter) and turns on.
    /// The loop is a copy: changing the time selection afterwards leaves the
    /// loop where it is until this command runs again.
    pub(super) fn loop_to_selection(&mut self) {
        let range = self.selection_span().map(|(r, _)| r);
        match range {
            Some(r) => self.set_loop_range(Some(r), true),
            None => self.set_status(
                "Drag on the ruler to select a time range (or use Edit > Select Range…, notes or a chapter) first.",
                StatusKind::Warning,
            ),
        }
    }

    /// The time selection as positions ("2:1:0 – 4:1:0"), if there is one.
    pub(super) fn time_selection_text(&mut self) -> Option<String> {
        let (a, b) = self.time_selection?;
        Some(match self.meter_map() {
            Some(m) => format!("{} – {}", m.format_position(a), m.format_position(b)),
            None => format!("{a} – {b}"),
        })
    }

    /// Sets (ruler drag, §081) or clears the time selection: the one time
    /// range that Select Range, Loop Selected Time, tool scopes, Playing Time
    /// and the range commands read (§082).
    pub(super) fn set_time_selection(&mut self, range: Option<(u64, u64)>) {
        self.time_selection = range.filter(|(a, b)| b > a);
        match self.time_selection_text() {
            Some(t) => self.set_status(format!("Time selection {t}."), StatusKind::Info),
            None => self.set_status("Time selection cleared.", StatusKind::Info),
        }
    }

    /// The notes of tracks `indices` that start in the time selection become
    /// their selection (the previous selection of those tracks is replaced).
    pub(super) fn select_in_time_selection(&mut self, indices: &[usize]) {
        let Some((a, b)) = self.time_selection else {
            self.set_status(
                "Drag on the ruler to select a time range first.",
                StatusKind::Warning,
            );
            return;
        };
        let text = self.time_selection_text().unwrap_or_default();
        // V09-S11: the track with the opened clip selects in the clip's
        // content, at the selection's content ticks.
        let session = self.session_view().map(|v| v.target);
        let (ca, cb) = self.range_to_content((a, b));
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let mut total = 0usize;
        for &i in indices {
            let (target, a, b) = match session {
                Some(t) if t.host_track() == i => (t, ca, cb),
                _ => (plumspirit_engine::editor::NoteTarget::Track(i), a, b),
            };
            if let Ok(n) = plumspirit_engine::editor::notes_mut(doc, target) {
                n.clear_selection();
                total += n.select_range(a, b, false);
            }
        }
        self.set_status(
            format!(
                "Selected {} note(s) starting in the time selection {text}.",
                group_digits(total as u64)
            ),
            StatusKind::Info,
        );
    }

    /// The selection's time span and what it is based on.
    pub(super) fn selection_span(&self) -> Option<((u64, u64), &'static str)> {
        if let Some(r) = self.time_selection {
            return Some((r, "time selection"));
        }
        if !self.conductor_selected()
            && let Some(b) = self.edit_notes().and_then(|n| n.selection_bounds())
            && b.max_end > b.min_start
        {
            // Song time (the opened clip's notes are in content ticks).
            return Some((
                (self.to_song(b.min_start), self.to_song(b.max_end)),
                "span of the selected notes",
            ));
        }
        if let Some(id) = self.roll.selected_marker
            && let Some((_, m)) = self.doc().and_then(|d| d.conductor().marker_by_id(id))
            && let Some(e) = m.end
            && e > m.start
        {
            return Some(((m.start, e), "selected chapter"));
        }
        None
    }

    /// Keeps the transport's loop equal to the project's.
    pub(super) fn sync_loop(&mut self) {
        let active = self.loop_config().active();
        self.transport.set_loop(active);
    }

    /// Loop requests from the ruler.
    pub(super) fn apply_loop_action(&mut self, a: crate::piano_roll::RollAction) {
        use crate::piano_roll::RollAction;
        let cur = self.loop_config().range;
        match a {
            RollAction::SetLoop(s, e) => self.set_loop_range(Some((s, e)), true),
            RollAction::SetLoopStart(s) => {
                let e = cur.map(|r| r.1).filter(|&e| e > s);
                match e {
                    Some(e) => self.set_loop_range(Some((s, e)), false),
                    None => {
                        // No valid end yet: one measure (or 4 beats) from here.
                        let e = self
                            .meter_map()
                            .and_then(|m| m.next_bar(s))
                            .unwrap_or(s.saturating_add(1920));
                        self.set_loop_range(Some((s, e)), false);
                    }
                }
            }
            RollAction::SetLoopEnd(e) => {
                let s = cur.map_or(0, |r| r.0);
                if e > s {
                    self.set_loop_range(Some((s, e)), false);
                } else {
                    self.set_status("The loop end must be after its start.", StatusKind::Warning);
                }
            }
            RollAction::ClearLoop => self.set_loop_range(None, false),
            _ => {}
        }
    }

    // ------------------------------------------------------------ speed

    pub(super) fn commit_speed_text(&mut self, text: &str) {
        match Speed::parse(text) {
            Ok(s) => {
                self.settings.speed = s;
                self.save_settings();
                if let Some(o) = self.open.as_ref() {
                    let (doc, generation) = (o.doc.clone(), o.generation);
                    if let Some(ev) = self.transport.set_speed(s, &doc, generation) {
                        drop(doc);
                        self.report_transport_event(ev);
                    }
                } else {
                    let d = PlumDocument::new_empty(plumspirit_engine::model::Division::ppqn(480));
                    let _ = self.transport.set_speed(s, &d, 0);
                }
                let note = if s.to_f64() >= 50.0 {
                    " Very high speeds play far more notes per second; if the synthesizer cannot keep up, notes are skipped and the status bar says so."
                } else {
                    ""
                };
                self.set_status(
                    format!("Playback speed {s}× (the project tempo is unchanged).{note}"),
                    StatusKind::Info,
                );
            }
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    // ------------------------------------------------------------ playing time

    pub(super) fn open_playing_time(&mut self) {
        self.dialog = Some(Dialog::PlayingTime {
            draft: Box::new(TimeDraft {
                audition: false,
                minutes: String::new(),
                seconds: String::new(),
                position: String::new(),
            }),
            lines: Vec::new(),
            converted: ("—".into(), "—".into()),
            speed: self.settings.speed,
        });
    }

    /// Refreshes the open Playing Time dialog's numbers (cheap: cached timing).
    pub(super) fn update_playing_time(&mut self) {
        let Some(Dialog::PlayingTime { draft, .. }) = &self.dialog else {
            return;
        };
        let d = (**draft).clone();
        let lines = self.playing_time_lines(d.audition);
        let meter = self.meter_map().cloned();
        let converted = match (self.time_map(d.audition), meter) {
            (Some(tm), Some(m)) => convert(&d, &tm, &m),
            _ => ("—".into(), "—".into()),
        };
        let speed = self.settings.speed;
        if let Some(Dialog::PlayingTime {
            lines: l,
            converted: c,
            speed: s,
            ..
        }) = self.dialog.as_mut()
        {
            *l = lines;
            *c = converted;
            *s = speed;
        }
    }

    /// Time map for Playing Time (cached per tempo revision and basis).
    pub(super) fn time_map(&mut self, audition: bool) -> Option<std::sync::Arc<TimeMap>> {
        let o = self.open.as_ref()?;
        let basis = if audition {
            TimeBasis::Audition(self.settings.speed)
        } else {
            TimeBasis::Musical
        };
        let key = (
            o.generation,
            o.doc.structure_revision(),
            o.doc.tempo_revision(),
            basis,
        );
        if self.time_cache.as_ref().is_none_or(|(k, _)| *k != key) {
            self.time_cache = Some((
                key,
                std::sync::Arc::new(TimeMap::for_document(&o.doc, basis)),
            ));
        }
        self.time_cache
            .as_ref()
            .map(|(_, m)| std::sync::Arc::clone(m))
    }

    /// Lines of the Playing Time dialog (computed from cached timing only).
    pub(super) fn playing_time_lines(&mut self, audition: bool) -> Vec<(String, String)> {
        let Some(tm) = self.time_map(audition) else {
            return Vec::new();
        };
        let Some(meter) = self.meter_map().cloned() else {
            return Vec::new();
        };
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let end = doc.duration_ticks();
        let pos = self.transport.position();
        let mb = |t: u64| {
            if meter.is_musical() {
                format!("{} · tick {}", meter.format_position(t), group_digits(t))
            } else {
                format!("tick {}", group_digits(t))
            }
        };
        let span_text = |a: u64, b: u64| match meter.span_in_measures(a, b) {
            Some((m, beats, ticks)) => format!("{m} measure(s) {beats} beat(s) {ticks} tick(s)"),
            None => format!("{} ticks", group_digits(b.saturating_sub(a))),
        };
        let mut lines = vec![
            (
                "Overall project time".to_owned(),
                format!("{}  (to {})", format_duration(tm.nanos_at(end)), mb(end)),
            ),
            (
                "Project end".to_owned(),
                format!("{} · {}", mb(end), span_text(0, end)),
            ),
            (
                "Time to playback marker".to_owned(),
                format!("{}  ({})", format_duration(tm.nanos_at(pos)), mb(pos)),
            ),
        ];
        match self.selection_span() {
            Some(((a, b), basis)) => lines.push((
                "Selected range".to_owned(),
                format!(
                    "{}  — {}; {} → {}; {}",
                    format_duration(tm.span_nanos(a, b)),
                    basis,
                    mb(a),
                    mb(b),
                    span_text(a, b)
                ),
            )),
            None => lines.push((
                "Selected range".to_owned(),
                "— (no range, notes or chapter selected)".to_owned(),
            )),
        }
        // Analytics over the time selection (§082): exact counts from the
        // project's analysis index (O(log n)), when it is up to date.
        if let Some((a, b)) = self.time_selection
            && let Some(idx) = self.analysis.index()
        {
            let scope = plumspirit_engine::analysis::Scope::Project;
            let (n, _) = idx.starts_in(&scope, a.checked_sub(1), b - 1);
            lines.push((
                "Notes in the time selection".to_owned(),
                format!(
                    "{} starting; {} sounding at its start",
                    group_digits(n),
                    group_digits(idx.poly(&scope, a))
                ),
            ));
        }
        let l = self.loop_config();
        if let Some((a, b)) = l.range {
            lines.push((
                "Loop (one iteration)".to_owned(),
                format!(
                    "{}  — {} → {}{}",
                    format_duration(tm.span_nanos(a, b)),
                    mb(a),
                    mb(b),
                    if l.enabled { " (on)" } else { " (off)" }
                ),
            ));
        }
        if let Some(id) = self.roll.selected_marker
            && let Some((_, m)) = self.doc().and_then(|d| d.conductor().marker_by_id(id))
            && let Some(e) = m.end
        {
            lines.push((
                format!("Chapter “{}”", m.title),
                format!(
                    "{}  — {} ticks",
                    format_duration(tm.span_nanos(m.start, e)),
                    group_digits(e.saturating_sub(m.start))
                ),
            ));
        }
        lines
    }
}

/// Playing Time dialog body.
pub fn playing_time_ui(
    ui: &mut egui::Ui,
    d: &mut TimeDraft,
    lines: &[(String, String)],
    speed: Speed,
    convert: &dyn Fn(&TimeDraft) -> (String, String),
) -> Option<u64> {
    let small = |s: &str| RichText::new(s).size(theme::size::SMALL);
    let mut go = None;
    ui.horizontal(|ui| {
        ui.label("Show");
        ui.radio_value(&mut d.audition, false, "Musical time (tempo map)");
        ui.radio_value(
            &mut d.audition,
            true,
            format!("At the current playback speed ({speed}×)"),
        );
    });
    ui.add_space(space::MICRO);
    egui::Grid::new("playing-time")
        .num_columns(2)
        .spacing([14.0, 5.0])
        .striped(true)
        .show(ui, |ui| {
            for (k, v) in lines {
                ui.label(small(k).strong());
                ui.label(small(v));
                ui.end_row();
            }
        });
    ui.add_space(space::NORMAL);
    ui.separator();
    ui.label(RichText::new("Convert").strong());
    let (to_pos, to_time) = convert(d);
    egui::Grid::new("playing-time-convert")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Minutes + seconds");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut d.minutes)
                        .desired_width(50.0)
                        .hint_text("min"),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut d.seconds)
                        .desired_width(70.0)
                        .hint_text("sec"),
                );
                ui.label("→");
                ui.label(small(&to_pos));
            });
            ui.end_row();
            ui.label("Musical position");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut d.position)
                        .desired_width(128.0)
                        .hint_text("M:B:T, e.g. 9:1:0"),
                );
                ui.label("→");
                ui.label(small(&to_time));
            });
            ui.end_row();
        });
    ui.label(
        small("Time → position gives the last tick that has started by that time (rounded down). Times use the tempo map with its steps and ramps.")
            .weak(),
    );
    if let Some(t) = to_pos
        .split("tick ")
        .nth(1)
        .and_then(|s| s.replace(',', "").trim().parse::<u64>().ok())
        && ui.button("Go to Converted Position").clicked()
    {
        go = Some(t);
    }
    go
}

/// The two conversions of the Playing Time dialog: (time → position, position → time).
pub fn convert(d: &TimeDraft, tm: &TimeMap, meter: &MeterMap) -> (String, String) {
    let to_pos = if d.minutes.trim().is_empty() && d.seconds.trim().is_empty() {
        "—".to_owned()
    } else {
        match parse_minutes_seconds(&d.minutes, &d.seconds) {
            Ok(ns) => {
                let t = tm.tick_at_nanos(ns);
                if meter.is_musical() {
                    format!("{} · tick {}", meter.format_position(t), group_digits(t))
                } else {
                    format!("tick {}", group_digits(t))
                }
            }
            Err(e) => e,
        }
    };
    let to_time = if d.position.trim().is_empty() {
        "—".to_owned()
    } else {
        let parsed = if meter.is_musical() {
            meter.parse_position(d.position.trim())
        } else {
            super::dialogs::parse_tick(&d.position, "position")
        };
        match parsed {
            Ok(t) => format!(
                "{} (tick {})",
                format_duration(tm.nanos_at(t)),
                group_digits(t)
            ),
            Err(e) => e,
        }
    };
    (to_pos, to_time)
}

/// Select Range dialog body. Returns true when Enter was pressed in a field.
pub fn select_range_ui(ui: &mut egui::Ui, d: &mut RangeDraft, musical: bool) -> bool {
    let small = |s: &str| RichText::new(s).size(theme::size::SMALL);
    let mut enter = false;
    egui::Grid::new("select-range")
        .num_columns(2)
        .spacing([12.0, 7.0])
        .show(ui, |ui| {
            ui.label("From");
            let r = ui.add(egui::TextEdit::singleline(&mut d.from).desired_width(110.0).hint_text("1:0"));
            if !d.focused {
                r.request_focus();
            }
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
            ui.label("To");
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut d.to).desired_width(110.0).hint_text("2"));
                enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label(small("(not included)").weak());
            });
            ui.end_row();
            ui.label("");
            ui.label(
                small(if musical {
                    "Measure, Measure:Tick or Measure:Beat:Tick — measures from 1, ticks from 0; time signatures are followed."
                } else {
                    "Positions in ticks (this project has no measures)."
                })
                .weak(),
            );
            ui.end_row();
            ui.label("Select");
            ui.vertical(|ui| {
                ui.radio_value(&mut d.target, RangeTarget::Notes, "Notes (Track List)");
                ui.radio_value(&mut d.target, RangeTarget::Graph(0), d.graph_labels[0].as_str());
                ui.radio_value(&mut d.target, RangeTarget::Graph(1), d.graph_labels[1].as_str());
                ui.radio_value(&mut d.target, RangeTarget::EventList, "Event List (active graph)");
            });
            ui.end_row();
            if d.target == RangeTarget::Notes {
                ui.label("Tracks");
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.scope, TrackScope::Current, "Current track only");
                    ui.radio_value(
                        &mut d.scope,
                        TrackScope::Selected,
                        format!("Selected tracks ({} Ctrl+clicked, plus the current one)", d.selected_tracks),
                    );
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.scope, TrackScope::Between, "From");
                        ui.add_enabled_ui(d.scope == TrackScope::Between, |ui| {
                            track_combo(ui, "range-from", &mut d.from_track, &d.tracks);
                            ui.label("until");
                            track_combo(ui, "range-until", &mut d.until_track, &d.tracks);
                        });
                    });
                    ui.radio_value(&mut d.scope, TrackScope::All, "All tracks (not the Conductor)");
                });
                ui.end_row();
                ui.label("Notes");
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.intersecting, false, "Starting in the range");
                    ui.radio_value(&mut d.intersecting, true, "Sounding in the range (intersecting)");
                });
                ui.end_row();
            }
            ui.label("");
            ui.vertical(|ui| {
                ui.checkbox(&mut d.add, "Add to the current selection");
                ui.checkbox(&mut d.set_loop, "Also set the loop to this range");
            });
            ui.end_row();
        });
    d.focused = true;
    enter
}

fn track_combo(ui: &mut egui::Ui, id: &str, value: &mut TrackId, tracks: &[(TrackId, String)]) {
    let current = tracks
        .iter()
        .find(|t| t.0 == *value)
        .map_or_else(String::new, |t| t.1.clone());
    egui::ComboBox::from_id_salt(id)
        .width(120.0)
        .selected_text(current)
        .show_ui(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for (tid, label) in tracks {
                        ui.selectable_value(value, *tid, label);
                    }
                });
        });
}
