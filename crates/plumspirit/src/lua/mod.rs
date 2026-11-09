//! The Lua scripting host (v1.0-beta1, B1-0020 / B1-0021 / B1-0024;
//! `docs/LUA_API.md`).
//!
//! One [`Host`] is one sandboxed Lua 5.4 state (mlua, the interpreter built
//! from source — the same on every PC). Community scripts are untrusted by
//! default: only `table`, `string`, `math`, `utf8`, `coroutine` and a reduced
//! base library are loaded — no `io`, `os` (but `os.clock`), `package` /
//! `require`, `debug`, `dofile` / `loadfile`, binary chunks (`load` reads
//! text only, `string.dump` is gone) or native modules; `print` goes to the
//! host's output, never to a console. Everything else a plugin may do is a
//! [`Capability`] the user grants per plugin; the API families that use them
//! arrive with their slices (B1-S52 on) and check them.
//!
//! Every run has a memory quota and a time budget checked every
//! [`CHECK_EVERY`] instructions, and can be canceled from any thread
//! ([`Host::cancel_handle`], also what closing PlumSpirit uses). Every
//! failure is a [`ScriptError`] with a [`Category`], the script's file and
//! line when known, and the message; a Rust panic inside a host function
//! never crosses into the app (it becomes an `Internal` error and the state
//! stays usable).
//!
//! Integers: Lua 5.4 integers are 64-bit, so ticks and note counts are exact
//! (no float rounding) up to 2⁶³ − 1; `plum.ticks` converts and computes
//! with checks ([`ticks`]). Objects are reached through fresh-checked
//! [`handles`] (B1-0022), resolved against one immutable [`World`] per run.

// The plugin manager and the API families (B1-S52 on) are its callers.
#![allow(dead_code)]

pub mod bridge;
pub mod clips;
pub mod config;
pub mod custom;
pub mod drawing;
pub mod events;
pub mod examples;
pub mod files;
pub(crate) mod format_review;
pub mod formats;
pub mod geometry;
pub mod handles;
pub mod images;
pub mod midi_input;
pub mod notes;
pub mod overlays;
pub mod package;
pub mod playback;
pub mod plugin_api;
pub mod plugins;
pub mod project;
pub mod reactive;
pub mod response;
pub mod selection;
pub mod service_calls;
pub mod services;
pub mod structured;
pub mod text;
pub mod ticks;
pub mod timing;
pub mod tracks;
pub mod tx;
pub mod ui;
pub mod visual;
pub mod worker;

pub use handles::{Handle, LuaHandle, World};

use mlua::chunk::ChunkMode;
use mlua::{Error as LuaError, HookTriggers, Lua, LuaOptions, StdLib, Value, VmState};
use std::collections::BTreeSet;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The scripting API's version (semantic versioning, `docs/LUA_API.md`
/// "Versions"): a script written for `major.minor` runs on any host of the
/// same major and an equal or later minor; a major change may break scripts.
pub const API_VERSION: (u32, u32, u32) = (0, 2, 0);

/// The API version as `plum.api_version` shows it ("0.2.0").
pub fn api_version() -> String {
    let (a, b, c) = API_VERSION;
    format!("{a}.{b}.{c}")
}

/// Instructions between two budget / cancellation checks.
pub const CHECK_EVERY: u32 = 10_000;

/// What a plugin may do beyond the sandbox, granted by the user per plugin
/// (`docs/LUA_API.md` "Capabilities"). The names are stable API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// Receive completed strokes from an explicitly selected Piano Roll drawing tool.
    DrawingTools,
    /// Read captured native transport state and receive bounded observations.
    ReadPlayback,
    /// Request bounded native transport actions from explicit invocations.
    ControlPlayback,
    /// Send bounded authored MIDI to isolated internal playback voices.
    EmitMidi,
    /// Enumerate MIDI inputs and request explicit per-device job-scoped consent.
    ReadMidiInput,
    /// Read the project: tracks, clips, notes, events, Conductor, selection.
    ReadProject,
    /// Change notes and controller events (through Preview / Undo).
    EditNotes,
    /// Add, remove and change tracks, subtracks, folders and clips.
    EditTracks,
    /// Change the Conductor (tempo, meter, markers).
    EditConductor,
    /// Read a file the user picks in a dialog.
    ReadUserFile,
    /// Write a file the user picks in a dialog.
    WriteUserFile,
    /// Receive bounded, worker-delivered project observations.
    ObserveProject,
    /// Request bounded automatic transactions from project events.
    AutomateProject,
    /// Publish bounded plain-data services.
    ProvideServices,
    /// Consume declared bounded services in other plugins.
    CallServices,
    /// Change this plugin's sparse project attributes through Undo.
    EditMetadata,
    /// Enumerate and load installed font faces from host font directories.
    ListFonts,
    /// Open explicitly invoked declarative panels and bounded canvas surfaces.
    UiPanels,
    /// Read authorized native view geometry and use bounded view overlays.
    UiOverlay,
}

impl Capability {
    pub const ALL: [Capability; 19] = [
        Capability::ReadProject,
        Capability::EditNotes,
        Capability::EditTracks,
        Capability::EditConductor,
        Capability::ReadUserFile,
        Capability::WriteUserFile,
        Capability::ObserveProject,
        Capability::AutomateProject,
        Capability::ProvideServices,
        Capability::CallServices,
        Capability::EditMetadata,
        Capability::ListFonts,
        Capability::UiPanels,
        Capability::UiOverlay,
        Capability::DrawingTools,
        Capability::ReadPlayback,
        Capability::ControlPlayback,
        Capability::EmitMidi,
        Capability::ReadMidiInput,
    ];

