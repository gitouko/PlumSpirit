//! The plugin manager (B1-0037 / B1-0038 / B1-0039, `docs/LUA_API.md`
//! "Plugins"): the installed packages, what the user allowed each one, their
//! lifecycle on script workers, their registrations and their logs.
//!
//! Each enabled plugin runs in its own [`ScriptWorker`] (its own Lua state on
//! its own thread). Loading runs the entry script, then `on_load()` and
//! `register()`; a project opening or closing calls `on_project_open(project)`
//! / `on_project_close()`; disabling, removing or reloading calls
//! `on_unload()` and drops the state, so registrations never pile up. A
//! syntax error, a failing hook or a newer API makes the plugin **Failed** or
//! **Incompatible** with the reason in its log; nothing a plugin does while
//! loading or in a lifecycle hook reaches the project (only commands' and
//! tools' transactions, which the app applies through the undo history).

#[path = "service_broker.rs"]
mod service_broker;

use super::package::{InstallReport, Manifest, Origin, Store};
use super::plugin_api::{Decls, PluginMeta};
use super::selection::UiSelection;
use super::tx::Commits;
use super::worker::{ScriptJob, ScriptResult, ScriptWorker};
use super::{Capability, Limits, ScriptError, Scripts, World};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the user decided about one installed plugin (`settings.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginState {
    pub enabled: bool,
    /// Capabilities the user granted (names; only those the manifest asks
    /// for take effect).
    pub granted: Vec<String>,
    pub origin: Origin,
}

/// Plugin settings (`settings.json` → `plugins`, B1-0059: trust and folders).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPrefs {
    /// Run Lua plugins at all (off: every plugin stays unloaded).
    pub run_plugins: bool,
    /// The plugins folder (`None`: `plugins` in the settings folder).
    pub folder: Option<PathBuf>,
    /// Enable a plugin as soon as it is installed (off by default: the user
    /// reviews its capabilities first).
    pub enable_on_install: bool,
    /// Every installed plugin the user made a decision about, by id.
    pub installed: BTreeMap<String, PluginState>,
    /// B1-0047: shortcuts that open custom tools (`"plugin/tool"` → chord
    /// text such as `Ctrl+Alt+1`).
    pub tool_keys: BTreeMap<String, String>,
}

impl Default for PluginPrefs {
    fn default() -> Self {
        PluginPrefs {
            run_plugins: true,
            folder: None,
            enable_on_install: false,
            installed: BTreeMap::new(),
            tool_keys: BTreeMap::new(),
        }
    }
}

impl PluginPrefs {
    /// The plugins folder in use.
    pub fn store_root(&self, settings_dir: Option<&Path>) -> Option<PathBuf> {
        match &self.folder {
            Some(f) => Some(f.clone()),
            None => settings_dir.map(|d| d.join("plugins")),
        }
    }
}

/// A plugin's state in this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Installed, not enabled.
    Disabled,
    /// Lua plugins are switched off in Settings.
    Off,
    /// The entry script and `on_load` / `register` are running.
    Loading,
    Ready,
    /// The manifest is unreadable or invalid.
    Broken(String),
    /// It needs a newer scripting API.
    Incompatible(String),
    /// Loading or a hook failed (the reason).
    Failed(String),
    /// `on_unload()` is running.
    Unloading,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Disabled => "Disabled",
            Status::Off => "Off (plugins are switched off in Settings)",
            Status::Loading => "Loading…",
            Status::Ready => "Ready",
            Status::Broken(_) => "Cannot be read",
            Status::Incompatible(_) => "Needs a newer PlumSpirit",
            Status::Failed(_) => "Failed",
            Status::Unloading => "Stopping…",
        }
    }
}

/// How serious a log line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    /// What the plugin printed.
    Output,
    Warning,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Output => "print",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// UTC time of day, `HH:MM:SS`.
    pub at: String,
    pub level: Level,
    pub text: String,
}

/// Lines kept per plugin.
pub const LOG_LINES: usize = 1000;
pub const LOG_BYTES: usize = 1024 * 1024;

/// A plugin's diagnostics.
#[derive(Clone, Debug, Default)]
pub struct PluginLog {
    lines: VecDeque<LogLine>,
    bytes: usize,
}

impl PluginLog {
    pub fn push(&mut self, level: Level, text: impl AsRef<str>) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let at = format!(
            "{:02}:{:02}:{:02}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60
        );
        let text = super::diagnostic_text(text.as_ref(), 64 * 1024);
        // Both scanning work and retained bytes are bounded. Preserve ordinary
        // multiline log semantics (the last1000lines in chronological order).
        let mut lines: Vec<_> = text.lines().rev().take(LOG_LINES).collect();
        lines.reverse();
        for line in lines {
            let line = super::diagnostic_text(line, super::MAX_DIAGNOSTIC_LINE_BYTES);
            while self.lines.len() >= LOG_LINES || self.bytes + line.len() > LOG_BYTES {
                if let Some(old) = self.lines.pop_front() {
                    self.bytes -= old.text.len();
                } else {
                    break;
                }
            }
            self.bytes += line.len();
            self.lines.push_back(LogLine {
                at: at.clone(),
                level,
                text: line.into_owned(),
            });
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &LogLine> {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }

    /// The log as text for a bug report, redacted ([`redact`]).
    pub fn report(&self, header: &str, home: Option<&str>, user: Option<&str>) -> String {
        let mut out = redact(header, home, user);
        out.push('\n');
        for l in &self.lines {
            out.push_str(&format!(
                "{} UTC [{}] {}\n",
                l.at,
                l.level.label(),
                redact(&l.text, home, user)
            ));
        }
        out
    }
}

/// Removes what identifies the user from text meant to be shared (B1-0039):
/// the home folder becomes `%USERPROFILE%`, the user name `<user>`, and other
/// absolute paths keep only their file name (`<path>\name.lua`).
pub fn redact(text: &str, home: Option<&str>, user: Option<&str>) -> String {
    let mut s = text.to_owned();
    if let Some(h) = home.filter(|h| h.len() > 3) {
        for variant in [h.to_owned(), h.replace('\\', "/"), h.replace('/', "\\")] {
            s = replace_ci(&s, &variant, "%USERPROFILE%");
        }
    }
    // Absolute paths: C:\…\file or /…/file.
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let drive = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && (chars[i + 2] == '\\' || chars[i + 2] == '/')
            && (i == 0 || !chars[i - 1].is_alphanumeric());
        let unix = chars[i] == '/'
            && (i == 0
                || chars[i - 1].is_whitespace()
                || chars[i - 1] == '('
                || chars[i - 1] == '"'
                || chars[i - 1] == '\'')
            && i + 1 < chars.len()
            && chars[i + 1].is_alphanumeric();
        if drive || unix {
            // The path runs to whitespace / a quote / a closing bracket or
            // a ':' that starts a line number.
            let mut j = i + if drive { 3 } else { 1 };
            while j < chars.len()
                && !chars[j].is_whitespace()
                && !"\"'()<>,;".contains(chars[j])
                && !(chars[j] == ':' && chars.get(j + 1).is_some_and(char::is_ascii_digit))
            {
                j += 1;
            }
            let path: String = chars[i..j].iter().collect();
            let name = path.rsplit(['\\', '/']).next().unwrap_or("");
            let sep = if drive { '\\' } else { '/' };
            out.push_str(&format!("<path>{sep}{name}"));
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    if let Some(u) = user.filter(|u| u.len() >= 3) {
        out = replace_ci(&out, u, "<user>");
    }
    out
}

/// `s` with every case-insensitive (ASCII) occurrence of `pat` replaced.
pub(crate) fn replace_ci(s: &str, pat: &str, with: &str) -> String {
    if pat.is_empty() {
        return s.to_owned();
    }
    let lower = s.to_ascii_lowercase();
    let p = pat.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(off) = lower[from..].find(&p) {
        let at = from + off;
        if !s.is_char_boundary(at) || !s.is_char_boundary(at + p.len()) {
            from = at + 1;
            continue;
        }
        out.push_str(&s[last..at]);
        out.push_str(with);
        last = at + p.len();
        from = last;
    }
    out.push_str(&s[last..]);
    out
}

/// A custom tool run (B1-0046): the tool, its parameters as a Lua table
/// literal, what it edits and whether it is a preview.
#[derive(Clone)]
pub struct ToolJob {
    /// Typed native captured input; never encoded in Lua source.
    pub stroke: Option<std::sync::Arc<super::drawing::Captured>>,
    pub tool: String,
    /// `{ key = value, … }` ([`super::custom::params_literal`]).
    pub params: String,
    pub track: u32,
    pub content: Option<u32>,
    pub preview: bool,
    pub seed: i64,
    /// 128 or 256.
    pub keys: u16,
    /// Matches results to requests (the window keeps only the newest).
    pub generation: u64,
    pub world: Option<World>,
}

/// A job a plugin waits to run.
enum Pending {
    Entry,
    Load,
    /// `None`: waiting for the app to supply the open project's world
    /// ([`PluginEvent::NeedWorld`]).
    ProjectOpen(Option<World>),
    ProjectClose,
    Observe(super::reactive::Delivery),
    Service {
        call_id: u64,
        job: super::services::Job,
        request: Option<super::service_calls::Request>,
    },
    Response {
        delivery: super::reactive::Delivery,
        request: super::response::Request,
        world: Option<World>,
    },
    Command {
        id: String,
        /// The options dialog's values as a Lua table literal (`nil`: none).
        params: String,
        world: Option<World>,
    },
    Tool(ToolJob),
    Format {
        request: super::formats::Request,
        world: Option<World>,
    },
    Unload,
}

impl Pending {
    fn label(&self) -> String {
        match self {
            Pending::Entry => "loading the entry script".into(),
            Pending::Load => "on_load / register".into(),
            Pending::ProjectOpen(_) => "on_project_open".into(),
            Pending::ProjectClose => "on_project_close".into(),
            Pending::Observe(d) => format!("the observer {} ({})", d.id, d.event.kind.name()),
            Pending::Service { job, .. } => format!("the service {}", job.id),
            Pending::Response { request, .. } => {
                format!("the automatic response {}", request.command)
            }
            Pending::Command { id, .. } => format!("the command {id}"),
            Pending::Tool(t) => format!("the tool {}", t.tool),
            Pending::Format { request, .. } => {
                format!("{} format {}", request.mode.name(), request.id)
            }
            Pending::Unload => "on_unload".into(),
        }
    }

    fn delivery(&self) -> Option<&super::reactive::Delivery> {
        match self {
            Self::Observe(d) | Self::Response { delivery: d, .. } => Some(d),
            _ => None,
        }
    }
    fn is_user_job(&self) -> bool {
        matches!(
            self,
            Pending::Command { .. } | Pending::Tool(_) | Pending::Format { .. }
        )
    }
}

const LOAD_SRC: &str = "plum.plugin.__phase('load') \
    if type(on_load) == 'function' then on_load() end \
    plum.plugin.__phase('register') \
    if type(register) == 'function' then register(plum.plugin) end \
    plum.plugin.__phase('ready')";
const OPEN_SRC: &str = "if type(on_project_open) == 'function' then \
    on_project_open(plum.has('project.read') and plum.project.current() or nil) end";
const CLOSE_SRC: &str = "if type(on_project_close) == 'function' then on_project_close() end";
const UNLOAD_SRC: &str = "if type(on_unload) == 'function' then on_unload() end";

/// How long a stopping plugin may take before its state is dropped anyway.
const UNLOAD_GRACE: Duration = Duration::from_secs(3);

/// One Lua state of a plugin and the jobs it runs.
struct Runner {
    worker: ScriptWorker,
    queue: VecDeque<Pending>,
    running: Option<Pending>,
    /// When `running` started.
    started: Option<Instant>,
    /// What this state registered.
    decls: Decls,
    /// `on_load` / `register` finished.
    ready: bool,
    unload_started: Option<Instant>,
    observations: super::reactive::Inbox,
    observation_failed: bool,
    lifetime: std::sync::Arc<super::reactive::Fence>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.lifetime.invalidate();
    }
}

impl Runner {
    fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty() || self.observations.len() > 0
    }

    fn preempt_automatic(&mut self) {
        for d in self.queue.iter().filter_map(Pending::delivery) {
            d.fence.invalidate();
        }
        self.queue.retain(|j| j.delivery().is_none());
        if let Some(d) = self.running.as_ref().and_then(Pending::delivery) {
            d.fence.invalidate();
            self.observations.invalidate_running();
            self.worker.cancel_job();
        }
    }

    /// Queues `on_unload()` (cancelling what runs) for a ready state.
    fn retire(mut self) -> Option<Runner> {
        self.lifetime.invalidate();
        self.observations.clear();
        if let Some(d) = self.running.as_ref().and_then(Pending::delivery) {
            d.fence.invalidate();
        }
        for d in self.queue.iter().filter_map(Pending::delivery) {
            d.fence.invalidate();
        }
        self.queue.clear();
        if self.worker.is_busy() {
            self.worker.cancel();
        }
        if !self.ready {
            return None;
        }
        self.queue.push_back(Pending::Unload);
        self.unload_started = Some(Instant::now());
        Some(self)
    }
}

/// One installed plugin.
pub struct Plugin {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
    /// The package's file stamp when it was scanned.
    stamp: u64,
    pub status: Status,
    pub log: PluginLog,
    /// What the running state registered (empty unless Ready).
    pub decls: Decls,
    runner: Option<Runner>,
    /// A new state loading the edited files (Refresh); it replaces `runner`
    /// only once it is ready, otherwise the old one keeps working.
    candidate: Option<Runner>,
    /// States running `on_unload()` before they are dropped.
    retiring: Vec<Runner>,
}

impl Plugin {
    /// The manifest's name, else the id.
    pub fn name(&self) -> String {
        self.manifest
            .as_ref()
            .map_or_else(|_| self.id.clone(), |m| m.name.clone())
    }

    pub fn is_busy(&self) -> bool {
        self.runner.as_ref().is_some_and(Runner::busy)
            || self.candidate.is_some()
            || !self.retiring.is_empty()
    }

    /// A command or tool is running or waiting.
    pub fn user_job_pending(&self) -> bool {
        self.runner.as_ref().is_some_and(|r| {
            r.running.as_ref().is_some_and(Pending::is_user_job)
                || r.queue.iter().any(Pending::is_user_job)
        })
    }

    /// Native UI actions keep the exact loaded instance, not just its plugin id.
    pub(crate) fn lifecycle(&self) -> Option<Arc<super::reactive::Fence>> {
        self.runner
            .as_ref()
            .filter(|r| r.ready)
            .map(|r| Arc::clone(&r.lifetime))
    }

    /// The capabilities in force: asked for by the manifest and granted.
    pub fn effective(&self, state: Option<&PluginState>) -> Vec<Capability> {
        let (Ok(m), Some(s)) = (&self.manifest, state) else {
            return Vec::new();
        };
        m.capabilities
            .iter()
            .filter(|c| s.granted.contains(c))
            .filter_map(|c| Capability::parse(c))
            .collect()
    }

    /// The running script's progress (`plum.progress`), when busy.
    /// B1-0034: the command running now (its name) and how long it has
    /// been running: the app's running indicator. Tool runs report in their
    /// own window.
    pub fn running_command(&self) -> Option<(String, Duration)> {
        let r = self.runner.as_ref()?;
        if let Some(Pending::Format { request, .. }) = &r.running {
            let name = self
                .decls
                .formats
                .iter()
                .find(|d| d.id == request.id)
                .map_or_else(|| request.id.clone(), |d| d.name.clone());
            return Some((
                format!("{} — {}", request.mode.name(), name),
                r.started.map_or_else(Default::default, |t| t.elapsed()),
            ));
        }
        let Some(Pending::Command { id, .. }) = &r.running else {
            return None;
        };
        let name = self
            .decls
            .commands
            .iter()
            .find(|c| &c.id == id)
            .map_or_else(|| id.clone(), |c| c.name.clone());
        Some((
            name,
            r.started.map_or_else(Default::default, |t| t.elapsed()),
        ))
    }

    pub fn progress(&self) -> Option<super::Progress> {
        self.runner
            .as_ref()
            .filter(|r| r.worker.is_busy())
            .map(|r| r.worker.progress())
    }
}

/// What the app must do after [`PluginManager::poll`].
pub enum PluginEvent {
    TransportLoop {
        plugin: String,
        intent: super::playback::loop_intent::Intent,
        lifecycle: std::sync::Arc<super::reactive::Fence>,
    },
    TransportMidi {
        plugin: String,
        intent: super::playback::midi::Intent,
        lifecycle: std::sync::Arc<super::reactive::Fence>,
    },
    DiscardMidi(super::playback::midi::Intent),
    /// A command finished with transactions: apply them through the undo
    /// history (ask first when `needs_confirmation`).
    Commits {
        plugin: String,
        command: String,
        commits: Commits,
    },
    /// Musical imports are held for explicit native review, never auto-applied.
    FormatResult {
        plugin: String,
        format: String,
        mode: super::formats::Mode,
        selected: bool,
        result: Result<Option<Commits>, String>,
        lifecycle: Arc<super::reactive::Fence>,
    },
    /// A custom tool run finished: its transactions (Preview = Apply: a
    /// preview's commits are what Apply applies), or why it failed.
    ToolResult {
        plugin: String,
        tool: String,
        generation: u64,
        preview: bool,
        result: Result<Option<Commits>, String>,
    },
    ResponseRequest {
        plugin: String,
        delivery: super::reactive::Delivery,
        request: super::response::Request,
    },
    ResponseResult {
        plugin: String,
        delivery: super::reactive::Delivery,
        commits: Option<Commits>,
        notices: Vec<(String, bool)>,
        guard: Box<super::response::SnapshotGuard>,
    },
    /// A command asked the UI to select these.
    Select {
        plugin: String,
        ui: UiSelection,
    },
    /// A short message for the status bar (`error`: shown as an error).
    Notice {
        text: String,
        error: bool,
    },
    /// Checked again by the app after any earlier commits in the same poll.
    ObservationNotice {
        delivery: super::reactive::Delivery,
        text: String,
        error: bool,
    },
    /// B1-0035: a plugin's project settings changed: store `text` (empty:
    /// remove) in the open project, outside the undo history.
    ProjectData {
        plugin: String,
        text: String,
    },
    /// Registrations changed (menus are rebuilt).
    DeclsChanged,
    /// A plugin waits to run `on_project_open`: call
    /// [`PluginManager::supply_world`] with the open project's world.
    NeedWorld,
}

/// Every installed plugin.
pub struct PluginManager {
    store: Option<Store>,
    plugins: Vec<Plugin>,
    scripts: Scripts,
    limits: Limits,
    events: Vec<PluginEvent>,
    /// A project is open (plugins that load now get `on_project_open`; the
    /// manager never keeps the document itself, which would keep it shared).
    project_open: bool,
    /// A plugin waits for the open project's world (asked once).
    asked_world: bool,
    /// `<settings>\plugin-data` (each plugin's settings and presets).
    data_root: Option<PathBuf>,
    observation_stamp: Option<super::bridge::ProjectStamp>,
    observation_sequence: u64,
    observation_views: [Option<super::visual::Snapshot>; 2],
    observation_playback: Option<super::playback::Snapshot>,
    automation_barrier: Option<super::bridge::ProjectStamp>,
    service_graph: super::service_calls::Graph,
}

impl PluginManager {
    /// Scans `root` (`None`: no settings folder — plugins unavailable). Loads
    /// nothing yet ([`PluginManager::start`]).
    pub fn new(root: Option<PathBuf>, scripts: Scripts) -> PluginManager {
        let data_root = root
            .as_ref()
            .and_then(|r| r.parent())
            .map(|p| p.join("plugin-data"));
        let mut m = PluginManager {
            data_root,
            store: root.map(Store::new),
            plugins: Vec::new(),
            scripts,
            limits: Limits::default(),
            events: Vec::new(),
            project_open: false,
            asked_world: false,
            observation_stamp: None,
            observation_sequence: 0,
            observation_views: [None, None],
            observation_playback: None,
            automation_barrier: None,
            service_graph: Default::default(),
        };
        m.rescan();
        m
    }

    /// Where plugins keep settings and presets (the app passes
    /// `<settings>\plugin-data`; by default beside the plugins folder).
    pub fn set_data_root(&mut self, root: Option<PathBuf>) {
        self.data_root = root;
    }

    pub fn store_root(&self) -> Option<&Path> {
        self.store.as_ref().map(|s| s.root.as_path())
    }

    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    pub fn plugin(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Plugin `id`'s log (the window's Clear).
    pub fn plugin_log_mut(&mut self, id: &str) -> Option<&mut PluginLog> {
        self.plugin_mut(id).map(|p| &mut p.log)
    }

    fn plugin_mut(&mut self, id: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|p| p.id == id)
    }

    /// Plugins matching `query` (name, id, author, description; case-
    /// insensitive; empty: all).
    pub fn search(&self, query: &str) -> Vec<&Plugin> {
        let q = query.trim().to_lowercase();
        self.plugins
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.id.contains(&q)
                    || p.manifest.as_ref().is_ok_and(|m| {
                        m.name.to_lowercase().contains(&q)
                            || m.author.to_lowercase().contains(&q)
                            || m.description.to_lowercase().contains(&q)
                    })
            })
            .collect()
    }

    /// Re-reads the store; plugins that disappeared are dropped, new ones
    /// are listed as disabled (or broken). Returns the ids whose files
    /// changed.
    fn rescan(&mut self) -> Vec<String> {
        let found = self.store.as_ref().map(Store::scan).unwrap_or_default();
        let ids: Vec<String> = found
            .iter()
            .filter_map(|i| i.dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        self.plugins.retain(|p| ids.contains(&p.id));
        let mut changed = Vec::new();
        for inst in found {
            let id = inst
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match self.plugins.iter_mut().find(|p| p.id == id) {
                Some(p) => {
                    if p.stamp != inst.stamp || p.manifest != inst.manifest {
                        p.stamp = inst.stamp;
                        p.manifest = inst.manifest;
                        p.log
                            .push(Level::Info, "The plugin's files changed on disk.");
                        changed.push(id);
                    }
                }
                None => {
                    let status = match &inst.manifest {
                        Err(e) => Status::Broken(e.clone()),
                        Ok(_) => Status::Disabled,
                    };
                    let mut log = PluginLog::default();
                    if let Status::Broken(e) = &status {
                        log.push(Level::Error, e.clone());
                    }
                    self.plugins.push(Plugin {
                        id,
                        dir: inst.dir,
                        manifest: inst.manifest,
                        stamp: inst.stamp,
                        status,
                        log,
                        decls: Decls::default(),
                        runner: None,
                        candidate: None,
                        retiring: Vec::new(),
                    });
                }
            }
        }
        self.plugins.sort_by_key(|p| p.name().to_lowercase());
        changed
    }

    /// Loads every enabled plugin (startup, and after Settings switched
    /// plugins on).
    pub fn start(&mut self, prefs: &PluginPrefs) {
        let ids: Vec<String> = self.plugins.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            self.apply_state(&id, prefs);
        }
    }

    /// Unloads every plugin (closing PlumSpirit, or plugins switched off).
    pub fn stop_all(&mut self, prefs: &PluginPrefs) {
        let ids: Vec<String> = self.plugins.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            self.unload(&id);
            if let Some(p) = self.plugin_mut(&id)
                && !prefs.run_plugins
                && !matches!(p.status, Status::Broken(_))
            {
                p.status = Status::Off;
            }
        }
    }

    /// Re-reads the store and brings every plugin to what `prefs` says
    /// (Refresh). A ready plugin whose files changed is loaded again in a
    /// new state; the old one keeps working until the new one is ready, and
    /// stays if the edited files fail (the reason is in the log).
    pub fn refresh(&mut self, prefs: &PluginPrefs) {
        let changed = self.rescan();
        let ids: Vec<String> = self.plugins.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            let ready = self.plugin(&id).is_some_and(|p| p.status == Status::Ready);
            let want = prefs.run_plugins && prefs.installed.get(&id).is_some_and(|s| s.enabled);
            if changed.contains(&id) && ready && want {
                let state = prefs.installed.get(&id).cloned();
                self.start_candidate(&id, state.as_ref());
            } else if changed.contains(&id) {
                self.unload(&id);
                self.apply_state(&id, prefs);
            } else {
                self.apply_state(&id, prefs);
            }
        }
    }

    /// Loads or unloads plugin `id` as `prefs` says.
    fn apply_state(&mut self, id: &str, prefs: &PluginPrefs) {
        let state = prefs.installed.get(id).cloned();
        let want = prefs.run_plugins && state.as_ref().is_some_and(|s| s.enabled);
        let Some(p) = self.plugin_mut(id) else { return };
        if matches!(p.status, Status::Broken(_)) {
            return;
        }
        let loaded = p.runner.is_some();
        if want && !loaded {
            self.load(id, state.as_ref());
        } else if !want {
            if loaded {
                self.unload(id);
            }
            if let Some(p) = self.plugin_mut(id) {
                p.status = if prefs.run_plugins {
                    Status::Disabled
                } else {
                    Status::Off
                };
            }
        }
    }

    /// A new Lua state for plugin `id` with its loading jobs, or why not.
    fn spawn_runner(
        &mut self,
        id: &str,
        state: Option<&PluginState>,
    ) -> Result<Runner, (String, bool)> {
        let scripts = self.scripts.clone();
        let limits = self.limits;
        let data_root = self.data_root.clone();
        let p = self
            .plugin_mut(id)
            .ok_or_else(|| ("not installed".to_owned(), false))?;
        let m = p.manifest.clone().map_err(|e| (e, false))?;
        m.compatible().map_err(|why| (why, true))?;
        let caps = p.effective(state);
        let meta = PluginMeta {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            root: p.dir.clone(),
            data_dir: data_root.map(|d| d.join(&m.id)),
            config_version: m.config_version,
        };
        let worker = ScriptWorker::spawn_plugin(limits, caps.clone(), &scripts, Some(meta))
            .map_err(|e| (e.to_string(), false))?;
        let names: Vec<&str> = caps.iter().map(|c| c.name()).collect();
        p.log.push(
            Level::Info,
            format!(
                "Loading {} {} (allowed: {}).",
                m.name,
                m.version,
                if names.is_empty() {
                    "nothing beyond the sandbox".to_owned()
                } else {
                    names.join(", ")
                }
            ),
        );
        Ok(Runner {
            worker,
            queue: VecDeque::from([Pending::Entry, Pending::Load]),
            running: None,
            started: None,
            decls: Decls::default(),
            ready: false,
            unload_started: None,
            observations: super::reactive::Inbox::default(),
            observation_failed: false,
            lifetime: super::reactive::Fence::live(),
        })
    }

    /// Starts plugin `id`'s Lua state and its loading jobs.
    fn load(&mut self, id: &str, state: Option<&PluginState>) {
        let open = self.project_open;
        match self.spawn_runner(id, state) {
            Ok(mut r) => {
                if open {
                    r.queue.push_back(Pending::ProjectOpen(None));
                }
                if let Some(p) = self.plugin_mut(id) {
                    p.runner = Some(r);
                    p.decls = Decls::default();
                    p.status = Status::Loading;
                }
            }
            Err((why, incompatible)) => {
                if let Some(p) = self.plugin_mut(id) {
                    p.log.push(Level::Error, why.clone());
                    p.status = if incompatible {
                        Status::Incompatible(why)
                    } else if p.manifest.is_err() {
                        Status::Broken(why)
                    } else {
                        Status::Failed(why)
                    };
                }
            }
        }
    }

    /// Loads the edited files of ready plugin `id` beside the running state.
    fn start_candidate(&mut self, id: &str, state: Option<&PluginState>) {
        match self.spawn_runner(id, state) {
            Ok(r) => {
                if let Some(p) = self.plugin_mut(id) {
                    p.candidate = Some(r);
                }
            }
            Err((why, _)) => {
                if let Some(p) = self.plugin_mut(id) {
                    p.log.push(
                        Level::Error,
                        format!("The edited plugin cannot load; the previous version keeps running: {why}"),
                    );
                }
            }
        }
    }

    /// Stops plugin `id`: what runs is canceled, `on_unload()` runs in the
    /// background and the state is dropped (registrations go with it).
    fn unload(&mut self, id: &str) {
        let Some(p) = self.plugin_mut(id) else { return };
        p.candidate = None;
        let Some(r) = p.runner.take() else { return };
        if let Some(r) = r.retire() {
            p.retiring.push(r);
        }
        let had = !p.decls.is_empty();
        p.decls = Decls::default();
        if !matches!(p.status, Status::Broken(_) | Status::Incompatible(_)) {
            p.status = Status::Disabled;
        }
        if had {
            self.events.push(PluginEvent::DeclsChanged);
        }
    }

    /// Enables or disables plugin `id` (recorded in `prefs`).
    pub fn set_enabled(&mut self, id: &str, enabled: bool, prefs: &mut PluginPrefs) {
        let origin = prefs
            .installed
            .get(id)
            .map_or(Origin::LocalFolder, |s| s.origin);
        let st = prefs.installed.entry(id.to_owned()).or_insert(PluginState {
            enabled,
            granted: Vec::new(),
            origin,
        });
        st.enabled = enabled;
        if let Some(p) = self.plugin_mut(id) {
            p.log.push(
                Level::Info,
                if enabled {
                    "Enabled by the user."
                } else {
                    "Disabled by the user."
                },
            );
        }
        self.apply_state(id, prefs);
    }

    /// Grants or withdraws capability `cap` of plugin `id`; a loaded plugin
    /// is reloaded with the new set.
    pub fn set_granted(&mut self, id: &str, cap: &str, on: bool, prefs: &mut PluginPrefs) {
        let st = prefs.installed.entry(id.to_owned()).or_insert(PluginState {
            enabled: false,
            granted: Vec::new(),
            origin: Origin::LocalFolder,
        });
        st.granted.retain(|c| c != cap);
        if on {
            st.granted.push(cap.to_owned());
            st.granted.sort();
        }
        if let Some(p) = self.plugin_mut(id) {
            p.log.push(
                Level::Info,
                format!(
                    "{} \u{201C}{cap}\u{201D}.",
                    if on { "Allowed" } else { "Withdrew" }
                ),
            );
        }
        self.reload(id, prefs);
    }

    /// Unloads and loads plugin `id` again (its registrations are rebuilt).
    pub fn reload(&mut self, id: &str, prefs: &PluginPrefs) {
        self.unload(id);
        self.apply_state(id, prefs);
    }

    /// Installs a package folder (or, `file`, a single `.lua` file).
    pub fn install(
        &mut self,
        src: &Path,
        file: bool,
        prefs: &mut PluginPrefs,
    ) -> Result<InstallReport, String> {
        let origin = if file {
            Origin::LocalFile
        } else {
            Origin::LocalFolder
        };
        self.install_as(src, file, origin, prefs)
    }

    /// Installs (or, when it is installed already, updates) the example
    /// plugin `ex` (B1-0049): its files are written into a fresh folder under
    /// the system's temporary folder and installed from there like any
    /// package, marked [`Origin::Example`]. Nothing is allowed or enabled
    /// beyond what [`PluginPrefs::enable_on_install`] says.
    pub fn install_example(
        &mut self,
        ex: &super::examples::Example,
        prefs: &mut PluginPrefs,
    ) -> Result<InstallReport, String> {
        let parent = std::env::temp_dir().join(format!(
            "plumspirit-example-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let src = ex.write_to(&parent)?;
        let r = if self.plugin(ex.id).is_some() {
            let r = self.update(ex.id, &src, false, prefs);
            if let (Ok(_), Some(st)) = (&r, prefs.installed.get_mut(ex.id)) {
                st.origin = Origin::Example;
            }
            r
        } else {
            self.install_as(&src, false, Origin::Example, prefs)
        };
        let _ = std::fs::remove_dir_all(&parent);
        r
    }

    fn install_as(
        &mut self,
        src: &Path,
        file: bool,
        origin: Origin,
        prefs: &mut PluginPrefs,
    ) -> Result<InstallReport, String> {
        let store = self.store.clone().ok_or_else(|| {
            "There is no plugins folder (the settings folder is unavailable).".to_owned()
        })?;
        let report = if file {
            store.install_file(src, false)?
        } else {
            store.install_folder(src, false)?
        };
        let id = report.manifest.id.clone();
        prefs.installed.insert(
            id.clone(),
            PluginState {
                enabled: prefs.enable_on_install,
                granted: if prefs.enable_on_install {
                    report.manifest.capabilities.clone()
                } else {
                    Vec::new()
                },
                origin,
            },
        );
        self.rescan();
        if let Some(p) = self.plugin_mut(&id) {
            p.log.push(
                Level::Info,
                format!(
                    "Installed {} {} from {} ({} files).",
                    report.manifest.name,
                    report.manifest.version,
                    if origin == Origin::Example {
                        "PlumSpirit's examples".to_owned()
                    } else {
                        src.display().to_string()
                    },
                    report.files
                ),
            );
        }
        self.apply_state(&id, prefs);
        Ok(report)
    }

    /// Replaces installed plugin `id` with the package at `src` (same id).
    /// Capabilities the new version no longer asks for are withdrawn; new
    /// ones wait for the user.
    pub fn update(
        &mut self,
        id: &str,
        src: &Path,
        file: bool,
        prefs: &mut PluginPrefs,
    ) -> Result<InstallReport, String> {
        let store = self
            .store
            .clone()
            .ok_or_else(|| "There is no plugins folder.".to_owned())?;
        if !file {
            let m = super::package::read_manifest(src)?;
            if m.id != id {
                return Err(format!(
                    "That package is \u{201C}{}\u{201D}, not \u{201C}{id}\u{201D}; install it as a separate plugin.",
                    m.id
                ));
            }
        }
        self.unload(id);
        let report = if file {
            store.install_file(src, true)?
        } else {
            store.install_folder(src, true)?
        };
        if report.manifest.id != id {
            return Err("The updated plugin has another id.".into());
        }
        if let Some(st) = prefs.installed.get_mut(id) {
            st.granted
                .retain(|c| report.manifest.capabilities.contains(c));
        }
        self.rescan();
        if let Some(p) = self.plugin_mut(id) {
            p.log.push(
                Level::Info,
                format!(
                    "Updated from {} to {}.",
                    report.replaced.as_deref().unwrap_or("?"),
                    report.manifest.version
                ),
            );
            p.status = Status::Disabled;
        }
        self.apply_state(id, prefs);
        Ok(report)
    }

    /// Removes plugin `id` (unloaded first) and forgets its settings.
    pub fn remove(&mut self, id: &str, prefs: &mut PluginPrefs) -> Result<(), String> {
        self.unload(id);
        let store = self
            .store
            .clone()
            .ok_or_else(|| "There is no plugins folder.".to_owned())?;
        store.remove(id)?;
        prefs.installed.remove(id);
        self.rescan();
        Ok(())
    }

    /// The ready plugin `plugin`, or why a job cannot be queued for it.
    fn ready_runner(&mut self, plugin: &str) -> Result<&mut Plugin, String> {
        let p = self
            .plugin_mut(plugin)
            .ok_or_else(|| format!("The plugin \u{201C}{plugin}\u{201D} is not installed."))?;
        if p.status != Status::Ready || p.runner.is_none() {
            return Err(format!("{} is not ready ({}).", p.name(), p.status.label()));
        }
        Ok(p)
    }

    /// Runs command `command` of plugin `plugin` on `world` (the open
    /// project's snapshot).
    pub fn run_command(
        &mut self,
        plugin: &str,
        command: &str,
        world: Option<World>,
    ) -> Result<(), String> {
        self.run_command_with(plugin, command, "nil".into(), world)
    }

    /// [`PluginManager::run_command`] with the values of the command's
    /// options dialog (`params`: a Lua table literal from
    /// [`super::custom::params_literal`]).
    pub fn run_command_with(
        &mut self,
        plugin: &str,
        command: &str,
        params: String,
        world: Option<World>,
    ) -> Result<(), String> {
        let p = self.ready_runner(plugin)?;
        if !p.decls.commands.iter().any(|c| c.id == command) {
            return Err(format!(
                "{} has no command \u{201C}{command}\u{201D}.",
                p.name()
            ));
        }
        // Lifecycle hooks in progress run first; one command or tool at a time.
        if p.user_job_pending() {
            return Err(format!(
                "{} is still running a command or tool; wait for it or cancel it.",
                p.name()
            ));
        }
        if let Some(r) = p.runner.as_mut() {
            r.preempt_automatic();
            r.queue.push_back(Pending::Command {
                id: command.to_owned(),
                params,
                world,
            });
        }
        self.pump();
        Ok(())
    }

    /// An explicit native format action. Imported commits must be reviewed before Apply.
    pub fn run_format(
        &mut self,
        plugin: &str,
        id: &str,
        mode: super::formats::Mode,
        world: Option<World>,
    ) -> Result<(), String> {
        let p = self.ready_runner(plugin)?;
        if !p
            .decls
            .formats
            .iter()
            .any(|d| d.id == id && d.supports(mode))
        {
            return Err("The plugin has no such format operation.".into());
        }
        if p.user_job_pending() {
            return Err(
                "The plugin is still processing a user action; finish or cancel it first.".into(),
            );
        }
        if let Some(r) = p.runner.as_mut() {
            r.preempt_automatic();
            r.queue.push_back(Pending::Format {
                request: super::formats::Request {
                    id: id.to_owned(),
                    mode,
                },
                world,
            });
        }
        self.pump();
        Ok(())
    }

    /// Runs custom tool `job.tool` of plugin `plugin`. A tool run waiting
    /// behind another one is replaced by the newer request (a preview that
    /// follows the controls never queues up).
    pub fn run_tool(&mut self, plugin: &str, job: ToolJob) -> Result<(), String> {
        let p = self.ready_runner(plugin)?;
        if !p.decls.tools.iter().any(|t| t.id == job.tool) {
            return Err(format!(
                "{} has no tool \u{201C}{}\u{201D}.",
                p.name(),
                job.tool
            ));
        }
        let Some(r) = p.runner.as_mut() else {
            return Err("The plugin stopped.".into());
        };
        if r.queue
            .iter()
            .any(|j| matches!(j, Pending::Command { .. } | Pending::Format { .. }))
            || r.running
                .as_ref()
                .is_some_and(|j| matches!(j, Pending::Command { .. } | Pending::Format { .. }))
        {
            return Err(format!("{} is running a command; wait for it.", p.name()));
        }
        r.queue.retain(|j| !matches!(j, Pending::Tool(_)));
        r.preempt_automatic();
        r.queue.push_back(Pending::Tool(job));
        self.pump();
        Ok(())
    }

    /// Cancels what plugin `id` runs for the user (commands, tools); the
    /// script stops at its next check.
    pub fn cancel(&mut self, id: &str) {
        if let Some(p) = self.plugin_mut(id)
            && let Some(r) = p.runner.as_mut()
        {
            r.queue
                .retain(|j| !j.is_user_job() && j.delivery().is_none());
            r.worker.cancel_midi_output();
            if r.running.as_ref().is_some_and(Pending::is_user_job) {
                r.worker.cancel_job();
            }
        }
    }

    /// A project was opened (or the open one replaced).
    pub fn project_opened(&mut self, world: World) {
        self.project_open = true;
        self.observation_stamp = Some(super::bridge::ProjectStamp {
            serial: world.project_serial(),
            revision: world.info.revision,
        });
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                r.observations.clear();
                if let Some(d) = r.running.as_ref().and_then(Pending::delivery) {
                    d.fence.invalidate();
                }
                for d in r.queue.iter().filter_map(Pending::delivery) {
                    d.fence.invalidate();
                }
                if r.running
                    .as_ref()
                    .is_some_and(|j| j.is_user_job() || j.delivery().is_some())
                {
                    r.worker.cancel();
                }
                r.queue
                    .retain(|j| !j.is_user_job() && j.delivery().is_none());
                r.queue.push_back(Pending::ProjectOpen(Some(world.clone())));
            }
        }
        let stamp = self.observation_stamp.expect("opened project stamp");
        self.publish_observation(
            super::reactive::Kind::ProjectOpened,
            stamp,
            super::reactive::Data::Project {
                tracks: world.doc.track_count() as u32,
                notes: world.doc.stored_note_count() as u64,
                keys: world.doc.project().keys.keys() as u16,
            },
        );
        self.pump();
    }

    /// The open project's world, after [`PluginEvent::NeedWorld`]: every
    /// plugin waiting to run `on_project_open` gets it.
    pub fn supply_world(&mut self, world: World) {
        self.asked_world = false;
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                for j in &mut r.queue {
                    if let Pending::ProjectOpen(w @ None) = j {
                        *w = Some(world.clone());
                    }
                }
            }
        }
        self.pump();
    }

    /// The open project is closing.
    pub fn project_closed(&mut self) {
        self.observation_views = [None, None];
        self.observation_playback = None;
        self.project_open = false;
        let closed = self.observation_stamp.take();
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                r.worker.release_ui_images();
                r.observations.clear();
                if let Some(d) = r.running.as_ref().and_then(Pending::delivery) {
                    d.fence.invalidate();
                }
                for d in r.queue.iter().filter_map(Pending::delivery) {
                    d.fence.invalidate();
                }
                if r.running
                    .as_ref()
                    .is_some_and(|j| j.is_user_job() || j.delivery().is_some())
                {
                    r.worker.cancel();
                }
                r.queue.retain(|j| {
                    !j.is_user_job()
                        && j.delivery().is_none()
                        && !matches!(j, Pending::ProjectOpen(_))
                });
                if r.ready {
                    r.queue.push_back(Pending::ProjectClose);
                }
            }
        }
        if let Some(stamp) = closed {
            self.publish_observation(
                super::reactive::Kind::ProjectClosed,
                stamp,
                super::reactive::Data::Closed,
            );
        }
        self.pump();
    }

    /// Whether native change sampling is needed. Disabled/no-observer paths do
    /// not inspect notes, allocate snapshots or dispatch worker jobs.
    pub fn observing(&self) -> bool {
        self.plugins.iter().any(|p| {
            p.status == Status::Ready
                && p.runner.as_ref().is_some_and(|r| {
                    r.decls.observers.iter().any(|d| {
                        d.kinds.iter().any(|k| {
                            !matches!(
                                k,
                                super::reactive::Kind::TransportChanged
                                    | super::reactive::Kind::SurfaceChanged
                            )
                        })
                    })
                })
        })
    }

    pub fn observing_playback(&self) -> bool {
        self.plugins.iter().any(|p| {
            p.status == Status::Ready
                && p.runner.as_ref().is_some_and(|r| {
                    !r.observation_failed
                        && r.decls
                            .observers
                            .iter()
                            .any(|d| d.kinds.contains(&super::reactive::Kind::TransportChanged))
                })
        })
    }
    pub fn observing_surfaces(&self) -> bool {
        self.plugins.iter().any(|p| {
            p.status == Status::Ready
                && p.runner.as_ref().is_some_and(|r| {
                    !r.observation_failed
                        && r.decls
                            .observers
                            .iter()
                            .any(|d| d.kinds.contains(&super::reactive::Kind::SurfaceChanged))
                })
        })
    }
    pub fn observing_overlay_playback(&self) -> bool {
        self.plugins.iter().any(|p| {
            p.status == Status::Ready
                && p.runner.as_ref().is_some_and(|r| {
                    !r.observation_failed
                        && r.worker.overlay_reads()[1]
                        && !r.decls.observers.is_empty()
                })
        })
    }
    /// Copied native geometry only; never stores a document root.
    pub fn set_observation_views(&mut self, views: [Option<super::visual::Snapshot>; 2]) {
        if self.observation_views == views {
            return;
        }
        self.observation_views = views;
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut()
                && let Some(d) = r.running.as_ref().and_then(Pending::delivery)
                && d.views.iter().any(Option::is_some)
            {
                d.fence.invalidate();
                r.observations.invalidate_running();
                r.worker.cancel_job();
            }
        }
    }
    pub fn set_observation_playback(&mut self, snapshot: Option<super::playback::Snapshot>) {
        self.observation_playback = snapshot.filter(|s| s.valid());
    }

    /// Cheap every-frame revision fence; no snapshot is acquired.
    pub fn observation_context(&mut self, stamp: Option<super::bridge::ProjectStamp>) {
        if stamp == self.observation_stamp {
            return;
        }
        self.observation_stamp = stamp;
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut()
                && let Some(delivery) = r.running.as_ref().and_then(Pending::delivery)
                && (Some(delivery.event.stamp) != stamp
                    || delivery.event.kind == super::reactive::Kind::ProjectClosed)
            {
                delivery.fence.invalidate();
                r.observations.invalidate_running();
                r.worker.cancel();
            }
        }
    }

    /// Trusted native owner notification. This is a coalesced state signal, not
    /// an edit log. Queues contain no document snapshots or per-note payloads.
    pub fn publish_observation(
        &mut self,
        kind: super::reactive::Kind,
        stamp: super::bridge::ProjectStamp,
        data: super::reactive::Data,
    ) -> bool {
        use super::reactive::{Event, Kind};
        if kind != Kind::ProjectClosed {
            if !self.project_open
                || self
                    .observation_stamp
                    .is_none_or(|s| s.serial != stamp.serial)
            {
                return false;
            }
        } else if self.project_open {
            return false;
        }
        let Some(sequence) = self
            .observation_sequence
            .checked_add(1)
            .filter(|s| *s <= i64::MAX as u64)
        else {
            return false;
        };
        let Some(mut event) = Event::new(kind, stamp, sequence, data) else {
            return false;
        };
        event.transaction_allowed &= self.automation_barrier != Some(stamp);
        if kind != Kind::ProjectClosed {
            self.observation_stamp = Some(stamp);
        }
        self.observation_sequence = sequence;
        let now = Instant::now();
        for p in &mut self.plugins {
            if p.status != Status::Ready {
                continue;
            }
            let Some(r) = p.runner.as_mut() else {
                continue;
            };
            if let super::reactive::Data::Surface { index, .. } = event.data
                && !r.worker.overlay_reads()[usize::from(index)]
            {
                continue;
            }
            if r.decls.observers.is_empty() || r.observation_failed {
                continue;
            }
            if let Some(d) = r.running.as_ref().and_then(Pending::delivery)
                && (d.event.stamp != stamp
                    || (d.event.kind == kind && kind != Kind::TransportChanged))
            {
                d.fence.invalidate();
                r.observations.invalidate_running();
                if matches!(kind, Kind::SurfaceChanged | Kind::ViewChanged) {
                    r.worker.cancel_job();
                } else {
                    r.worker.cancel();
                }
            }
            let budget = super::bridge::NativeBudget::for_plugin(&p.id);
            if let Err(e) = r.observations.push(&r.decls, &event, now, &budget) {
                r.observations.dropped = r.observations.dropped.saturating_add(1);
                if r.observations.dropped.is_power_of_two() {
                    p.log.push(
                        Level::Warning,
                        format!("Project observation queue refused native resources: {e:?}."),
                    );
                }
            }
        }
        true
    }

    pub fn set_automation_barrier(&mut self, stamp: Option<super::bridge::ProjectStamp>) {
        self.automation_barrier = stamp;
    }
    pub fn response_log(&mut self, id: &str, text: impl Into<String>) {
        if let Some(p) = self.plugin_mut(id) {
            let text: String = text.into();
            p.log.push(Level::Warning, text);
        }
    }
    pub fn response_permitted(&self, id: &str, prefs: &PluginPrefs) -> bool {
        let Some(p) = self.plugin(id) else {
            return false;
        };
        let state = prefs.installed.get(id);
        let caps = p.effective(state);
        prefs.run_plugins
            && state.is_some_and(|s| s.enabled)
            && p.status == Status::Ready
            && [
                super::Capability::ReadProject,
                super::Capability::ObserveProject,
                super::Capability::AutomateProject,
                super::Capability::EditNotes,
            ]
            .iter()
            .all(|c| caps.contains(c))
    }
    pub fn queue_response(
        &mut self,
        id: &str,
        mut delivery: super::reactive::Delivery,
        request: super::response::Request,
        world: World,
        prefs: &PluginPrefs,
    ) -> Result<(), String> {
        if self.automation_barrier == Some(delivery.event.stamp)
            || !delivery.valid()
            || self.observation_stamp != Some(delivery.event.stamp)
            || !self.project_open
            || !delivery.event.transaction_allowed
            || world.project_serial() != delivery.event.stamp.serial
            || world.info.revision != delivery.event.stamp.revision
        {
            return Err("The automatic response source expired.".into());
        }
        let p = self
            .plugin_mut(id)
            .ok_or_else(|| "The response plugin is unavailable.".to_owned())?;
        let state = prefs.installed.get(id);
        let caps = p.effective(state);
        if !prefs.run_plugins
            || !state.is_some_and(|s| s.enabled)
            || p.status != Status::Ready
            || [
                super::Capability::ReadProject,
                super::Capability::ObserveProject,
                super::Capability::AutomateProject,
                super::Capability::EditNotes,
            ]
            .iter()
            .any(|c| !caps.contains(c))
        {
            return Err(
                "The plugin is not allowed to make automatic transaction responses.".into(),
            );
        }
        let r = p
            .runner
            .as_mut()
            .ok_or_else(|| "The response plugin stopped.".to_owned())?;
        if r.observation_failed
            || !r.decls.observers.iter().any(|d| d.id == delivery.id)
            || !r.decls.commands.iter().any(|c| c.id == request.command)
            || r.queue
                .iter()
                .any(|j| j.is_user_job() || matches!(j, Pending::Response { .. }))
            || r.running
                .as_ref()
                .is_some_and(|j| j.is_user_job() || matches!(j, Pending::Response { .. }))
        {
            return Err(
                "The response is unavailable while user work or another response is active.".into(),
            );
        }
        if let Some(d) = r.running.as_ref().and_then(Pending::delivery) {
            d.fence.invalidate();
            r.observations.invalidate_running();
            r.worker.cancel();
        }
        delivery.fence = super::reactive::Fence::live();
        r.queue.push_front(Pending::Response {
            delivery,
            request,
            world: Some(world),
        });
        self.pump();
        Ok(())
    }

    /// Whether any plugin runs something (the app keeps polling while so).
    pub fn is_busy(&self) -> bool {
        self.plugins.iter().any(Plugin::is_busy)
    }

    /// Submits the next job of every idle state.
    fn pump(&mut self) {
        let mut active_services = self
            .plugins
            .iter()
            .filter(|p| {
                p.runner
                    .as_ref()
                    .is_some_and(|r| matches!(r.running, Some(Pending::Service { .. })))
                    && !self.service_graph.waiting(&p.id)
            })
            .count();
        for p in &mut self.plugins {
            let entry = p
                .manifest
                .as_ref()
                .map(|m| m.entry.clone())
                .unwrap_or_default();
            let dir = p.dir.clone();
            let mut failures: Vec<(u8, String)> = Vec::new();
            let mut slots: Vec<(u8, &mut Runner)> = Vec::new();
            if let Some(r) = p.runner.as_mut() {
                slots.push((0, r));
            }
            if let Some(r) = p.candidate.as_mut() {
                slots.push((1, r));
            }
            for r in &mut p.retiring {
                slots.push((2, r));
            }
            for (which, r) in slots {
                if r.running.is_some() {
                    continue;
                }
                if matches!(r.queue.front(), Some(Pending::ProjectOpen(None))) {
                    continue; // waits for the app's world (`poll` asks)
                }
                let mut job = match r.queue.pop_front() {
                    Some(job) => job,
                    None if which == 0 && r.ready && !r.worker.has_pending_overlay() => {
                        match r.observations.take(Instant::now(), self.observation_stamp) {
                            Some(mut delivery) => {
                                if let super::reactive::Data::Surface {
                                    index,
                                    view_revision,
                                } = delivery.event.data
                                    && self.observation_views[usize::from(index)]
                                        .map(|v| v.revision)
                                        != view_revision
                                {
                                    continue;
                                }
                                let reads = r.worker.overlay_reads();
                                delivery.views = std::array::from_fn(|i| {
                                    self.observation_views[i]
                                        .filter(|v| reads[i] && v.stamp == delivery.event.stamp)
                                });
                                delivery.playback = self
                                    .observation_playback
                                    .filter(|s| reads[1] && s.stamp == delivery.event.stamp);
                                delivery.lifecycle = std::sync::Arc::clone(&r.lifetime);
                                Pending::Observe(delivery)
                            }
                            None => continue,
                        }
                    }
                    None => continue,
                };
                if let Pending::Service {
                    call_id, request, ..
                } = &mut job
                    && request
                        .as_ref()
                        .is_none_or(|q| !q.frame.matches(self.observation_stamp))
                {
                    self.service_graph.complete(*call_id);
                    if let Some(request) = request.take() {
                        request.respond(Err(super::service_calls::Failure::new(
                            super::Category::Expired,
                            "The queued service call expired.",
                        )));
                    }
                    continue;
                }
                if matches!(job, Pending::Service { .. }) && active_services >= 4 {
                    r.queue.push_front(job);
                    continue;
                }
                let is_service = matches!(job, Pending::Service { .. });
                match submit(&mut r.worker, &mut job, &dir, &entry) {
                    Ok(()) => {
                        if is_service {
                            active_services += 1;
                        }
                        r.running = Some(job);
                        r.started = Some(Instant::now());
                    }
                    Err(e) => failures.push((which, e)),
                }
            }
            for (which, e) in failures {
                p.log.push(Level::Error, e.clone());
                match which {
                    0 => {
                        p.runner = None;
                        p.decls = Decls::default();
                        p.status = Status::Failed(e);
                    }
                    1 => p.candidate = None,
                    _ => p.retiring.retain(|r| r.running.is_some()),
                }
            }
        }
    }

    /// Collects finished jobs and starts the next ones; call it every frame
    /// while [`PluginManager::is_busy`] (never blocks). Returns what the app
    /// must apply or show.
    pub fn poll(&mut self, prefs: &PluginPrefs) -> Vec<PluginEvent> {
        self.poll_service_calls(prefs);
        let mut service_results = Vec::new();
        let mut events = std::mem::take(&mut self.events);
        let project_open = self.project_open;
        for p in &mut self.plugins {
            // States stopping: dropped after on_unload (or the grace time).
            let mut k = 0;
            while k < p.retiring.len() {
                let r = &mut p.retiring[k];
                let done = match r.worker.poll() {
                    Some(res) => {
                        let stale = r
                            .running
                            .as_ref()
                            .and_then(Pending::delivery)
                            .is_some_and(|d| !d.valid())
                            || matches!(&r.running,Some(Pending::Service {job,..}) if !job.lifecycle.active());
                        if !stale {
                            for line in &res.output {
                                p.log.push(Level::Output, line.clone());
                            }
                            if let Err(e) = &res.result {
                                p.log.push(Level::Warning, format!("on_unload: {e}"));
                            }
                        }
                        r.running = None;
                        r.queue.is_empty()
                    }
                    None => r.unload_started.is_some_and(|t| t.elapsed() > UNLOAD_GRACE),
                };
                if done {
                    let timed_out = p.retiring[k].running.is_some();
                    p.retiring.remove(k);
                    p.log.push(
                        if timed_out {
                            Level::Warning
                        } else {
                            Level::Info
                        },
                        if timed_out {
                            "on_unload() did not finish in time; the old state was stopped."
                        } else {
                            "Stopped."
                        },
                    );
                } else {
                    k += 1;
                }
            }
            // A candidate (edited files) replaces the state once ready.
            if let Some(c) = p.candidate.as_mut()
                && let Some(res) = c.worker.poll()
            {
                let job = c.running.take();
                for line in &res.output {
                    p.log.push(Level::Output, line.clone());
                }
                match (&res.result, job) {
                    (Ok(_), Some(Pending::Entry)) => {
                        if let Some(d) = res.decls {
                            c.decls = d;
                        }
                    }
                    (Ok(_), Some(Pending::Load)) => {
                        if let Some(d) = res.decls {
                            c.decls = d;
                        }
                        let mut c = p.candidate.take().expect("candidate");
                        c.ready = true;
                        c.worker.freeze_extensions();
                        if project_open {
                            c.queue.push_back(Pending::ProjectOpen(None));
                        }
                        if let Some(old) = p.runner.take().and_then(Runner::retire) {
                            p.retiring.push(old);
                        }
                        p.decls = c.decls.clone();
                        p.runner = Some(c);
                        p.status = Status::Ready;
                        p.log.push(
                            Level::Info,
                            format!(
                                "Reloaded after its files changed: {} command(s), {} tool(s).",
                                p.decls.commands.len(),
                                p.decls.tools.len()
                            ),
                        );
                        events.push(PluginEvent::DeclsChanged);
                    }
                    (Err(e), _) => {
                        let msg = format!(
                            "The edited plugin failed to load; the previous version keeps running: {e}"
                        );
                        p.log.push(Level::Error, msg.clone());
                        events.push(PluginEvent::Notice {
                            text: format!("{}: {msg}", p.name()),
                            error: true,
                        });
                        p.candidate = None;
                    }
                    _ => {}
                }
            }
            // The running state.
            let res = p.runner.as_mut().and_then(|r| {
                let res = r.worker.poll()?;
                let job = r.running.take()?;
                Some((job, res))
            });
            if let Some((job, res)) = res {
                if let Pending::Service {
                    call_id,
                    job: service,
                    request: Some(request),
                } = job
                {
                    service_results.push((p.id.clone(), call_id, service, request, res));
                } else {
                    finish(p, job, res, &mut events);
                }
            }
        }
        for (provider, id, job, request, result) in service_results {
            self.complete_service(&provider, id, job, request, result, prefs);
        }
        self.poll_service_calls(prefs);
        // Give staged native intents their owner turn before starting another run scope.
        if !events.iter().any(|e| {
            matches!(
                e,
                PluginEvent::TransportLoop { .. } | PluginEvent::TransportMidi { .. }
            )
        }) {
            self.pump();
        }
        let waiting = self.plugins.iter().any(|p| {
            p.runner.as_ref().is_some_and(|r| {
                r.running.is_none() && matches!(r.queue.front(), Some(Pending::ProjectOpen(None)))
            })
        });
        if waiting && !self.asked_world {
            self.asked_world = true;
            events.push(PluginEvent::NeedWorld);
        }
        events
    }

    /// Only the active, user-invoked worker can produce UI effects. Loading
    /// candidates and retiring states never acquire UI authority.
    pub fn poll_panel_request(&self) -> Option<std::sync::Arc<super::ui::Shared>> {
        self.plugins
            .iter()
            .filter_map(|p| p.runner.as_ref())
            .find_map(|r| r.worker.poll_panel_request())
    }
    pub fn poll_overlay_request(&self) -> Option<(String, super::overlays::Batch)> {
        self.plugins
            .iter()
            .filter(|p| p.status == Status::Ready)
            .find_map(|p| {
                p.runner
                    .as_ref()?
                    .worker
                    .poll_overlay_request()
                    .map(|b| (p.id.clone(), b))
            })
    }
    pub fn poll_playback_request(&self) -> Option<super::playback::control::Request> {
        self.plugins
            .iter()
            .filter_map(|p| p.runner.as_ref())
            .find_map(|r| r.worker.poll_playback_request())
    }
    pub fn poll_ui_request(&self) -> Option<super::bridge::UiRequest> {
        self.plugins
            .iter()
            .filter_map(|p| p.runner.as_ref())
            .find_map(|runner| runner.worker.poll_ui_request())
    }
    pub fn poll_midi_input_request(&self) -> Option<super::midi_input::ConsentRequest> {
        self.plugins
            .iter()
            .filter_map(|p| p.runner.as_ref())
            .find_map(|runner| runner.worker.poll_midi_input_request())
    }
}

/// Starts `job` on `worker`.
fn submit(
    worker: &mut ScriptWorker,
    job: &mut Pending,
    dir: &Path,
    entry: &str,
) -> Result<(), String> {
    if let Pending::Service { job, .. } = job {
        return worker
            .submit_service(job.clone())
            .map_err(|e| e.to_string());
    }
    if let Pending::Response {
        delivery,
        request,
        world,
    } = job
    {
        let world = world
            .take()
            .ok_or_else(|| "The automatic response snapshot was already submitted.".to_owned())?;
        return worker
            .submit_response(world, delivery.clone(), request.clone())
            .map_err(|e| e.to_string());
    }
    if let Pending::Format { request, world } = job {
        return worker
            .submit_format(request.clone(), world.take())
            .map_err(|e| e.to_string());
    }
    if let Pending::Observe(delivery) = job {
        return worker
            .submit_observation(delivery.clone())
            .map_err(|e| e.to_string());
    }
    let (file, source, world) = match job {
        Pending::Entry => (
            entry.to_owned(),
            std::fs::read_to_string(dir.join(entry.replace('\\', "/")))
                .map_err(|e| format!("The entry script {entry} could not be read ({e})."))?,
            None,
        ),
        Pending::Load => ("=load".into(), LOAD_SRC.into(), None),
        Pending::ProjectOpen(w) => ("=on_project_open".into(), OPEN_SRC.into(), w.clone()),
        Pending::ProjectClose => ("=on_project_close".into(), CLOSE_SRC.into(), None),
        Pending::Unload => ("=on_unload".into(), UNLOAD_SRC.into(), None),
        Pending::Observe(_)
        | Pending::Response { .. }
        | Pending::Service { .. }
        | Pending::Format { .. } => {
            unreachable!("typed native work handled above")
        }
        // Ids are checked: a - z, 0 - 9, '.', '_', '-'.
        Pending::Command { id, params, world } => (
            format!("={id}"),
            format!("return plum.plugin.__run('{id}', {params})"),
            world.clone(),
        ),
        Pending::Tool(t) => (
            format!("={}", t.tool),
            format!(
                "return plum.plugin.__run_tool('{}', {}, {{ track = {}, content = {}, preview = {}, seed = {}, keys = {} }})",
                t.tool,
                t.params,
                t.track,
                t.content
                    .map_or_else(|| "nil".to_owned(), |c| c.to_string()),
                t.preview,
                t.seed,
                t.keys
            ),
            t.world.clone(),
        ),
    };
    // B1-0040: what the user started (and can Cancel) may run long.
    let time_limit =
        matches!(job, Pending::Command { .. } | Pending::Tool(_)).then_some(super::USER_RUN_TIME);
    worker
        .submit_with_stroke(
            ScriptJob {
                file,
                source,
                world,
                time_limit,
            },
            matches!(job, Pending::Command { .. }) || matches!(job, Pending::Tool(t) if !t.preview),
            match job {
                Pending::Tool(t) => t.stroke.clone(),
                _ => None,
            },
        )
        .map_err(|e| e.to_string())
}

/// Applies a finished job's result to plugin `p` (its running state).
fn finish(p: &mut Plugin, job: Pending, res: ScriptResult, events: &mut Vec<PluginEvent>) {
    if let Some(mut decls) = res.decls.clone()
        && let Some(r) = p.runner.as_mut()
        && r.ready
    {
        if r.observation_failed {
            decls.observers.clear();
        }
        r.observations.sync(&decls);
        r.decls = decls.clone();
        p.decls = decls;
    }
    if let Some(delivery) = job.delivery()
        && !delivery.valid()
    {
        return;
    }
    let ScriptResult {
        result,
        commits,
        ui,
        output,
        decls,
        plugin,
        response_guard,
        service_reply: _,
    } = res;
    if let Some(intent) = plugin.loop_intent {
        if result.is_ok()
            && matches!(job, Pending::Command { .. })
            && commits.is_none()
            && intent.valid()
        {
            if let Some(r) = p.runner.as_ref() {
                events.push(PluginEvent::TransportLoop {
                    plugin: p.id.clone(),
                    intent,
                    lifecycle: std::sync::Arc::clone(&r.lifetime),
                });
            }
        } else {
            p.log.push(Level::Warning,"The staged loop was discarded: use a successful standalone command without musical transactions.");
        }
    }
    if let Some(intent) = plugin.midi_intent {
        if result.is_ok()
            && matches!(job, Pending::Command { .. })
            && commits.is_none()
            && intent.valid()
        {
            if let Some(r) = p.runner.as_ref() {
                events.push(PluginEvent::TransportMidi {
                    plugin: p.id.clone(),
                    intent,
                    lifecycle: std::sync::Arc::clone(&r.lifetime),
                });
            }
        } else {
            intent.refuse();
            events.push(PluginEvent::DiscardMidi(intent));
            p.log.push(Level::Warning,"The staged MIDI plan was discarded: use a successful standalone command without musical transactions.");
        }
    }
    for line in output {
        p.log.push(Level::Output, line);
    }
    for note in plugin.notes {
        p.log.push(Level::Warning, format!("settings: {note}"));
    }
    if job.delivery().is_none()
        && !matches!(job, Pending::Format { .. })
        && let Some(text) = plugin.project_data
    {
        events.push(PluginEvent::ProjectData {
            plugin: p.id.clone(),
            text,
        });
    }
    let mut response_notices = Vec::new();
    for (text, kind) in plugin.notices {
        if matches!(job, Pending::Response { .. }) {
            response_notices.push((
                format!("{}: {text}", p.name()),
                kind == super::plugin_api::NoticeKind::Error,
            ));
            continue;
        }
        p.log.push(Level::Info, format!("notify: {text}"));
        let text = format!("{}: {text}", p.name());
        let error = kind == super::plugin_api::NoticeKind::Error;
        events.push(if let Some(delivery) = job.delivery() {
            PluginEvent::ObservationNotice {
                delivery: delivery.clone(),
                text,
                error,
            }
        } else {
            PluginEvent::Notice { text, error }
        });
    }
    if result.is_ok()
        && let Pending::Observe(delivery) = &job
        && let Some(request) = plugin.response_request
    {
        events.push(PluginEvent::ResponseRequest {
            plugin: p.id.clone(),
            delivery: delivery.clone(),
            request,
        });
    }
    let what = job.label();
    let failed = |p: &mut Plugin, e: &ScriptError, events: &mut Vec<PluginEvent>| {
        p.log.push(Level::Error, format!("{what}: {e}"));
        events.push(PluginEvent::Notice {
            text: format!("{}: {e}", p.name()),
            error: true,
        });
    };
    match job {
        Pending::Entry | Pending::Load => match &result {
            Ok(_) => {
                let load = matches!(job, Pending::Load);
                if let Some(r) = p.runner.as_mut() {
                    if let Some(d) = decls {
                        r.decls = d;
                    }
                    if load {
                        r.ready = true;
                        r.worker.freeze_extensions();
                    }
                }
                if load {
                    p.decls = p
                        .runner
                        .as_ref()
                        .map(|r| r.decls.clone())
                        .unwrap_or_default();
                    p.status = Status::Ready;
                    p.log.push(
                        Level::Info,
                        format!(
                            "Ready: {} command(s), {} tool(s).",
                            p.decls.commands.len(),
                            p.decls.tools.len()
                        ),
                    );
                    events.push(PluginEvent::DeclsChanged);
                }
                if commits.is_some() {
                    p.log.push(
                        Level::Warning,
                        format!("{what}: changes made while loading are not applied."),
                    );
                }
            }
            Err(e) => {
                failed(p, e, events);
                p.status = Status::Failed(e.to_string());
                p.decls = Decls::default();
                p.runner = None;
            }
        },
        Pending::ProjectOpen(_) | Pending::ProjectClose => {
            if let Err(e) = &result {
                failed(p, e, events);
            }
            if commits.is_some() {
                p.log.push(
                    Level::Warning,
                    format!(
                        "{what}: lifecycle hooks may only read the project; its changes were not applied."
                    ),
                );
            }
        }
        Pending::Service { .. } => unreachable!("service results are handled by the native broker"),
        Pending::Response { delivery, .. } => match result {
            Ok(_) => {
                if let Some(guard) = response_guard {
                    events.push(PluginEvent::ResponseResult {
                        plugin: p.id.clone(),
                        delivery,
                        commits,
                        guard: Box::new(guard),
                        notices: response_notices,
                    });
                }
            }
            Err(e) => {
                if let Some(r) = p.runner.as_mut() {
                    r.observation_failed = true;
                    r.observations.clear();
                    r.decls.observers.clear();
                    p.decls = r.decls.clone();
                }
                failed(p, &e, events);
            }
        },
        Pending::Observe(_) => {
            if let Err(e) = &result {
                // Pause observation until reload. A handler cannot be retried
                // forever or revive itself by publishing later declarations.
                if let Some(r) = p.runner.as_mut() {
                    r.observation_failed = true;
                    r.observations.clear();
                    r.decls.observers.clear();
                    p.decls = r.decls.clone();
                }
                failed(p, e, events);
            }
        }
        Pending::Unload => {
            if let Err(e) = &result {
                p.log.push(Level::Warning, format!("{what}: {e}"));
            }
        }
        Pending::Format { request, .. } => {
            let selected = matches!(result, Ok(super::Plain::Boolean(true)));
            let result = result.map(|_| commits).map_err(|e| e.to_string());
            if let Some(r) = p.runner.as_ref() {
                events.push(PluginEvent::FormatResult {
                    plugin: p.id.clone(),
                    format: request.id,
                    mode: request.mode,
                    selected,
                    result,
                    lifecycle: Arc::clone(&r.lifetime),
                });
            }
        }
        Pending::Command { id, .. } => match result {
            Ok(v) => {
                p.log.push(Level::Info, format!("Ran {id}."));
                if let Some(c) = commits {
                    events.push(PluginEvent::Commits {
                        plugin: p.id.clone(),
                        command: id.clone(),
                        commits: c,
                    });
                }
                if let Some(u) = ui {
                    events.push(PluginEvent::Select {
                        plugin: p.id.clone(),
                        ui: u,
                    });
                }
                if let super::Plain::String(s) = v
                    && !s.is_empty()
                {
                    events.push(PluginEvent::Notice {
                        text: format!("{}: {}", p.name(), s.chars().take(200).collect::<String>()),
                        error: false,
                    });
                }
            }
            Err(e) => failed(p, &e, events),
        },
        Pending::Tool(t) => {
            let result = match result {
                Ok(_) => Ok(commits),
                Err(e) => {
                    // A canceled preview is routine (the controls moved on).
                    if !(t.preview && e.category == super::Category::Canceled) {
                        p.log.push(Level::Error, format!("{what}: {e}"));
                    }
                    Err(e.to_string())
                }
            };
            if !t.preview && result.is_ok() {
                p.log.push(Level::Info, format!("Applied {}.", t.tool));
            }
            events.push(PluginEvent::ToolResult {
                plugin: p.id.clone(),
                tool: t.tool,
                generation: t.generation,
                preview: t.preview,
                result,
            });
        }
    }
}
