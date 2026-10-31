//! External MIDI output routes (Windows MIDI output devices through winmm, and
//! OmniMIDI through KDMAPI).
//!
//! ```text
//!  synth worker ──ExternalSink (stamps each message with its ring position)──► route queue
//!                                                                                  │
//!  dispatcher thread ── sends each message when the device clock reaches it ───────┘──► MIDI device
//! ```
//!
//! The worker renders audio ahead of the device (up to the ring's ~85 ms). A
//! message for an external device is therefore not sent when the scheduler emits
//! it, but when the audio it belongs to is played, so external instruments stay
//! in time with the internal synthesizer (the device's own output latency is not
//! compensated).
//!
//! v1.0-Launch (L-B02, "the spam tone is lost"): each message carries its own
//! sample position — the block's ring position plus the message's frame offset
//! in the block ([`SynthSink::at_offset`]) — and the dispatcher sends it when the
//! **device clock** reaches that position: the audio callback publishes where the
//! device is and when ([`ClockAnchor`]), and the dispatcher interpolates between
//! callbacks and waits for each message with a high-resolution sleep plus a short
//! spin. Before, every message of a device period was released at once when the
//! callback consumed it (WASAPI: every 10 ms), which turned rapid repeated notes
//! into 100 Hz bursts.
//!
//! The dispatcher never holds the queue lock while it talks to the device, so a
//! slow device (a busy KDMAPI synth) never blocks the synth worker that queues
//! messages (L-B05). Stuck-note safety: the route remembers which keys it has
//! turned on. Pause, seek, loop restarts that reset the device, route changes and
//! closing all end them (note-offs, sustain off, All Notes Off) and drop queued
//! messages that were never heard — including a batch the dispatcher took out of
//! the queue just before (an epoch check under the send lock). A device that stops
//! accepting messages (unplugged) marks its route failed; the transport reports it
//! and the project is unchanged.
//!
//! Only Windows has a backend; elsewhere no external outputs are listed.

use super::engine::Shared;
use plumspirit_engine::model::conductor::{SYSEX_ALL_DEVICES, master_volume_sysex};
use plumspirit_engine::playback::SynthSink;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// Queued messages per route before new note-ons are dropped (the dispatcher is
/// not keeping up; counted as skipped).
const MAX_PENDING: usize = 262_144;

/// Messages the dispatcher takes from the queue at a time.
const DISPATCH_BATCH: usize = 4_096;

/// L-B06: an output with this many messages waiting is shown as catching up.
pub const CATCHING_UP: usize = DISPATCH_BATCH;

/// v1.0-Launch L-B03: with "keep every event", the worker stops rendering
/// ahead while a route has this many messages waiting (far enough below
/// [`MAX_PENDING`] that one rendering batch never reaches it).
pub const KEEP_ALL_HIGH_WATER: usize = MAX_PENDING - 131_072;

/// L-B02: where the audio device is — the anchor its latest callback
/// published: the ring position of the first sample it took (`read`), how many
/// samples (`took`, interleaved stereo) and when (`nanos`, since the engine's
/// epoch).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockAnchor {
    pub read: u64,
    pub took: u64,
    pub nanos: u64,
}

impl ClockAnchor {
    /// How far past `read` the clock may run before the next callback: the
    /// samples taken plus half again (a late callback never lets MIDI run more
    /// than half a period ahead of the audio; nothing runs ahead of audio the
    /// device did not take).
    fn reach(&self) -> u64 {
        self.took + self.took / 2
    }

    /// The ring position the device has reached at `now` (ns), `rate` ring
    /// samples per second (2 × the sample rate).
    pub fn position_at(&self, now: u64, rate: u64) -> u64 {
        let elapsed = now.saturating_sub(self.nanos);
        let adv = u128::from(elapsed) * u128::from(rate) / 1_000_000_000;
        self.read + u64::try_from(adv).unwrap_or(u64::MAX).min(self.reach())
    }

    /// When (ns) the clock reaches ring position `at`: now or before for a
    /// past position, `None` beyond what this anchor covers (wait for the
    /// next callback).
    pub fn due_at(&self, at: u64, rate: u64) -> Option<u64> {
        let Some(d) = at.checked_sub(self.read) else {
            return Some(self.nanos);
        };
        if d > self.reach() || rate == 0 {
            return None;
        }
        let ns = (u128::from(d) * 1_000_000_000).div_ceil(u128::from(rate));
        Some(self.nanos + u64::try_from(ns).unwrap_or(u64::MAX))
    }
}

/// A MIDI output the routes can write to.
pub trait MidiPort: Send + Sync {
    /// A short message: `status | d0 << 8 | d1 << 16`. False when the device
    /// refused it.
    fn short(&self, msg: u32) -> bool;
    /// A complete System Exclusive message.
    fn sysex(&self, bytes: &[u8]) -> bool;
}

#[cfg(windows)]
impl MidiPort for crate::platform::MidiOut {
    fn short(&self, msg: u32) -> bool {
        crate::platform::MidiOut::short(self, msg)
    }
    fn sysex(&self, bytes: &[u8]) -> bool {
        crate::platform::MidiOut::sysex(self, bytes)
    }
}

/// Names of the MIDI output devices on this computer (Windows; none elsewhere).
pub fn available_outputs() -> Vec<String> {
    crate::platform::midi_output_names()
}

/// Whether this build can play to external MIDI devices.
pub const EXTERNAL_SUPPORTED: bool = cfg!(windows);

/// Label of OmniMIDI's direct interface in every chooser.
pub const KDMAPI_LABEL: &str = "OmniMIDI (KDMAPI)";

/// Where tracks set to "Internal" play (v1.0-beta1 B1-0135, Audio Settings →
/// Default output; an application setting, never in the project). A track
/// routed to a named device in Track Properties keeps that device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputTarget {
    /// PlumSpirit's SoundFont synthesizer (always works).
    #[default]
    Internal,
    /// An installed Windows MIDI output device, by its system name.
    Device(String),
    /// OmniMIDI through its Keppy's Direct MIDI API (no WinMM layer).
    Kdmapi,
}

impl OutputTarget {
    pub fn label(&self) -> String {
        match self {
            OutputTarget::Internal => "Internal SoundFont Synthesizer".to_owned(),
            OutputTarget::Device(n) => n.clone(),
            OutputTarget::Kdmapi => KDMAPI_LABEL.to_owned(),
        }
    }

    pub fn kind(&self) -> ProviderKind {
        match self {
            OutputTarget::Internal => ProviderKind::Internal,
            OutputTarget::Device(_) => ProviderKind::Device,
            OutputTarget::Kdmapi => ProviderKind::Kdmapi,
        }
    }

