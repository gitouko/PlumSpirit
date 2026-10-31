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

use super::package::{InstallReport, Manifest, Origin, Store};
use super::plugin_api::{Decls, PluginMeta};
use super::selection::UiSelection;
use super::tx::Commits;
use super::worker::{ScriptJob, ScriptResult, ScriptWorker};
use super::{Capability, Limits, ScriptError, Scripts, World};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
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

/// A plugin's diagnostics.
#[derive(Clone, Debug, Default)]
pub struct PluginLog {
    lines: VecDeque<LogLine>,
}

impl PluginLog {
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let at = format!(
            "{:02}:{:02}:{:02}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60
        );
        for l in text.into().lines() {
            if self.lines.len() == LOG_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(LogLine {
                at: at.clone(),
                level,
                text: l.to_owned(),
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
    Command {
        id: String,
        /// The options dialog's values as a Lua table literal (`nil`: none).
        params: String,
        world: Option<World>,
    },
    Tool(ToolJob),
    Unload,
}

impl Pending {
    fn label(&self) -> String {
        match self {
            Pending::Entry => "loading the entry script".into(),
            Pending::Load => "on_load / register".into(),
            Pending::ProjectOpen(_) => "on_project_open".into(),
            Pending::ProjectClose => "on_project_close".into(),
            Pending::Command { id, .. } => format!("the command {id}"),
            Pending::Tool(t) => format!("the tool {}", t.tool),
            Pending::Unload => "on_unload".into(),
        }
    }

    fn is_user_job(&self) -> bool {
        matches!(self, Pending::Command { .. } | Pending::Tool(_))
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
}

impl Runner {
    fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    /// Queues `on_unload()` (cancelling what runs) for a ready state.
    fn retire(mut self) -> Option<Runner> {
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
    /// A command finished with transactions: apply them through the undo
    /// history (ask first when `needs_confirmation`).
    Commits {
        plugin: String,
        command: String,
        commits: Commits,
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
    /// A command asked the UI to select these.
    Select { plugin: String, ui: UiSelection },
    /// A short message for the status bar (`error`: shown as an error).
    Notice { text: String, error: bool },
    /// B1-0035: a plugin's project settings changed: store `text` (empty:
    /// remove) in the open project, outside the undo history.
    ProjectData { plugin: String, text: String },
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
            r.queue.push_back(Pending::Command {
                id: command.to_owned(),
                params,
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
        if r.queue.iter().any(|j| matches!(j, Pending::Command { .. }))
            || r.running
                .as_ref()
                .is_some_and(|j| matches!(j, Pending::Command { .. }))
        {
            return Err(format!("{} is running a command; wait for it.", p.name()));
        }
        r.queue.retain(|j| !matches!(j, Pending::Tool(_)));
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
            r.queue.retain(|j| !j.is_user_job());
            if r.running.as_ref().is_some_and(Pending::is_user_job) {
                r.worker.cancel();
            }
        }
    }

    /// A project was opened (or the open one replaced).
    pub fn project_opened(&mut self, world: World) {
        self.project_open = true;
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                r.queue.push_back(Pending::ProjectOpen(Some(world.clone())));
            }
        }
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
        self.project_open = false;
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                r.queue.retain(|j| !matches!(j, Pending::ProjectOpen(_)));
                if r.ready {
                    r.queue.push_back(Pending::ProjectClose);
                }
            }
        }
        self.pump();
    }

    /// Whether any plugin runs something (the app keeps polling while so).
    pub fn is_busy(&self) -> bool {
        self.plugins.iter().any(Plugin::is_busy)
    }

    /// Submits the next job of every idle state.
    fn pump(&mut self) {
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
                let Some(job) = r.queue.pop_front() else {
                    continue;
                };
                match submit(&mut r.worker, &job, &dir, &entry) {
                    Ok(()) => {
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
    pub fn poll(&mut self, _prefs: &PluginPrefs) -> Vec<PluginEvent> {
        let mut events = std::mem::take(&mut self.events);
        let project_open = self.project_open;
        for p in &mut self.plugins {
            // States stopping: dropped after on_unload (or the grace time).
            let mut k = 0;
            while k < p.retiring.len() {
                let r = &mut p.retiring[k];
                let done = match r.worker.poll() {
                    Some(res) => {
                        for line in &res.output {
                            p.log.push(Level::Output, line.clone());
                        }
                        if let Err(e) = &res.result {
                            p.log.push(Level::Warning, format!("on_unload: {e}"));
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
                finish(p, job, res, &mut events);
            }
        }
        self.pump();
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
}

/// Starts `job` on `worker`.
fn submit(worker: &mut ScriptWorker, job: &Pending, dir: &Path, entry: &str) -> Result<(), String> {
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
        .submit(ScriptJob {
            file,
            source,
            world,
            time_limit,
        })
        .map_err(|e| e.to_string())
}

/// Applies a finished job's result to plugin `p` (its running state).
fn finish(p: &mut Plugin, job: Pending, res: ScriptResult, events: &mut Vec<PluginEvent>) {
    let ScriptResult {
        result,
        commits,
        ui,
        output,
        decls,
        plugin,
    } = res;
    for line in output {
        p.log.push(Level::Output, line);
    }
    for note in plugin.notes {
        p.log.push(Level::Warning, format!("settings: {note}"));
    }
    if let Some(text) = plugin.project_data {
        events.push(PluginEvent::ProjectData {
            plugin: p.id.clone(),
            text,
        });
    }
    for (text, kind) in plugin.notices {
        p.log.push(Level::Info, format!("notify: {text}"));
        events.push(PluginEvent::Notice {
            text: format!("{}: {text}", p.name()),
            error: kind == super::plugin_api::NoticeKind::Error,
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
        Pending::Unload => {
            if let Err(e) = &result {
                p.log.push(Level::Warning, format!("{what}: {e}"));
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
