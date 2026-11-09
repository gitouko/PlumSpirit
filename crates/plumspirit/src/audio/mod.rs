//! Playback: transport state, SoundFont management and the audio pipeline.
//!
//! [`Transport`] lives on the UI thread and is the only place that decides what the
//! audio side does. It owns the transport state machine:
//!
//! ```text
//!             Play                      Pause
//!  Stopped ─────────► Seeking ──first audio──► Playing ─────► Paused
//!     ▲                  ▲                        │  ▲           │
//!     │ end of song      └──── seek / resume ─────┘  └── Play ───┘
//!     │
//!  AudioUnavailable  (no device, no SoundFont, device lost) — Play retries
//! ```
//!
//! The musical position is a tick. While playing it is derived from the frames the
//! device actually received (never from GUI frame timing), converted through the
//! exact tempo map. Every Play/seek/resume starts a new *generation*; position
//! reports and "end of song" signals from older generations are ignored, so stale
//! audio can never move the playhead or stop newer playback.
//!
//! Nothing audio-related is created until the first Play: an idle editor opens no
//! device, starts no thread and loads no SoundFont.

#[cfg(test)]
mod delivery_analysis;
pub mod engine;
pub mod midi_out;
pub mod midi_overlay;
pub mod output;
#[cfg(test)]
pub mod profile;
#[cfg(test)]
mod profile_s66;
#[cfg(test)]
mod profile_voices;
pub mod reference;
pub mod ring;
pub mod soundfont;
pub mod synth;

use engine::{AudioEngine, Command};
use midi_out::{ExternalRoute, OutputHealth, OutputStatus, OutputTarget};
use plumspirit_engine::model::tempo::Speed;
use plumspirit_engine::model::{OutputRoute, PlumDocument, TempoMap};
use plumspirit_engine::playback::{ControlIndex, PlaybackIndex, PlaybackSetup, eligibility};
use soundfont::{LoadedSoundFont, SoundFontError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long the device stream keeps running after playback stops (lets the fade-out
/// reach the device) before it is paused to save CPU.
const IDLE_STREAM_PAUSE: Duration = Duration::from_secs(2);

/// v1.0-beta1 (B1-0080): how Play, Pause and Stop use the Edit / Anchor
/// playhead (application settings, never in the project).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayheadPolicy {
    /// The Edit / Anchor playhead: a stationary position moved only by the
    /// user (ruler, Go to…, measure steps); Play starts there. Off: one
    /// playhead, which Pause leaves where it is (v0.9 behaviour).
    pub anchor: bool,
    /// With the anchor: Pause returns the playback playhead to the anchor
    /// (true) instead of holding its place for a resume (false, default).
    pub pause_rewinds: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportState {
    Stopped,
    Playing,
    Paused,
    /// Playback was (re)started; waiting for the first audio of the new position.
    Seeking,
    AudioUnavailable,
}

impl TransportState {
    pub fn is_running(self) -> bool {
        matches!(self, TransportState::Playing | TransportState::Seeking)
    }
}

/// Something the UI should tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportEvent {
    SoundFontLoaded {
        name: String,
        path: PathBuf,
        bytes: u64,
        /// From the start of loading to the result reaching the UI thread.
        elapsed: Duration,
    },
    SoundFontFailed {
        path: PathBuf,
        message: String,
        elapsed: Duration,
    },
    Unavailable(String),
    /// Playback continues, but something the user should know about (an
    /// external output is missing, notes are being skipped).
    Notice(String),
    Ended,
}

/// Which document state the running audio reflects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DocKey {
    doc_generation: u64,
    structure: u64,
    /// The Conductor's tempo map.
    tempo: u64,
    /// Every note store and placement (`PlumDocument::analysis_revision`).
    notes: u64,
    /// Pitch bend, pan and the other preserved channel events.
    events: u64,
    /// Track properties (corrections, routes) and explicit ends.
    props: u64,
    speed: Speed,
    routes: u64,
}

/// Sum of every track's events revision. Revisions only grow (undo and redo
/// bump them too), so any edit changes the sum.
/// L-S46: whether an overload notice may be given now — the first at once,
/// then at most one a second (the status keeps the latest count; the log is
/// not flooded at Black MIDI rates).
fn notice_due(last: &mut Option<Instant>) -> bool {
    notice_due_at(last, Instant::now())
}

fn notice_due_at(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|t| now.duration_since(t) < NOTICE_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

const NOTICE_INTERVAL: Duration = Duration::from_secs(1);

fn events_key(doc: &PlumDocument) -> u64 {
    doc.tracks()
        .iter()
        .fold(0u64, |a, t| a.wrapping_add(t.events_revision()))
}

/// What the cached control indexes depend on: the document generation and
/// structure, every track's events and properties, the routes and (v0.9) the
/// placements and owned contents' events (0 without clips).
fn controls_key(doc: &PlumDocument, generation: u64, routes_key: u64) -> ControlsKey {
    (
        generation,
        doc.structure_revision(),
        events_key(doc),
        props_key(doc),
        routes_key,
        doc.clip_events_key(),
    )
}

/// Sum of every track's properties revision.
fn props_key(doc: &PlumDocument) -> u64 {
    doc.tracks()
        .iter()
        .fold(0u64, |a, t| a.wrapping_add(t.props_revision()))
}

impl DocKey {
    fn of(doc: &PlumDocument, doc_generation: u64, speed: Speed, routes: u64) -> Self {
        DocKey {
            doc_generation,
            structure: doc.structure_revision(),
            tempo: doc.tempo_revision(),
            // B2-A02: every track's and owned content's notes and every
            // placement — a moved clip or an edit inside a clip switches the
            // running audio too.
            notes: doc.analysis_revision(),
            events: events_key(doc),
            props: props_key(doc),
            speed,
            routes,
        }
    }
}

/// (document generation, structure revision, tempo revision, sample rate, speed).
type TempoKey = (u64, u64, u64, u32, Speed);
/// (document generation, structure, events, properties, routes).
type ControlsKey = (u64, u64, u64, u64, u64, u64);

struct Loader {
    path: PathBuf,
    /// When loading started (task duration in the status bar).
    started: Instant,
    rx: Receiver<Result<LoadedSoundFont, SoundFontError>>,
    handle: Option<JoinHandle<()>>,
}

pub struct Transport {
    midi_overlay: Option<midi_overlay::Watch>,
    // Native action generation for safely rejecting stale extension requests.
    action_revision: u64,
    state: TransportState,
    /// Musical position (ticks) while not playing; the start point while playing.
    position: u64,
    generation: u64,
    engine: Option<AudioEngine>,
    soundfont: Option<LoadedSoundFont>,
    loader: Option<Loader>,
    /// Play as soon as the SoundFont finishes loading.
    play_when_ready: bool,
    unavailable: Option<String>,
    /// Control indexes of every route, for one document state.
    controls: Option<(ControlsKey, Vec<Arc<ControlIndex>>)>,
    tempo: Option<(TempoKey, Arc<TempoMap>)>,
    /// Audition speed (the Conductor is unchanged).
    speed: Speed,
    /// Loop range `[start, end)` while the loop is on.
    looping: Option<(u64, u64)>,
    /// Open external outputs (route `i + 1`), and a key that changes with the list.
    routes: Vec<Arc<ExternalRoute>>,
    routes_key: u64,
    /// Where tracks set to "Internal" play (B1-0135), and why it is not open:
    /// `Unavailable` (could not be opened) or `Failed` (stopped working).
    default_output: OutputTarget,
    default_error: Option<OutputHealth>,
    /// L-B06: outputs whose open failed after playback started (not retried
    /// before the next Play).
    open_failed: Vec<OutputTarget>,
    /// Tests: opens external outputs instead of the platform (fake ports).
    #[cfg(test)]
    test_opener: Option<Arc<TestOpener>>,
    /// Which tracks the running audio plays (mute / solo).
    eligible_sent: Option<Arc<[bool]>>,
    /// Notices reported once per Play (missing outputs).
    notices: Vec<String>,
    /// Skipped-note count already reported.
    skipped_reported: u64,
    /// v1.0-Launch L-B03: external note-ons dropped (output overloaded)
    /// already reported.
    external_dropped_reported: u64,
    /// Clipped-sample count already reported (limiter off).
    clipped_reported: u64,
    /// L-S46: when each overload notice (skipped, dropped, clipped) was last
    /// given: after the first, at most one a second — at Black MIDI rates
    /// "once per 1,000 more" was one a frame (149 log lines in 7 s at
    /// 1.8M notes / s on KDMAPI).
    notice_at: [Option<Instant>; 3],
    playing_key: Option<DocKey>,
    /// Last playhead shown (for smooth display between audio callbacks).
    last_playhead: u64,
    /// The tick of the last pause, until the playhead moves: playing from
    /// there is a resume (v1.0-beta1 B1-0083).
    paused_at: Option<u64>,
    /// Re-strike the held notes on a resume (Audio Settings; default off).
    resume_restrike: bool,
    /// B1-0080: the anchor (Edit playhead, or where the last playback
    /// started without one) and how Play / Pause / Stop use it.
    anchor: u64,
    policy: PlayheadPolicy,
    retired: Vec<JoinHandle<()>>,
    /// When playback last stopped (for pausing the idle device stream).
    idle_since: Option<Instant>,
    /// Audition notes held down (the device stream stays on while > 0).
    auditions_held: Vec<(u8, u8)>,
    /// An audition note (held or timed) was sent since the last all-off.
    audition_sent: bool,
    /// Reference audio (v0.8): what the worker plays (with the speed) and
    /// what it was made from.
    reference: Option<Arc<reference::RefPlay>>,
    /// Synthesis settings (v0.8 Audio Settings).
    synth_config: synth::SynthConfig,
    /// Output device / rate / buffer and render mode (Pre-Render, ms ahead).
    output: output::OutputConfig,
    render: (bool, u32),
}

/// Live audio diagnostics (v0.8 §145) for the Audio Settings window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioDiagnostics {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub callback_frames: u32,
    pub buffer: Option<u32>,
    pub ring_frames: usize,
    /// Buffered audio ahead of the device (frames).
    pub buffered: usize,
    /// Pre-Render: fraction of the start buffer filled (`None`: playing / not
    /// Pre-Render).
    pub preroll: Option<f32>,
    pub synth_load: f64,
    pub underruns: u64,
    pub sounding: u32,
    pub capacity: u32,
    pub overflow_blocks: u64,
    pub at_capacity: u64,
    pub not_started: u64,
    pub released_oldest: u64,
    pub skipped: u64,
    pub deferred: u64,
    pub late_dropped: u64,
    pub max_lateness_ms: f64,
    pub peak_pre: f32,
    pub peak_post: f32,
    pub clipped: u64,
    pub limited: u64,
    /// v1.0-beta2 (B2-F12): notes Event Processing kept from the outputs.
    pub filtered_keys: u64,
    pub filtered_velocity: u64,
    pub resets_sent: u64,
    pub resets_ignored: u64,
    pub retriggered: u64,
    pub notes: Vec<String>,
}

/// Tests: what [`Transport::prepare_routes`] opens outputs with.
#[cfg(test)]
type TestOpener = midi_out::SharedOpener;

/// L-B06: how long Play / a route change waits for outputs to open before
/// playback goes on without them (they join when ready). A MIDI device opens
/// in milliseconds; OmniMIDI's KDMAPI start can take seconds.
const OPEN_WAIT: Duration = Duration::from_millis(100);

impl Default for Transport {
    fn default() -> Self {
        Transport {
            midi_overlay: None,
            action_revision: 0,
            state: TransportState::Stopped,
            position: 0,
            generation: 1,
            engine: None,
            soundfont: None,
            loader: None,
            play_when_ready: false,
            unavailable: None,
            controls: None,
            tempo: None,
            speed: Speed::NORMAL,
            looping: None,
            routes: Vec::new(),
            routes_key: 0,
            default_output: OutputTarget::Internal,
            default_error: None,
            open_failed: Vec::new(),
            #[cfg(test)]
            test_opener: None,
            eligible_sent: None,
            notices: Vec::new(),
            skipped_reported: 0,
            external_dropped_reported: 0,
            clipped_reported: 0,
            notice_at: [None; 3],
            playing_key: None,
            last_playhead: 0,
            paused_at: None,
            resume_restrike: false,
            anchor: 0,
            policy: PlayheadPolicy::default(),
            retired: Vec::new(),
            idle_since: None,
            auditions_held: Vec::new(),
            audition_sent: false,
            reference: None,
            synth_config: synth::SynthConfig::default(),
            output: output::OutputConfig::default(),
            render: (false, engine::DEFAULT_AHEAD_MS),
        }
    }
}