    /// Settings text: `internal`, `kdmapi` or `device:<name>`.
    pub fn to_setting(&self) -> String {
        match self {
            OutputTarget::Internal => "internal".to_owned(),
            OutputTarget::Device(n) => format!("device:{n}"),
            OutputTarget::Kdmapi => "kdmapi".to_owned(),
        }
    }

    pub fn from_setting(s: &str) -> Option<OutputTarget> {
        match s {
            "internal" => Some(OutputTarget::Internal),
            "kdmapi" => Some(OutputTarget::Kdmapi),
            _ => s
                .strip_prefix("device:")
                .filter(|n| !n.trim().is_empty())
                .map(|n| OutputTarget::Device(n.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Internal,
    Device,
    Kdmapi,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Internal => "Internal SoundFont",
            ProviderKind::Device => "Windows MIDI device",
            ProviderKind::Kdmapi => "OmniMIDI KDMAPI",
        }
    }
}

/// One choice of the output chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    pub target: OutputTarget,
    pub available: bool,
    /// Why it cannot be used now (a sentence), or a detail (KDMAPI version).
    pub reason: Option<String>,
}

impl OutputInfo {
    pub fn label(&self) -> String {
        self.target.label()
    }

    pub fn kind(&self) -> ProviderKind {
        self.target.kind()
    }
}

/// What the system offers now: `devices` (WinMM names in device order) and
/// the KDMAPI probe (`Ok(version)` / `Err(reason)`). Internal comes first and
/// is always available; the KDMAPI entry is always listed (with its reason
/// when it cannot be used) so the choice is never hidden.
pub fn outputs_from(
    devices: &[String],
    kdmapi: Result<String, String>,
    external: bool,
) -> Vec<OutputInfo> {
    let mut list = vec![OutputInfo {
        target: OutputTarget::Internal,
        available: true,
        reason: None,
    }];
    let mut seen: Vec<&str> = Vec::new();
    for n in devices {
        if seen.contains(&n.as_str()) {
            continue; // two devices with one name: the first is the one opened
        }
        seen.push(n);
        list.push(OutputInfo {
            target: OutputTarget::Device(n.clone()),
            available: true,
            reason: None,
        });
    }
    let (available, reason) = if !external {
        (
            false,
            "OmniMIDI KDMAPI is available in the Windows version.".to_owned(),
        )
    } else {
        match kdmapi {
            Ok(v) => (true, v),
            Err(e) => (false, e),
        }
    };
    list.push(OutputInfo {
        target: OutputTarget::Kdmapi,
        available,
        reason: Some(reason),
    });
    list
}

/// The list with `selected` kept as an unavailable entry when the system no
/// longer offers it (a stale device name stays visible, never silently
/// replaced).
pub fn with_selected(mut list: Vec<OutputInfo>, selected: &OutputTarget) -> Vec<OutputInfo> {
    if !list.iter().any(|o| &o.target == selected) {
        list.push(OutputInfo {
            target: selected.clone(),
            available: false,
            reason: Some(format!("“{}” is not connected.", selected.label())),
        });
    }
    list
}

/// Every output PlumSpirit can play to now (device enumeration and the
/// KDMAPI probe; call off the UI thread, it can take a moment).
pub fn list_outputs() -> Vec<OutputInfo> {
    outputs_from(
        &available_outputs(),
        crate::platform::kdmapi_probe(),
        EXTERNAL_SUPPORTED,
    )
}

/// Opens an output synchronously (tests of the routing rules).
#[cfg(test)]
pub type OpenFn<'a> = dyn Fn(&OutputTarget) -> Result<Arc<dyn MidiPort>, String> + 'a;

/// L-B06: an opener a background thread can own ([`connect`]).
pub type SharedOpener = dyn Fn(&OutputTarget) -> Result<Arc<dyn MidiPort>, String> + Send + Sync;

/// Opens `target` for playback (`Internal` has no port).
pub fn open_target(target: &OutputTarget) -> Result<Arc<dyn MidiPort>, String> {
    match target {
        OutputTarget::Internal => Err("The internal synthesizer has no MIDI port.".to_owned()),
        OutputTarget::Device(n) => open_output(n),
        OutputTarget::Kdmapi => open_kdmapi(),
    }
}

/// OmniMIDI's KDMAPI entry points as plain closures (B1-0136): the platform
/// layer builds them from `OmniMIDI.dll` (`platform::kdmapi_fns`), tests use
/// fakes. None of them may panic (they wrap FFI calls).
pub struct KdmapiFns {
    /// `InitializeKDMAPIStream`: starts OmniMIDI's threads; false = failed.
    pub init: Box<dyn Fn() -> bool + Send + Sync>,
    /// `TerminateKDMAPIStream`.
    pub terminate: Box<dyn Fn() -> bool + Send + Sync>,
    /// `ResetKDMAPIStream` (resets every channel), when exported.
    pub reset: Option<Box<dyn Fn() + Send + Sync>>,
    /// `SendDirectData` (buffered; "fails quietly", so it reports nothing).
    pub send: Box<dyn Fn(u32) + Send + Sync>,
    /// `PrepareLongData` / `SendDirectLongData` / `UnprepareLongData` for a
    /// complete F0 … F7 message, when exported.
    pub sysex: Option<Box<SysexFn>>,
}

/// Sends one complete SysEx message; false when it was not taken.
pub type SysexFn = dyn Fn(&[u8]) -> bool + Send + Sync;

/// The one KDMAPI stream of the process: started by the first route that
/// opens it, shared by later ones, reset and terminated when the last one
/// closes. Start and stop are serialized, so a new stream never starts
/// while the old one is still terminating.
pub struct KdmapiStream {
    fns: KdmapiFns,
    lifecycle: Arc<Mutex<()>>,
    /// SysEx messages OmniMIDI refused or could not take (KDMAPI has no
    /// failure for short messages, so the route never fails on SysEx alone).
    pub sysex_refused: AtomicU64,
}

impl MidiPort for KdmapiStream {
    fn short(&self, msg: u32) -> bool {
        (self.fns.send)(msg);
        true
    }
    fn sysex(&self, bytes: &[u8]) -> bool {
        let ok = self.fns.sysex.as_ref().is_some_and(|f| f(bytes));
        if !ok && self.sysex_refused.fetch_add(1, Ordering::Relaxed) == 0 {
            log::warn!("OmniMIDI KDMAPI did not take a SysEx message (Master Volume)");
        }
        true
    }
}

impl Drop for KdmapiStream {
    fn drop(&mut self) {
        let _serial = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(reset) = &self.fns.reset {
            reset();
        }
        if !(self.fns.terminate)() {
            log::warn!("OmniMIDI could not stop its KDMAPI stream cleanly");
        }
        log::info!("OmniMIDI KDMAPI stream closed");
    }
}

/// Where the shared stream lives (one per process; tests make their own).
#[derive(Default)]
pub struct KdmapiSlot {
    stream: Mutex<std::sync::Weak<KdmapiStream>>,
    lifecycle: Arc<Mutex<()>>,
}

impl KdmapiSlot {
    /// The running stream, or a new one from `load` (loaded and started
    /// only when none is running).
    pub fn open(
        &self,
        load: impl FnOnce() -> Result<KdmapiFns, String>,
    ) -> Result<Arc<KdmapiStream>, String> {
        let mut slot = self.stream.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = slot.upgrade() {
            return Ok(s);
        }
        let fns = load()?;
        let _serial = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        if !(fns.init)() {
            return Err(format!(
                "{KDMAPI_LABEL} could not start (InitializeKDMAPIStream failed; OmniMIDI may be busy or misconfigured)."
            ));
        }
        log::info!("OmniMIDI KDMAPI stream started");
        let s = Arc::new(KdmapiStream {
            fns,
            lifecycle: Arc::clone(&self.lifecycle),
            sysex_refused: AtomicU64::new(0),
        });
        *slot = Arc::downgrade(&s);
        Ok(s)
    }