    /// The name scripts and manifests use.
    pub fn name(self) -> &'static str {
        match self {
            Capability::ReadProject => "project.read",
            Capability::EditNotes => "notes.edit",
            Capability::EditTracks => "tracks.edit",
            Capability::EditConductor => "conductor.edit",
            Capability::ReadUserFile => "file.read_user_choice",
            Capability::WriteUserFile => "file.write_user_choice",
            Capability::ObserveProject => "project.observe",
            Capability::AutomateProject => "project.automate",
            Capability::ProvideServices => "services.provide",
            Capability::CallServices => "services.call",
            Capability::EditMetadata => "metadata.edit",
            Capability::ListFonts => "fonts.list",
            Capability::UiPanels => "ui.panels",
            Capability::UiOverlay => "ui.overlay",
            Capability::DrawingTools => "tools.draw",
            Capability::ReadPlayback => "playback.read",
            Capability::ControlPlayback => "playback.control",
            Capability::EmitMidi => "playback.emit_midi",
            Capability::ReadMidiInput => "midi.input",
        }
    }

    pub fn parse(name: &str) -> Option<Capability> {
        Capability::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// What a run may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes the Lua state may allocate in total.
    pub memory: usize,
    /// Wall-clock time one run may take.
    pub time: Duration,
    /// Lua instructions one run may execute (`None`: only the time budget
    /// limits it), counted in steps of [`CHECK_EVERY`].
    pub instructions: Option<u64>,
    /// Notes one run's transactions may create (inserted, changed or
    /// duplicated) — checked before any is built.
    pub max_output_notes: u64,
}

/// B1-0040: the time budget of a command or a custom tool the user started
/// — it shows a running indicator with Cancel, so a long batch over millions
/// of notes may finish; loading and lifecycle hooks keep [`Limits::time`].
pub const USER_RUN_TIME: Duration = Duration::from_secs(600);

/// Default [`Limits::max_output_notes`]: about 1.6 GB of note records at
/// most, below what a 2 GB PC can hold twice.
pub const DEFAULT_MAX_OUTPUT_NOTES: u64 = 50_000_000;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            memory: 256 * 1024 * 1024,
            time: Duration::from_secs(10),
            instructions: None,
            max_output_notes: DEFAULT_MAX_OUTPUT_NOTES,
        }
    }
}

/// How far a running script says it is (`plum.progress`), readable from
/// any thread ([`Host::progress_handle`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// 0 – 1, or `None` before the script reports.
    pub fraction: Option<f64>,
    pub message: String,
}

pub const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
pub const MAX_PRINT_LINES: usize = 1000;
pub const MAX_PRINT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct OutputBuffer {
    lines: Vec<String>,
    bytes: usize,
}
impl OutputBuffer {
    fn check_room(&self, bytes: usize) -> mlua::Result<()> {
        if self.lines.len() >= MAX_PRINT_LINES
            || self
                .bytes
                .checked_add(bytes)
                .and_then(|n| n.checked_add(1))
                .is_none_or(|n| n > MAX_PRINT_BYTES)
        {
            return Err(stop(
                Category::Budget,
                "The plugin's print output budget was used up; reduce diagnostic output.",
            ));
        }
        Ok(())
    }
    fn push(&mut self, line: String) -> mlua::Result<()> {
        self.check_room(line.len())?;
        self.lines
            .try_reserve(1)
            .map_err(|_| stop(Category::Memory, "Unable to reserve diagnostic output."))?;
        self.bytes += line.len() + 1;
        self.lines.push(line);
        Ok(())
    }
}

fn print_line(args: &[Value], stringify: &mlua::Function) -> mlua::Result<String> {
    fn append(line: &mut String, text: &str) -> mlua::Result<()> {
        if line
            .len()
            .checked_add(text.len())
            .is_none_or(|n| n > MAX_DIAGNOSTIC_LINE_BYTES)
        {
            return Err(stop(
                Category::Budget,
                "A print line exceeds the 16 KiB diagnostic limit.",
            ));
        }
        line.try_reserve(text.len())
            .map_err(|_| stop(Category::Memory, "Unable to reserve a diagnostic line."))?;
        line.push_str(text);
        Ok(())
    }
    fn string(line: &mut String, value: &mlua::LuaString) -> mlua::Result<()> {
        // Check borrowed Lua bytes before any potentially expanding lossy copy.
        if value.as_bytes().len() > MAX_DIAGNOSTIC_LINE_BYTES {
            return Err(stop(
                Category::Budget,
                "A print line exceeds the 16 KiB diagnostic limit.",
            ));
        }
        let bytes = value.as_bytes();
        append(line, &String::from_utf8_lossy(&bytes))
    }
    let mut line = String::new();
    for (i, value) in args.iter().enumerate() {
        if i != 0 {
            append(&mut line, "\t")?;
        }
        match value {
            Value::String(s) => string(&mut line, s)?,
            Value::Nil | Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => {
                append(&mut line, &value.to_string()?)?;
            }
            other => match stringify.call::<mlua::LuaString>(other.clone()) {
                Ok(s) if s.to_str().is_ok() => string(&mut line, &s)?,
                _ => append(&mut line, other.type_name())?,
            },
        }
    }
    Ok(line)
}