impl Transport {
    pub fn midi_resources(&self) -> Option<midi_overlay::Resources> {
        if !self.state.is_running() || self.render.0 {
            return None;
        }
        let engine = self
            .engine
            .as_ref()
            .filter(|e| e.worker_alive() && !e.shared.device_failed.load(Ordering::Acquire))?;
        let (_, tempo) = self.tempo.as_ref()?;
        if tempo.sample_rate() != engine.sample_rate {
            return None;
        }
        Some(midi_overlay::Resources {
            tempo: Arc::clone(tempo),
            font: Arc::clone(&self.soundfont.as_ref()?.font),
            sample_rate: engine.sample_rate,
        })
    }
    pub fn midi_status(&self) -> midi_overlay::Status {
        self.midi_overlay
            .as_ref()
            .map_or(midi_overlay::Status::Idle, |w| w.status())
    }
    pub fn midi_watch(&self) -> Option<&midi_overlay::Watch> {
        self.midi_overlay.as_ref()
    }
    pub fn midi_pending(&self) -> u32 {
        self.midi_overlay
            .as_ref()
            .map_or(0, |w| w.pending.load(Ordering::Acquire))
    }
    pub fn cancel_plugin_midi(&mut self) {
        if let Some(w) = self
            .midi_overlay
            .as_ref()
            .filter(|w| w.alive.load(Ordering::Acquire))
        {
            w.cancel();
        }
    }
    pub fn submit_plugin_midi(
        &mut self,
        voice: Box<midi_overlay::PreparedVoice>,
        owner: Arc<str>,
        project: (u64, u64),
        plugin: Arc<std::sync::atomic::AtomicBool>,
        epoch: Arc<std::sync::atomic::AtomicU64>,
        version: u64,
    ) -> Result<(), &'static str> {
        if self
            .midi_overlay
            .as_ref()
            .is_some_and(|w| w.alive.load(Ordering::Acquire))
        {
            return Err("Another plugin MIDI stream is active.");
        }
        if self.midi_resources().is_none() {
            return Err(
                "MIDI output requires running native internal playback with a SoundFont and without Pre-Render.",
            );
        }
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pending = Arc::new(std::sync::atomic::AtomicU32::new(voice.count()));
        let status = Arc::new(std::sync::atomic::AtomicU8::new(
            midi_overlay::Status::Queued as u8,
        ));
        let life = midi_overlay::Life {
            alive: Arc::clone(&alive),
            status: Arc::clone(&status),
            pending: Arc::clone(&pending),
            plugin,
            epoch,
            version,
        };
        let sent = self
            .engine
            .as_ref()
            .is_some_and(|e| e.send(Command::PluginMidi { voice, life }));
        self.midi_overlay = Some(midi_overlay::Watch {
            owner,
            project,
            alive,
            status,
            pending,
        });
        if !sent {
            let watch = self.midi_overlay.as_ref().expect("set watch");
            watch
                .status
                .store(midi_overlay::Status::Refused as u8, Ordering::Release);
            watch.alive.store(false, Ordering::Release);
            return Err("The native audio command queue is busy or unavailable.");
        }
        Ok(())
    }
    pub fn action_revision(&self) -> u64 {
        self.action_revision
    }
    fn action_changed(&mut self) {
        self.cancel_plugin_midi();
        self.action_revision = self.action_revision.saturating_add(1);
    }

    pub fn speed(&self) -> Speed {
        self.speed
    }

    pub fn loop_range(&self) -> Option<(u64, u64)> {
        self.looping
    }

    pub fn state(&self) -> TransportState {
        self.state
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub fn soundfont(&self) -> Option<&LoadedSoundFont> {
        self.soundfont.as_ref()
    }

    pub fn loading_soundfont(&self) -> Option<&Path> {
        self.loader.as_ref().map(|l| l.path.as_path())
    }

    /// Tick shown as the playhead / edit position.
    pub fn position(&self) -> u64 {
        if self.state.is_running() {
            self.last_playhead
        } else {
            self.position
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn tempo_map(
        &mut self,
        doc: &PlumDocument,
        doc_generation: u64,
        sample_rate: u32,
    ) -> Arc<TempoMap> {
        let key = (
            doc_generation,
            doc.structure_revision(),
            doc.tempo_revision(),
            sample_rate,
            self.speed,
        );
        if let Some((k, m)) = &self.tempo
            && *k == key
        {
            return Arc::clone(m);
        }
        let m = Arc::new(TempoMap::for_document_at(doc, sample_rate, self.speed));
        self.tempo = Some((key, Arc::clone(&m)));
        m
    }

    /// Each track's route (0 = internal synthesizer, `i + 1` = `self.routes[i]`),
    /// mute / solo and corrections. Tracks set to Internal play on the default
    /// output while it is open; a track routed to a missing device plays on
    /// the internal synthesizer.
    fn setup(&self, doc: &PlumDocument) -> PlaybackSetup {
        let mut setup = PlaybackSetup::for_document(doc);
        setup.route_count = 1 + self.routes.len();
        let open = |target: &OutputTarget| {
            self.routes
                .iter()
                .position(|x| &x.target == target && !x.has_failed())
        };
        let default = open(&self.default_output);
        for (i, t) in doc.tracks().iter().enumerate() {
            let r = match &t.props().route {
                OutputRoute::Internal => default,
                OutputRoute::External(name) => open(&OutputTarget::Device(name.clone())),
            };
            if let Some(r) = r {
                setup.routes[i] = (r + 1) as u16;
            }
        }
        setup
    }

    fn build_index(
        &mut self,
        doc: &PlumDocument,
        doc_generation: u64,
        sample_rate: u32,
    ) -> Arc<PlaybackIndex> {
        let setup = self.setup(doc);
        let ckey = controls_key(doc, doc_generation, self.routes_key);
        let controls = match &self.controls {
            Some((k, c)) if *k == ckey => c.clone(),
            _ => {
                let c = setup.control_indexes(doc);
                self.controls = Some((ckey, c.clone()));
                c
            }
        };
        let tempo = self.tempo_map(doc, doc_generation, sample_rate);
        self.eligible_sent = Some(Arc::clone(&setup.eligible));
        Arc::new(PlaybackIndex::new(doc, controls, tempo, &setup))
    }

    /// L-B06: whether an audible track with notes plays on the internal
    /// synthesizer — as the open routes place it (`resolved`), else as the
    /// settings route it (a track set to a device is taken to play there).
    fn internal_audible(&self, doc: &PlumDocument, resolved: bool) -> bool {
        let setup = resolved.then(|| self.setup(doc));
        let audible = eligibility(doc);
        let notes = doc.placed_note_counts();
        doc.tracks().iter().enumerate().any(|(i, t)| {
            audible.get(i).copied().unwrap_or(true)
                && notes.get(i).is_some_and(|&n| n > 0)
                && match &setup {
                    Some(s) => s.routes.get(i).is_none_or(|&r| r == 0),
                    None => {
                        matches!(t.props().route, OutputRoute::Internal)
                            && self.default_output == OutputTarget::Internal
                    }
                }
        })
    }

    /// Opens the external outputs the tracks use and the default output
    /// (keeping those already open) and tells the worker. Missing devices are
    /// reported; their tracks play on the internal synthesizer (never on both).
    ///
    /// L-B06: outputs open on their own threads, all at once; this waits at
    /// most [`OPEN_WAIT`] for them. One still opening after that stays in the
    /// routes as connecting — its tracks are silent (never on both outputs),
    /// playback and the playhead run, and the worker joins it at the playhead
    /// when it is ready (`Scheduler::join_route`); one that fails later is
    /// reported by [`Transport::update`] and its tracks move to the internal
    /// synthesizer.
    fn prepare_routes(&mut self, doc: &PlumDocument) {
        #[cfg(test)]
        if let Some(open) = self.test_opener.clone() {
            self.prepare_routes_async(doc, &open, OPEN_WAIT);
            return;
        }
        let opener: Arc<midi_out::SharedOpener> = Arc::new(midi_out::open_target);
        self.prepare_routes_async(doc, &opener, OPEN_WAIT);
    }

    /// The outputs the project plays to: the default output (unless it is
    /// the internal synthesizer) and every device a track is routed to.
    fn wanted_outputs(&self, doc: &PlumDocument) -> Vec<OutputTarget> {
        let mut wanted: Vec<OutputTarget> = Vec::new();
        if self.default_output != OutputTarget::Internal {
            wanted.push(self.default_output.clone());
        }
        for t in doc.tracks() {
            if let OutputRoute::External(n) = &t.props().route {
                let target = OutputTarget::Device(n.clone());
                if !wanted.contains(&target) {
                    wanted.push(target);
                }
            }
        }
        wanted
    }

    /// The open route to `target`, if any.
    fn open_route(&self, target: &OutputTarget) -> Option<&Arc<ExternalRoute>> {
        self.routes
            .iter()
            .find(|r| &r.target == target && !r.has_failed())
    }

    /// Opens synchronously with `open` (tests of the routing rules).
    #[cfg(test)]
    fn prepare_routes_with(&mut self, doc: &PlumDocument, open: &midi_out::OpenFn<'_>) {
        let wanted = self.wanted_outputs(doc);
        self.apply_routes(wanted, &mut |t: &OutputTarget| {
            open(t).map(|port| ExternalRoute::for_target(t.clone(), port))
        });
    }

    /// [`Transport::prepare_routes`] with `opener`, waiting at most `wait`.
    fn prepare_routes_async(
        &mut self,
        doc: &PlumDocument,
        opener: &Arc<midi_out::SharedOpener>,
        wait: Duration,
    ) {
        let mut wanted = self.wanted_outputs(doc);
        // An output that failed to open after Play started is not retried
        // before the next Play (a 9 s failing start must not loop).
        wanted.retain(|t| !self.open_failed.contains(t));
        let started: Vec<Arc<ExternalRoute>> = wanted
            .iter()
            .filter(|t| self.open_route(t).is_none())
            .map(|t| midi_out::connect(t.clone(), Arc::clone(opener)))
            .collect();
        let deadline = Instant::now() + wait;
        while started.iter().any(|r| r.is_connecting()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.apply_routes(wanted, &mut |t: &OutputTarget| {
            let r = started
                .iter()
                .find(|r| &r.target == t)
                .ok_or_else(|| format!("The MIDI output “{}” was not opened.", t.label()))?;
            if let Some(e) = r.open_error() {
                return Err(e);
            }
            // Opened in time: the seek that follows (Play / Update) makes
            // it live — no separate join.
            r.begin_join();
            Ok(Arc::clone(r))
        });
    }

    /// Keeps the open routes of `wanted`, takes new ones from `get` (an
    /// error: reported, the tracks play on the internal synthesizer) and
    /// tells the worker when the list changed.
    fn apply_routes(
        &mut self,
        wanted: Vec<OutputTarget>,
        get: &mut dyn FnMut(&OutputTarget) -> Result<Arc<ExternalRoute>, String>,
    ) {
        self.notices.clear();
        let mut next: Vec<Arc<ExternalRoute>> = Vec::new();
        for target in wanted {
            let is_default = target == self.default_output;
            if let Some(r) = self.open_route(&target) {
                next.push(Arc::clone(r));
                continue;
            }
            match get(&target) {
                Ok(route) => {
                    if route.is_connecting() {
                        log::info!(
                            "MIDI output “{}” is still opening; its tracks join at the playhead when it is ready",
                            target.label()
                        );
                    } else {
                        log::info!("MIDI output “{}” opened", target.label());
                    }
                    if is_default {
                        self.default_error = None;
                    }
                    next.push(route);
                }
                Err(e) => {
                    let whose = if is_default {
                        self.default_error = Some(OutputHealth::Unavailable(e.clone()));
                        "Tracks set to Internal play"
                    } else {
                        "Its tracks play"
                    };
                    self.notices.push(format!(
                        "{} {whose} on the internal synthesizer instead.",
                        e
                    ));
                }
            }
        }
        let same = next.len() == self.routes.len()
            && next
                .iter()
                .zip(&self.routes)
                .all(|(a, b)| Arc::ptr_eq(a, b));
        if !same {
            self.routes = next;
            self.routes_key = self.routes_key.wrapping_add(1);
            if let Some(e) = &self.engine {
                e.send(Command::SetRoutes(self.routes.clone()));
            }
        }
    }

    /// The reference audio (v0.8 Audio Reference): `offset` = project seconds
    /// where its start plays, `gain` 0–2 (0 = muted). Sent to the worker only
    /// when something changed; takes effect at the next block.
    pub fn set_reference(
        &mut self,
        audio: Option<Arc<reference::RefAudio>>,
        offset: f64,
        gain: f32,
    ) {
        let next = audio.map(|audio| {
            Arc::new(reference::RefPlay {
                audio,
                offset,
                gain,
                speed: self.speed.to_f64(),
            })
        });
        let same = match (&self.reference, &next) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                Arc::ptr_eq(&a.audio, &b.audio)
                    && a.offset == b.offset
                    && a.gain == b.gain
                    && a.speed == b.speed
            }
            _ => false,
        };
        if same {
            return;
        }
        self.reference = next;
        if let Some(e) = &self.engine {
            e.send(Command::SetReference(self.reference.clone()));
        }
    }

    /// The reference audio as the worker plays it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn reference(&self) -> Option<&Arc<reference::RefPlay>> {
        self.reference.as_ref()
    }

    /// Audition speed (see [`Speed`]); restarts running audio at the new speed so
    /// no audio rendered at the old speed is heard.
    pub fn set_speed(
        &mut self,
        speed: Speed,
        doc: &PlumDocument,
        doc_generation: u64,
    ) -> Option<TransportEvent> {
        if self.speed == speed {
            return None;
        }
        self.action_changed();
        self.speed = speed;
        if let Some(r) = &self.reference {
            let (a, o, g) = (r.audio.clone(), r.offset, r.gain);
            self.set_reference(Some(a), o, g);
        }
        if self.state.is_running() && !self.play_when_ready {
            self.position = self.last_playhead;
            return self.start(doc, doc_generation);
        }
        None
    }

    /// Sets (or clears) the loop range; takes effect immediately while playing.
    pub fn set_loop(&mut self, range: Option<(u64, u64)>) {
        let range = range.filter(|(a, b)| b > a);
        if self.looping == range {
            return;
        }
        self.action_changed();
        self.looping = range;
        if self.state.is_running()
            && let Some(e) = &self.engine
        {
            e.send(Command::SetLoop(range));
        }
    }

    /// Starts loading a SoundFont in the background. The previous one stays active
    /// until the new one has loaded and validated.
    pub fn load_soundfont(&mut self, path: PathBuf) {
        if self.loader.as_ref().is_some_and(|l| l.path == path) {
            return;
        }
        self.action_changed();
        if let Some(old) = self.loader.take()
            && let Some(h) = old.handle
        {
            self.retired.push(h);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        let handle = std::thread::Builder::new()
            .name("plumspirit-soundfont".into())
            .spawn(move || {
                let _ = tx.send(soundfont::load(&p));
            })
            .ok();
        log::info!("loading SoundFont {}", path.display());
        self.loader = Some(Loader {
            path,
            started: Instant::now(),
            rx,
            handle,
        });
    }

    /// Forgets the loaded SoundFont (e.g. a new project references another one);
    /// memory is released once the synthesizer switches.
    pub fn has_soundfont_for(&self, path: &Path) -> bool {
        self.soundfont.as_ref().is_some_and(|s| s.path == path)
            || self.loader.as_ref().is_some_and(|l| l.path == path)
    }

    /// New synthesis settings: applied to running audio at the next block
    /// (voices / effects rebuild the synthesizers, sounding notes re-struck).
    pub fn configure_synth(&mut self, cfg: synth::SynthConfig) {
        let cfg = cfg.clamped();
        if cfg == self.synth_config {
            return;
        }
        self.synth_config = cfg;
        if let Some(e) = &self.engine {
            e.send(Command::Configure(cfg));
        }
    }

    /// Output device / sample rate / buffer (applied by a new engine: at
    /// once while stopped, else at the next Play).
    pub fn configure_output(&mut self, cfg: output::OutputConfig) {
        if self.output != cfg {
            self.action_changed();
        }
        self.output = cfg;
        if !self.state.is_running() && self.auditions_held.is_empty() {
            self.release_stale_engine();
        }
    }

    /// Realtime / Pre-Render (`ahead_ms` rendered ahead). Applies from the
    /// next start position; a larger buffer than the running engine has needs
    /// a new engine (at once while stopped, else at the next Play).
    pub fn configure_render(&mut self, prerender: bool, ahead_ms: u32) {
        self.render = (
            prerender,
            ahead_ms.clamp(engine::MIN_AHEAD_MS, engine::MAX_AHEAD_MS),
        );
        if let Some(e) = &self.engine {
            let r = engine::RenderConfig::new(self.render.0, self.render.1, e.sample_rate);
            if r.ring_frames() <= e.ring_frames {
                e.send(Command::SetRender(r));
            } else if !self.state.is_running() && self.auditions_held.is_empty() {
                self.release_stale_engine();
            }
        }
    }

    /// The running engine no longer matches the settings.
    fn engine_stale(&self, e: &AudioEngine) -> bool {
        e.output != self.output
            || engine::RenderConfig::new(self.render.0, self.render.1, e.sample_rate).ring_frames()
                > e.ring_frames
    }

    fn release_stale_engine(&mut self) {
        if self.engine.as_ref().is_some_and(|e| self.engine_stale(e)) {
            log::info!("audio settings changed: the device stream is reopened");
            self.engine = None;
        }
    }

    fn ensure_engine(&mut self) -> Result<(), String> {
        if let Some(e) = &self.engine
            && e.worker_alive()
            && !e.shared.device_failed.load(Ordering::Acquire)
            && !self.engine_stale(e)
        {
            return Ok(());
        }
        self.engine = None;
        let engine = AudioEngine::start(self.synth_config, &self.output, self.render)?;
        if let Some(sf) = &self.soundfont {
            engine.send(Command::SetSoundFont(Arc::clone(&sf.font)));
        }
        if self.reference.is_some() {
            engine.send(Command::SetReference(self.reference.clone()));
        }
        self.engine = Some(engine);
        Ok(())
    }

    fn set_unavailable(&mut self, reason: String) -> TransportEvent {
        log::warn!("playback unavailable: {reason}");
        self.auditions_held.clear();
        self.audition_sent = false;
        self.state = TransportState::AudioUnavailable;
        self.unavailable = Some(reason.clone());
        self.play_when_ready = false;
        TransportEvent::Unavailable(reason)
    }

    // ------------------------------------------------------------ audition (v0.6)

    /// Plays `key` on the audition synthesizer (its own voices: it can never end
    /// a note the transport plays, and the transport cannot end it). The
    /// channel's program, bank and controllers at `tick` on the track's route
    /// are applied first (pedals left out). `hold`: stops by itself after this
    /// long; `None`: until [`Transport::audition_off`].
    #[allow(clippy::too_many_arguments)]
    pub fn audition_on(
        &mut self,
        doc: &PlumDocument,
        doc_generation: u64,
        track: usize,
        channel: u8,
        key: u8,
        velocity: u8,
        tick: u64,
        hold: Option<Duration>,
    ) -> Result<(), String> {
        // v0.9: keys above 127 (256-Key Mode) have no sound — MIDI keys stop
        // at 127; never folded onto another key.
        if key > synth::MIDI_TOP_KEY {
            return Ok(());
        }
        if self.soundfont.is_none() {
            return Err(if self.loader.is_some() {
                "The SoundFont is still loading; note audition starts when it is ready.".into()
            } else {
                "No SoundFont available for note audition.".into()
            });
        }
        self.ensure_engine()?;
        if let Some(Err(e)) = self.engine.as_mut().map(|e| e.set_active(true)) {
            self.engine = None;
            return Err(e);
        }
        let ch = channel & 0x0F;
        let state = self.channel_state_at(doc, doc_generation, track, ch, tick);
        let sr = self.engine.as_ref().map_or(48_000, |e| e.sample_rate);
        let hold_frames = hold.map(|d| ((d.as_secs_f64() * f64::from(sr)) as u32).max(1));
        let sent = self.engine.as_ref().is_some_and(|e| {
            e.send(Command::AuditionOn {
                channel: ch,
                key,
                velocity: velocity.clamp(1, 127),
                state: Box::new(state),
                hold: hold_frames,
            })
        });
        if !sent {
            return Err("The audio engine is not responding.".into());
        }
        self.audition_sent = true;
        if hold.is_none() && !self.auditions_held.contains(&(ch, key)) {
            self.auditions_held.push((ch, key));
        }
        if !self.state.is_running() {
            self.idle_since = Some(Instant::now());
        }
        Ok(())
    }

    /// Ends a held audition note.
    pub fn audition_off(&mut self, channel: u8, key: u8) {
        if key > synth::MIDI_TOP_KEY {
            return;
        }
        let ch = channel & 0x0F;
        self.auditions_held.retain(|&n| n != (ch, key));
        if let Some(e) = &self.engine {
            e.send(Command::AuditionOff { channel: ch, key });
        }
        if !self.state.is_running() {
            self.idle_since = Some(Instant::now());
        }
    }

    /// Releases every audition note (focus lost, dialog, project closed).
    pub fn audition_all_off(&mut self) {
        let had = !self.auditions_held.is_empty() || self.audition_sent;
        self.auditions_held.clear();
        self.audition_sent = false;
        if let Some(e) = &self.engine
            && had
        {
            e.send(Command::AuditionAllOff);
        }
    }

    /// Held audition notes (tests).
    #[cfg(test)]
    pub fn auditions_held(&self) -> usize {
        self.auditions_held.len()
    }

    /// The state of MIDI channel `ch` at `tick` for `track`'s output route,
    /// without held pedals. Uses the cached control indexes (built once per
    /// change of the events, never per click).
    fn channel_state_at(
        &mut self,
        doc: &PlumDocument,
        doc_generation: u64,
        track: usize,
        ch: u8,
        tick: u64,
    ) -> plumspirit_engine::playback::ChannelState {
        let setup = self.setup(doc);
        let ckey = controls_key(doc, doc_generation, self.routes_key);
        let controls = match &self.controls {
            Some((k, c)) if *k == ckey => c.clone(),
            _ => {
                let c = setup.control_indexes(doc);
                self.controls = Some((ckey, c.clone()));
                c
            }
        };
        let route = usize::from(setup.routes.get(track).copied().unwrap_or(0));
        let idx = controls.get(route).or_else(|| controls.first());
        idx.map(|c| c.state_before(tick.saturating_add(1)).0[usize::from(ch)])
            .unwrap_or_default()
            .without_pedals()
    }

    /// Play (the user's Play): from the anchor when the Edit / Anchor
    /// playhead is on (B1-0080) unless it continues a Pause that held its
    /// place, else from the current position (from the start when at or past
    /// the end).
    pub fn play(&mut self, doc: &PlumDocument, doc_generation: u64) -> Option<TransportEvent> {
        self.action_changed();
        if self.policy.anchor && self.paused_at != Some(self.position) {
            self.position = self.anchor;
        }
        let ev = self.start(doc, doc_generation);
        if !self.policy.anchor {
            self.anchor = self.position; // Stop returns to where this playback started
        }
        ev
    }

    /// Starts the audio at `self.position` (Play, and restarts that keep the
    /// place: speed change, SoundFont ready).
    fn start(&mut self, doc: &PlumDocument, doc_generation: u64) -> Option<TransportEvent> {
        let end = doc.duration_ticks();
        if end == 0 && doc.note_count() == 0 && self.looping.is_none() {
            return Some(self.notice("There is nothing to play yet: the project has no notes."));
        }
        match self.looping {
            // Loop on: from a position at or after the loop's end, start at its start.
            Some((ls, le)) if self.position >= le => self.position = ls,
            Some(_) => {}
            None if self.position >= end => self.position = 0,
            None => {}
        }
        // L-B06: only a project with an audible track on the internal
        // synthesizer waits for (or needs) a SoundFont; one that plays on
        // external outputs only starts at once — the synthesizer joins at the
        // playhead when a SoundFont is ready.
        let needs_synth = self.internal_audible(doc, false);
        // A SoundFont is loading (first play, or the project uses another one):
        // start as soon as it is ready.
        if needs_synth && self.loader.is_some() {
            self.play_when_ready = true;
            self.state = TransportState::Seeking;
            return None;
        }
        if needs_synth && self.soundfont.is_none() {
            return Some(self.set_unavailable(
                "No SoundFont selected. Choose one with Browse… next to “SoundFont”.".into(),
            ));
        }
        if let Err(e) = self.ensure_engine() {
            return Some(self.set_unavailable(e));
        }
        if let Some(Err(e)) = self.engine.as_mut().map(|e| e.set_active(true)) {
            self.engine = None;
            return Some(self.set_unavailable(e));
        }
        self.idle_since = None;
        let sr = self.engine.as_ref().map_or(48_000, |e| e.sample_rate);
        // Every Play tries the outputs again.
        self.open_failed.clear();
        self.prepare_routes(doc);
        // An output that could not be opened sent its tracks to a
        // synthesizer that has no SoundFont yet: say so (they are silent).
        if self.soundfont.is_none() && self.internal_audible(doc, true) {
            self.notices.push(if self.loader.is_some() {
                "Tracks on the internal synthesizer join when the SoundFont has loaded.".to_owned()
            } else {
                "No SoundFont is loaded: tracks on the internal synthesizer are silent. Choose one with Browse… next to “SoundFont”.".to_owned()
            });
        }
        let index = self.build_index(doc, doc_generation, sr);
        let generation = self.next_generation();
        let tick = self.position;
        let looping = self.looping;
        // A resume at the paused tick strikes no held note again unless the
        // user asked for it (B1-0083); every other start is a seek.
        let resume = self.paused_at.take() == Some(tick);
        let restrike = !resume || self.resume_restrike;
        let sent = self.engine.as_ref().is_some_and(|e| {
            e.send(Command::Play {
                index,
                tick,
                generation,
                looping,
                restrike,
            })
        });
        if !sent {
            self.engine = None;
            return Some(self.set_unavailable("The audio engine stopped responding.".into()));
        }
        self.playing_key = Some(DocKey::of(doc, doc_generation, self.speed, self.routes_key));
        self.unavailable = None;
        self.last_playhead = tick;
        self.state = TransportState::Seeking;
        self.skipped_reported = self
            .engine
            .as_ref()
            .map_or(0, |e| e.shared.skipped_notes.load(Ordering::Relaxed));
        self.clipped_reported = self
            .engine
            .as_ref()
            .map_or(0, |e| e.shared.clipped.load(Ordering::Relaxed));
        self.external_dropped_reported = self
            .routes
            .iter()
            .map(|r| r.dropped.load(Ordering::Relaxed))
            .sum();
        self.notice_at = [None; 3];
        (!self.notices.is_empty())
            .then(|| TransportEvent::Notice(std::mem::take(&mut self.notices).join(" ")))
    }

    /// A non-error reason not to play (shown as a status message, state unchanged).
    fn notice(&mut self, msg: &str) -> TransportEvent {
        TransportEvent::Unavailable(msg.to_owned())
    }

    /// Pauses, keeping the position that was last heard.
    pub fn pause(&mut self) {
        self.action_changed();
        if !self.state.is_running() {
            self.play_when_ready = false;
            if self.state == TransportState::Seeking {
                self.state = TransportState::Paused;
            }
            return;
        }
        self.position = self.last_playhead;
        if let Some(e) = &self.engine {
            e.send(Command::Pause);
        }
        self.paused_at = Some(self.position);
        self.next_generation();
        self.state = TransportState::Paused;
        self.idle_since = Some(Instant::now());
        if self.policy.anchor && self.policy.pause_rewinds {
            self.position = self.anchor;
            self.last_playhead = self.anchor;
            self.paused_at = None;
        }
    }

    /// Stop (B1-0080): ends playback and puts the playback playhead back on
    /// the anchor — the Edit playhead, or where this playback started. The
    /// anchor itself never moves.
    pub fn stop(&mut self) {
        self.action_changed();
        self.pause();
        self.play_when_ready = false;
        self.position = self.anchor;
        self.last_playhead = self.anchor;
        self.paused_at = None;
        if matches!(self.state, TransportState::Paused | TransportState::Seeking) {
            self.state = TransportState::Stopped;
        }
    }

    /// B1-0103 (Change PPQN, Preserve beats): every tick the transport
    /// holds follows the rescaled project. Called while stopped.
    pub fn rescale_ticks(&mut self, f: impl Fn(u64) -> u64) {
        self.action_changed();
        self.position = f(self.position);
        self.last_playhead = f(self.last_playhead);
        self.paused_at = self.paused_at.map(&f);
        self.anchor = f(self.anchor);
    }

    /// The Edit / Anchor playhead while it is on (drawn by every view).
    pub fn anchor(&self) -> Option<u64> {
        self.policy.anchor.then_some(self.anchor)
    }

    #[cfg(test)]
    pub fn playhead_policy(&self) -> PlayheadPolicy {
        self.policy
    }

    /// Turning the anchor on places it at the current position.
    pub fn set_playhead_policy(&mut self, policy: PlayheadPolicy) {
        if self.policy != policy {
            self.action_changed();
        }
        if policy.anchor && !self.policy.anchor {
            self.anchor = if self.state.is_running() {
                self.position
            } else {
                self.position()
            };
        }
        self.policy = policy;
    }

    pub fn toggle(&mut self, doc: &PlumDocument, doc_generation: u64) -> Option<TransportEvent> {
        if self.state.is_running() || self.play_when_ready {
            self.pause();
            None
        } else {
            self.play(doc, doc_generation)
        }
    }

    /// Moves the transport. While playing, audio continues from `tick`.
    pub fn seek(
        &mut self,
        tick: u64,
        doc: &PlumDocument,
        doc_generation: u64,
    ) -> Option<TransportEvent> {
        self.action_changed();
        self.position = tick;
        self.last_playhead = tick;
        self.paused_at = None;
        self.anchor = tick; // an explicit placement moves the anchor
        if self.state.is_running() && !self.play_when_ready {
            return self.play(doc, doc_generation);
        }
        None
    }

    /// Re-strike held notes on a resume after a pause (Audio Settings).
    pub fn set_resume_restrike(&mut self, on: bool) {
        self.resume_restrike = on;
    }

    /// Where tracks set to Internal play (Audio Settings / transport chooser).
    /// While playing, the switch happens at once: the old output's notes end
    /// (stuck-note safety) and the new one opens.
    pub fn set_default_output(&mut self, target: OutputTarget) {
        if self.default_output == target {
            return;
        }
        self.action_changed();
        self.default_output = target;
        self.default_error = None;
        self.routes_key = self.routes_key.wrapping_add(1);
    }

    /// The default output's provider, device and health (transport and
    /// diagnostics).
    pub fn output_status(&self) -> OutputStatus {
        let target = self.default_output.clone();
        let route = self.routes.iter().find(|r| r.target == target);
        let health = if target == OutputTarget::Internal {
            OutputHealth::Internal
        } else if let Some(r) = route {
            if let Some(e) = r.open_error() {
                OutputHealth::Unavailable(e)
            } else if r.is_connecting() {
                OutputHealth::Connecting
            } else if r.has_failed() {
                OutputHealth::Failed(format!(
                    "“{}” stopped accepting messages (unplugged?).",
                    r.name
                ))
            } else {
                OutputHealth::Open
            }
        } else {
            self.default_error.clone().unwrap_or(OutputHealth::Idle)
        };
        OutputStatus {
            target,
            health,
            dropped: route.map_or(0, |r| r.dropped.load(Ordering::Relaxed)),
            sent: route.map_or(0, |r| r.sent.load(Ordering::Relaxed)),
            max_lag_samples: route.map_or(0, |r| r.max_lag.load(Ordering::Relaxed)),
            waiting: route.map_or(0, |r| r.pending_len()),
        }
    }

    /// Whether the next Play is a resume at the paused tick (tests).
    #[cfg(test)]
    pub fn resumes(&self) -> bool {
        self.paused_at == Some(self.position)
    }

    /// Stops and forgets per-document caches (a different project was opened or
    /// created). Keeps the device and SoundFont.
    pub fn reset_for_new_document(&mut self) {
        self.action_changed();
        self.audition_all_off();
        if let Some(e) = &self.engine {
            e.send(Command::Pause);
        }
        self.next_generation();
        self.state = TransportState::Stopped;
        self.idle_since = Some(Instant::now());
        self.position = 0;
        self.last_playhead = 0;
        self.paused_at = None;
        self.anchor = 0;
        self.controls = None;
        self.tempo = None;
        self.playing_key = None;
        self.play_when_ready = false;
        self.looping = None;
        self.eligible_sent = None;
        self.open_failed.clear();
        if !self.routes.is_empty() {
            self.routes.clear();
            self.routes_key = self.routes_key.wrapping_add(1);
            if let Some(e) = &self.engine {
                e.send(Command::SetRoutes(Vec::new()));
            }
        }
    }

    /// Close Project: detach UI-side playback data immediately; free the
    /// worker scheduler and confirm it from the existing project reaper.
    pub fn project_release(&mut self) -> impl FnOnce() + Send + 'static {
        let data = (
            self.controls.take(),
            self.tempo.take(),
            self.reference.take(),
        );
        if let Some(e) = &self.engine {
            e.send(Command::SetReference(None));
        }
        self.reset_for_new_document();
        let work = self
            .engine
            .as_ref()
            .map(|e| e.project_release(self.generation));
        move || {
            if let Some(work) = work {
                work();
            }
            drop(data);
        }
    }

    /// Per-frame update: SoundFont loading, device status, end of song, live edits
    /// and the playhead. Returns events for the status bar.
    pub fn update(
        &mut self,
        doc: Option<&PlumDocument>,
        doc_generation: u64,
    ) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        self.retired.retain(|h| !h.is_finished());
        // SoundFont loader.
        if let Some(l) = &self.loader {
            match l.rx.try_recv() {
                Ok(result) => {
                    let mut l = self.loader.take().expect("checked");
                    if let Some(h) = l.handle.take() {
                        let _ = h.join();
                    }
                    match result {
                        Ok(sf) => {
                            log::info!(
                                "SoundFont loaded: {} ({} presets, {} MB of samples)",
                                sf.path.display(),
                                sf.presets,
                                sf.sample_bytes / (1024 * 1024)
                            );
                            events.push(TransportEvent::SoundFontLoaded {
                                name: sf.file_name(),
                                path: sf.path.clone(),
                                bytes: sf.sample_bytes,
                                elapsed: l.started.elapsed(),
                            });
                            if let Some(e) = &self.engine {
                                e.send(Command::SetSoundFont(Arc::clone(&sf.font)));
                            }
                            // The worker replaces the audition synthesizer too:
                            // nothing auditioned keeps sounding.
                            self.auditions_held.clear();
                            self.audition_sent = false;
                            // The previous SoundFont is released once the worker swaps.
                            self.soundfont = Some(sf);
                            if self.state == TransportState::AudioUnavailable {
                                self.state = TransportState::Stopped;
                                self.unavailable = None;
                            }
                            if self.play_when_ready {
                                self.play_when_ready = false;
                                self.state = TransportState::Stopped;
                                if let Some(d) = doc
                                    && let Some(ev) = self.start(d, doc_generation)
                                {
                                    events.push(ev);
                                }
                            }
                        }
                        Err(e) => {
                            let message = e.user_message();
                            log::warn!("SoundFont {} failed: {message}", l.path.display());
                            events.push(TransportEvent::SoundFontFailed {
                                path: l.path.clone(),
                                message: message.clone(),
                                elapsed: l.started.elapsed(),
                            });
                            if self.play_when_ready {
                                let ev = self.set_unavailable(message);
                                events.push(ev);
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.loader = None;
                    events.push(
                        self.set_unavailable("The SoundFont loader stopped unexpectedly.".into()),
                    );
                }
            }
        }
        let Some(engine) = &self.engine else {
            return events;
        };
        // Device failure (unplugged, driver reset).
        if engine.shared.device_failed.load(Ordering::Acquire) {
            let detail = engine
                .shared
                .device_error
                .lock()
                .ok()
                .and_then(|e| e.clone())
                .unwrap_or_default();
            if self.state.is_running() {
                self.position = self.last_playhead;
            }
            self.engine = None;
            events.push(self.set_unavailable(format!(
                "The audio device stopped ({detail}). Press Play to try again."
            )));
            return events;
        }
        let worker_error = engine
            .shared
            .worker_error
            .lock()
            .ok()
            .and_then(|mut e| e.take());
        if let Some(msg) = worker_error {
            events.push(self.set_unavailable(msg));
            return events;
        }
        if !self.state.is_running() {
            if self.auditions_held.is_empty()
                && self
                    .idle_since
                    .is_some_and(|t| t.elapsed() >= IDLE_STREAM_PAUSE)
            {
                self.idle_since = None;
                if let Some(e) = self.engine.as_mut() {
                    let _ = e.set_active(false);
                }
            }
            return events;
        }
        let Some(doc) = doc else {
            return events;
        };
        let engine = self.engine.as_ref().expect("checked above");
        // An external output stopped working (unplugged), or (L-B06) could
        // not be opened after all.
        let failed: Vec<Arc<ExternalRoute>> = self
            .routes
            .iter()
            .filter(|r| r.has_failed())
            .cloned()
            .collect();
        if !failed.is_empty() {
            for r in &failed {
                let is_default = r.target == self.default_output;
                let name = &r.name;
                if let Some(e) = r.open_error() {
                    if is_default {
                        self.default_error = Some(OutputHealth::Unavailable(e.clone()));
                    }
                    self.open_failed.push(r.target.clone());
                    let whose = if is_default {
                        "Tracks set to Internal play"
                    } else {
                        "Its tracks play"
                    };
                    events.push(TransportEvent::Notice(format!(
                        "{e} {whose} on the internal synthesizer instead."
                    )));
                } else {
                    if is_default {
                        self.default_error = Some(OutputHealth::Failed(format!(
                            "“{name}” stopped accepting messages (unplugged?)."
                        )));
                    }
                    events.push(TransportEvent::Notice(format!(
                        "The MIDI output “{name}” stopped working; its tracks now play on the internal synthesizer. The project is unchanged."
                    )));
                }
            }
            self.routes.retain(|r| !r.has_failed());
            self.routes_key = self.routes_key.wrapping_add(1);
            engine.send(Command::SetRoutes(self.routes.clone()));
        }
        // Notes skipped because too many start at once (very high speed or density).
        let skipped = engine.shared.skipped_notes.load(Ordering::Relaxed);
        if skipped > self.skipped_reported.saturating_add(999) && notice_due(&mut self.notice_at[0])
        {
            self.skipped_reported = skipped;
            events.push(TransportEvent::Notice(format!(
                "Playback is dropping notes: more start than can be rendered in real time ({} so far{}). Pre-Render or a larger note-on budget (Playback → Audio Settings) helps. The project is unchanged.",
                plumspirit_engine::midi::group_digits(skipped),
                if self.speed.is_normal() {
                    String::new()
                } else {
                    format!(" at {}", self.speed)
                }
            )));
        }
        // L-B03: an external output that could not keep up dropped note-ons
        // (Realtime's "keep time" policy) — said at the first and then once
        // per 1,000 more, with the way to keep every event instead.
        let dropped: u64 = self
            .routes
            .iter()
            .map(|r| r.dropped.load(Ordering::Relaxed))
            .sum();
        if dropped > self.external_dropped_reported && notice_due(&mut self.notice_at[1]) {
            self.external_dropped_reported = dropped.saturating_add(999);
            events.push(TransportEvent::Notice(format!(
                "A MIDI output could not keep up: {} note-ons were not sent so far (the project is unchanged). Playback → Audio Settings → MIDI output → Keep every event makes playback wait for the output instead.",
                plumspirit_engine::midi::group_digits(dropped)
            )));
        }
        let clipped = engine.shared.clipped.load(Ordering::Relaxed);
        if clipped > self.clipped_reported.saturating_add(4_799)
            && notice_due(&mut self.notice_at[2])
        {
            self.clipped_reported = clipped;
            events.push(TransportEvent::Notice(format!(
                "The output is clipping ({} samples beyond full scale). Lower the master output gain or turn on the safety limiter in Playback → Audio Settings.",
                plumspirit_engine::midi::group_digits(clipped)
            )));
        }
        // End of song for the current generation.
        if engine.shared.ended_generation.load(Ordering::Acquire) == self.generation {
            self.position = doc.duration_ticks();
            self.last_playhead = self.position;
            self.state = TransportState::Stopped;
            self.idle_since = Some(Instant::now());
            events.push(TransportEvent::Ended);
            return events;
        }
        let sr = engine.sample_rate;
        // Live edit: switch the worker to the edited notes (immutable snapshots, so
        // the audio never reads a half-applied edit).
        let key = DocKey::of(doc, doc_generation, self.speed, self.routes_key);
        if self.playing_key != Some(key) {
            self.prepare_routes(doc);
            events.extend(
                (!self.notices.is_empty())
                    .then(|| TransportEvent::Notice(std::mem::take(&mut self.notices).join(" "))),
            );
            let index = self.build_index(doc, doc_generation, sr);
            let generation = self.next_generation();
            if let Some(e) = &self.engine {
                e.send(Command::Update { index, generation });
            }
            self.playing_key = Some(DocKey::of(doc, doc_generation, self.speed, self.routes_key));
        } else {
            // Mute / Solo changed: tell the scheduler without rebuilding anything.
            let now: Arc<[bool]> = eligibility(doc).into();
            if self.eligible_sent.as_ref().is_none_or(|e| **e != *now) {
                if let Some(e) = &self.engine {
                    e.send(Command::SetEligible(Arc::clone(&now)));
                }
                self.eligible_sent = Some(now);
            }
        }
        // Playhead from the frames the device received.
        let tempo = self.tempo_map(doc, doc_generation, sr);
        if let Some(frame) = self
            .engine
            .as_ref()
            .and_then(|e| e.shared.heard_frame(self.generation))
        {
            self.last_playhead = tempo.tick_at_frame(frame).min(doc.duration_ticks());
            self.state = TransportState::Playing;
        }
        events
    }

    /// Time until the idle device stream should be paused (the UI schedules one more
    /// update then), if a pause is pending.
    pub fn idle_pause_due_in(&self) -> Option<Duration> {
        let t = self.idle_since?;
        self.engine.as_ref()?;
        Some(IDLE_STREAM_PAUSE.saturating_sub(t.elapsed()))
    }

    /// Diagnostics for the developer overlay (two lines).
    pub fn diagnostics(&self) -> Option<[String; 2]> {
        let e = self.engine.as_ref()?;
        let s = &e.shared;
        let block_us = s.block_nanos.load(Ordering::Relaxed) as f64 / 1000.0;
        let budget_us = engine::BLOCK as f64 / f64::from(e.sample_rate) * 1e6;
        Some([
            format!(
                "audio: {} · {} Hz × {} ch · {} frames per callback",
                e.device_name,
                e.sample_rate,
                e.channels,
                s.callback_frames.load(Ordering::Relaxed)
            ),
            format!(
                "synth {:.0}% of real time · underruns {} · sounding {} · skipped {}",
                block_us / budget_us * 100.0,
                s.underruns.load(Ordering::Relaxed),
                s.active_voices.load(Ordering::Relaxed),
                s.skipped_notes.load(Ordering::Relaxed),
            ),
        ])
    }

    /// Pre-Render: whether the start buffer is full (`true` in Realtime;
    /// `None` without an audio engine). Two atomic loads, no allocation.
    pub fn prerolled(&self) -> Option<bool> {
        Some(
            self.engine
                .as_ref()?
                .shared
                .prerolled
                .load(Ordering::Acquire),
        )
    }

    /// The audio device's callback period (`None` before the first Play):
    /// the position moves once per period (v0.9 V09-S17: the MIDI Player
    /// extrapolates between them, `docs/PLAYER.md` P9).
    pub fn period(&self) -> Option<Duration> {
        let e = self.engine.as_ref()?;
        let frames = e.shared.callback_frames.load(Ordering::Relaxed);
        (frames > 0 && e.sample_rate > 0)
            .then(|| Duration::from_secs_f64(f64::from(frames) / f64::from(e.sample_rate)))
    }

    /// Live audio diagnostics (`None` before the first Play).
    pub fn audio_diagnostics(&self) -> Option<AudioDiagnostics> {
        let e = self.engine.as_ref()?;
        let s = &e.shared;
        let block_us = s.block_nanos.load(Ordering::Relaxed) as f64 / 1000.0;
        let budget_us = engine::BLOCK as f64 / f64::from(e.sample_rate) * 1e6;
        let preroll = (!s.prerolled.load(Ordering::Relaxed)).then(|| {
            let need = s.preroll.load(Ordering::Relaxed).max(1) as f32;
            ((s.ring.available() / 2) as f32 / need).min(1.0)
        });
        Some(AudioDiagnostics {
            device: e.device_name.clone(),
            sample_rate: e.sample_rate,
            channels: e.channels,
            callback_frames: s.callback_frames.load(Ordering::Relaxed),
            buffer: e.buffer,
            ring_frames: e.ring_frames,
            buffered: s.ring.available() / 2,
            preroll,
            synth_load: block_us / budget_us,
            underruns: s.underruns.load(Ordering::Relaxed),
            sounding: s.active_voices.load(Ordering::Relaxed),
            capacity: s.voice_capacity.load(Ordering::Relaxed),
            overflow_blocks: s.overflow_blocks.load(Ordering::Relaxed),
            at_capacity: s.at_capacity.load(Ordering::Relaxed),
            not_started: s.not_started.load(Ordering::Relaxed),
            released_oldest: s.released_oldest.load(Ordering::Relaxed),
            skipped: s.skipped_notes.load(Ordering::Relaxed),
            deferred: s.deferred_notes.load(Ordering::Relaxed),
            late_dropped: s.late_dropped.load(Ordering::Relaxed),
            max_lateness_ms: s.max_lateness.load(Ordering::Relaxed) as f64 * 1000.0
                / f64::from(e.sample_rate),
            peak_pre: engine::take_peak(&s.peak_pre),
            peak_post: engine::take_peak(&s.peak_post),
            clipped: s.clipped.load(Ordering::Relaxed),
            limited: s.limited.load(Ordering::Relaxed),
            filtered_keys: s.filtered_keys.load(Ordering::Relaxed),
            filtered_velocity: s.filtered_velocity.load(Ordering::Relaxed),
            resets_sent: s.resets_sent.load(Ordering::Relaxed),
            resets_ignored: s.resets_ignored.load(Ordering::Relaxed),
            retriggered: s.retriggered.load(Ordering::Relaxed),
            notes: e.notes.clone(),
        })
    }

    /// A transport wired to an already running (test) engine and SoundFont.
    #[cfg(test)]
    pub(crate) fn for_test(engine: AudioEngine, soundfont: LoadedSoundFont) -> Transport {
        engine.send(Command::SetSoundFont(Arc::clone(&soundfont.font)));
        Transport {
            engine: Some(engine),
            soundfont: Some(soundfont),
            ..Transport::default()
        }
    }

    /// Stops audio and joins every thread (application exit).
    /// At exit: the engine stops (every voice and external note off, its
    /// worker joined — it answers between blocks). A SoundFont still
    /// loading is left to finish on its own (B1-0170: never joined, a
    /// 30 MB SoundFont must not hold the window open).
    pub fn shutdown(&mut self) {
        self.engine = None;
        if let Some(mut l) = self.loader.take() {
            drop(l.handle.take());
        }
        self.retired.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::engine::render_offline;
    use super::soundfont::tests::tiny_sf2;
    use plumspirit_engine::model::tempo::Speed;
    use plumspirit_engine::model::{
        Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TempoMap,
    };
    use plumspirit_engine::playback::PlaybackIndex;
    use std::sync::Arc;

    fn font() -> Arc<rustysynth::SoundFont> {
        let bytes = tiny_sf2();
        Arc::new(rustysynth::SoundFont::new(&mut &bytes[..]).expect("tiny sf2"))
    }

    /// L-S46: an overload notice is given at once, then at most once a
    /// second however often the counts grow (Windows, 1.8M notes / s on
    /// KDMAPI: 149 notices in 7 s before).
    #[test]
    fn overload_notices_come_at_once_then_at_most_once_a_second() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut last = None;
        assert!(super::notice_due_at(&mut last, t0), "the first at once");
        let given = (1..=200u64)
            .filter(|ms| super::notice_due_at(&mut last, t0 + Duration::from_millis(ms * 17)))
            .count();
        // 200 frames of 17 ms = 3.4 s: one more each whole second.
        assert_eq!(given, 3);
    }

    /// v1.0-beta2 B2-A02: the key that tells the running audio to switch to
    /// the edited document follows the clips — a moved clip, a new clip and
    /// a note added inside an owned clip's content each change it (before,
    /// only the tracks' own notes counted: playback kept the old placement).
    #[test]
    fn the_live_edit_key_follows_a_clip_move_and_an_owned_clip_edit() {
        use plumspirit_engine::editor::clips::{add_clip, move_clips};
        use plumspirit_engine::history::History;
        use plumspirit_engine::model::{ClipId, ContentRef, SubchannelId};
        let mut d = doc(&[(0, 480, 60, 100)]);
        let key = |d: &PlumDocument| super::DocKey::of(d, 1, Speed::NORMAL, 0);
        let k0 = key(&d);
        let id = ClipId::Primary(d.track(0).unwrap().id());
        let op = move_clips(&d, &[(id, 0, 960)]).unwrap();
        History::default().apply(&mut d, op).unwrap();
        let k1 = key(&d);
        assert_ne!(k0, k1, "a moved clip reaches the running audio");
        let op = add_clip(&d, 0, 4_000, 960).unwrap();
        History::default().apply(&mut d, op).unwrap();
        let k2 = key(&d);
        assert_ne!(k1, k2, "a new clip reaches the running audio");
        let c = d
            .clips()
            .contents()
            .last()
            .expect("the new clip's content")
            .id;
        let rec = NoteRecord {
            start_tick: 10,
            duration_ticks: 100,
            attrs: NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        };
        d.notes_of_mut(ContentRef::Owned(c))
            .unwrap()
            .insert_sorted_batch(&[rec], false)
            .unwrap();
        assert_ne!(
            k2,
            key(&d),
            "a note added inside a clip reaches the running audio"
        );
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    fn doc(notes: &[(u64, u64, u8, u8)]) -> PlumDocument {
        // 120 BPM, 480 PPQN: at 48 kHz one tick = 50 frames.
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        let recs: Vec<NoteRecord> = notes
            .iter()
            .map(|&(s, dur, p, v)| NoteRecord {
                start_tick: s,
                duration_ticks: dur,
                attrs: NoteAttrs::new(p, v, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    #[test]
    fn rendered_audio_follows_the_notes() {
        let f = font();
        // One note from tick 480 (0.5 s) to 960 (1.0 s).
        let d = doc(&[(480, 480, 69, 127)]);
        let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
        let out = render_offline(&f, idx, 0, 48_000, 96_000);
        let sec = |a: f64, b: f64| &out[(a * 96_000.0) as usize..(b * 96_000.0) as usize];
        assert!(rms(sec(0.0, 0.45)) < 1e-4, "silent before the note");
        assert!(rms(sec(0.55, 0.95)) > 0.01, "audible during the note");
        assert!(
            rms(sec(1.9, 2.0)) < rms(sec(0.55, 0.95)) / 10.0,
            "released after the note"
        );
    }

    #[test]
    fn velocity_changes_loudness_and_seek_restores() {
        let f = font();
        let loud = doc(&[(0, 960, 69, 127)]);
        let soft = doc(&[(0, 960, 69, 20)]);
        let a = render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&loud, 48_000)),
            0,
            48_000,
            24_000,
        );
        let b = render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&soft, 48_000)),
            0,
            48_000,
            24_000,
        );
        assert!(rms(&a[4800..]) > rms(&b[4800..]) * 2.0);
        // Seeking into the middle of the note re-strikes it (no silence), and
        // seeking past its end is silent (no stuck note).
        let mid = render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&loud, 48_000)),
            480,
            48_000,
            12_000,
        );
        assert!(rms(&mid[2400..]) > 0.01);
        let after = render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&loud, 48_000)),
            1000,
            48_000,
            12_000,
        );
        assert!(rms(&after) < 1e-4);
    }

    // ---- Transport + worker + ring with a headless device --------------------------

    use super::engine::AudioEngine;
    use super::soundfont::LoadedSoundFont;
    use super::{Transport, TransportEvent, TransportState};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    fn loaded(font: Arc<rustysynth::SoundFont>, name: &str) -> LoadedSoundFont {
        LoadedSoundFont {
            path: PathBuf::from(name),
            font,
            file_bytes: 0,
            sample_bytes: 0,
            presets: 1,
            name: name.into(),
        }
    }

    struct Rig {
        t: Transport,
        last: (f32, f32),
    }

    impl Rig {
        fn new() -> Rig {
            Rig {
                t: Transport::for_test(AudioEngine::headless(48_000), loaded(font(), "tiny.sf2")),
                last: (0.0, 0.0),
            }
        }
        fn engine(&self) -> &AudioEngine {
            self.t.engine.as_ref().expect("engine")
        }
        /// Lets the "device" take `frames`, then runs the UI-side update.
        fn pump(
            &mut self,
            frames: usize,
            d: &PlumDocument,
            g: u64,
        ) -> (Vec<f32>, Vec<TransportEvent>) {
            let mut last = self.last;
            let out = self.engine().pump(frames, &mut last);
            self.last = last;
            let ev = self.t.update(Some(d), g);
            (out, ev)
        }
        fn play(&mut self, d: &PlumDocument, g: u64) {
            assert_eq!(self.t.play(d, g), None);
            self.engine().wait_playing(true);
        }
        fn pause(&mut self) {
            self.t.pause();
            self.engine().wait_playing(false);
        }
    }

    // ---- Edit / Anchor playhead (v1.0-beta1 B1-0080) ---------------------------------

    #[test]
    fn closing_a_project_releases_audio_data_keeps_the_font_and_protects_new_playback() {
        let d = doc(&[(0, 4_800, 69, 100)]);
        let mut r = Rig::new();
        let font = Arc::downgrade(&r.t.soundfont.as_ref().unwrap().font);
        let audio = quarter_clicks();
        let old_audio = Arc::downgrade(&audio);
        r.t.set_reference(Some(audio), 0.0, 1.0);
        r.play(&d, 1);
        r.pause();
        let release = r.t.project_release();
        assert!(r.t.reference.is_none());
        release();
        assert_eq!(old_audio.strong_count(), 0);
        assert!(font.strong_count() > 0, "SoundFont retained by policy");
        assert_eq!(r.t.state(), TransportState::Stopped);
        r.play(&d, 2);
        let release = r.t.project_release();
        r.play(&d, 3);
        release(); // deliberately delayed until the new generation plays
        assert!(
            r.engine()
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Acquire)
        );
        r.pause();
    }

    #[test]
    fn with_the_anchor_play_starts_there_pause_holds_and_stop_returns() {
        use super::PlayheadPolicy;
        let d = doc(&[(0, 4_800, 69, 100), (4_800, 480, 72, 100)]);
        let mut r = Rig::new();
        r.t.set_playhead_policy(PlayheadPolicy {
            anchor: true,
            pause_rewinds: false,
        });
        assert_eq!(r.t.anchor(), Some(0));
        let _ = r.t.seek(480, &d, 1); // the user places the anchor
        assert_eq!(r.t.anchor(), Some(480));
        r.play(&d, 1);
        let _ = r.pump(12_000, &d, 1); // 240 ticks
        assert_eq!(r.t.position(), 720);
        r.pause();
        assert_eq!(r.t.position(), 720, "Pause holds the playback playhead");
        assert_eq!(r.t.anchor(), Some(480), "the anchor never follows playback");
        r.play(&d, 1); // resume
        let _ = r.pump(4_800, &d, 1);
        assert!(r.t.position() > 720, "resumed in place: {}", r.t.position());
        r.t.stop();
        r.engine().wait_playing(false);
        assert_eq!(r.t.state(), TransportState::Stopped);
        assert_eq!(r.t.position(), 480, "Stop returns to the anchor");
        // Play after Stop starts at the anchor again, also from Stopped at the end.
        r.play(&d, 1);
        let _ = r.pump(4_800, &d, 1);
        assert!((480..=600).contains(&r.t.position()));
        // A speed change keeps the place (an internal restart, not a Play).
        let at = r.t.position();
        let _ = r.t.set_speed(Speed::parse("2").unwrap(), &d, 1);
        assert!(r.t.position() >= at, "no jump back to the anchor");
        assert_eq!(r.t.anchor(), Some(480));
        r.t.stop();
        r.engine().wait_playing(false);
    }

    #[test]
    fn pause_can_rewind_to_the_anchor_and_loops_never_move_it() {
        use super::PlayheadPolicy;
        let d = doc(&[(0, 9_600, 69, 100)]);
        let mut r = Rig::new();
        r.t.set_playhead_policy(PlayheadPolicy {
            anchor: true,
            pause_rewinds: true,
        });
        let _ = r.t.seek(960, &d, 1);
        r.t.set_loop(Some((960, 1_200)));
        r.play(&d, 1);
        let _ = r.pump(24_000, &d, 1); // 480 ticks: wrapped once inside the loop
        let p = r.t.position();
        assert!((960..1_200).contains(&p), "inside the loop: {p}");
        assert_eq!(r.t.anchor(), Some(960));
        r.pause();
        assert_eq!(r.t.position(), 960, "Pause rewinds to the anchor");
        assert!(!r.t.resumes(), "a rewound pause is not a resume");
        // Turning the anchor off keeps v0.9 behaviour: Pause holds, Play resumes.
        r.t.set_loop(None);
        r.t.set_playhead_policy(PlayheadPolicy::default());
        assert_eq!(r.t.anchor(), None);
        r.play(&d, 1);
        let _ = r.pump(12_000, &d, 1);
        r.pause();
        let held = r.t.position();
        assert_eq!(held, 1_200);
        assert!(r.t.resumes());
        // Stop without the anchor: back to where this playback started.
        r.t.stop();
        assert_eq!(r.t.position(), 960);
    }

    // ---- Resume after pause (v1.0-beta1 B1-0082 / B1-0083) --------------------------

    /// Root cause of "pausing and playing replays a held note": a pause ends
    /// every voice and Play started with a seek, which re-strikes the notes
    /// held at that tick. By default a resume at the paused tick no longer
    /// strikes them (silent until their end); the Audio Settings switch
    /// brings the re-strike back; moving the playhead always re-strikes.
    #[test]
    fn a_resume_does_not_strike_a_held_note_again_unless_asked() {
        let mut r = Rig::new();
        r.t.configure_synth(super::synth::SynthConfig {
            effects: false,
            ..super::synth::SynthConfig::default()
        });
        // One long note (20 s at 120 BPM, 480 PPQN) and a later one.
        let d = doc(&[(0, 19_200, 69, 100), (38_400, 480, 72, 100)]);
        r.play(&d, 1);
        let (a, _) = r.pump(8192, &d, 1);
        assert!(rms(&a) > 1e-3, "the note sounds");
        r.pause();
        assert!(r.t.resumes());
        r.play(&d, 1);
        let (_, _) = r.pump(2048, &d, 1);
        let (b, _) = r.pump(8192, &d, 1);
        assert!(
            rms(&b) < 1e-5,
            "resumed: the held note is not struck again ({})",
            rms(&b)
        );
        // Asked for: the held note sounds again on resume.
        r.pause();
        r.t.set_resume_restrike(true);
        r.play(&d, 1);
        let (_, _) = r.pump(2048, &d, 1);
        let (c, _) = r.pump(8192, &d, 1);
        assert!(rms(&c) > 1e-3, "re-struck on request");
        // A seek (even while paused) always sounds the notes held there.
        r.t.set_resume_restrike(false);
        r.pause();
        let here = r.t.position();
        assert!(r.t.seek(here, &d, 1).is_none());
        assert!(!r.t.resumes());
        r.play(&d, 1);
        let (_, _) = r.pump(2048, &d, 1);
        let (e, _) = r.pump(8192, &d, 1);
        assert!(rms(&e) > 1e-3, "late entry after a seek still sounds");
        r.pause();
    }

    /// v1.0-beta1 B1-0129: the internal synthesizer plays velocity 1 far
    /// quieter than 127 (the SoundFont's velocity-to-attenuation curve,
    /// SF2 2.04 default modulator): PlumSpirit passes the velocity through
    /// unchanged (`SynthBank::note_on`, `midi_out` packs it as is). Measured
    /// levels are printed for the evidence.
    #[test]
    fn velocity_one_is_far_quieter_than_full_velocity() {
        let mut level = Vec::new();
        for v in [1u8, 2, 64, 127] {
            let mut r = Rig::new();
            r.t.configure_synth(super::synth::SynthConfig {
                effects: false,
                limiter: false,
                ..super::synth::SynthConfig::default()
            });
            let d = doc(&[(0, 4_800, 69, v)]);
            r.play(&d, 1);
            let (_, _) = r.pump(1024, &d, 1);
            let (a, _) = r.pump(8192, &d, 1);
            r.pause();
            level.push((v, rms(&a)));
        }
        let db = |x: f32| 20.0 * x.max(1e-12).log10();
        let full = level[3].1;
        for (v, l) in &level {
            println!(
                "velocity {v:3}: rms {l:.6} ({:+.1} dB against 127)",
                db(*l) - db(full)
            );
        }
        assert!(full > 1e-3);
        // RustySynth ends a voice whose gain is below its audibility floor,
        // so velocities 1 and 2 are digital silence with this SoundFont.
        assert!(
            level[0].1 <= level[1].1 && level[1].1 < level[2].1 && level[2].1 < full,
            "{level:?}"
        );
        assert!(
            db(full) - db(level[0].1) > 20.0,
            "velocity 1 at least 20 dB below 127: {level:?}"
        );
    }

    // ---- Pre-Render (v0.8 §137 / §231) ----------------------------------------------

    #[test]
    fn live_voice_policy_counts_refusals_and_releases_after_a_seek() {
        use super::synth::{SynthConfig, VoicePolicy};
        let mut r = Rig::new();
        let cfg = SynthConfig {
            max_voices: 64,
            voice_policy: VoicePolicy::KeepSounding,
            effects: false,
            ..SynthConfig::default()
        };
        r.t.configure_synth(cfg);
        let notes: Vec<_> = (0..128).map(|k| (0, 9600, k, 100)).collect();
        let d = doc(&notes);
        r.play(&d, 1);
        r.pump(2048, &d, 1);
        let diag = r.t.audio_diagnostics().unwrap();
        assert_eq!(diag.not_started, 64);
        assert_eq!(diag.released_oldest, 0);
        let shared = Arc::clone(&r.engine().shared);
        r.pause();
        r.t.configure_synth(SynthConfig {
            voice_policy: VoicePolicy::Oldest,
            ..cfg
        });
        r.t.seek(0, &d, 1);
        r.play(&d, 1);
        r.pump(2048, &d, 1);
        let diag = r.t.audio_diagnostics().unwrap();
        assert!(diag.released_oldest >= 64);
        assert!(Arc::ptr_eq(&shared, &r.engine().shared));
        r.pause();
    }

    #[test]
    fn voice_policy_pre_render_matches_realtime_under_pressure_and_loop_seeks() {
        use super::synth::{SynthConfig, VoicePolicy};
        let notes: Vec<_> = (0..128).map(|k| (0, 960, k, 100)).collect();
        let d = doc(&notes);
        for policy in VoicePolicy::ALL {
            let cfg = SynthConfig {
                max_voices: 64,
                voice_policy: policy,
                effects: false,
                ..SynthConfig::default()
            };
            let mut rt = Rig::new();
            let mut pr = prerender_rig(1000);
            for r in [&mut rt, &mut pr] {
                r.t.configure_synth(cfg);
            }
            rt.play(&d, 1);
            let (a, _) = rt.pump(24000, &d, 1);
            pr.play(&d, 1);
            wait_prerolled(&pr);
            let (b, _) = pr.pump(24000, &d, 1);
            assert!(
                a == b,
                "{policy:?}: pressure first mismatch {:?}",
                a.iter().zip(&b).position(|(a, b)| a != b)
            );
            for r in [&mut rt, &mut pr] {
                r.pause();
                r.t.seek(120, &d, 1);
                r.t.set_loop(Some((0, 480)));
            }
            rt.play(&d, 1);
            let (a, _) = rt.pump(48000, &d, 1);
            pr.play(&d, 1);
            wait_prerolled(&pr);
            let (b, _) = pr.pump(48000, &d, 1);
            assert!(
                a == b,
                "{policy:?}: seek and loop first mismatch {:?}",
                a.iter().zip(&b).position(|(a, b)| a != b)
            );
        }
    }

    #[test]
    fn live_audio_settings_change_gain_capacity_limiter_and_scheduler_budget() {
        use super::synth::{LIMITER_THRESHOLD, SynthConfig, db_to_gain};
        let mut r = Rig::new();
        let base = SynthConfig {
            max_voices: 2048,
            effects: false,
            gain_db: 0.0,
            limiter: false,
            ..SynthConfig::default()
        };
        r.t.configure_synth(base);
        let d = doc(&[(0, 9600, 69, 100)]);
        r.play(&d, 1);
        let shared = Arc::clone(&r.engine().shared);
        let (a, _) = r.pump(8192, &d, 1);
        assert!(rms(&a) > 1e-3 && a.iter().all(|v| v.abs() < 1.0));
        r.pause();
        assert!(r.t.seek(0, &d, 1).is_none());
        let cfg = SynthConfig {
            max_voices: 3000,
            audition_voices: 128,
            gain_db: -6.0,
            ..base
        };
        r.t.configure_synth(cfg);
        r.play(&d, 1);
        let (b, _) = r.pump(8192, &d, 1);
        assert!(
            Arc::ptr_eq(&shared, &r.engine().shared),
            "synthesis settings keep the device engine"
        );
        assert_eq!(shared.voice_capacity.load(Ordering::Relaxed), 3000);
        assert!(
            a.iter()
                .zip(&b)
                .all(|(a, b)| (b - a * db_to_gain(-6.0)).abs() < 1e-6),
            "the running master bus must actually apply the requested gain"
        );

        let burst = super::profile::bursts(1024, 2, 240);
        for (budget, expect_deferral) in [(0, true), (16_384, false)] {
            r.pause();
            assert!(r.t.seek(0, &burst, 2).is_none());
            r.t.configure_synth(SynthConfig {
                gain_db: 12.0,
                limiter: true,
                note_ons_per_block: budget,
                ..cfg
            });
            r.play(&burst, 2);
            let (out, _) = r.pump(8192, &burst, 2);
            assert!(out.iter().any(|v| v.abs() > 0.1));
            assert!(
                out.iter()
                    .all(|v| v.is_finite() && v.abs() <= LIMITER_THRESHOLD + 1e-6)
            );
            assert_eq!(
                shared.deferred_notes.load(Ordering::Relaxed) > 0,
                expect_deferral
            );
            assert_eq!(shared.skipped_notes.load(Ordering::Relaxed), 0);
            assert_eq!(shared.clipped.load(Ordering::Relaxed), 0);
            assert!(
                f32::from_bits(shared.peak_pre.load(Ordering::Relaxed)) > 1.0,
                "overload must reach the real limiter rather than a fake setting"
            );
            assert!(
                f32::from_bits(shared.peak_post.load(Ordering::Relaxed))
                    <= LIMITER_THRESHOLD + 1e-6
            );
        }
    }

    #[test]
    fn audio_output_and_render_changes_keep_or_release_the_engine_at_safe_boundaries() {
        let d = doc(&[(0, 9600, 69, 100)]);
        let mut r = prerender_rig(1000);
        r.play(&d, 1);
        wait_prerolled(&r);
        r.pump(4096, &d, 1);
        let shared = Arc::clone(&r.engine().shared);
        r.t.configure_render(false, 1000);
        assert!(!r.t.engine_stale(r.engine()));
        assert!(Arc::ptr_eq(&shared, &r.engine().shared));
        r.pause();
        assert!(r.t.seek(0, &d, 1).is_none());
        r.play(&d, 1);
        assert!(rms(&r.pump(4096, &d, 1).0) > 1e-3);

        let output = super::output::OutputConfig {
            device: Some("headless fixture request".into()),
            sample_rate: Some(44_100),
            buffer_frames: Some(512),
        };
        r.t.configure_output(output.clone());
        assert_eq!(r.t.output, output);
        assert!(r.t.engine_stale(r.engine()));
        assert!(
            Arc::ptr_eq(&shared, &r.engine().shared),
            "running output is deferred to next Play"
        );
        r.pause();
        r.t.configure_output(output);
        assert!(
            r.t.engine.is_none(),
            "stopped stale output releases worker and helpers"
        );

        let mut r = Rig::new();
        r.play(&d, 1);
        r.pump(4096, &d, 1);
        let shared = Arc::clone(&r.engine().shared);
        r.t.configure_render(true, 1000);
        assert!(r.t.engine_stale(r.engine()));
        assert!(
            Arc::ptr_eq(&shared, &r.engine().shared),
            "larger buffer is deferred while playing"
        );
        r.pause();
        r.t.configure_render(true, 1000);
        assert!(
            r.t.engine.is_none(),
            "stopped larger buffer releases the old bounded ring"
        );
    }

    fn prerender_rig(ahead_ms: u32) -> Rig {
        use super::{engine, synth};
        let render = engine::RenderConfig::new(true, ahead_ms, 48_000);
        let mut t = Transport::for_test(
            AudioEngine::headless_render(48_000, synth::SynthConfig::default(), render),
            loaded(font(), "tiny.sf2"),
        );
        t.render = (true, ahead_ms);
        Rig {
            t,
            last: (0.0, 0.0),
        }
    }

    fn wait_prerolled(r: &Rig) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !r.engine().shared.prerolled.load(Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            // The device callback skips flushed samples even while waiting
            // for pre-roll. This headless consumer must do the same: stale
            // audio can otherwise look like a full buffer after a restart.
            r.engine().shared.ring.skip_stale();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let sh = &r.engine().shared;
        assert!(
            sh.prerolled.load(Ordering::Acquire),
            "pre-roll: buffered {} frames, target {}, playing {}",
            sh.ring.available() / 2,
            sh.preroll.load(Ordering::Acquire),
            sh.playing.load(Ordering::Acquire)
        );
    }

    #[test]
    fn pre_render_refills_after_restarting_an_unconsumed_buffer() {
        let d = doc(&[(0, 9_600, 69, 100)]);
        let mut r = prerender_rig(1_000);
        r.play(&d, 1);
        wait_prerolled(&r);
        assert!(r.engine().shared.ring.available() / 2 >= 48_000);
        // Restart with a full, unconsumed buffer. Its samples are flushed;
        // the stand-in callback must skip them before waiting for new ones.
        r.pause();
        r.t.seek(0, &d, 1);
        r.play(&d, 1);
        wait_prerolled(&r);
        assert!(r.engine().shared.ring.available() / 2 >= 48_000);
    }

    /// §231: Pre-Render plays exactly the audio Realtime plays (deterministic
    /// synthesis, same output stage) once its start buffer is filled — from
    /// the start, after a seek, and across loop wraps; the playhead does not
    /// move while the buffer fills, and the buffer is bounded.
    #[test]
    fn pre_render_matches_realtime_from_the_start_after_seeks_and_in_loops() {
        let notes: Vec<(u64, u64, u8, u8)> = (0..64)
            .map(|i| {
                (
                    i * 120,
                    300 + (i % 5) * 40,
                    48 + (i % 24) as u8,
                    60 + (i % 60) as u8,
                )
            })
            .collect();
        let d = doc(&notes);
        // From the start.
        let mut rt = Rig::new();
        rt.play(&d, 1);
        let (a, _) = rt.pump(96_000, &d, 1);
        let mut pr = prerender_rig(3_000);
        assert!(pr.engine().ring_frames >= 3 * 48_000);
        pr.play(&d, 1);
        // While filling, nothing is consumed: the playhead stays at the start.
        assert_eq!(
            pr.engine().shared.heard_frame(pr.t.generation).unwrap_or(0),
            0
        );
        wait_prerolled(&pr);
        assert!(pr.engine().shared.ring.available() / 2 >= 3 * 48_000 - 64);
        let (b, _) = pr.pump(96_000, &d, 1);
        assert!(rms(&a) > 1e-3);
        assert_eq!(a, b, "same samples as Realtime");
        // After a seek (pause, seek, play): buffered again, same audio.
        for r in [&mut rt, &mut pr] {
            r.pause();
            assert!(r.t.seek(2_000, &d, 1).is_none());
        }
        rt.play(&d, 1);
        let (a, _) = rt.pump(48_000, &d, 1);
        pr.play(&d, 1);
        wait_prerolled(&pr);
        let (b, _) = pr.pump(48_000, &d, 1);
        assert_eq!(a, b, "same audio after a seek");
        // Loops: prepared audio wraps exactly like Realtime.
        for r in [&mut rt, &mut pr] {
            r.pause();
            assert!(r.t.seek(0, &d, 1).is_none());
            r.t.set_loop(Some((0, 960)));
        }
        rt.play(&d, 1);
        let (a, _) = rt.pump(144_000, &d, 1);
        pr.play(&d, 1);
        wait_prerolled(&pr);
        let (b, _) = pr.pump(144_000, &d, 1);
        assert_eq!(a, b, "same audio across loop wraps");
        assert!(pr.engine().shared.loop_wraps.load(Ordering::Relaxed) >= 2);
    }

    /// The Pre-Render buffer is bounded by its setting (memory plan, §215).
    #[test]
    fn pre_render_buffers_are_bounded_by_the_setting() {
        use super::engine;
        for ms in [0u32, 1_000, 8_000, 60_000, 10_000_000] {
            let r = engine::RenderConfig::new(true, ms, 48_000);
            let clamped = ms.clamp(engine::MIN_AHEAD_MS, engine::MAX_AHEAD_MS);
            assert_eq!(r.ahead_frames as u64, u64::from(clamped) * 48);
            // At most 60 s × 48 kHz × 2 × 4 bytes, rounded up to a power of two.
            assert!(r.buffer_bytes() <= (60 * 48_000 + 16_384usize).next_power_of_two() * 8);
        }
        let rt = engine::RenderConfig::new(false, 60_000, 48_000);
        assert_eq!(rt.ring_frames(), engine::RING_CAPACITY_FRAMES);
    }

    // ---- Audio Reference (v0.8) ----------------------------------------------------

    /// Left-channel frames of the clicks in `out`.
    fn click_frames(out: &[f32]) -> Vec<usize> {
        // the loudest frame of each run above 0.2 (a click between two
        // samples spreads over both)
        let mut v: Vec<usize> = Vec::new();
        let mut run: Option<(usize, f32)> = None;
        for (i, f) in out.chunks_exact(2).enumerate() {
            if f[0] > 0.2 {
                if run.is_none_or(|(_, m)| f[0] > m) {
                    run = Some((i, f[0]));
                }
            } else if let Some((j, _)) = run.take() {
                v.push(j);
            }
        }
        v.extend(run.map(|r| r.0));
        v
    }

    fn quarter_clicks() -> Arc<super::reference::RefAudio> {
        let at: Vec<f64> = (1..200).map(|k| f64::from(k) * 0.25).collect();
        Arc::new(super::reference::RefAudio::test_clicks(48_000, 60.0, &at))
    }

    /// 120 BPM, 180 BPM from tick 960 (1 s), 90 BPM from tick 2,880.
    fn tempo_doc() -> PlumDocument {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::TempoValue;
        use plumspirit_engine::model::conductor::Interp;
        // a far, soft note keeps playback running; nothing sounds before it
        let mut d = doc(&[(200_000, 10, 30, 1)]);
        let mut h = History::default();
        for (tick, bpm) in [(960, "180"), (2_880, "90")] {
            let edit = d
                .conductor()
                .prepare_set_tempo(
                    None,
                    tick,
                    TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                    Interp::Step,
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
        }
        d
    }

    #[test]
    fn the_reference_plays_from_any_start_in_time_with_the_tempo_map() {
        let d = tempo_doc();
        let tempo = TempoMap::for_document_at(&d, 48_000, Speed::NORMAL);
        let mut r = Rig::new();
        r.t.set_reference(Some(quarter_clicks()), 0.0, 1.0);
        for start in [0u64, 1_000, 2_000, 2_900, 4_321] {
            let _ = r.t.seek(start, &d, 1);
            r.play(&d, 1);
            let (out, _) = r.pump(72_000, &d, 1); // 1.5 s
            r.pause();
            let t0 = tempo.seconds_at_tick(start);
            let c = (t0 / 0.25).floor() * 0.25 + 0.25;
            let want = ((c - t0) * 48_000.0).round() as i64;
            let got = click_frames(&out);
            assert!(!got.is_empty(), "start {start}: no click heard");
            assert!(
                (got[0] as i64 - want).abs() <= 1,
                "start {start}: click at {} not {want}",
                got[0]
            );
            // clicks keep their spacing whatever the tempo: the recording is time
            for w in got.windows(2) {
                assert!(
                    (w[1] - w[0]).abs_diff(12_000) <= 1,
                    "start {start}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn the_reference_follows_offsets_speed_and_mute() {
        let d = tempo_doc();
        let mut r = Rig::new();
        // starts 0.1 s late: the first click at project 0.35 s
        r.t.set_reference(Some(quarter_clicks()), 0.1, 1.0);
        r.play(&d, 1);
        let (out, _) = r.pump(48_000, &d, 1);
        r.pause();
        assert_eq!(click_frames(&out)[..2], [16_800, 28_800]);
        // a negative offset: the recording started 0.2 s before the project
        let a = r.t.reference().unwrap().audio.clone();
        r.t.set_reference(Some(a.clone()), -0.2, 1.0);
        let _ = r.t.seek(0, &d, 1);
        r.play(&d, 1);
        let (out, _) = r.pump(48_000, &d, 1);
        r.pause();
        assert_eq!(click_frames(&out)[..2], [2_400, 14_400]);
        // double speed: the recording runs twice as fast, so do its clicks
        let _ = r.t.set_speed(Speed::parse("2").unwrap(), &d, 1);
        assert_eq!(r.t.reference().unwrap().speed, 2.0);
        let _ = r.t.seek(0, &d, 1);
        r.play(&d, 1);
        let (out, _) = r.pump(48_000, &d, 1);
        r.pause();
        let got = click_frames(&out);
        assert_eq!(got[..3], [1_200, 7_200, 13_200]);
        // muted: silence
        let _ = r.t.set_speed(Speed::NORMAL, &d, 1);
        r.t.set_reference(Some(a), 0.0, 0.0);
        let _ = r.t.seek(0, &d, 1);
        r.play(&d, 1);
        let (out, _) = r.pump(48_000, &d, 1);
        r.pause();
        assert!(click_frames(&out).is_empty());
    }

    #[test]
    fn a_thousand_loops_never_drift_the_reference() {
        // loop ticks 480–960 (0.5 s – 1 s at 120 BPM); clicks at 0.6 s and 0.85 s
        let d = tempo_doc();
        let a = Arc::new(super::reference::RefAudio::test_clicks(
            48_000,
            5.0,
            &[0.6, 0.85],
        ));
        let mut r = Rig::new();
        r.t.set_reference(Some(a), 0.0, 1.0);
        r.t.set_loop(Some((480, 960)));
        let _ = r.t.seek(480, &d, 1);
        r.play(&d, 1);
        let mut all = Vec::new();
        let mut base = 0usize;
        for _ in 0..50 {
            let (out, _) = r.pump(480_000, &d, 1); // 10 s = 20 loops
            all.extend(click_frames(&out).into_iter().map(|f| f + base));
            base += 480_000;
        }
        r.pause();
        assert!(r.engine().shared.loop_wraps.load(Ordering::Relaxed) >= 999);
        assert_eq!(all.len(), 2_000, "two clicks in each of 1,000 loops");
        for (k, pair) in all.chunks_exact(2).enumerate() {
            let loop_start = k * 24_000;
            assert_eq!(pair, [loop_start + 4_800, loop_start + 16_800], "loop {k}");
        }
    }

    // ---- Note audition (v0.6) ------------------------------------------------------

    #[test]
    fn audition_without_a_soundfont_explains_itself() {
        let d = doc(&[]);
        let mut t = Transport::default();
        let e = t.audition_on(&d, 1, 0, 0, 60, 100, 0, None).unwrap_err();
        assert_eq!(e, "No SoundFont available for note audition.");
        assert_eq!(t.auditions_held(), 0);
    }

    #[test]
    fn a_held_audition_sounds_while_stopped_and_ends_on_release() {
        let d = doc(&[]);
        let mut r = Rig::new();
        r.t.audition_on(&d, 1, 0, 0, 69, 110, 0, None).unwrap();
        assert_eq!(r.t.auditions_held(), 1);
        r.engine().wait_auditioning();
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(rms(&out[4_800..]) > 0.01, "audible while held");
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(rms(&out) > 0.01, "still held");
        r.t.audition_off(0, 69);
        assert_eq!(r.t.auditions_held(), 0);
        let _ = r.pump(48_000, &d, 1); // release
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out) < 1e-3, "silent after release (no stuck note)");
        assert_eq!(
            r.t.state(),
            TransportState::Stopped,
            "audition never starts playback"
        );
        assert_eq!(r.t.position(), 0);
    }

    #[test]
    fn a_timed_audition_stops_by_itself() {
        let d = doc(&[]);
        let mut r = Rig::new();
        r.t.audition_on(
            &d,
            1,
            0,
            0,
            72,
            110,
            0,
            Some(std::time::Duration::from_millis(100)),
        )
        .unwrap();
        assert_eq!(r.t.auditions_held(), 0, "timed notes are not held");
        r.engine().wait_auditioning();
        let (out, _) = r.pump(2_400, &d, 1);
        assert!(rms(&out[1_200..]) > 0.01, "audible at first");
        let _ = r.pump(48_000, &d, 1);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out) < 1e-3, "silent after its length and release");
    }

    /// v0.9 256-Key Mode (S21 step 4a): keys above 127 have no sound and
    /// never reach an output. A project whose only note is key 200 plays
    /// silence (the same project at key 60 is audible); auditioning key 200
    /// is silent, holds nothing and never panics; the synthesizer bank
    /// ignores keys above 127 (never folded onto key & 127).
    #[test]
    fn keys_above_127_stay_silent_and_never_reach_an_output() {
        use plumspirit_engine::model::project::KeyRange;
        let mut d = PlumDocument::new_project("w".into(), Bpm::DEFAULT, 480);
        d.project_mut().keys = KeyRange::Extended256;
        let rec = |p: u8| NoteRecord {
            start_tick: 0,
            duration_ticks: 4_800,
            attrs: NoteAttrs::new_wide(p, 110, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        };
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(200)], false)
            .unwrap();
        let mut r = Rig::new();
        r.play(&d, 1);
        let (out, _) = r.pump(12_000, &d, 1);
        assert!(rms(&out) < 1e-4, "key 200 is silent ({})", rms(&out));
        r.pause();
        // Key 72 (= 200 & 127) would have been audible: the guard is real.
        let audible = doc(&[(0, 4_800, 72, 110)]);
        let mut r2 = Rig::new();
        r2.play(&audible, 1);
        let (out, _) = r2.pump(12_000, &audible, 1);
        assert!(rms(&out) > 0.01);
        // Audition of key 200: silent, nothing held.
        r.t.audition_on(&d, 1, 0, 0, 200, 110, 0, None).unwrap();
        assert_eq!(r.t.auditions_held(), 0);
        r.t.audition_off(0, 200);
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(rms(&out) < 1e-4);
        // The synthesizer bank ignores wide keys (key 72 would sound).
        use super::{engine, synth};
        let mut bank =
            synth::SynthBank::new(&font(), 48_000, &synth::SynthConfig::default()).unwrap();
        let mut block = [0.0f32; engine::BLOCK * 2];
        let loud = |bank: &mut synth::SynthBank, block: &mut [f32; engine::BLOCK * 2]| {
            (0..40)
                .map(|_| {
                    bank.render(block);
                    rms(&block[..])
                })
                .fold(0.0f32, f32::max)
        };
        bank.note_on(0, 200, 110);
        bank.note_on(0, 255, 110);
        assert_eq!(loud(&mut bank, &mut block), 0.0);
        bank.note_off(0, 200);
        bank.note_on(0, 72, 110);
        assert!(loud(&mut bank, &mut block) > 0.01);
    }

    #[test]
    fn audition_note_off_never_ends_the_same_key_played_by_the_transport() {
        // The transport plays A4 for 5 s; the user auditions the same key.
        let d = doc(&[(0, 4_800, 69, 110)]);
        let mut r = Rig::new();
        r.play(&d, 1);
        let (out, _) = r.pump(12_000, &d, 1);
        let alone = rms(&out[4_800..]);
        assert!(alone > 0.01);
        r.t.audition_on(&d, 1, 0, 0, 69, 110, r.t.position(), None)
            .unwrap();
        let _ = r.pump(9_600, &d, 1);
        r.t.audition_off(0, 69);
        let _ = r.pump(48_000, &d, 1); // the audition note's release
        let (out, _) = r.pump(9_600, &d, 1);
        let after = rms(&out);
        assert!(
            after > alone * 0.5,
            "the transport's note keeps sounding ({after} vs {alone})"
        );
        // Release-all is just as harmless to the transport.
        r.t.audition_on(&d, 1, 0, 0, 69, 110, r.t.position(), None)
            .unwrap();
        r.t.audition_all_off();
        let _ = r.pump(48_000, &d, 1);
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(rms(&out) > alone * 0.5);
        // And pausing the transport silences only it; a held audition is
        // released by the app separately (pause leaves nothing sounding).
        r.pause();
        let _ = r.pump(48_000, &d, 1);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out) < 1e-3, "silent after pause");
    }

    #[test]
    #[ignore = "diagnostic: needs /usr/share/sounds/sf2/TimGM6mb.sf2 and AUDITION_MID"]
    fn audition_tail_with_a_real_soundfont() {
        let bytes = std::fs::read("/usr/share/sounds/sf2/TimGM6mb.sf2").unwrap();
        let f = Arc::new(rustysynth::SoundFont::new(&mut &bytes[..]).unwrap());
        let d = match std::env::var("AUDITION_MID") {
            Ok(p) => {
                plumspirit_engine::midi::import_file(
                    std::path::Path::new(&p),
                    plumspirit_engine::midi::ImportControl::default(),
                )
                .unwrap()
                .0
            }
            Err(_) => doc(&[]),
        };
        let mut r = Rig {
            t: Transport::for_test(AudioEngine::headless(48_000), loaded(f, "gm.sf2")),
            last: (0.0, 0.0),
        };
        let track = if d.track_count() > 1 { 1 } else { 0 };
        r.t.audition_on(&d, 1, track, 0, 31, 100, 15_600, None)
            .unwrap();
        r.engine().wait_auditioning();
        let (out, _) = r.pump(14_400, &d, 1);
        eprintln!("held: {}", rms(&out[4_800..]));
        r.t.audition_all_off();
        for k in 0..12 {
            let busy = r.engine().shared.auditioning.load(Ordering::Acquire);
            let (out, _) = r.pump(24_000, &d, 1);
            eprintln!(
                "after {:.1} s: rms {} auditioning {busy}",
                k as f64 * 0.5,
                rms(&out)
            );
        }
    }

    #[test]
    fn release_all_and_soundfont_changes_leave_no_audition_sounding() {
        let d = doc(&[]);
        let mut r = Rig::new();
        r.t.audition_on(&d, 1, 0, 0, 60, 110, 0, None).unwrap();
        r.t.audition_on(&d, 1, 0, 0, 64, 110, 0, None).unwrap();
        r.t.audition_on(
            &d,
            1,
            0,
            9,
            67,
            110,
            0,
            Some(std::time::Duration::from_secs(5)),
        )
        .unwrap();
        r.engine().wait_auditioning();
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out[2_400..]) > 0.01);
        r.t.audition_all_off();
        assert_eq!(r.t.auditions_held(), 0);
        let _ = r.pump(48_000, &d, 1);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out) < 1e-3, "release-all ends held and timed notes");
        // A SoundFont swap replaces the audition synthesizer: silence after.
        r.t.audition_on(&d, 1, 0, 0, 60, 110, 0, None).unwrap();
        let _ = r.pump(4_800, &d, 1);
        r.engine()
            .send(super::engine::Command::SetSoundFont(font()));
        let _ = r.pump(48_000, &d, 1);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(
            rms(&out) < 1e-3,
            "no audition note survives a SoundFont change"
        );
    }

    #[test]
    fn transport_plays_pauses_resumes_and_ends_without_stuck_notes() {
        // Three notes, 0.5 s each, ending at tick 2400 (2.5 s).
        let d = doc(&[(0, 480, 69, 100), (960, 480, 72, 100), (1920, 480, 76, 100)]);
        let mut r = Rig::new();
        r.play(&d, 1);
        assert_eq!(r.t.state(), TransportState::Seeking);
        let (out, _) = r.pump(12_000, &d, 1); // 0.25 s
        assert_eq!(r.t.state(), TransportState::Playing);
        // The playhead comes from frames the device took: 12,000 frames = tick 240.
        assert_eq!(r.t.position(), 240);
        assert!(rms(&out[4_800..]) > 0.01, "first note audible");

        r.pause();
        assert_eq!(r.t.state(), TransportState::Paused);
        let paused_at = r.t.position();
        assert_eq!(paused_at, 240);
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(
            rms(&out[9_600..]) < 1e-3,
            "silence after pause (no stuck note)"
        );
        assert_eq!(r.t.position(), paused_at, "position frozen while paused");

        // Resume continues from the paused position. Inside note 1: since
        // v1.0-beta1 (B1-0083) the held note is not struck a second time —
        // silent until note 2 (v0.8 re-struck it here).
        r.play(&d, 1);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(r.t.position() >= paused_at + 90 && r.t.position() <= paused_at + 96);
        assert!(
            rms(&out[2_400..]) < 1e-3,
            "the held note is not replayed on resume"
        );
        let (out, _) = r.pump(36_000, &d, 1); // to about tick 1056, inside note 2
        assert!(rms(&out[out.len() - 2_400..]) > 0.01, "the next note plays");

        // Run to the end: Ended, stopped at the authoritative duration, silent.
        let mut ended = false;
        for _ in 0..100 {
            let (_, ev) = r.pump(4_800, &d, 1);
            if ev.contains(&TransportEvent::Ended) {
                ended = true;
                break;
            }
        }
        assert!(ended, "end of song reported");
        assert_eq!(r.t.state(), TransportState::Stopped);
        assert_eq!(r.t.position(), d.duration_ticks());
        // Idle: the device stream is paused a moment after playback stops, and
        // restarted by the next Play.
        assert!(r.t.idle_pause_due_in().is_some());
        r.t.idle_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(3));
        let _ = r.t.update(Some(&d), 1);
        assert!(!r.engine().is_active(), "idle stream paused");
        assert_eq!(r.t.idle_pause_due_in(), None);
        let (out, _) = r.pump(4_800, &d, 1);
        assert!(rms(&out) < 1e-3, "silent after the end");
        r.play(&d, 1);
        assert!(r.engine().is_active(), "Play restarts the stream");
        r.pause();
        assert_eq!(r.engine().shared.skipped_notes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn replacing_the_project_discards_old_audio_and_positions() {
        let a = doc(&[(0, 2_400, 60, 127)]);
        let b = doc(&[(1_200, 480, 64, 127)]); // first note at 1.2 s
        let mut r = Rig::new();
        r.play(&a, 1);
        let (out, _) = r.pump(9_600, &a, 1);
        assert!(rms(&out[4_800..]) > 0.01);
        // A different project is opened (new document generation).
        r.t.reset_for_new_document();
        r.engine().wait_playing(false);
        assert_eq!(r.t.state(), TransportState::Stopped);
        assert_eq!(r.t.position(), 0);
        let (_, ev) = r.pump(4_800, &b, 2);
        assert!(ev.is_empty());
        assert_eq!(r.t.position(), 0, "old generation cannot move the playhead");
        // Playing the new project starts from its beginning with its own notes.
        r.play(&b, 2);
        let (out, _) = r.pump(48_000, &b, 2); // 1.0 s
        assert!(
            rms(&out[4_800..]) < 1e-3,
            "nothing of the old project is heard"
        );
        let (out, _) = r.pump(24_000, &b, 2);
        assert!(rms(&out[9_600..]) > 0.01, "the new project's note plays");
    }

    #[test]
    fn soundfont_replacement_while_playing_keeps_the_audio_going() {
        let d = doc(&[(0, 4_800, 69, 110)]); // one 5 s note
        let mut r = Rig::new();
        r.play(&d, 1);
        let (out, _) = r.pump(12_000, &d, 1);
        assert!(rms(&out[4_800..]) > 0.01);
        // Load another SF2 through the real loader thread (file on disk).
        let dir = std::env::temp_dir().join(format!("plumspirit-sf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.sf2");
        std::fs::write(&path, tiny_sf2()).unwrap();
        r.t.load_soundfont(path.clone());
        let mut loaded_ok = false;
        for _ in 0..500 {
            let (_, ev) = r.pump(480, &d, 1);
            // The event carries the load's duration for the status bar (§157).
            if ev.iter().any(|e| {
                matches!(e, TransportEvent::SoundFontLoaded { elapsed, .. } if !elapsed.is_zero())
            }) {
                loaded_ok = true;
                break;
            }
        }
        assert!(loaded_ok, "SoundFont loaded");
        assert_eq!(r.t.soundfont().map(|s| s.path.clone()), Some(path.clone()));
        assert!(r.t.state().is_running());
        // The worker swaps synthesizers at a block boundary and re-strikes the note.
        let (out, _) = r.pump(24_000, &d, 1);
        assert!(
            rms(&out[12_000..]) > 0.01,
            "still audible with the new SoundFont"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_soundfont_file_is_refused_and_the_old_one_stays() {
        let dir = std::env::temp_dir().join(format!("plumspirit-sf-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.sf2");
        let mut bytes = tiny_sf2();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&path, &bytes).unwrap();
        let d = doc(&[(0, 480, 69, 100)]);
        let mut r = Rig::new();
        r.t.load_soundfont(path.clone());
        let mut failed = false;
        // The loader runs on its own thread: wait for it by time, not by a
        // number of pumps (a release build pumps 500 times before it starts).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let (_, ev) = r.pump(480, &d, 1);
            if ev.iter().any(|e| {
                matches!(e, TransportEvent::SoundFontFailed { elapsed, .. } if !elapsed.is_zero())
            }) {
                failed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(failed, "failure reported");
        assert_eq!(
            r.t.soundfont().map(|s| s.name.clone()),
            Some("tiny.sf2".into())
        );
        r.play(&d, 1);
        let (out, _) = r.pump(9_600, &d, 1);
        assert!(rms(&out[4_800..]) > 0.01, "previous SoundFont still plays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_failure_makes_audio_unavailable_but_keeps_the_position() {
        let d = doc(&[(0, 4_800, 69, 110)]);
        let mut r = Rig::new();
        r.play(&d, 1);
        let _ = r.pump(24_000, &d, 1); // 0.5 s = tick 480
        let shared = Arc::clone(&r.engine().shared);
        *shared.device_error.lock().unwrap() = Some("device unplugged".into());
        shared.device_failed.store(true, Ordering::Release);
        let ev = r.t.update(Some(&d), 1);
        assert!(ev.iter().any(
            |e| matches!(e, TransportEvent::Unavailable(m) if m.contains("device unplugged"))
        ));
        assert_eq!(r.t.state(), TransportState::AudioUnavailable);
        assert_eq!(r.t.position(), 480, "position kept for the next Play");
        assert!(r.t.engine.is_none(), "failed device released");
        // The editor keeps working: seeking and pausing do not need audio.
        assert_eq!(r.t.seek(960, &d, 1), None);
        r.t.pause();
        assert_eq!(r.t.position(), 960);
        // §252: another device can be chosen (Audio Settings); the next Play
        // opens it (no stale engine is kept).
        r.t.configure_output(super::output::OutputConfig {
            device: Some("USB Headphones".into()),
            ..Default::default()
        });
        assert_eq!(r.t.output.device.as_deref(), Some("USB Headphones"));
        assert!(r.t.engine.is_none());
    }

    #[test]
    fn velocity_edits_reach_running_playback() {
        // Eight notes at full velocity, one every 0.5 s.
        let notes: Vec<(u64, u64, u8, u8)> = (0..8).map(|i| (i * 480, 400, 69, 127)).collect();
        let mut d = doc(&notes);
        let mut r = Rig::new();
        r.play(&d, 1);
        let (first, _) = r.pump(24_000, &d, 1); // note 1 (0.0–0.5 s)
        // While playing, soften every note from tick 960 (1.0 s) on.
        {
            let store = &mut d.track_mut(0).unwrap().notes;
            store.select_in_region(960, u64::MAX, 0, 127);
            store.map_selected_attrs(|a| a.with_velocity(15).unwrap());
        }
        let _ = r.pump(24_000, &d, 1); // 0.5–1.0 s (the edit is picked up here)
        let (third, _) = r.pump(24_000, &d, 1); // note 3 (1.0–1.5 s)
        let loud = rms(&first[2_400..30_000]);
        let soft = rms(&third[2_400..30_000]);
        assert!(loud > 0.01);
        assert!(
            loud > soft * 2.0,
            "edited velocity is heard (loud {loud}, soft {soft})"
        );
    }

    /// Left / right RMS of interleaved stereo samples.
    fn rms_lr(s: &[f32]) -> (f32, f32) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for f in s.chunks_exact(2) {
            l += f[0] * f[0];
            r += f[1] * f[1];
        }
        let n = (s.len() / 2).max(1) as f32;
        ((l / n).sqrt(), (r / n).sqrt())
    }

    #[test]
    fn pan_and_pitch_bend_edits_reach_running_playback() {
        use plumspirit_engine::editor::controllers::{self, ControllerKind};
        use plumspirit_engine::history::{History, Operation};
        // One 4 s note on channel 1, centred.
        let mut d = doc(&[(0, 3_840, 69, 110)]);
        let mut h = History::default();
        let mut r = Rig::new();
        r.play(&d, 1);
        let (before, _) = r.pump(24_000, &d, 1); // 0.0–0.5 s
        let (l0, r0) = rms_lr(&before[9_600..]);
        assert!(l0 > 0.005 && r0 > 0.005);
        assert!((l0 / r0 - 1.0).abs() < 0.2, "centered: {l0} / {r0}");
        // While playing: pan hard left from tick 960 (1.0 s).
        let e = controllers::prepare_set(d.track(0).unwrap(), ControllerKind::PAN, 0, None, 960, 0)
            .unwrap();
        h.apply(
            &mut d,
            Operation::Events {
                edit: e,
                label: "Pan",
            },
        )
        .unwrap();
        let _ = r.pump(24_000, &d, 1); // 0.5–1.0 s (the edit is picked up here)
        let (after, _) = r.pump(24_000, &d, 1); // 1.0–1.5 s
        let (l1, r1) = rms_lr(&after[4_800..]);
        assert!(l1 > r1 * 3.0, "panned left: {l1} / {r1}");
        // Undo while playing puts it back in the centre (the stale index is replaced).
        h.undo(&mut d).unwrap();
        let _ = r.pump(4_800, &d, 1);
        let (undone, _) = r.pump(24_000, &d, 1);
        let (l2, r2) = rms_lr(&undone[9_600..]);
        assert!((l2 / r2 - 1.0).abs() < 0.2, "centered again: {l2} / {r2}");
        // A pitch bend changes what is heard (the waveform differs from the
        // unbent note at the same position).
        let mut bent = doc(&[(0, 3_840, 69, 110)]);
        let e = controllers::prepare_set(
            bent.track(0).unwrap(),
            ControllerKind::PitchBend,
            0,
            None,
            0,
            16_383,
        )
        .unwrap();
        h.apply(
            &mut bent,
            Operation::Events {
                edit: e,
                label: "Pitch Bend",
            },
        )
        .unwrap();
        let f = font();
        let plain = super::engine::render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&doc(&[(0, 3_840, 69, 110)]), 48_000)),
            0,
            48_000,
            24_000,
        );
        let with_bend = super::engine::render_offline(
            &f,
            Arc::new(PlaybackIndex::build(&bent, 48_000)),
            0,
            48_000,
            24_000,
        );
        let diff: f32 = plain
            .iter()
            .zip(&with_bend)
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(diff > 1.0, "pitch bend is audible (difference {diff})");
        r.pause();
    }

    #[test]
    fn tempo_edits_during_playback_change_the_speed_from_the_playhead_on() {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::TempoValue;
        let mut d = doc(&[(0, 9_600, 69, 100)]);
        let mut h = History::default();
        let mut r = Rig::new();
        r.play(&d, 1);
        let _ = r.pump(24_000, &d, 1); // 0.5 s at 120 BPM = tick 480
        let at = r.t.position();
        assert!((470..=490).contains(&at), "{at}");
        let edit = d
            .conductor()
            .prepare_set_start_tempo(TempoValue::Bpm(Bpm::parse("240").unwrap()))
            .unwrap();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
        // The new tempo map is picked up; audio already buffered at the old tempo
        // plays out first.
        // (The transport sends the new index after this pump; the audio already
        // buffered at the old tempo plays out during the next one.)
        let _ = r.pump(4_800, &d, 1);
        let _ = r.pump(9_600, &d, 1);
        let p0 = r.t.position();
        let _ = r.pump(24_000, &d, 1); // 0.5 s at 240 BPM = 960 ticks
        let advanced = r.t.position() - p0;
        assert!((930..=990).contains(&advanced), "advanced {advanced} ticks");
        r.pause();
    }

    #[test]
    fn a_loop_repeats_its_range_without_ending_or_drifting() {
        // Notes every quarter; loop the first two beats (ticks 0–960 = 1 s).
        let notes: Vec<(u64, u64, u8, u8)> = (0..16).map(|i| (i * 480, 400, 69, 110)).collect();
        let d = doc(&notes);
        let mut r = Rig::new();
        r.t.set_loop(Some((0, 960)));
        r.play(&d, 1);
        let mut max_pos = 0;
        for _ in 0..12 {
            let (out, ev) = r.pump(12_000, &d, 1); // 0.25 s each, 3 s in total
            assert!(!ev.contains(&TransportEvent::Ended), "a loop never ends");
            max_pos = max_pos.max(r.t.position());
            assert!(r.t.position() <= 960, "playhead stays in the loop");
            assert!(rms(&out) > 1e-4, "audible");
        }
        assert!(max_pos > 700, "the whole loop plays ({max_pos})");
        assert!(r.engine().shared.loop_wraps.load(Ordering::Relaxed) >= 2);
        // Turning the loop off lets playback run on to the end.
        r.t.set_loop(None);
        let mut ended = false;
        for _ in 0..200 {
            let (_, ev) = r.pump(12_000, &d, 1);
            if ev.contains(&TransportEvent::Ended) {
                ended = true;
                break;
            }
        }
        assert!(ended);
    }

    #[test]
    fn playback_speed_scales_time_but_not_the_tempo_map() {
        use plumspirit_engine::model::tempo::Speed;
        let d = doc(&[(0, 9_600, 69, 100)]);
        let mut r = Rig::new();
        assert_eq!(r.t.set_speed(Speed::parse("2").unwrap(), &d, 1), None);
        r.play(&d, 1);
        let _ = r.pump(24_000, &d, 1); // 0.5 s at 2× = 960 ticks
        let p = r.t.position();
        assert!((930..=990).contains(&p), "{p}");
        // Changing it while playing restarts at the same position.
        let _ = r.t.set_speed(Speed::parse("0.5").unwrap(), &d, 1);
        r.engine().wait_playing(true);
        let p0 = r.t.position();
        let _ = r.pump(4_800, &d, 1);
        let _ = r.pump(24_000, &d, 1);
        let advanced = r.t.position() - p0;
        assert!(
            (200..=300).contains(&advanced),
            "0.5×: {advanced} ticks in ~0.6 s"
        );
        assert_eq!(
            d.conductor().start_tempo(),
            plumspirit_engine::model::TempoValue::Bpm(Bpm::DEFAULT)
        );
        r.pause();
    }

    #[test]
    fn muting_the_playing_track_silences_it_immediately() {
        let mut d = doc(&[(0, 9_600, 69, 110)]);
        let mut r = Rig::new();
        r.play(&d, 1);
        let (out, _) = r.pump(24_000, &d, 1);
        assert!(rms(&out[4_800..]) > 0.01);
        d.track_mut(0).unwrap().mute = true;
        let _ = r.pump(4_800, &d, 1); // the change is sent here
        // Buffered audio, the release and the synthesizer's reverb tail play
        // out (where the note-off lands in the rendered stream depends on how
        // far ahead the worker was, so allow a full second).
        let _ = r.pump(48_000, &d, 1);
        let (silent, _) = r.pump(24_000, &d, 1);
        assert!(rms(&silent) < 1e-3, "muted: {}", rms(&silent));
        // Unmuting re-strikes the sounding note.
        d.track_mut(0).unwrap().mute = false;
        let _ = r.pump(4_800, &d, 1);
        let _ = r.pump(9_600, &d, 1);
        let (back, _) = r.pump(24_000, &d, 1);
        assert!(rms(&back) > 0.01, "audible again");
        r.pause();
    }

    /// Synthesis cost with a real General MIDI SoundFont (GeneralUser GS). Run:
    /// `cargo test --release -p plumspirit synth_cost -- --ignored --nocapture`
    /// (set PLUMSPIRIT_TEST_SF2 to use another SF2).
    #[test]
    #[ignore = "measurement: needs GeneralUser-GS.sf2"]
    fn synth_cost_with_a_real_soundfont() {
        use super::engine::{BLOCK, synth_settings};
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        use plumspirit_engine::playback::{Scheduler, SchedulerLimits};
        use std::time::Instant;
        let path = std::env::var_os("PLUMSPIRIT_TEST_SF2")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/debug/SoundFonts/GeneralUser-GS.sf2")
            });
        let Ok(sf) = super::soundfont::load(&path) else {
            println!("skipped: {} not found", path.display());
            return;
        };
        let cases: Vec<(&str, PlumDocument)> = vec![
            (
                "Dense fixture (1M notes, ~40 sounding)",
                generate(FixtureShape::Dense, 1_000_000, 960, 42),
            ),
            ("Black MIDI wall (256-voice limit reached)", {
                // 128-note chords on 16 channels every 1/32: far beyond 256 voices.
                let mut d = PlumDocument::new_project("wall".into(), Bpm::DEFAULT, 480);
                let mut recs = Vec::new();
                for step in 0..2_000u64 {
                    for k in 0..128u64 {
                        recs.push(NoteRecord {
                            start_tick: step * 60,
                            duration_ticks: 240,
                            attrs: NoteAttrs::new(
                                k as u8,
                                100,
                                (k % 16) as u8,
                                0,
                                NoteOffKind::NoteOff,
                            )
                            .unwrap(),
                            sub: plumspirit_engine::model::SubchannelId::MAIN,
                            group: 0,
                        });
                    }
                }
                d.track_mut(0)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&recs, false)
                    .unwrap();
                d
            }),
        ];
        for (name, doc) in cases {
            let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
            let mut synth =
                rustysynth::Synthesizer::new(&sf.font, &synth_settings(48_000)).unwrap();
            let mut sched = Scheduler::new(idx, SchedulerLimits::default());
            struct S<'a>(&'a mut rustysynth::Synthesizer);
            impl plumspirit_engine::playback::SynthSink for S<'_> {
                fn note_on(&mut self, c: u8, k: u8, v: u8) {
                    self.0.note_on(c.into(), k.into(), v.into());
                }
                fn note_off(&mut self, c: u8, k: u8) {
                    self.0.note_off(c.into(), k.into());
                }
                fn channel_message(&mut self, st: u8, a: u8, b: u8) {
                    self.0.process_midi_message(
                        (st & 15).into(),
                        (st & 0xF0).into(),
                        a.into(),
                        b.into(),
                    );
                }
                fn silence(&mut self) {
                    self.0.note_off_all(true);
                }
                fn reset(&mut self) {
                    self.0.reset();
                }
            }
            sched.seek(0, &mut S(&mut synth));
            let (mut l, mut r) = ([0f32; BLOCK], [0f32; BLOCK]);
            let blocks = 48_000 * 10 / BLOCK; // 10 s of audio
            let mut worst = std::time::Duration::ZERO;
            let started = Instant::now();
            for _ in 0..blocks {
                let t = Instant::now();
                sched.advance(BLOCK as u32, &mut S(&mut synth));
                synth.render(&mut l, &mut r);
                worst = worst.max(t.elapsed());
            }
            let total = started.elapsed();
            let st = sched.stats();
            println!(
                "{name}: 10 s rendered in {:.0} ms = {:.1}% of real time; worst block {:.0} µs of {:.0} µs; {} note-ons, {} skipped",
                total.as_secs_f64() * 1e3,
                total.as_secs_f64() / 10.0 * 100.0,
                worst.as_secs_f64() * 1e6,
                BLOCK as f64 / 48_000.0 * 1e6,
                st.note_ons,
                st.note_ons_skipped
            );
        }
    }

    // ---- Default output and providers (B1-0135), platform-free ---------------------

    mod providers {
        use super::super::midi_out::tests::FakePort;
        use super::super::midi_out::{
            self, ExternalSink, MidiPort, OutputHealth, OutputTarget, outputs_from, with_selected,
        };
        use super::super::{Transport, TransportEvent};
        use super::doc;
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::{OutputRoute, PlumDocument, TrackProps};
        use plumspirit_engine::playback::SynthSink;
        use std::cell::RefCell;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        fn route_track(d: &mut PlumDocument, i: usize, route: OutputRoute) {
            let t = d.track(i).unwrap();
            let props = TrackProps {
                route,
                ..t.props().clone()
            };
            let track = t.id();
            History::default()
                .apply(
                    d,
                    Operation::TrackProps {
                        track,
                        props,
                        end: None,
                        label: "Track Properties",
                    },
                )
                .unwrap();
        }

        /// Opens every target with a fresh fake port and remembers them.
        struct Opener {
            ports: RefCell<Vec<(OutputTarget, Arc<FakePort>)>>,
            missing: Vec<OutputTarget>,
        }

        impl Opener {
            fn new(missing: &[OutputTarget]) -> Opener {
                Opener {
                    ports: RefCell::default(),
                    missing: missing.to_vec(),
                }
            }
            fn open(&self, t: &OutputTarget) -> Result<Arc<dyn MidiPort>, String> {
                if self.missing.contains(t) {
                    return Err(format!("The MIDI output “{}” is not connected.", t.label()));
                }
                let p = Arc::new(FakePort::default());
                self.ports.borrow_mut().push((t.clone(), Arc::clone(&p)));
                Ok(p)
            }
            fn port(&self, i: usize) -> Arc<FakePort> {
                Arc::clone(&self.ports.borrow()[i].1)
            }
        }

        fn synth_a() -> OutputTarget {
            OutputTarget::Device("Synth A".into())
        }

        #[test]
        fn tracks_set_to_internal_play_on_the_open_default_output() {
            let d = doc(&[(0, 480, 60, 100)]);
            let mut t = Transport::default();
            assert_eq!(t.output_status().health, OutputHealth::Internal);
            t.set_default_output(synth_a());
            assert_eq!(t.output_status().health, OutputHealth::Idle);
            let o = Opener::new(&[]);
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.routes.len(), 1);
            assert_eq!(
                t.setup(&d).routes[0],
                1,
                "the Internal track plays on route 1"
            );
            assert_eq!(t.output_status().health, OutputHealth::Open);
            assert!(!t.output_status().is_warning());
            // Back to Internal: the route closes and the track plays inside.
            t.set_default_output(OutputTarget::Internal);
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert!(t.routes.is_empty());
            assert_eq!(t.setup(&d).routes[0], 0);
        }

        #[test]
        fn a_stale_default_output_falls_back_to_internal_with_a_notice() {
            let d = doc(&[(0, 480, 60, 100)]);
            let mut t = Transport::default();
            let gone = OutputTarget::Device("Unplugged Synth".into());
            t.set_default_output(gone.clone());
            let o = Opener::new(std::slice::from_ref(&gone));
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert!(t.routes.is_empty(), "nothing half-open");
            assert_eq!(t.setup(&d).routes[0], 0, "internal playback still works");
            let st = t.output_status();
            assert!(
                matches!(st.health, OutputHealth::Unavailable(ref r) if r.contains("not connected"))
            );
            assert!(st.is_warning());
            assert!(
                st.health_text()
                    .contains("internal synthesizer plays instead")
            );
            assert_eq!(t.notices.len(), 1);
            assert!(
                t.notices[0]
                    .ends_with("Tracks set to Internal play on the internal synthesizer instead.")
            );
            // Retrying does not pile up notices.
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.notices.len(), 1);
            // The device comes back (Refresh / next Play): it opens and the
            // reason is gone.
            let o2 = Opener::new(&[]);
            t.prepare_routes_with(&d, &|x| o2.open(x));
            assert_eq!(t.output_status().health, OutputHealth::Open);
            assert!(t.notices.is_empty());
        }

        #[test]
        fn a_default_output_that_stops_working_reports_it_and_the_next_play_reopens() {
            let d = doc(&[(0, 480, 60, 100)]);
            let mut t = Transport::default();
            t.set_default_output(OutputTarget::Kdmapi);
            let o = Opener::new(&[]);
            t.prepare_routes_with(&d, &|x| o.open(x));
            let port = o.port(0);
            port.fail.store(true, Ordering::Relaxed);
            t.routes[0].flush_now(); // any send now fails
            assert!(t.routes[0].has_failed());
            assert!(matches!(t.output_status().health, OutputHealth::Failed(_)));
            assert_eq!(t.setup(&d).routes[0], 0, "a failed output is never used");
            // The next Play opens a fresh port.
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(o.ports.borrow().len(), 2);
            assert_eq!(t.output_status().health, OutputHealth::Open);
            assert_eq!(t.setup(&d).routes[0], 1);
        }

        #[test]
        fn a_track_routed_to_the_default_device_shares_one_route() {
            let mut d = doc(&[(0, 480, 60, 100)]);
            d.add_track().unwrap();
            route_track(&mut d, 1, OutputRoute::External("Synth A".into()));
            let mut t = Transport::default();
            t.set_default_output(synth_a());
            let o = Opener::new(&[]);
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.routes.len(), 1, "one device opened once");
            let s = t.setup(&d);
            assert_eq!((s.routes[0], s.routes[1]), (1, 1));
            // A track routed to another device keeps it; Internal follows the default.
            route_track(&mut d, 1, OutputRoute::External("Synth B".into()));
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.routes.len(), 2);
            let s = t.setup(&d);
            assert_eq!(t.routes[usize::from(s.routes[0]) - 1].target, synth_a());
            assert_eq!(
                t.routes[usize::from(s.routes[1]) - 1].target,
                OutputTarget::Device("Synth B".into())
            );
        }

        #[test]
        fn switching_the_default_output_ends_the_old_outputs_notes() {
            let d = doc(&[(0, 480, 60, 100)]);
            let mut t = Transport::default();
            t.set_default_output(synth_a());
            let o = Opener::new(&[]);
            t.prepare_routes_with(&d, &|x| o.open(x));
            let key = t.routes_key;
            let mut sink = ExternalSink::new(Arc::clone(&t.routes[0]));
            sink.note_on(0, 60, 100);
            t.routes[0].dispatch(0);
            drop(sink);
            t.set_default_output(OutputTarget::Kdmapi);
            assert_ne!(
                t.routes_key, key,
                "a running transport re-prepares its routes"
            );
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.routes.len(), 1);
            assert_eq!(t.routes[0].target, OutputTarget::Kdmapi);
            let sent = o.port(0).sent.lock().unwrap().clone();
            assert!(sent.contains(&0x40_3C_80), "note-off for key 60: {sent:x?}");
            assert!(sent.contains(&0x00_7B_B0), "All Notes Off");
        }

        /// v1.0-Launch L-A01 (contract §003, `plumspirit-flopp.log`: "Tracks
        /// set to Internal now play on OmniMIDI (KDMAPI)" then a panic at
        /// playback.rs:1506): the default output switched from the internal
        /// synthesizer to KDMAPI while playing — the running worker's route
        /// count goes 1 → 2 — keeps playing. The KDMAPI port receives the
        /// song's notes after the switch, the worker still answers Pause,
        /// and switching back (2 → 1) and again (1 → 2) while playing works
        /// too; nothing is left sounding on either KDMAPI port.
        #[test]
        fn switching_internal_to_kdmapi_while_playing_keeps_the_worker_running() {
            use std::sync::Mutex;
            // A note every 60 ticks (≈ 62 ms at 120 BPM / 480 PPQN), 20 s.
            let notes: Vec<(u64, u64, u8, u8)> = (0..320u64)
                .map(|i| (i * 60, 50, 60 + (i % 12) as u8, 100))
                .collect();
            let mut d = doc(&notes);
            d.add_track().unwrap();
            let ports: Arc<Mutex<Vec<Arc<FakePort>>>> = Arc::default();
            let opened = Arc::clone(&ports);
            let mut r = super::Rig::new();
            r.t.test_opener = Some(Arc::new(move |_t: &OutputTarget| {
                let p = Arc::new(FakePort::default());
                opened.lock().unwrap().push(Arc::clone(&p));
                Ok(p as Arc<dyn MidiPort>)
            }));
            let note_ons = |p: &FakePort| {
                p.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|m| *m & 0xF0 == 0x90 && (*m >> 16) & 0x7F > 0)
                    .count()
            };
            // Let the dispatcher send what the "device" played.
            let settle = || std::thread::sleep(std::time::Duration::from_millis(60));
            r.play(&d, 1);
            let _ = r.pump(9_600, &d, 1);
            for round in 0..2 {
                r.t.set_default_output(OutputTarget::Kdmapi);
                // This pump's update sends SetRoutes + Update (1 → 2 routes);
                // before the repair the worker panicked on that Update.
                let _ = r.pump(256, &d, 1);
                std::thread::sleep(std::time::Duration::from_millis(300));
                assert!(
                    r.engine().worker_alive(),
                    "round {round}: the synthesis worker survives the route change"
                );
                for _ in 0..10 {
                    let _ = r.pump(4_800, &d, 1);
                }
                settle();
                let port = Arc::clone(&ports.lock().unwrap()[round]);
                assert!(
                    note_ons(&port) > 0,
                    "round {round}: KDMAPI receives the notes after the switch"
                );
                assert!(
                    r.engine()
                        .shared
                        .playing
                        .load(std::sync::atomic::Ordering::Acquire),
                    "round {round}: still playing"
                );
                // Back to the internal synthesizer (2 → 1 routes) while playing.
                r.t.set_default_output(OutputTarget::Internal);
                for _ in 0..5 {
                    let _ = r.pump(4_800, &d, 1);
                }
                settle();
                let before = note_ons(&port);
                for _ in 0..5 {
                    let _ = r.pump(4_800, &d, 1);
                }
                settle();
                assert_eq!(
                    note_ons(&port),
                    before,
                    "round {round}: a closed output receives nothing more"
                );
            }
            r.pause();
            assert!(
                !r.engine()
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Acquire),
                "the worker answers Pause"
            );
            for (i, p) in ports.lock().unwrap().iter().enumerate() {
                let mut open = [0u128; 16];
                for m in p.sent.lock().unwrap().iter() {
                    let (st, k, v) = ((m & 0xFF) as u8, ((m >> 8) & 0x7F) as u8, (m >> 16) & 0x7F);
                    let ch = usize::from(st & 0x0F);
                    match st & 0xF0 {
                        0x90 if v > 0 => open[ch] |= 1u128 << k,
                        0x80 | 0x90 => open[ch] &= !(1u128 << k),
                        0xB0 if k == 123 => open[ch] = 0,
                        _ => {}
                    }
                }
                assert!(
                    open.iter().all(|k| *k == 0),
                    "KDMAPI port {i} left a note sounding"
                );
            }
        }

        /// Keys a fake port was left holding (Note On without a later Note
        /// Off or All Notes Off on its channel).
        fn held_keys(p: &FakePort) -> Vec<(u8, u8)> {
            let mut open = [0u128; 16];
            for m in p.sent.lock().unwrap().iter() {
                let (st, k, v) = ((m & 0xFF) as u8, ((m >> 8) & 0x7F) as u8, (m >> 16) & 0x7F);
                let ch = usize::from(st & 0x0F);
                match st & 0xF0 {
                    0x90 if v > 0 => open[ch] |= 1u128 << k,
                    0x80 | 0x90 => open[ch] &= !(1u128 << k),
                    0xB0 if k == 123 => open[ch] = 0,
                    _ => {}
                }
            }
            let mut v = Vec::new();
            for (ch, keys) in open.iter().enumerate() {
                for k in 0..128u8 {
                    if keys & (1u128 << k) != 0 {
                        v.push((ch as u8, k));
                    }
                }
            }
            v
        }

        type Ports = Arc<std::sync::Mutex<Vec<(OutputTarget, Arc<FakePort>)>>>;
        type Unplugged = Arc<std::sync::Mutex<Vec<OutputTarget>>>;

        /// A rig whose external outputs open as fake ports (all kept); the
        /// targets in `Unplugged` fail to open ("not connected").
        fn rig_with_ports() -> (super::Rig, Ports, Unplugged) {
            let ports: Ports = Arc::default();
            let unplugged: Unplugged = Arc::default();
            let (opened, gone) = (Arc::clone(&ports), Arc::clone(&unplugged));
            let mut r = super::Rig::new();
            r.t.test_opener = Some(Arc::new(move |t: &OutputTarget| {
                if gone.lock().unwrap().contains(t) {
                    return Err(format!("The MIDI output “{}” is not connected.", t.label()));
                }
                let p = Arc::new(FakePort::default());
                opened.lock().unwrap().push((t.clone(), Arc::clone(&p)));
                Ok(p as Arc<dyn MidiPort>)
            }));
            (r, ports, unplugged)
        }

        /// v1.0-Launch L-A02 (contract §003 "two outputs then removing one",
        /// "disconnect / fallback"): Track 0 on KDMAPI (default output), Track
        /// 1 on "Synth A" — three routes. Synth A fails while playing; the
        /// transport drops it (3 → 2 routes under the running worker), Track
        /// 1 plays on the internal synthesizer, KDMAPI keeps receiving Track
        /// 0, the worker survives and nothing is left sounding on KDMAPI.
        #[test]
        fn an_output_failing_while_playing_shrinks_the_routes_without_a_panic() {
            let notes: Vec<(u64, u64, u8, u8)> = (0..320u64)
                .map(|i| (i * 60, 200, 60 + (i % 12) as u8, 100))
                .collect();
            let mut d = doc(&notes);
            d.add_track().unwrap();
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(
                    &(0..320u64)
                        .map(|i| plumspirit_engine::model::NoteRecord {
                            start_tick: i * 60 + 30,
                            duration_ticks: 200,
                            attrs: plumspirit_engine::model::NoteAttrs::new(
                                40,
                                90,
                                1,
                                0,
                                plumspirit_engine::model::NoteOffKind::NoteOff,
                            )
                            .unwrap(),
                            sub: Default::default(),
                            group: 0,
                        })
                        .collect::<Vec<_>>(),
                    false,
                )
                .unwrap();
            route_track(&mut d, 1, OutputRoute::External("Synth A".into()));
            let (mut r, ports, unplugged) = rig_with_ports();
            r.t.set_default_output(OutputTarget::Kdmapi);
            r.play(&d, 1);
            for _ in 0..10 {
                let _ = r.pump(4_800, &d, 1);
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
            let port = |t: &OutputTarget| {
                ports
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(x, _)| x == t)
                    .map(|(_, p)| Arc::clone(p))
                    .expect("opened")
            };
            let (kd, sa) = (port(&OutputTarget::Kdmapi), port(&synth_a()));
            assert_eq!(r.t.routes.len(), 2, "three routes: synth + two outputs");
            let ons = |p: &FakePort, ch: u8| {
                p.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|m| *m & 0xFF == u32::from(0x90 | ch) && (*m >> 16) & 0x7F > 0)
                    .count()
            };
            assert!(
                ons(&kd, 0) > 0 && ons(&kd, 1) == 0,
                "Track 0 on KDMAPI only"
            );
            assert!(
                ons(&sa, 1) > 0 && ons(&sa, 0) == 0,
                "Track 1 on Synth A only"
            );
            // Synth A is unplugged: its next send fails and it cannot reopen.
            unplugged.lock().unwrap().push(synth_a());
            sa.fail.store(true, Ordering::Relaxed);
            let mut notice = false;
            for _ in 0..10 {
                let (_, ev) = r.pump(4_800, &d, 1);
                notice |= ev.iter().any(|e| {
                    matches!(e, TransportEvent::Notice(t) if t.contains("Synth A") && t.contains("stopped working"))
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert!(notice, "the failure is reported once");
            assert_eq!(r.t.routes.len(), 1, "3 → 2 routes");
            assert!(r.engine().worker_alive(), "no panic on the shrink");
            let before = ons(&kd, 0);
            for _ in 0..5 {
                let _ = r.pump(4_800, &d, 1);
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert!(ons(&kd, 0) > before, "KDMAPI keeps playing Track 0");
            assert_eq!(ons(&kd, 1), 0, "Track 1 never moves to KDMAPI");
            r.pause();
            // The worker clears `playing` before it ends the notes.
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert!(held_keys(&kd).is_empty(), "nothing left sounding on KDMAPI");
        }

        /// L-A02: on KDMAPI, Pause, Seek (into sustained notes), a Loop that
        /// wraps several times and an output switch inside the loop keep the
        /// worker alive and leave no note sounding on the port; resuming
        /// after a pause re-strikes nothing by default (the transport rule).
        #[test]
        fn pause_seek_and_loop_on_kdmapi_leave_no_stuck_note() {
            // Long overlapping notes: something is always sounding.
            let notes: Vec<(u64, u64, u8, u8)> = (0..160u64)
                .map(|i| (i * 120, 1_000, 48 + (i % 24) as u8, 100))
                .collect();
            let d = doc(&notes);
            let (mut r, ports, _) = rig_with_ports();
            r.t.set_default_output(OutputTarget::Kdmapi);
            r.play(&d, 1);
            for _ in 0..4 {
                let _ = r.pump(4_800, &d, 1);
            }
            r.pause();
            std::thread::sleep(std::time::Duration::from_millis(60));
            let kd = Arc::clone(&ports.lock().unwrap()[0].1);
            assert!(held_keys(&kd).is_empty(), "Pause ends every note");
            let _ = r.t.seek(4_000, &d, 1);
            r.play(&d, 1);
            for _ in 0..4 {
                let _ = r.pump(4_800, &d, 1);
            }
            r.t.set_loop(Some((4_000, 4_800)));
            for _ in 0..20 {
                let _ = r.pump(4_800, &d, 1);
            }
            // Switch away and back inside the loop.
            r.t.set_default_output(OutputTarget::Internal);
            for _ in 0..4 {
                let _ = r.pump(4_800, &d, 1);
            }
            r.t.set_default_output(OutputTarget::Kdmapi);
            for _ in 0..8 {
                let _ = r.pump(4_800, &d, 1);
            }
            assert!(r.engine().worker_alive());
            r.pause();
            std::thread::sleep(std::time::Duration::from_millis(60));
            for (i, (_, p)) in ports.lock().unwrap().iter().enumerate() {
                assert!(held_keys(p).is_empty(), "port {i} left {:?}", held_keys(p));
            }
        }

        /// A port whose Note Ons block until its gate opens (a device that
        /// cannot keep up with the song; resets and note-offs pass), counting
        /// Note Ons.
        #[derive(Default)]
        struct GatePort {
            open: std::sync::Mutex<bool>,
            cv: std::sync::Condvar,
            ons: std::sync::atomic::AtomicU64,
        }

        impl GatePort {
            fn set_open(&self) {
                *self.open.lock().unwrap() = true;
                self.cv.notify_all();
            }
        }

        /// Opens the gate when dropped (a failing test never leaves the
        /// worker waiting for it).
        struct OpenOnDrop(Arc<GatePort>);

        impl Drop for OpenOnDrop {
            fn drop(&mut self) {
                self.0.set_open();
            }
        }

        impl MidiPort for GatePort {
            fn short(&self, m: u32) -> bool {
                if m & 0xF0 == 0x90 && (m >> 16) & 0x7F > 0 {
                    let mut g = self.open.lock().unwrap();
                    while !*g {
                        g = self.cv.wait(g).unwrap();
                    }
                    self.ons.fetch_add(1, Ordering::Relaxed);
                }
                true
            }
            fn sysex(&self, _: &[u8]) -> bool {
                true
            }
        }

        /// Lets the "device" take only what is buffered (never waits for the
        /// worker), up to `frames`; the transport events of that pump go to
        /// `events` (the pump's update may be the one that reports a notice).
        fn take_buffered(
            r: &mut super::Rig,
            d: &PlumDocument,
            frames: usize,
            events: &mut Vec<TransportEvent>,
        ) -> usize {
            let avail = r.engine().shared.ring.available() / 2;
            let n = avail.min(frames);
            if n > 0 {
                events.extend(r.pump(n, d, 1).1);
            }
            n
        }

        /// v1.0-Launch L-B03: an output that cannot keep up (its sends block)
        /// under a 300,000-note burst. "Keep every event": the worker stops
        /// rendering ahead at the backlog mark (counted as waits), nothing is
        /// dropped, and once the output catches up every note is sent. "Keep
        /// time" (Realtime default): playback goes on, the note-ons beyond
        /// the backlog are dropped, counted and reported once, and sent +
        /// dropped = the song's notes.
        #[test]
        fn an_overloaded_output_waits_or_drops_by_the_chosen_policy() {
            // 300 notes per tick for 1,000 ticks (~1 s at 120 BPM / 480 PPQN).
            let notes: Vec<(u64, u64, u8, u8)> = (0..300_000u64)
                .map(|i| (i / 300, 2, (i % 128) as u8, 100))
                .collect();
            let d = doc(&notes);
            for keep_all in [true, false] {
                let gate = Arc::new(GatePort::default());
                let g = Arc::clone(&gate);
                let mut r = super::Rig::new();
                // Declared after the rig: dropped (gate opened) before it.
                let _guard = OpenOnDrop(Arc::clone(&gate));
                r.t.test_opener = Some(Arc::new(move |_t: &OutputTarget| {
                    Ok(Arc::clone(&g) as Arc<dyn MidiPort>)
                }));
                r.t.configure_synth(crate::audio::synth::SynthConfig {
                    external_keep_all: keep_all,
                    ..Default::default()
                });
                r.t.set_default_output(OutputTarget::Kdmapi);
                r.play(&d, 1);
                let shared = Arc::clone(&r.engine().shared);
                // The device plays on while the output is stuck.
                let mut notice = false;
                let mut taken = 0;
                for _ in 0..4_000 {
                    // L-S41: the pump's own update reports too (in release the
                    // notice usually comes from it; dropping its events lost
                    // the notice, a test-harness fault, not the transport's).
                    let mut ev = Vec::new();
                    taken += take_buffered(&mut r, &d, 1_024, &mut ev);
                    ev.extend(r.t.update(Some(&d), 1));
                    notice |= ev.iter().any(|e| {
                        matches!(e, TransportEvent::Notice(t) if t.contains("could not keep up"))
                    });
                    let dropped =
                        r.t.routes
                            .first()
                            .map_or(0, |x| x.dropped.load(Ordering::Relaxed));
                    if (keep_all && shared.external_waits.load(Ordering::Relaxed) > 0)
                        || (!keep_all && dropped > 0 && notice)
                        || taken > 96_000
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let route = Arc::clone(&r.t.routes[0]);
                if keep_all {
                    assert!(
                        shared.external_waits.load(Ordering::Relaxed) > 0,
                        "rendering waited"
                    );
                    assert_eq!(route.dropped.load(Ordering::Relaxed), 0, "nothing dropped");
                    assert!(!notice);
                } else {
                    assert!(
                        route.dropped.load(Ordering::Relaxed) > 0,
                        "note-ons dropped"
                    );
                    assert!(notice, "and reported");
                }
                // The output catches up; the song plays to its end.
                gate.set_open();
                for _ in 0..20_000 {
                    if take_buffered(&mut r, &d, 4_800, &mut Vec::new()) == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    let _ = r.t.update(Some(&d), 1);
                    if !shared.playing.load(Ordering::Acquire) {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                let sent = gate.ons.load(Ordering::Relaxed);
                let dropped = route.dropped.load(Ordering::Relaxed);
                if keep_all {
                    assert_eq!((sent, dropped), (300_000, 0), "every note sent");
                } else {
                    assert_eq!(sent + dropped, 300_000, "sent + dropped = the notes");
                }
                r.t.stop();
            }
        }

        /// A device that takes `delay_us` per message until `fast` is set,
        /// counting what it was sent (Note Ons and everything).
        #[derive(Default)]
        struct SlowPort {
            delay_us: u64,
            fast: std::sync::atomic::AtomicBool,
            sent: std::sync::Mutex<Vec<u32>>,
        }

        impl MidiPort for SlowPort {
            fn short(&self, m: u32) -> bool {
                if !self.fast.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_micros(self.delay_us));
                }
                self.sent.lock().unwrap().push(m);
                true
            }
            fn sysex(&self, _: &[u8]) -> bool {
                true
            }
        }

        /// v1.0-Launch L-B05: an output far slower than the song (5 ms per
        /// message; 1.5 s of song is ~3,000 messages = 15 s of sending) holds
        /// neither the synthesis worker nor the playhead: the "device" takes
        /// 1.5 s of audio at the worker's pace, the playhead follows it, the
        /// output is simply behind. Pause while the output is sending waits
        /// for at most one of its messages (the dispatcher checks the flush
        /// per message), and leaves no note sounding on it.
        #[test]
        fn a_slow_output_never_holds_the_worker_the_playhead_or_pause() {
            use std::time::{Duration, Instant};
            // A note per tick (≈ 1 ms at 120 BPM / 480 PPQN) for 2,000 ticks.
            let notes: Vec<(u64, u64, u8, u8)> = (0..2_000u64)
                .map(|i| (i, 1, 60 + (i % 12) as u8, 100))
                .collect();
            let d = doc(&notes);
            let port = Arc::new(SlowPort {
                delay_us: 5_000,
                ..Default::default()
            });
            let p = Arc::clone(&port);
            let mut r = super::Rig::new();
            r.t.test_opener = Some(Arc::new(move |_t: &OutputTarget| {
                Ok(Arc::clone(&p) as Arc<dyn MidiPort>)
            }));
            r.t.set_default_output(OutputTarget::Kdmapi);
            r.play(&d, 1);
            let t0 = Instant::now();
            for _ in 0..15 {
                let _ = r.pump(4_800, &d, 1);
            }
            let took = t0.elapsed();
            assert!(
                took < Duration::from_secs(4),
                "the worker rendered 1.5 s without waiting for the output: {took:?}"
            );
            let behind = port.sent.lock().unwrap().len();
            assert!(behind < 1_500, "the output is behind ({behind} sent)");
            // 1.5 s = 1,440 ticks (the playhead is what the device took).
            let pos = r.t.position();
            assert!((1_300..=1_500).contains(&pos), "playhead at tick {pos}");
            // Pause while the dispatcher is sending a batch to the slow port:
            // after its first batch (the start's ~170 channel resets, ~0.9 s)
            // everything the device played is due — one batch of ~3,000.
            std::thread::sleep(Duration::from_millis(1_500));
            let before = port.sent.lock().unwrap().len();
            let t1 = Instant::now();
            r.pause();
            // Pause reached the output when its last All Notes Off
            // (channel 16) was sent — the worker sends them itself.
            let deadline = t1 + Duration::from_secs(30);
            while !port.sent.lock().unwrap()[before..].contains(&0x00_7B_BF)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            let paused = t1.elapsed();
            port.fast.store(true, Ordering::Relaxed);
            assert!(
                paused < Duration::from_millis(1_500),
                "Pause waited {paused:?} for the slow output"
            );
            std::thread::sleep(Duration::from_millis(100));
            let mut open = [0u128; 16];
            for m in port.sent.lock().unwrap().iter() {
                let (st, k, v) = ((m & 0xFF) as u8, ((m >> 8) & 0x7F) as u8, (m >> 16) & 0x7F);
                let ch = usize::from(st & 0x0F);
                match st & 0xF0 {
                    0x90 if v > 0 => open[ch] |= 1u128 << k,
                    0x80 | 0x90 => open[ch] &= !(1u128 << k),
                    0xB0 if k == 123 => open[ch] = 0,
                    _ => {}
                }
            }
            assert!(open.iter().all(|k| *k == 0), "a note was left sounding");
        }

        /// v1.0-Launch L-B06: OmniMIDI's KDMAPI takes 0.6 s to start (a
        /// large SoundFont loading; the log shows 9.4 s). Play returns at
        /// once, the default output is "connecting", the playhead runs and
        /// the song's messages for it are counted, not queued. When it opens
        /// it joins at the playhead: reset, the channel state there (Pan 0
        /// set at tick 0) and the long note held there struck again; none of
        /// the short notes that ended before the join is sent (no backlog
        /// dump) and later ones play in time.
        #[test]
        fn an_output_that_opens_late_joins_at_the_playhead_with_its_channel_state() {
            use plumspirit_engine::editor::controllers::{self, ControllerKind};
            use plumspirit_engine::history::{History, Operation};
            use std::time::{Duration, Instant};
            // A long note (key 50) and a short one (key 62) every 120 ticks.
            let mut notes = vec![(0u64, 20_000u64, 50u8, 90u8)];
            notes.extend((0..100u64).map(|i| (i * 120 + 60, 30, 62, 100)));
            let mut d = doc(&notes);
            let e =
                controllers::prepare_set(d.track(0).unwrap(), ControllerKind::PAN, 0, None, 0, 0)
                    .unwrap();
            History::default()
                .apply(
                    &mut d,
                    Operation::Events {
                        edit: e,
                        label: "Pan",
                    },
                )
                .unwrap();
            let port = Arc::new(FakePort::default());
            let p = Arc::clone(&port);
            let mut r = super::Rig::new();
            r.t.test_opener = Some(Arc::new(move |_t: &OutputTarget| {
                std::thread::sleep(Duration::from_millis(600));
                Ok(Arc::clone(&p) as Arc<dyn MidiPort>)
            }));
            r.t.set_default_output(OutputTarget::Kdmapi);
            let t0 = Instant::now();
            r.play(&d, 1);
            assert!(
                t0.elapsed() < Duration::from_millis(400),
                "Play did not wait for the output: {:?}",
                t0.elapsed()
            );
            assert_eq!(r.t.output_status().health, OutputHealth::Connecting);
            assert!(!r.t.output_status().is_warning());
            assert_eq!(r.t.setup(&d).routes[0], 1, "never on both outputs");
            // 0.25 s of song while it opens.
            for _ in 0..5 {
                let _ = r.pump(2_400, &d, 1);
            }
            assert!(r.t.position() >= 200, "the playhead runs");
            assert!(port.sent.lock().unwrap().is_empty(), "nothing sent yet");
            assert!(r.t.routes[0].not_ready.load(Ordering::Relaxed) > 0);
            // Wait for the open, then play on.
            let deadline = Instant::now() + Duration::from_secs(5);
            while r.t.routes[0].is_connecting() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let joined_at = r.t.position();
            for _ in 0..20 {
                let _ = r.pump(2_400, &d, 1);
            }
            std::thread::sleep(Duration::from_millis(60));
            assert_eq!(r.t.output_status().health, OutputHealth::Open);
            let sent = port.sent.lock().unwrap().clone();
            let pos = |m: u32| sent.iter().position(|x| *x == m);
            let reset = pos(0x00_79_B0).expect("Reset All Controllers first");
            let pan = pos(0x00_0A_B0).expect("Pan 0 restored on channel 1");
            let long = pos(0x5A_32_90).expect("the held long note struck again");
            assert!(reset < pan && pan < long, "{sent:x?}");
            // Short notes: only those starting after the join point.
            let shorts: Vec<usize> = sent
                .iter()
                .enumerate()
                .filter(|(_, m)| **m == 0x64_3E_90)
                .map(|(i, _)| i)
                .collect();
            // Short notes that had ended before the join, and every one
            // started by now (one more for the clock's lead).
            let ended = (0..100u64).filter(|i| i * 120 + 90 <= joined_at).count();
            let end = r.t.position();
            let started = (0..100u64).filter(|i| i * 120 + 60 < end).count() + 1;
            assert!(ended >= 2, "joined at tick {joined_at}");
            assert!(
                shorts.len() + ended <= started,
                "no backlog: {} short notes sent, {ended} had ended before the join, {started} started by tick {end}",
                shorts.len()
            );
            assert!(!shorts.is_empty(), "later notes play");
            assert!(shorts[0] > long);
            r.pause();
            std::thread::sleep(Duration::from_millis(60));
            assert!(held_keys(&port).is_empty());
        }

        /// v1.0-Launch L-B06: with every audible track on an external
        /// output, Play needs no SoundFont (with a track on the internal
        /// synthesizer it still says one is missing). Track 1's device is
        /// missing, so its long note falls back to the internal synthesizer,
        /// silent without a SoundFont (said once). When the SoundFont
        /// arrives the synthesizer joins at the playhead: the held note
        /// sounds, and the KDMAPI output is neither reset nor interrupted.
        #[test]
        fn external_playback_starts_without_a_soundfont_and_the_synthesizer_joins_later() {
            use std::time::Duration;
            let notes: Vec<(u64, u64, u8, u8)> = (0..40u64)
                .map(|i| (i * 240, 200, 60 + (i % 12) as u8, 100))
                .collect();
            let mut d = doc(&notes);
            let mut t = Transport {
                engine: Some(crate::audio::AudioEngine::headless(48_000)),
                ..Transport::default()
            };
            assert!(matches!(
                t.play(&d, 1),
                Some(TransportEvent::Unavailable(m)) if m.contains("No SoundFont")
            ));
            t.state = super::super::TransportState::Stopped;
            d.add_track().unwrap();
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(
                    &[plumspirit_engine::model::NoteRecord {
                        start_tick: 0,
                        duration_ticks: 20_000,
                        attrs: plumspirit_engine::model::NoteAttrs::new(
                            57,
                            120,
                            1,
                            0,
                            plumspirit_engine::model::NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                        sub: Default::default(),
                        group: 0,
                    }],
                    false,
                )
                .unwrap();
            route_track(&mut d, 1, OutputRoute::External("Gone".into()));
            let port = Arc::new(FakePort::default());
            let p = Arc::clone(&port);
            let mut r = super::Rig {
                t,
                last: (0.0, 0.0),
            };
            r.t.test_opener = Some(Arc::new(move |t: &OutputTarget| {
                if *t == OutputTarget::Device("Gone".into()) {
                    return Err("The MIDI output “Gone” is not connected.".into());
                }
                Ok(Arc::clone(&p) as Arc<dyn MidiPort>)
            }));
            r.t.set_default_output(OutputTarget::Kdmapi);
            let ev = r.t.play(&d, 1);
            let Some(TransportEvent::Notice(text)) = ev else {
                panic!("a notice: {ev:?}")
            };
            assert!(
                text.contains("not connected") && text.contains("No SoundFont"),
                "{text}"
            );
            r.engine().wait_playing(true);
            let mut audio = Vec::new();
            for _ in 0..10 {
                let (out, ev) = r.pump(4_800, &d, 1);
                assert!(
                    !ev.iter()
                        .any(|e| matches!(e, TransportEvent::Unavailable(_))),
                    "{ev:?}"
                );
                audio.extend(out);
            }
            std::thread::sleep(Duration::from_millis(60));
            let ons = |p: &FakePort| {
                p.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|m| *m & 0xF0 == 0x90 && (*m >> 16) & 0x7F > 0)
                    .count()
            };
            assert!(ons(&port) >= 3, "the output plays without a SoundFont");
            assert!(
                super::rms(&audio) < 1e-6,
                "the internal synthesizer is silent"
            );
            let resets = |p: &FakePort| {
                p.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|m| **m == 0x00_79_B0)
                    .count()
            };
            let (before_resets, before_ons) = (resets(&port), ons(&port));
            let at = r.t.position();
            r.engine()
                .send(super::super::Command::SetSoundFont(super::font()));
            let mut later = Vec::new();
            for _ in 0..10 {
                let (out, _) = r.pump(4_800, &d, 1);
                later.extend(out);
            }
            std::thread::sleep(Duration::from_millis(60));
            assert!(
                super::rms(&later[later.len() / 2..]) > 0.003,
                "the held note sounds once the synthesizer joined"
            );
            assert!(r.t.position() > at, "the playhead went on");
            assert_eq!(resets(&port), before_resets, "the output was not reset");
            assert!(ons(&port) > before_ons, "the output keeps playing");
            r.pause();
            std::thread::sleep(Duration::from_millis(60));
            assert!(held_keys(&port).is_empty());
        }

        #[test]
        fn play_reports_every_output_notice_once() {
            let mut d = doc(&[(0, 480, 60, 100)]);
            d.add_track().unwrap();
            route_track(&mut d, 1, OutputRoute::External("Gone B".into()));
            let mut t = Transport::default();
            t.set_default_output(OutputTarget::Device("Gone A".into()));
            let o = Opener::new(&[
                OutputTarget::Device("Gone A".into()),
                OutputTarget::Device("Gone B".into()),
            ]);
            t.prepare_routes_with(&d, &|x| o.open(x));
            assert_eq!(t.notices.len(), 2);
            let joined = (!t.notices.is_empty())
                .then(|| TransportEvent::Notice(std::mem::take(&mut t.notices).join(" ")));
            let Some(TransportEvent::Notice(text)) = joined else {
                panic!("a notice")
            };
            assert!(text.contains("Gone A") && text.contains("Gone B"));
        }

        #[test]
        fn the_output_list_puts_internal_first_and_always_shows_kdmapi() {
            let devs = vec![
                "Synth A".to_owned(),
                "Synth A".to_owned(),
                "MIDI Mapper".to_owned(),
            ];
            let l = outputs_from(&devs, Err("OmniMIDI is not installed.".into()), true);
            let labels: Vec<String> = l.iter().map(|o| o.label()).collect();
            assert_eq!(
                labels,
                [
                    "Internal SoundFont Synthesizer",
                    "Synth A",
                    "MIDI Mapper",
                    midi_out::KDMAPI_LABEL
                ]
            );
            assert!(l[0].available && l[1].available);
            let k = l.last().unwrap();
            assert!(!k.available);
            assert_eq!(k.reason.as_deref(), Some("OmniMIDI is not installed."));
            let ok = outputs_from(&[], Ok("KDMAPI 4.1.0 (revision 0)".into()), true);
            assert!(ok[1].available);
            assert_eq!(ok[1].reason.as_deref(), Some("KDMAPI 4.1.0 (revision 0)"));
            let other = outputs_from(&[], Ok("KDMAPI".into()), false);
            assert!(!other[1].available, "not on this platform");
            // A selected device that went away stays listed, marked unavailable.
            let stale = with_selected(l.clone(), &OutputTarget::Device("Old Synth".into()));
            assert_eq!(stale.len(), l.len() + 1);
            assert!(!stale.last().unwrap().available);
            assert_eq!(
                with_selected(l.clone(), &OutputTarget::Kdmapi).len(),
                l.len()
            );
        }

        #[test]
        fn the_default_output_setting_round_trips_and_rejects_junk() {
            for t in [
                OutputTarget::Internal,
                OutputTarget::Kdmapi,
                OutputTarget::Device("Roland SC-88 Pro: device:1".into()),
            ] {
                assert_eq!(OutputTarget::from_setting(&t.to_setting()), Some(t));
            }
            for junk in ["", "device:", "device:  ", "winmm", "Internal"] {
                assert_eq!(OutputTarget::from_setting(junk), None, "{junk:?}");
            }
        }

        #[test]
        fn this_build_lists_outputs_without_crashing() {
            let l = midi_out::list_outputs();
            assert_eq!(l[0].target, OutputTarget::Internal);
            assert!(l.iter().any(|o| o.target == OutputTarget::Kdmapi));
        }
    }
}