    /// Whether a stream is running now.
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.stream
            .lock()
            .map(|w| w.strong_count() > 0)
            .unwrap_or(false)
    }
}

static KDMAPI: std::sync::LazyLock<KdmapiSlot> = std::sync::LazyLock::new(KdmapiSlot::default);

/// OmniMIDI through KDMAPI (B1-0136): the process-wide stream, started on
/// first use. Fails with the probe's reason when OmniMIDI cannot be used.
fn open_kdmapi() -> Result<Arc<dyn MidiPort>, String> {
    KDMAPI
        .open(crate::platform::kdmapi_fns)
        .map(|s| s as Arc<dyn MidiPort>)
}

/// The default output's state, for the transport and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputHealth {
    /// The internal synthesizer (nothing to open).
    Internal,
    /// Opens at the next Play.
    Idle,
    /// L-B06: still opening (a slow KDMAPI start) — playback runs, its
    /// tracks join at the playhead when it is ready.
    Connecting,
    /// Open and accepting messages.
    Open,
    /// Could not be opened; tracks play on the internal synthesizer.
    Unavailable(String),
    /// Stopped accepting messages during playback (unplugged).
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputStatus {
    pub target: OutputTarget,
    pub health: OutputHealth,
    /// Messages dropped because the device fell behind (queue full).
    pub dropped: u64,
    /// Messages sent, and the largest send delay after a message was due
    /// (ring samples; the ring is interleaved stereo).
    pub sent: u64,
    pub max_lag_samples: u64,
    /// L-B06: messages due or queued and not yet sent (a slow output
    /// catching up).
    pub waiting: usize,
}

impl OutputStatus {
    /// Something the user should look at.
    pub fn is_warning(&self) -> bool {
        matches!(
            self.health,
            OutputHealth::Unavailable(_) | OutputHealth::Failed(_)
        ) || self.dropped > 0
    }

    pub fn health_text(&self) -> String {
        let mut s = match &self.health {
            OutputHealth::Internal => "ready".to_owned(),
            OutputHealth::Idle => "opens at the next Play".to_owned(),
            OutputHealth::Connecting => {
                "connecting — playback runs; its tracks join at the playhead when it is ready"
                    .to_owned()
            }
            OutputHealth::Open => "open".to_owned(),
            OutputHealth::Unavailable(r) => {
                format!("unavailable — {r} The internal synthesizer plays instead.")
            }
            OutputHealth::Failed(r) => {
                format!("stopped — {r} The internal synthesizer plays instead.")
            }
        };
        if self.waiting >= CATCHING_UP {
            s.push_str(&format!(
                " · catching up ({} messages waiting)",
                self.waiting
            ));
        }
        if self.dropped > 0 {
            s.push_str(&format!(" · {} messages dropped", self.dropped));
        }
        s
    }
}

/// Opens the output named `name`.
pub fn open_output(name: &str) -> Result<Arc<dyn MidiPort>, String> {
    let names = available_outputs();
    let Some(id) = names.iter().position(|n| n == name) else {
        return Err(format!("The MIDI output “{name}” is not connected."));
    };
    open_id(id as u32, name)
}

#[cfg(windows)]
fn open_id(id: u32, name: &str) -> Result<Arc<dyn MidiPort>, String> {
    crate::platform::MidiOut::open(id)
        .map(|p| Arc::new(p) as Arc<dyn MidiPort>)
        .map_err(|code| {
            format!("The MIDI output “{name}” could not be opened (it may be in use by another program; error {code}).")
        })
}

#[cfg(not(windows))]
fn open_id(_id: u32, name: &str) -> Result<Arc<dyn MidiPort>, String> {
    Err(format!(
        "The MIDI output “{name}” cannot be used: external MIDI output is available on Windows only."
    ))
}

fn pack(status: u8, d0: u8, d1: u8) -> u32 {
    u32::from(status) | (u32::from(d0 & 0x7F) << 8) | (u32::from(d1 & 0x7F) << 16)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    Short(u32),
    Master(u16),
    /// v1.0-beta2 (B2-F12B): a system reset of the song, sent as it is.
    Sysex(Arc<[u8]>),
}

struct RouteState {
    /// (ring sample position at which the message is due, message).
    pending: VecDeque<(u64, Msg)>,
}

/// What the device was sent (under the send lock).
struct OutState {
    /// Keys turned on and not yet off, per channel (bit = key).
    sounding: [u128; 16],
}

/// L-B06: where a route is on its way to playing (`ExternalRoute::link`).
/// `Connecting` → (port opened on its thread) `Opened` → (the transport asked
/// the worker to re-position) `Joining` → (the worker's next seek reset it)
/// `Live`. Only a live route queues messages: before, the song's messages for
/// it are counted in `not_ready` and never sent later (no backlog dump); the
/// seek that makes it live restores every channel at the playhead and strikes
/// the notes held there again.
const LINK_CONNECTING: u8 = 0;
const LINK_OPENED: u8 = 1;
const LINK_JOINING: u8 = 2;
const LINK_LIVE: u8 = 3;

/// One external output in use by the playing project.
pub struct ExternalRoute {
    pub name: String,
    /// What it plays to (never `Internal`): tracks routed to a device match
    /// `Device(name)`; the default output matches its own target.
    pub target: OutputTarget,
    /// Set once: at creation, or by the thread that opens it ([`connect`]).
    port: OnceLock<Arc<dyn MidiPort>>,
    link: AtomicU8,
    /// Why opening it failed (it is then failed too).
    open_error: Mutex<Option<String>>,
    /// Messages of the song not sent because the output was not live yet.
    pub not_ready: AtomicU64,
    /// The queue (the worker pushes, the dispatcher takes; never held while
    /// the device is called).
    state: Mutex<RouteState>,
    /// Serializes everything sent to the device.
    out: Mutex<OutState>,
    /// Incremented by every flush: a batch taken from the queue before a
    /// flush is never sent after it.
    flushes: AtomicU64,
    failed: AtomicBool,
    /// Messages dropped because the queue was full.
    pub dropped: AtomicU64,
    /// B1-0137: messages handed to the port, and the largest delay between a
    /// message becoming due and its send (ring samples).
    pub sent: AtomicU64,
    pub max_lag: AtomicU64,
}