/// Clip bounded host diagnostics without splitting UTF-8 or copying a huge input.
fn diagnostic_text(text: &str, limit: usize) -> std::borrow::Cow<'_, str> {
    if text.len() <= limit {
        return std::borrow::Cow::Borrowed(text);
    }
    const SUFFIX: &str = " [truncated]";
    let mut end = limit.saturating_sub(SUFFIX.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!("{}{SUFFIX}", &text[..end]))
}

/// Why a script failed (stable API: scripts and error reports use the
/// names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// The script does not parse.
    Syntax,
    /// An error raised while running (by the script, or a host function it
    /// called wrongly).
    Runtime,
    /// The memory quota was reached.
    Memory,
    /// The time budget ran out.
    TimeLimit,
    /// Canceled (the user, or PlumSpirit closing).
    Canceled,
    /// A capability the plugin was not granted.
    Capability,
    /// The script needs a newer API than this PlumSpirit has.
    Compatibility,
    /// A handle whose object no longer exists (deleted, its creation undone,
    /// another project), or a note / event handle whose notes changed.
    Expired,
    /// A PlumSpirit defect (a host function panicked); the state stays usable.
    Internal,
    /// An instruction or output budget was used up.
    Budget,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Syntax => "syntax",
            Category::Runtime => "runtime",
            Category::Memory => "memory",
            Category::TimeLimit => "time_limit",
            Category::Canceled => "canceled",
            Category::Capability => "capability",
            Category::Compatibility => "compatibility",
            Category::Expired => "expired",
            Category::Internal => "internal",
            Category::Budget => "budget",
        }
    }
}

/// A failed script run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub category: Category,
    /// The script file (the chunk name), when the error names a place.
    pub file: Option<String>,
    pub line: Option<u32>,
    /// What went wrong, without the place.
    pub message: String,
    /// The Lua traceback, when there is one.
    pub traceback: Option<String>,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.category {
            Category::Syntax => "Syntax error",
            Category::Runtime => "Error",
            Category::Memory => "Out of script memory",
            Category::TimeLimit => "Time limit reached",
            Category::Canceled => "Canceled",
            Category::Capability => "Not allowed",
            Category::Compatibility => "Needs a newer PlumSpirit",
            Category::Expired => "Expired handle",
            Category::Internal => "Internal error",
            Category::Budget => "Budget used up",
        };
        match (&self.file, self.line) {
            (Some(file), Some(line)) => {
                write!(f, "{what} in {file}, line {line}: {}", self.message)
            }
            (Some(file), None) => write!(f, "{what} in {file}: {}", self.message),
            _ => write!(f, "{what}: {}", self.message),
        }
    }
}

impl ScriptError {
    fn new(category: Category, message: impl Into<String>) -> ScriptError {
        ScriptError {
            category,
            file: None,
            line: None,
            message: message.into(),
            traceback: None,
        }
    }

    /// From an error message that may start with Lua's `file:line:` place.
    fn located(category: Category, raw: &str) -> ScriptError {
        let (file, line, message) = split_place(raw);
        ScriptError {
            category,
            file,
            line,
            message,
            traceback: None,
        }
    }
}

/// An interruption raised by the host (budget, cancellation, a refused
/// capability, an API version), carried through Lua as an external error.
#[derive(Debug)]
struct HostStop {
    category: Category,
    message: String,
    file: Option<String>,
    line: Option<u32>,
}

impl fmt::Display for HostStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostStop {}

fn stop(category: Category, message: impl Into<String>) -> LuaError {
    LuaError::external(HostStop {
        category,
        message: message.into(),
        file: None,
        line: None,
    })
}