impl ExternalRoute {
    #[cfg(test)]
    pub fn new(name: String, port: Arc<dyn MidiPort>) -> Arc<ExternalRoute> {
        Self::for_target(OutputTarget::Device(name), port)
    }

    /// A route to an output that is already open: live at once.
    #[cfg(test)]
    pub fn for_target(target: OutputTarget, port: Arc<dyn MidiPort>) -> Arc<ExternalRoute> {
        let r = Self::unopened(target);
        let _ = r.port.set(port);
        r.link.store(LINK_LIVE, Ordering::Release);
        Arc::new(r)
    }

    fn unopened(target: OutputTarget) -> ExternalRoute {
        ExternalRoute {
            name: target.label(),
            target,
            port: OnceLock::new(),
            link: AtomicU8::new(LINK_CONNECTING),
            open_error: Mutex::new(None),
            not_ready: AtomicU64::new(0),
            state: Mutex::new(RouteState {
                pending: VecDeque::new(),
            }),
            out: Mutex::new(OutState { sounding: [0; 16] }),
            flushes: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            max_lag: AtomicU64::new(0),
        }
    }

    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// L-B06: the result of opening its port (on the opening thread).
    fn attach(&self, opened: Result<Arc<dyn MidiPort>, String>) {
        match opened {
            Ok(port) => {
                let _ = self.port.set(port);
                let _ = self.link.compare_exchange(
                    LINK_CONNECTING,
                    LINK_OPENED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            Err(e) => {
                if let Ok(mut slot) = self.open_error.lock() {
                    *slot = Some(e);
                }
                self.failed.store(true, Ordering::Release);
            }
        }
    }

    /// Still opening (neither open nor failed).
    pub fn is_connecting(&self) -> bool {
        self.link.load(Ordering::Acquire) == LINK_CONNECTING && !self.has_failed()
    }

    /// Why it could not be opened.
    pub fn open_error(&self) -> Option<String> {
        self.open_error.lock().ok().and_then(|e| e.clone())
    }

    /// Takes an output that finished opening into the join (`Opened` →
    /// `Joining`): true once per route — the caller then re-positions the
    /// worker, whose seek makes it live.
    pub fn begin_join(&self) -> bool {
        !self.has_failed()
            && self
                .link
                .compare_exchange(
                    LINK_OPENED,
                    LINK_JOINING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// The worker's seek reset it: an opened output goes live from here.
    fn join(&self) {
        if matches!(
            self.link.load(Ordering::Acquire),
            LINK_OPENED | LINK_JOINING
        ) {
            self.link.store(LINK_LIVE, Ordering::Release);
        }
    }

    /// Queues and sends the song's messages.
    pub fn is_live(&self) -> bool {
        self.link.load(Ordering::Acquire) == LINK_LIVE
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_out(&self) -> std::sync::MutexGuard<'_, OutState> {
        self.out.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Sends one message now and tracks sounding keys. Stops at the first failure.
    fn send(&self, st: &mut OutState, msg: Msg) {
        if self.has_failed() {
            return;
        }
        // Not open yet: nothing was ever sent to it.
        let Some(port) = self.port.get() else {
            return;
        };
        let ok = match msg {
            Msg::Short(m) => {
                let (status, key, vel) =
                    ((m & 0xFF) as u8, ((m >> 8) & 0x7F) as u8, (m >> 16) as u8);
                let ch = usize::from(status & 0x0F);
                match status & 0xF0 {
                    0x90 if vel > 0 => st.sounding[ch] |= 1u128 << key,
                    0x80 | 0x90 => st.sounding[ch] &= !(1u128 << key),
                    _ => {}
                }
                port.short(m)
            }
            Msg::Master(v) => {
                let mut bytes = vec![0xF0];
                bytes.extend_from_slice(&master_volume_sysex(SYSEX_ALL_DEVICES, v));
                port.sysex(&bytes)
            }
            // Sounding keys stay tracked: if the device keeps notes through a
            // reset, Stop still ends them.
            Msg::Sysex(bytes) => port.sysex(&bytes),
        };
        self.sent.fetch_add(1, Ordering::Relaxed);
        if !ok {
            self.failed.store(true, Ordering::Release);
            log::warn!("MIDI output “{}” stopped accepting messages", self.name);
        }
    }

    fn push(&self, at: u64, msg: Msg) {
        // L-B06: not live yet — counted, never sent later.
        if !self.is_live() {
            self.not_ready.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut st = self.lock();
        if st.pending.len() >= MAX_PENDING
            && matches!(msg, Msg::Short(m) if m & 0xF0 == 0x90 && (m >> 16) & 0x7F > 0)
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        st.pending.push_back((at, msg));
    }

    /// Sends every queued message due at ring position `read` (in queue
    /// order, a bounded batch at a time) and returns the position of the
    /// next one still queued. The queue lock is released before the device
    /// is called.
    pub fn dispatch(&self, read: u64) -> Option<u64> {
        loop {
            let flushes = self.flushes.load(Ordering::Acquire);
            let (batch, next) = {
                let mut st = self.lock();
                let mut batch = Vec::new();
                while let Some(&(at, _)) = st.pending.front() {
                    if at > read || batch.len() >= DISPATCH_BATCH {
                        break;
                    }
                    let Some((at, msg)) = st.pending.pop_front() else {
                        break;
                    };
                    self.max_lag.fetch_max(read - at, Ordering::Relaxed);
                    batch.push(msg);
                }
                (batch, st.pending.front().map(|p| p.0))
            };
            if batch.is_empty() {
                return next;
            }
            {
                let mut out = self.lock_out();
                for msg in batch {
                    // A flush since the batch was taken: the rest was never
                    // heard. Checked per message, so a flush (Pause, seek,
                    // route change on the worker) waits for one message of a
                    // slow device, never for a whole batch (L-B05).
                    if self.flushes.load(Ordering::Acquire) != flushes {
                        break;
                    }
                    self.send(&mut out, msg);
                }
            }
            match next {
                Some(at) if at <= read => continue,
                _ => return next,
            }
        }
    }

    /// Messages waiting to be sent.
    pub fn pending_len(&self) -> usize {
        self.lock().pending.len()
    }

    #[cfg(test)]
    pub fn has_pending(&self) -> bool {
        !self.lock().pending.is_empty()
    }

    /// Drops what was never heard and ends every sounding note now.
    pub fn flush_now(&self) {
        {
            let mut st = self.lock();
            st.pending.clear();
            self.flushes.fetch_add(1, Ordering::AcqRel);
        }
        if self.port.get().is_none() {
            return; // never opened: nothing sounds
        }
        let mut out = self.lock_out();
        for ch in 0..16u8 {
            let mut keys = out.sounding[usize::from(ch)];
            while keys != 0 {
                let k = keys.trailing_zeros() as u8;
                keys &= keys - 1;
                self.send(&mut out, Msg::Short(pack(0x80 | ch, k, 64)));
            }
            out.sounding[usize::from(ch)] = 0;
            // Sustain off, then All Notes Off (voices held by the pedal).
            self.send(&mut out, Msg::Short(pack(0xB0 | ch, 64, 0)));
            self.send(&mut out, Msg::Short(pack(0xB0 | ch, 123, 0)));
        }
    }
}

impl Drop for ExternalRoute {
    fn drop(&mut self) {
        self.flush_now();
    }
}

/// L-B06: a route to `target` that opens on its own thread, so a slow start
/// (OmniMIDI loading a large SoundFont before `InitializeKDMAPIStream`
/// returns, `plumspirit-flopp.log`: 9.4 s) never holds the UI thread, the
/// playhead or the other outputs. The route is `Connecting` until the open
/// returns; dropped before that, the port opened late is closed at once.
pub fn connect(target: OutputTarget, opener: Arc<SharedOpener>) -> Arc<ExternalRoute> {
    let route = Arc::new(ExternalRoute::unopened(target.clone()));
    let weak = Arc::downgrade(&route);
    let spawned = std::thread::Builder::new()
        .name("plumspirit-midi-open".into())
        .spawn(move || {
            let opened = opener(&target);
            // Nobody waits for it any more: what was opened closes here.
            if let Some(r) = weak.upgrade() {
                r.attach(opened);
            }
        });
    if let Err(e) = spawned {
        route.attach(Err(format!(
            "The MIDI output “{}” could not be opened (no thread: {e}).",
            route.name
        )));
    }
    route
}

/// The worker's view of a route: queues messages at their ring position —
/// the block's (`stamp`, set by the worker before every scheduler call) plus
/// the message's frame offset in the block (L-B02).
pub struct ExternalSink {
    pub route: Arc<ExternalRoute>,
    pub stamp: u64,
    /// Frames into the block (stereo ring: 2 samples per frame).
    pub offset: u32,
}

impl ExternalSink {
    pub fn new(route: Arc<ExternalRoute>) -> ExternalSink {
        ExternalSink {
            route,
            stamp: 0,
            offset: 0,
        }
    }

    /// The block now scheduled starts at ring position `stamp`.
    pub fn set_block(&mut self, stamp: u64) {
        self.stamp = stamp;
        self.offset = 0;
    }

    fn at(&self) -> u64 {
        self.stamp + 2 * u64::from(self.offset)
    }
}

impl SynthSink for ExternalSink {
    // v0.9: a key above 127 (256-Key Mode) is never sent to a MIDI output —
    // `pack` would fold it onto another key.
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        if key > super::synth::MIDI_TOP_KEY {
            return;
        }
        self.route.push(
            self.at(),
            Msg::Short(pack(0x90 | (channel & 15), key, velocity)),
        );
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        if key > super::synth::MIDI_TOP_KEY {
            return;
        }
        self.route
            .push(self.at(), Msg::Short(pack(0x80 | (channel & 15), key, 64)));
    }
    fn channel_message(&mut self, status: u8, d0: u8, d1: u8) {
        self.route.push(self.at(), Msg::Short(pack(status, d0, d1)));
    }
    fn silence(&mut self) {
        self.route.flush_now();
    }
    /// Stops everything and brings every channel to the General MIDI power-on
    /// state (the scheduler then restores the song's state from there). The
    /// seek this belongs to is where an output that finished opening joins
    /// (L-B06).
    fn reset(&mut self) {
        self.route.join();
        self.route.flush_now();
        for ch in 0..16u8 {
            for (st, d0, d1) in [
                (0xB0, 121, 0), // Reset All Controllers
                (0xB0, 0, 0),   // Bank Select MSB / LSB
                (0xB0, 32, 0),
                (0xC0, 0, 0),  // Program 1
                (0xE0, 0, 64), // Pitch Bend center
                (0xB0, 7, 100),
                (0xB0, 10, 64),
                (0xB0, 11, 127),
                (0xB0, 91, 40),
                (0xB0, 93, 0),
            ] {
                self.route
                    .push(self.at(), Msg::Short(pack(st | ch, d0, d1)));
            }
        }
    }
    fn master_volume(&mut self, value: u16) {
        self.route.push(self.at(), Msg::Master(value));
    }
    /// B2-F12B: the song's reset itself (the device handles GM / GS / XG).
    fn system_reset(&mut self, message: &[u8]) {
        self.route.push(self.at(), Msg::Sysex(message.into()));
    }
    fn timed(&self) -> bool {
        true
    }
    fn at_offset(&mut self, frames: u32) {
        self.offset = frames;
    }
}

/// Sends queued messages as the device plays them. One thread per audio engine,
/// started only when an external route is in use.
pub struct Dispatcher {
    routes: Arc<Mutex<Vec<Arc<ExternalRoute>>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// L-B02: waits until `due` (ns since `epoch`): a high-resolution sleep to
/// shortly before, then a short spin (Windows 10 1803+: `std::thread::sleep`
/// uses a high-resolution waitable timer).
fn wait_until(shared: &Shared, due: u64) {
    loop {
        let now = shared.now_nanos();
        let Some(left) = due.checked_sub(now).filter(|&l| l > 0) else {
            return;
        };
        if left > 2_000_000 {
            std::thread::sleep(Duration::from_nanos(left - 1_000_000));
        } else if left > 400_000 {
            std::thread::sleep(Duration::from_nanos(left - 300_000));
        } else {
            std::hint::spin_loop();
            std::thread::yield_now();
        }
    }
}

impl Dispatcher {
    pub fn start(shared: &Arc<Shared>, sample_rate: u32) -> Result<Dispatcher, String> {
        let routes: Arc<Mutex<Vec<Arc<ExternalRoute>>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s, sh) = (Arc::clone(&routes), Arc::clone(&stop), Arc::clone(shared));
        let rate = 2 * u64::from(sample_rate.max(1));
        let handle = std::thread::Builder::new()
            .name("plumspirit-midi-out".into())
            .spawn(move || {
                // The furthest position sent (the clock never goes back).
                let mut released = 0u64;
                while !s.load(Ordering::Acquire) {
                    let list: Vec<Arc<ExternalRoute>> =
                        r.lock().map(|l| l.clone()).unwrap_or_default();
                    let clock = sh.clock();
                    let pos = match clock {
                        Some(c) => c.position_at(sh.now_nanos(), rate),
                        // No callback yet: what the device has taken.
                        None => sh.ring.read_pos(),
                    };
                    released = released.max(pos);
                    let mut next: Option<u64> = None;
                    for route in &list {
                        if let Some(at) = route.dispatch(released) {
                            next = Some(next.map_or(at, |n| n.min(at)));
                        }
                    }
                    match (next, clock) {
                        (Some(at), Some(c)) => match c.due_at(at, rate) {
                            Some(due) => wait_until(&sh, due),
                            // Beyond this callback's audio: the next one moves the clock.
                            None => std::thread::sleep(Duration::from_micros(500)),
                        },
                        (Some(_), None) => std::thread::sleep(Duration::from_micros(500)),
                        (None, _) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
            .map_err(|e| format!("The MIDI output thread could not start: {e}."))?;
        Ok(Dispatcher {
            routes,
            stop,
            handle: Some(handle),
        })
    }

    pub fn set_routes(&self, routes: Vec<Arc<ExternalRoute>>) {
        if let Ok(mut l) = self.routes.lock() {
            *l = routes;
        }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A port that records what it was sent (and can fail on demand).
    #[derive(Default)]
    pub struct FakePort {
        pub sent: Mutex<Vec<u32>>,
        pub sysex: Mutex<Vec<Vec<u8>>>,
        pub fail: AtomicBool,
    }

    impl MidiPort for FakePort {
        fn short(&self, msg: u32) -> bool {
            if self.fail.load(Ordering::Relaxed) {
                return false;
            }
            self.sent.lock().unwrap().push(msg);
            true
        }
        fn sysex(&self, bytes: &[u8]) -> bool {
            self.sysex.lock().unwrap().push(bytes.to_vec());
            !self.fail.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn messages_wait_until_their_audio_is_played() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 128,
            offset: 0,
        };
        sink.note_on(0, 60, 100);
        sink.stamp = 256;
        sink.note_off(0, 60);
        route.dispatch(127);
        assert!(port.sent.lock().unwrap().is_empty());
        route.dispatch(128);
        assert_eq!(*port.sent.lock().unwrap(), vec![pack(0x90, 60, 100)]);
        route.dispatch(1_000);
        assert_eq!(port.sent.lock().unwrap().len(), 2);
        assert!(!route.has_pending());
        // B1-0137: sends and the worst delay after a message was due.
        assert_eq!(route.sent.load(Ordering::Relaxed), 2);
        assert_eq!(route.max_lag.load(Ordering::Relaxed), 1_000 - 256);
    }

    /// v0.9 256-Key Mode: a key above 127 never becomes a MIDI message (it
    /// would be folded onto key & 127); keys 0–127 are sent as before.
    /// L-B06: the output states the transport chip and Audio Settings show.
    #[test]
    fn the_output_status_names_connecting_and_catching_up() {
        let mut st = OutputStatus {
            target: OutputTarget::Kdmapi,
            health: OutputHealth::Connecting,
            dropped: 0,
            sent: 0,
            max_lag_samples: 0,
            waiting: 0,
        };
        assert!(st.health_text().starts_with("connecting"));
        assert!(!st.is_warning());
        st.health = OutputHealth::Open;
        st.waiting = CATCHING_UP;
        assert_eq!(
            st.health_text(),
            "open · catching up (4096 messages waiting)"
        );
        assert!(!st.is_warning());
    }

    #[test]
    fn keys_above_127_are_never_sent_to_a_midi_output() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        sink.note_on(0, 200, 100);
        sink.note_off(0, 200);
        sink.note_on(0, 255, 100);
        assert!(!route.has_pending());
        sink.note_on(0, 127, 100);
        sink.note_off(0, 127);
        route.dispatch(10);
        assert_eq!(
            *port.sent.lock().unwrap(),
            vec![pack(0x90, 127, 100), pack(0x80, 127, 64)]
        );
    }

    #[test]
    fn flushing_ends_sounding_notes_and_drops_unheard_ones() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        sink.note_on(3, 64, 90);
        route.dispatch(0); // heard
        sink.stamp = 64;
        sink.note_on(3, 67, 90); // never heard
        sink.silence();
        let sent = port.sent.lock().unwrap().clone();
        assert!(sent.contains(&pack(0x83, 64, 64)), "the sounding note ends");
        assert!(
            !sent.contains(&pack(0x93, 67, 90)),
            "the unheard note never starts"
        );
        assert!(sent.contains(&pack(0xB3, 123, 0)));
        assert!(!route.has_pending());
    }

    #[test]
    fn a_failing_device_marks_the_route_failed() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        port.fail.store(true, Ordering::Relaxed);
        sink.note_on(0, 60, 100);
        route.dispatch(10);
        assert!(route.has_failed());
    }

    #[test]
    fn reset_brings_channels_to_general_midi_defaults_and_master_volume_is_sysex() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        sink.reset();
        sink.master_volume(8_192);
        route.dispatch(0);
        let sent = port.sent.lock().unwrap().clone();
        assert!(sent.contains(&pack(0xB9, 121, 0)));
        assert!(sent.contains(&pack(0xC9, 0, 0)));
        let sx = port.sysex.lock().unwrap().clone();
        assert_eq!(
            sx,
            vec![vec![0xF0, 0x7F, 0x7F, 0x04, 0x01, 0x00, 0x40, 0xF7]]
        );
    }

    /// B2-F12B: a song's system reset reaches an external output as the
    /// message itself, in order with the rest; the internal-synthesizer
    /// default turns it into the General MIDI power-on channel messages.
    #[test]
    fn a_sent_system_reset_is_the_songs_own_sysex_on_an_external_output() {
        let port = Arc::new(FakePort::default());
        let route = ExternalRoute::new("fake".into(), port.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        let gs = [
            0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7,
        ];
        sink.note_on(0, 60, 100);
        sink.system_reset(&gs);
        sink.channel_message(0xC0, 5, 0);
        route.dispatch(0);
        assert_eq!(port.sysex.lock().unwrap().clone(), vec![gs.to_vec()]);
        assert!(port.sent.lock().unwrap().contains(&pack(0xC0, 5, 0)));
        // Stop still ends the key the device may hold.
        route.flush_now();
        assert!(port.sent.lock().unwrap().contains(&pack(0x80, 60, 64)));
        // The default (internal synthesizer): channel messages, no voices cut.
        struct Ch(Vec<(u8, u8, u8)>);
        impl SynthSink for Ch {
            fn note_on(&mut self, _: u8, _: u8, _: u8) {}
            fn note_off(&mut self, _: u8, _: u8) {}
            fn channel_message(&mut self, s: u8, a: u8, b: u8) {
                self.0.push((s, a, b));
            }
            fn silence(&mut self) {
                panic!("a reset does not silence");
            }
            fn reset(&mut self) {
                panic!("a reset does not stop voices");
            }
        }
        let mut c = Ch(Vec::new());
        c.system_reset(&gs);
        assert_eq!(
            c.0.len(),
            16 * plumspirit_engine::playback::GM_POWER_ON.len()
        );
        assert!(c.0.contains(&(0xC9, 0, 0)) && c.0.contains(&(0xB0, 121, 0)));
    }

    // ---- KDMAPI stream (B1-0136), with fake OmniMIDI functions ----------------------

    /// What the fake OmniMIDI was asked to do, in order.
    #[derive(Default)]
    struct Omni {
        log: Mutex<Vec<String>>,
        init_fails: AtomicBool,
    }

    impl Omni {
        fn note(&self, s: String) {
            self.log.lock().unwrap().push(s);
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn count(&self, what: &str) -> usize {
            self.log().iter().filter(|l| *l == what).count()
        }
    }

    fn fake_fns(o: &Arc<Omni>, sysex: bool) -> KdmapiFns {
        let (a, b, c, d, e) = (o.clone(), o.clone(), o.clone(), o.clone(), o.clone());
        KdmapiFns {
            init: Box::new(move || {
                a.note("init".into());
                !a.init_fails.load(Ordering::Relaxed)
            }),
            terminate: Box::new(move || {
                b.note("terminate".into());
                true
            }),
            reset: Some(Box::new(move || c.note("reset".into()))),
            send: Box::new(move |m| d.note(format!("{m:06x}"))),
            sysex: sysex.then(|| {
                Box::new(move |bytes: &[u8]| {
                    e.note(format!("sysex {}", bytes.len()));
                    true
                }) as Box<SysexFn>
            }),
        }
    }

    #[test]
    fn two_routes_share_one_kdmapi_stream_started_once_and_stopped_once() {
        let slot = KdmapiSlot::default();
        let o = Arc::new(Omni::default());
        let a = slot.open(|| Ok(fake_fns(&o, true))).unwrap();
        let mut loads = 0;
        let b = slot
            .open(|| {
                loads += 1;
                Ok(fake_fns(&o, true))
            })
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(loads, 0, "a running stream is not loaded again");
        assert_eq!(o.count("init"), 1);
        assert!(slot.is_running());
        drop(a);
        assert_eq!(o.count("terminate"), 0, "still used by the second route");
        drop(b);
        assert_eq!(o.log()[o.log().len() - 2..], ["reset", "terminate"]);
        assert!(!slot.is_running());
        // Opening again starts a fresh stream.
        let c = slot.open(|| Ok(fake_fns(&o, true))).unwrap();
        assert_eq!(o.count("init"), 2);
        drop(c);
        assert_eq!(o.count("terminate"), 2);
    }

    #[test]
    fn a_kdmapi_route_ends_its_notes_before_the_stream_resets_and_stops() {
        let slot = KdmapiSlot::default();
        let o = Arc::new(Omni::default());
        let stream = slot.open(|| Ok(fake_fns(&o, true))).unwrap();
        let route = ExternalRoute::for_target(OutputTarget::Kdmapi, stream);
        assert_eq!(route.name, KDMAPI_LABEL);
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        sink.note_on(3, 64, 90);
        sink.master_volume(16_383);
        route.dispatch(0);
        drop(sink);
        drop(route); // pause / stop / switch / shutdown all end here
        let log = o.log();
        let on = log
            .iter()
            .position(|l| l == "5a4093")
            .expect("note-on sent");
        let off = log
            .iter()
            .position(|l| l == "404083")
            .expect("note-off sent");
        let all_off = log
            .iter()
            .position(|l| l == "007bb3")
            .expect("All Notes Off");
        let reset = log.iter().position(|l| l == "reset").unwrap();
        let term = log.iter().position(|l| l == "terminate").unwrap();
        assert!(
            on < off && off < all_off && all_off < reset && reset < term,
            "{log:?}"
        );
        assert!(
            log.contains(&"sysex 8".to_owned()),
            "Master Volume through long data"
        );
    }

    #[test]
    fn a_kdmapi_stream_that_cannot_start_says_why_and_is_not_kept() {
        let slot = KdmapiSlot::default();
        let o = Arc::new(Omni::default());
        o.init_fails.store(true, Ordering::Relaxed);
        let e = slot.open(|| Ok(fake_fns(&o, true))).err().unwrap();
        assert!(e.contains("could not start"), "{e}");
        assert!(!slot.is_running());
        assert_eq!(o.count("terminate"), 0, "nothing to stop");
        let e = slot
            .open(|| Err("OmniMIDI is not installed.".into()))
            .err()
            .unwrap();
        assert_eq!(e, "OmniMIDI is not installed.");
        // This build (Linux) explains instead of crashing.
        if !cfg!(windows) {
            let e = open_target(&OutputTarget::Kdmapi).err().unwrap();
            assert!(e.contains("Windows"), "{e}");
        }
    }

    #[test]
    fn kdmapi_without_long_data_skips_sysex_without_failing_the_route() {
        let slot = KdmapiSlot::default();
        let o = Arc::new(Omni::default());
        let stream = slot.open(|| Ok(fake_fns(&o, false))).unwrap();
        let route = ExternalRoute::for_target(OutputTarget::Kdmapi, stream.clone());
        let mut sink = ExternalSink {
            route: Arc::clone(&route),
            stamp: 0,
            offset: 0,
        };
        sink.master_volume(100);
        sink.master_volume(200);
        sink.note_on(0, 60, 1);
        route.dispatch(0);
        assert!(!route.has_failed());
        assert_eq!(stream.sysex_refused.load(Ordering::Relaxed), 2);
        assert!(
            o.log().contains(&"013c90".to_owned()),
            "velocity 1 is sent as 1"
        );
    }

    /// v1.0-Launch L-B02: the device clock interpolates between callbacks,
    /// never runs more than half a callback past the audio taken, and maps
    /// positions to due times consistently (no early wake-up after rounding).
    #[test]
    fn the_device_clock_interpolates_between_callbacks_and_stops_at_its_reach() {
        let rate = 96_000; // 48 kHz stereo ring samples per second
        let c = ClockAnchor {
            read: 10_000,
            took: 960,
            nanos: 1_000_000_000,
        };
        assert_eq!(c.position_at(1_000_000_000, rate), 10_000);
        assert_eq!(c.position_at(1_005_000_000, rate), 10_480); // 5 ms
        assert_eq!(c.position_at(1_015_000_000, rate), 11_440); // capped: 1.5 × took
        assert_eq!(c.position_at(2_000_000_000, rate), 11_440);
        assert_eq!(c.position_at(0, rate), 10_000, "before the anchor");
        for at in [10_000u64, 10_001, 10_480, 10_959, 11_440] {
            let due = c.due_at(at, rate).unwrap();
            assert!(c.position_at(due, rate) >= at, "{at}");
            assert!(c.position_at(due.saturating_sub(20_000), rate) < at || at == 10_000);
        }
        assert_eq!(c.due_at(9_000, rate), Some(1_000_000_000), "late: now");
        assert_eq!(c.due_at(11_441, rate), None, "beyond the reach");
        let silent = ClockAnchor { took: 0, ..c };
        assert_eq!(
            silent.position_at(2_000_000_000, rate),
            10_000,
            "nothing taken, nothing released"
        );
    }

    /// v1.0-Launch L-B02, the real dispatcher thread: a key every 413.79
    /// frames (145 BPM, 1,920 PPQN, every 40 ticks = 116 Hz) queued ahead
    /// as the worker does, a simulated device publishing its clock every
    /// 10 ms (480 frames) like WASAPI; the port timestamps arrivals. The
    /// intervals keep the song's 8.62 ms instead of the device period, and
    /// the rate the port sees is 116 Hz. Bounds are loose for a loaded
    /// 2-core test machine; Windows timing is checked on the owner's PC.
    #[test]
    fn the_dispatcher_sends_rapid_notes_at_their_own_times_not_in_device_periods() {
        let shared = Arc::new(super::super::engine::Shared::new(4096));
        #[derive(Default)]
        struct Clocked {
            shared: Mutex<Option<Arc<super::super::engine::Shared>>>,
            at: Mutex<Vec<u64>>,
        }
        impl MidiPort for Clocked {
            fn short(&self, m: u32) -> bool {
                if m & 0xF0 == 0x90 && (m >> 16) & 0x7F > 0 {
                    let now = self.shared.lock().unwrap().as_ref().unwrap().now_nanos();
                    self.at.lock().unwrap().push(now);
                }
                true
            }
            fn sysex(&self, _: &[u8]) -> bool {
                true
            }
        }
        let port = Arc::new(Clocked::default());
        *port.shared.lock().unwrap() = Some(Arc::clone(&shared));
        let route = ExternalRoute::for_target(OutputTarget::Kdmapi, port.clone());
        let mut sink = ExternalSink::new(Arc::clone(&route));
        // 1.5 s of the probe, starting 50 ms in.
        let n = 170u64;
        let frame = |i: u64| 2_400 + (i as f64 * 413.793_103).round() as u64;
        for i in 0..n {
            let f = frame(i);
            sink.set_block(2 * (f / 64 * 64));
            sink.at_offset((f % 64) as u32);
            sink.note_on(0, 46, 100);
            sink.at_offset(((f + 200) % 64) as u32);
            sink.set_block(2 * ((f + 200) / 64 * 64));
            sink.at_offset(((f + 200) % 64) as u32);
            sink.note_off(0, 46);
        }
        let disp = Dispatcher::start(&shared, 48_000).unwrap();
        disp.set_routes(vec![Arc::clone(&route)]);
        // The device: a callback every 10 ms takes 480 frames.
        let t0 = shared.now_nanos();
        let mut k = 0u64;
        while frame(n - 1) + 1_000 > k * 480 {
            let due = t0 + k * 10_000_000;
            while shared.now_nanos() < due {
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            shared.publish_clock(2 * 480 * k, 960);
            k += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(disp);
        let at = port.at.lock().unwrap().clone();
        assert_eq!(at.len() as u64, n, "every note sent");
        let mut err: Vec<f64> = at
            .windows(2)
            .zip(0..)
            .map(|(w, i)| {
                let want = (frame(i + 1) - frame(i)) as f64 / 48_000.0 * 1e6;
                ((w[1] as f64 - w[0] as f64) / 1e3 - want).abs()
            })
            .collect();
        err.sort_by(f64::total_cmp);
        let p50 = err[err.len() / 2];
        let p90 = err[err.len() * 9 / 10];
        println!(
            "interval error: p50 {p50:.0} µs, p90 {p90:.0} µs, max {:.0} µs",
            err[err.len() - 1]
        );
        assert!(p50 < 400.0, "p50 {p50} µs");
        assert!(p90 < 2_000.0, "p90 {p90} µs (device period 10,000 µs)");
        // Intervals of the device period (the beta2 bursts) are rare.
        let bursts = at
            .windows(2)
            .filter(|w| {
                let d = (w[1] - w[0]) as f64 / 1e3;
                d < 1_000.0 || (d - 10_000.0).abs() < 300.0
            })
            .count();
        assert!(bursts * 10 < at.len(), "{bursts} device-period intervals");
    }

    /// B1-0136 burst stress on PlumSpirit's side of an external output: the
    /// real dispatcher thread draining one route into a counting port. The
    /// port's own cost (OmniMIDI's SendDirectData) is not in it (Windows).
    #[test]
    #[ignore]
    fn external_route_burst_cost() {
        use std::time::Instant;
        #[derive(Default)]
        struct Count(AtomicU64);
        impl MidiPort for Count {
            fn short(&self, _: u32) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed);
                true
            }
            fn sysex(&self, _: &[u8]) -> bool {
                true
            }
        }
        let shared = Arc::new(super::super::engine::Shared::new(4096));
        let disp = Dispatcher::start(&shared, 48_000).unwrap();
        for burst in [10_000u64, 100_000, 262_144, 1_000_000] {
            let port = Arc::new(Count::default());
            let route = ExternalRoute::for_target(OutputTarget::Kdmapi, port.clone());
            disp.set_routes(vec![Arc::clone(&route)]);
            let mut sink = ExternalSink {
                route: Arc::clone(&route),
                stamp: 0,
                offset: 0,
            };
            // One block's worth: every message due at once (the worst case).
            let t0 = Instant::now();
            for i in 0..burst / 2 {
                let key = (i % 128) as u8;
                let ch = ((i / 128) % 16) as u8;
                sink.note_on(ch, key, 100);
                sink.note_off(ch, key);
            }
            let queued = t0.elapsed();
            let dropped = route.dropped.load(Ordering::Relaxed);
            let want = burst - dropped;
            while port.0.load(Ordering::Relaxed) < want {
                std::hint::spin_loop();
            }
            let all = t0.elapsed();
            println!(
                "burst {burst}: queue {:.2} ms, last message sent after {:.2} ms, {:.1} M messages/s, {} note-ons dropped",
                queued.as_secs_f64() * 1e3,
                all.as_secs_f64() * 1e3,
                want as f64 / all.as_secs_f64() / 1e6,
                dropped
            );
            disp.set_routes(Vec::new());
            drop(sink);
            drop(route);
        }
    }
}