/// Lua's `file:line: message` (or `[string "x"]:line: message`) split into
/// its parts; anything else is a message without a place.
fn split_place(raw: &str) -> (Option<String>, Option<u32>, String) {
    let first = raw.lines().next().unwrap_or("");
    // The place ends at the first ":<digits>: ".
    let bytes = first.as_bytes();
    let mut i = 0;
    while let Some(off) = first[i..].find(':') {
        let colon = i + off;
        let digits = first[colon + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let after = colon + 1 + digits;
        if digits > 0 && bytes.get(after) == Some(&b':') {
            let file = first[..colon].trim();
            if let Ok(line) = first[colon + 1..after].parse::<u32>()
                && !file.is_empty()
            {
                let rest = first[after + 1..].trim_start();
                let mut message = rest.to_owned();
                // Keep any further lines (Lua's own detail) but not the
                // traceback, which mlua reports apart.
                for l in raw.lines().skip(1) {
                    if l.starts_with("stack traceback:") {
                        break;
                    }
                    message.push('\n');
                    message.push_str(l);
                }
                return (Some(file.to_owned()), Some(line), message);
            }
        }
        i = colon + 1;
    }
    (None, None, raw.trim().to_owned())
}

/// The first frame of a Lua traceback that is script code (`file:line:`),
/// skipping host (`[C]`) frames.
fn script_frame(traceback: &str) -> Option<(String, u32)> {
    traceback.lines().skip(1).find_map(|l| {
        let l = l.trim_start();
        if l.starts_with("[C]") {
            return None;
        }
        match split_place(l) {
            (Some(file), Some(line), _) => Some((file, line)),
            _ => None,
        }
    })
}

/// The message of a Lua error without its place (wrapping a host error in a
/// longer sentence).
pub(crate) fn error_text(e: &LuaError) -> String {
    classify(e).message
}

/// A [`LuaError`] as a [`ScriptError`].
fn classify(e: &LuaError) -> ScriptError {
    match e {
        LuaError::SyntaxError { message, .. } => ScriptError::located(Category::Syntax, message),
        LuaError::RuntimeError(m) => ScriptError::located(Category::Runtime, m),
        LuaError::MemoryError(m) => ScriptError::located(Category::Memory, m),
        LuaError::CallbackError { traceback, cause } => {
            let mut s = classify(cause);
            // An error raised by a host function names no place itself: the
            // script line that called it is the first Lua frame.
            if s.file.is_none()
                && let Some((file, line)) = script_frame(traceback)
            {
                s.file = Some(file);
                s.line = Some(line);
            }
            s.traceback = Some(traceback.clone());
            s
        }
        LuaError::WithContext { cause, .. } => classify(cause),
        LuaError::ExternalError(x) => match x.downcast_ref::<HostStop>() {
            Some(h) => ScriptError {
                category: h.category,
                file: h.file.clone(),
                line: h.line,
                message: h.message.clone(),
                traceback: None,
            },
            None => ScriptError::located(Category::Runtime, &x.to_string()),
        },
        LuaError::SafetyError(m) => ScriptError::new(Category::Capability, m.clone()),
        LuaError::BadArgument { .. }
        | LuaError::FromLuaConversionError { .. }
        | LuaError::CoroutineUnresumable
        | LuaError::MetaMethodRestricted(_)
        | LuaError::MetaMethodTypeError { .. } => {
            ScriptError::located(Category::Runtime, &e.to_string())
        }
        other => ScriptError::new(Category::Internal, other.to_string()),
    }
}

/// What a run returned, for callers that only need plain values.
#[derive(Clone, Debug, PartialEq)]
pub enum Plain {
    Nil,
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
    /// Tables, functions and the like (not converted).
    Other(&'static str),
}

impl Plain {
    fn of(v: &Value) -> Plain {
        match v {
            Value::Nil => Plain::Nil,
            Value::Boolean(b) => Plain::Boolean(*b),
            Value::Integer(i) => Plain::Integer(*i),
            Value::Number(n) => Plain::Number(*n),
            Value::String(s) => Plain::String(s.to_string_lossy()),
            other => Plain::Other(other.type_name()),
        }
    }
}

/// The Lua hosts of one app (B1-0024): closing PlumSpirit cancels every
/// running script through it, without waiting for one to end. Holds only weak
/// references to the hosts' cancel flags.
#[derive(Clone, Default)]
pub struct Scripts(
    Arc<Mutex<Vec<std::sync::Weak<AtomicBool>>>>,
    Arc<Mutex<Vec<std::sync::Weak<AtomicU64>>>>,
);

impl Scripts {
    /// Follows `host` (its runs are canceled by [`Scripts::cancel_all`]).
    pub fn add(&self, host: &Host) {
        self.add_flag(&host.cancel);
        if let Some(s) = host.lua.app_data_ref::<playback::midi::Slot>() {
            self.add_midi_epoch(&s.cancel_epoch);
        }
    }
    pub(crate) fn add_midi_epoch(&self, epoch: &Arc<AtomicU64>) {
        if let Ok(mut v) = self.1.lock() {
            v.retain(|w| w.strong_count() > 0);
            v.push(Arc::downgrade(epoch));
        }
    }

    /// Follows a host's cancel flag (a host on a script worker's thread).
    pub fn add_flag(&self, flag: &Arc<AtomicBool>) {
        if let Ok(mut v) = self.0.lock() {
            v.retain(|w| w.strong_count() > 0);
            v.push(Arc::downgrade(flag));
        }
    }

    /// Raises every live host's cancel flag; a running script stops at its
    /// next check with the `canceled` category. Returns how many hosts were
    /// told. O(hosts); never blocks on a script.
    pub fn cancel_all(&self) -> usize {
        if let Ok(mut v) = self.1.lock() {
            v.retain(|w| w.strong_count() > 0);
            for w in v.iter() {
                if let Some(e) = w.upgrade() {
                    e.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let Ok(mut v) = self.0.lock() else {
            return 0;
        };
        v.retain(|w| w.strong_count() > 0);
        for w in v.iter() {
            if let Some(c) = w.upgrade() {
                c.store(true, Ordering::SeqCst);
            }
        }
        v.len()
    }
}

/// One sandboxed Lua state.
pub struct Host {
    lua: Lua,
    limits: Limits,
    granted: Arc<BTreeSet<Capability>>,
    cancel: Arc<AtomicBool>,
    /// The current run's deadline, nanoseconds after `epoch` (0: no run).
    deadline: Arc<AtomicU64>,
    /// The current run's time budget in nanoseconds (for messages).
    budget_ns: Arc<AtomicU64>,
    /// B1-0040: a time budget for the next runs instead of
    /// [`Limits::time`] (0: none) — [`Host::set_time_limit`].
    time_override_ns: AtomicU64,
    epoch: Instant,
    /// What `print` wrote.
    output: Arc<Mutex<OutputBuffer>>,
    /// What the last successful run committed ([`Host::take_commits`]).
    commits: Mutex<Option<tx::Commits>>,
    /// Instructions the current run executed (in steps of [`CHECK_EVERY`]).
    executed: Arc<AtomicU64>,
    /// What `plum.progress` reported in the current run.
    progress: Arc<Mutex<Progress>>,
    /// What the last successful run asked the app to select
    /// ([`Host::take_ui_selection`]).
    ui: Mutex<Option<selection::UiSelection>>,
    /// A plugin's run: its project settings change, settings notes and
    /// notifications ([`Host::take_plugin_out`]).
    plugin_out: Mutex<PluginOut>,
}

/// What a plugin's run hands to the app besides its transactions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginOut {
    /// The plugin's new project settings (JSON; empty: remove them).
    pub project_data: Option<String>,
    /// Lines for the plugin's log (settings migrated, damaged, …).
    pub notes: Vec<String>,
    /// `plum.ui.notify` messages.
    pub notices: Vec<(String, plugin_api::NoticeKind)>,
    pub response_request: Option<response::Request>,
    pub loop_intent: Option<playback::loop_intent::Intent>,
    pub midi_intent: Option<playback::midi::Intent>,
}

impl Host {
    /// A new sandboxed state with `limits` and the capabilities the user
    /// granted to this plugin.
    pub fn new(
        limits: Limits,
        granted: impl IntoIterator<Item = Capability>,
    ) -> Result<Host, ScriptError> {
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
        let lua = Lua::new_with(libs, LuaOptions::new()).map_err(|e| classify(&e))?;
        let host = Host {
            lua,
            limits,
            granted: Arc::new(granted.into_iter().collect()),
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: Arc::new(AtomicU64::new(0)),
            budget_ns: Arc::new(AtomicU64::new(
                u64::try_from(limits.time.as_nanos()).unwrap_or(u64::MAX),
            )),
            time_override_ns: AtomicU64::new(0),
            epoch: Instant::now(),
            output: Arc::new(Mutex::new(OutputBuffer::default())),
            commits: Mutex::new(None),
            ui: Mutex::new(None),
            plugin_out: Mutex::new(PluginOut::default()),
            executed: Arc::new(AtomicU64::new(0)),
            progress: Arc::new(Mutex::new(Progress::default())),
        };
        host.sandbox().map_err(|e| classify(&e))?;
        host.install_plum().map_err(|e| classify(&e))?;
        host.lua.set_app_data(handles::WorldSlot(None));
        host.lua.set_app_data(tx::TxSlot::default());
        host.lua.set_app_data(selection::UiSlot::default());
        host.lua.set_app_data(timing::IndexCache::default());
        host.lua.set_app_data(bridge::Session::default());
        host.lua.set_app_data(drawing::Slot::default());
        host.lua.set_app_data(bridge::NativeBudget::default());
        host.lua.set_app_data(files::FileSlot::default());
        host.lua.set_app_data(images::ImageSlot::default());
        host.lua.set_app_data(text::TextSlot::default());
        host.lua.set_app_data(reactive::Scope::default());
        host.lua.set_app_data(playback::control::Slot::default());
        host.lua
            .set_app_data(playback::loop_intent::Slot::default());
        host.lua.set_app_data(playback::plan::Slot::default());
        host.lua.set_app_data(playback::midi::Slot::default());
        host.lua.set_app_data(visual::Slot::default());
        host.lua.set_app_data(overlays::Slot::default());
        host.lua.set_app_data(ui::images::RetainedSlot::default());
        host.lua.set_app_data(notes::Budget {
            cancel: Arc::clone(&host.cancel),
            deadline: Arc::clone(&host.deadline),
            epoch: host.epoch,
            seconds: limits.time.as_secs_f64(),
            max_output_notes: limits.max_output_notes,
        });
        host.install_progress().map_err(|e| classify(&e))?;
        host.lua
            .set_app_data(handles::Granted(Arc::clone(&host.granted)));
        host.lua
            .set_memory_limit(limits.memory)
            .map_err(|e| classify(&e))?;
        host.install_budget().map_err(|e| classify(&e))?;
        Ok(host)
    }

    /// Makes this host a plugin's: installs `plum.plugin` (identity,
    /// registrations, `include`) for `meta` (B1-0038).
    pub fn set_plugin(&self, meta: plugin_api::PluginMeta) -> Result<(), ScriptError> {
        self.lua
            .set_app_data(bridge::NativeBudget::for_plugin(&meta.id));
        plugin_api::install(&self.lua, meta).map_err(|e| classify(&e))
    }

    /// What the last run of a plugin handed over besides transactions.
    pub fn take_plugin_out(&self) -> PluginOut {
        self.plugin_out
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default()
    }

    /// What the plugin registered, when it changed since the last call.
    pub fn take_decls(&self) -> Option<plugin_api::Decls> {
        plugin_api::take_decls(&self.lua)
    }

    /// Raise it (from any thread) to stop the running script at its next
    /// check; the run fails with [`Category::Canceled`]. Lowered at the
    /// start of every run.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// What the last run committed: the app applies it after the run
    /// ([`tx::Commits::apply`]; ask the user first when
    /// [`tx::Commits::needs_confirmation`]). `None` when the run failed or
    /// committed nothing.
    /// The host forgets its world (the commits must be the only other holder
    /// of the document); set a new one with [`Host::set_world`] before the
    /// next run, whether the commits were applied or not.
    pub fn take_commits(&self) -> Option<tx::Commits> {
        let c = self.commits.lock().ok().and_then(|mut c| c.take());
        if c.is_some() {
            self.set_world(None);
        }
        c
    }

    /// What the last successful run asked the app to select (active track,
    /// selected tracks and clips, time selection); the app applies it so the
    /// UI highlight follows. `None` when nothing was asked.
    pub fn take_ui_selection(&self) -> Option<selection::UiSelection> {
        self.ui.lock().ok().and_then(|mut u| u.take())
    }

    /// What the running script last reported with `plum.progress` (a
    /// progress bar can poll it from the UI thread while the script runs on
    /// a worker).
    pub fn progress_handle(&self) -> Arc<Mutex<Progress>> {
        Arc::clone(&self.progress)
    }

    /// What `print` wrote since the last call.
    pub fn take_output(&self) -> Vec<String> {
        self.output
            .lock()
            .map(|mut o| std::mem::take(&mut *o).lines)
            .unwrap_or_default()
    }

    /// Runs `source` as the script file `file` (shown in errors), returning
    /// its first result. Text only; never panics.
    /// B1-0040: the time budget of the following runs (`None`: the host's
    /// [`Limits::time`]). Commands and custom tools the user started (they
    /// show Cancel) run with [`USER_RUN_TIME`].
    pub fn set_time_limit(&self, t: Option<Duration>) {
        let ns = t.map_or(0, |t| {
            u64::try_from(t.as_nanos()).unwrap_or(u64::MAX).max(1)
        });
        self.time_override_ns.store(ns, Ordering::SeqCst);
    }

    pub fn run(&self, file: &str, source: &str) -> Result<Plain, ScriptError> {
        self.cancel.store(false, Ordering::SeqCst);
        let budget = match self.time_override_ns.load(Ordering::SeqCst) {
            0 => u64::try_from(self.limits.time.as_nanos()).unwrap_or(u64::MAX),
            ns => ns,
        };
        self.budget_ns.store(budget, Ordering::SeqCst);
        if let Some(mut b) = self.lua.app_data_mut::<notes::Budget>() {
            b.seconds = Duration::from_nanos(budget).as_secs_f64();
        }
        let now = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.deadline
            .store(now.saturating_add(budget).max(1), Ordering::SeqCst);
        notes::end_automatic_run(&self.lua);
        service_calls::begin_run(&self.lua);
        response::begin_run(&self.lua);
        tx::begin_run(&self.lua);
        selection::begin_run(&self.lua);
        bridge::begin_run(
            &self.lua,
            Duration::from_nanos(budget),
            Arc::clone(&self.cancel),
        );
        files::begin_run(&self.lua);
        midi_input::begin_run(&self.lua);
        images::end_run(&self.lua);
        text::end_run(&self.lua);
        playback::control::begin_run(&self.lua);
        playback::loop_intent::begin_run(&self.lua);
        playback::plan::end_run(&self.lua);
        playback::midi::begin_run(&self.lua);
        visual::begin_run(&self.lua);
        ui::begin_run(&self.lua);
        ui::images::begin_retained_run(&self.lua);
        overlays::begin_run(&self.lua);
        drawing::begin_run(&self.lua);
        self.executed.store(0, Ordering::SeqCst);
        if let Ok(mut p) = self.progress.lock() {
            *p = Progress::default();
        }
        if let Ok(mut c) = self.commits.lock() {
            *c = None;
        }
        let mut result = catch_unwind(AssertUnwindSafe(|| {
            self.lua
                .load(source)
                .set_name(format!("@{file}"))
                .set_mode(ChunkMode::Text)
                .eval::<Value>()
                .and_then(|v| {
                    reactive::check_read(&self.lua)?;
                    notes::check_budget(&self.lua)?;
                    Ok(Plain::of(&v))
                })
        }));
        if let Err(e) = ui::end_run(&self.lua) {
            result = Ok(Err(e));
        }
        if let Err(e) = overlays::end_run(&self.lua, matches!(result, Ok(Ok(_)))) {
            result = Ok(Err(e));
        }
        ui::images::end_retained_run(&self.lua, matches!(result, Ok(Ok(_))));
        let loop_intent = playback::loop_intent::end_run(&self.lua, matches!(result, Ok(Ok(_))));
        let midi_intent = playback::midi::end_run(&self.lua, matches!(result, Ok(Ok(_))));
        playback::control::end_run(&self.lua);
        playback::plan::end_run(&self.lua);
        drawing::end_run(&self.lua);
        images::end_run(&self.lua);
        text::end_run(&self.lua);
        if let Err(e) = formats::end_run(&self.lua, matches!(result, Ok(Ok(_)))) {
            result = Ok(Err(e));
        }
        files::end_run(&self.lua);
        midi_input::end_run(&self.lua);
        bridge::end_run(&self.lua);
        self.deadline.store(0, Ordering::SeqCst);
        notes::end_automatic_run(&self.lua);
        let ok = matches!(result, Ok(Ok(_)));
        let commits = tx::end_run(&self.lua, ok);
        if let Ok(mut u) = self.ui.lock() {
            *u = selection::end_run(&self.lua, ok);
        }
        if let Ok(mut c) = self.commits.lock() {
            *c = commits;
        }
        let (project_data, notes) = config::end_run(&self.lua, ok);
        let notices = plugin_api::take_notices(&self.lua);
        if let Ok(mut p) = self.plugin_out.lock() {
            *p = PluginOut {
                project_data,
                notes,
                response_request: response::end_run(&self.lua, ok),
                loop_intent,
                midi_intent,
                notices: if ok { notices } else { Vec::new() },
            };
        }
        match result {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(classify(&e)),
            Err(panic) => {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a host function failed".into());
                log::error!("internal error in a Lua host function: {what}");
                Err(ScriptError::new(
                    Category::Internal,
                    format!(
                        "PlumSpirit failed inside a script call ({what}); the script was stopped and nothing was changed."
                    ),
                ))
            }
        }
    }

    /// The project state the next runs read (`None`: no project is open, so
    /// every handle reports `expired`). The caller builds it from the open
    /// document and the history's id ledger after
    /// `History::observe_ids`.
    pub fn set_world(&self, world: Option<World>) {
        self.lua.set_app_data(handles::WorldSlot(world));
    }

    /// The world runs currently read.
    pub fn world(&self) -> Option<World> {
        self.lua
            .app_data_ref::<handles::WorldSlot>()
            .and_then(|s| s.0.clone())
    }

    /// `h` as a Lua value (the API families return handles this way).
    pub fn handle_value(lua: &Lua, h: Handle) -> mlua::Result<Value> {
        lua.create_userdata(LuaHandle(h)).map(Value::UserData)
    }

    /// Sets the global `name` to `h` (hosts and tests hand a script its
    /// starting objects this way).
    pub fn set_global_handle(&self, name: &str, h: Handle) -> Result<(), ScriptError> {
        let v = Host::handle_value(&self.lua, h).map_err(|e| classify(&e))?;
        self.lua.globals().set(name, v).map_err(|e| classify(&e))
    }

    /// Registers a host function under `plum.<name>` (the API families use
    /// it; tests register their own).
    pub fn register<F>(&self, name: &str, f: F) -> Result<(), ScriptError>
    where
        F: Fn(&Lua, mlua::MultiValue) -> mlua::Result<mlua::MultiValue> + 'static,
    {
        let plum: mlua::Table = self.lua.globals().get("plum").map_err(|e| classify(&e))?;
        let func = self.lua.create_function(f).map_err(|e| classify(&e))?;
        plum.set(name, func).map_err(|e| classify(&e))
    }

    /// Fails with [`Category::Capability`] unless `cap` was granted (every
    /// API function that needs one calls this first).
    pub fn require(granted: &BTreeSet<Capability>, cap: Capability) -> mlua::Result<()> {
        if granted.contains(&cap) {
            Ok(())
        } else {
            Err(stop(
                Category::Capability,
                format!(
                    "this plugin was not allowed \u{201C}{}\u{201D}; grant it in the plugin's settings",
                    cap.name()
                ),
            ))
        }
    }

    pub fn granted(&self) -> Arc<BTreeSet<Capability>> {
        Arc::clone(&self.granted)
    }

    /// The Lua interpreter's version ("Lua 5.4").
    pub fn lua_version(&self) -> String {
        self.lua
            .globals()
            .get::<String>("_VERSION")
            .unwrap_or_else(|_| "Lua".into())
    }

    // ------------------------------------------------------------ setup

    /// Removes or narrows what an untrusted script must not reach.
    fn sandbox(&self) -> mlua::Result<()> {
        let g = self.lua.globals();
        for name in ["dofile", "loadfile", "require", "module"] {
            g.set(name, Value::Nil)?;
        }
        // `load`: text chunks only (binary chunks can crash the interpreter).
        let raw_load: mlua::Function = g.get("load")?;
        let text_load = self.lua.create_function(
            move |_, (chunk, name, _mode, env): (Value, Option<String>, Value, Value)| {
                raw_load.call::<mlua::MultiValue>((chunk, name, "t", env))
            },
        )?;
        g.set("load", text_load)?;
        if let Ok(string) = g.get::<mlua::Table>("string") {
            string.set("dump", Value::Nil)?;
        }
        // collectgarbage: reading and collecting only (no "stop" / tuning).
        let raw_gc: mlua::Function = g.get("collectgarbage")?;
        let gc = self
            .lua
            .create_function(move |_, (opt, arg): (Option<String>, Value)| {
                let opt = opt.unwrap_or_else(|| "collect".into());
                match opt.as_str() {
                    "collect" | "count" | "step" | "isrunning" => {
                        raw_gc.call::<mlua::MultiValue>((opt, arg))
                    }
                    other => Err(stop(
                        Category::Capability,
                        format!("collectgarbage(\"{other}\") is not available to scripts"),
                    )),
                }
            })?;
        g.set("collectgarbage", gc)?;
        // print → the host's output.
        let out = Arc::clone(&self.output);
        let stringify: mlua::Function = g.get("tostring")?;
        let print = self
            .lua
            .create_function(move |lua, args: mlua::Variadic<Value>| {
                notes::check_budget(lua)?;
                out.lock()
                    .map_err(|_| stop(Category::Internal, "Diagnostic output is unavailable."))?
                    .check_room(0)?;
                // __tostring can call print recursively: never hold the mutex
                // while invoking Lua, and recheck capacity at publication.
                let line = print_line(&args, &stringify)?;
                out.lock()
                    .map_err(|_| stop(Category::Internal, "Diagnostic output is unavailable."))?
                    .push(line)
            })?;
        g.set("print", print)?;
        // os: the script's own CPU-independent clock only.
        let epoch = self.epoch;
        let os = self.lua.create_table()?;
        os.set(
            "clock",
            self.lua
                .create_function(move |_, ()| Ok(epoch.elapsed().as_secs_f64()))?,
        )?;
        g.set("os", os)?;
        Ok(())
    }

    /// The `plum` table: versions, capability discovery, compatibility.
    fn install_plum(&self) -> mlua::Result<()> {
        let plum = self.lua.create_table()?;
        plum.set("version", env!("CARGO_PKG_VERSION"))?;
        plum.set("api_version", api_version())?;
        let (a, b, c) = API_VERSION;
        let api = self.lua.create_table()?;
        api.set("major", a)?;
        api.set("minor", b)?;
        api.set("patch", c)?;
        plum.set("api", api)?;
        plum.set("lua_version", self.lua.globals().get::<String>("_VERSION")?)?;
        plum.set("max_integer", i64::MAX)?;
        ticks::install(&self.lua, &plum)?;
        project::install(&self.lua, &plum)?;
        tx::install(&self.lua, &plum)?;
        selection::install(&self.lua, &plum)?;
        timing::install(&self.lua, &plum)?;
        files::install(&self.lua, &plum)?;
        images::install(&self.lua, &plum)?;
        text::install(&self.lua, &plum)?;
        ui::install(&self.lua, &plum)?;
        geometry::install(&self.lua, &plum)?;
        playback::install(&self.lua, &plum)?;
        midi_input::install(&self.lua, &plum)?;
        let granted = Arc::clone(&self.granted);
        plum.set(
            "capabilities",
            self.lua.create_function(move |lua, ()| {
                let granted = lua
                    .app_data_ref::<handles::Granted>()
                    .map(|g| Arc::clone(&g.0))
                    .unwrap_or_else(|| Arc::clone(&granted));
                let t = lua.create_table()?;
                for cap in Capability::ALL {
                    t.set(cap.name(), granted.contains(&cap))?;
                }
                Ok(t)
            })?,
        )?;
        let granted = Arc::clone(&self.granted);
        plum.set(
            "has",
            self.lua.create_function(move |lua, name: String| {
                let granted = lua
                    .app_data_ref::<handles::Granted>()
                    .map(|g| Arc::clone(&g.0))
                    .unwrap_or_else(|| Arc::clone(&granted));
                Ok(Capability::parse(&name).is_some_and(|c| granted.contains(&c)))
            })?,
        )?;
        plum.set(
            "require_api",
            self.lua
                .create_function(|_, (major, minor): (u32, Option<u32>)| {
                    let minor = minor.unwrap_or(0);
                    let (a, b, _) = API_VERSION;
                    if major == a && minor <= b {
                        Ok(true)
                    } else {
                        Err(stop(
                            Category::Compatibility,
                            format!(
                                "the script needs API {major}.{minor}; this PlumSpirit has {}",
                                api_version()
                            ),
                        ))
                    }
                })?,
        )?;
        self.lua.globals().set("plum", plum)?;
        Ok(())
    }

    /// The time budget and cancellation, checked every [`CHECK_EVERY`]
    /// instructions.
    /// `plum.progress(fraction [, message])` and `plum.yield()`: report
    /// progress and give PlumSpirit a point to stop a canceled script.
    fn install_progress(&self) -> mlua::Result<()> {
        let plum: mlua::Table = self.lua.globals().get("plum")?;
        let progress = Arc::clone(&self.progress);
        plum.set(
            "progress",
            self.lua.create_function(
                move |lua, (fraction, message): (f64, Option<mlua::LuaString>)| {
                    if !fraction.is_finite() {
                        return Err(stop(
                            Category::Runtime,
                            "plum.progress needs a fraction from 0 to 1.",
                        ));
                    }
                    let message = message
                        .map(|m| m.to_str().map(|s| s.chars().take(200).collect::<String>()))
                        .transpose()?;
                    if let Ok(mut p) = progress.lock() {
                        p.fraction = Some(fraction.clamp(0.0, 1.0));
                        if let Some(m) = message {
                            p.message = m;
                        }
                    }
                    notes::check_budget(lua)
                },
            )?,
        )?;
        plum.set(
            "yield",
            self.lua
                .create_function(|lua, ()| notes::check_budget(lua))?,
        )?;
        Ok(())
    }

    fn install_budget(&self) -> mlua::Result<()> {
        let cancel = Arc::clone(&self.cancel);
        let deadline = Arc::clone(&self.deadline);
        let epoch = self.epoch;
        let budget_ns = Arc::clone(&self.budget_ns);
        let executed = Arc::clone(&self.executed);
        let max_instructions = self.limits.instructions;
        self.lua.set_hook(
            HookTriggers::new().every_nth_instruction(CHECK_EVERY),
            move |_, debug| {
                let place = || {
                    let src = debug.source();
                    let file = src.short_src.map(|s| s.into_owned());
                    let line = debug.current_line().and_then(|l| u32::try_from(l).ok());
                    (file, line)
                };
                if cancel.load(Ordering::Relaxed) {
                    let (file, line) = place();
                    return Err(LuaError::external(HostStop {
                        category: Category::Canceled,
                        message: "the script was canceled".into(),
                        file,
                        line,
                    }));
                }
                let ran = executed.fetch_add(u64::from(CHECK_EVERY), Ordering::Relaxed)
                    + u64::from(CHECK_EVERY);
                if let Some(max) = max_instructions
                    && ran > max
                {
                    let (file, line) = place();
                    return Err(LuaError::external(HostStop {
                        category: Category::Budget,
                        message: format!(
                            "the script used more than its {max} instructions and was stopped"
                        ),
                        file,
                        line,
                    }));
                }
                let d = deadline.load(Ordering::Relaxed);
                let now = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                if d != 0 && now > d {
                    let (file, line) = place();
                    let seconds =
                        Duration::from_nanos(budget_ns.load(Ordering::Relaxed)).as_secs_f64();
                    return Err(LuaError::external(HostStop {
                        category: Category::TimeLimit,
                        message: format!(
                            "the script ran longer than {seconds:.1} s and was stopped"
                        ),
                        file,
                        line,
                    }));
                }
                Ok(VmState::Continue)
            },
        )
    }
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod clips_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod custom_tests;
#[cfg(test)]
mod diagnostic_tests;
#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod example_tests;
#[cfg(test)]
mod handle_tests;
#[cfg(test)]
mod note_metadata_tests;
#[cfg(test)]
mod notes_tests;
#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod sel_timing_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tx_tests;
#[cfg(test)]
mod worker_tests;

#[cfg(all(test, windows))]
mod perf_scaling_tests;
