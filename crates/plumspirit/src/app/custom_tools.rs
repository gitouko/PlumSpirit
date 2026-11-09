//! v1.0-beta1 (B1-0045 – B1-0048): Tools → Custom — note tools that Lua
//! plugins declare, in a native PlumSpirit window built from their parameter
//! schema (the same controls and curve editor as the built-in tools).
//!
//! * **Preview** runs the tool on the plugin's worker against a snapshot of
//!   the project and keeps its transactions; the Piano Roll draws the result
//!   notes of the edited track or clip (the originals hidden), as for built-in
//!   tools. Nothing in the document changes while previewing.
//! * **Apply** applies exactly the previewed transactions through the undo
//!   history (Preview = Apply; one Undo step). When no preview is current —
//!   or the project changed since — the tool runs once more and its result
//!   is applied.
//! * **Cancel** drops everything: the document was never touched.
//! * Presets (`<settings>/presets/custom/<plugin>/<tool>/<name>.json`), Reset,
//!   a seed dice and an optional shortcut per tool (refused when PlumSpirit
//!   uses the chord).

use super::PlumSpiritApp;
use crate::lua::custom::{NoteFilter, ToolDecl, ToolParamKind, ToolValue, params_literal};
use crate::lua::plugins::{Status, ToolJob};
use crate::lua::tx::Commits;
use crate::ui::status_bar::StatusKind;
use egui::RichText;
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::transform::TNote;
use plumspirit_engine::transform::param::Curve;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

/// Preview result notes drawn at most (more: the window says so and Apply
/// still works).
pub const PREVIEW_NOTE_CAP: usize = 2_000_000;

/// The kept result of a run for one set of inputs.
pub struct ToolResultSlot {
    key: String,
    commits: Commits,
    target: NoteTarget,
    /// The result notes of `target`, sorted by start (drawn).
    pub notes: Vec<TNote>,
    pub max_dur: u64,
    pub rev: u64,
    /// More than [`PREVIEW_NOTE_CAP`] notes: none are drawn.
    pub too_many: bool,
}

/// An open custom tool window.
pub struct CustomToolSession {
    pub plugin: String,
    pub plugin_name: String,
    pub decl: ToolDecl,
    pub values: Vec<ToolValue>,
    pub preview: bool,
    pub stroke: Option<std::sync::Arc<crate::lua::drawing::Captured>>,
    pub stroke_overlay: Vec<(u64, f64)>,
    pub stroking: bool,
    stroke_rev: u64,
    stroke_target: Option<NoteTarget>,
    generation: u64,
    /// The inputs of the newest request and its generation.
    requested: Option<(String, u64)>,
    pub result: Option<ToolResultSlot>,
    /// An Apply run waiting (its generation).
    applying: Option<u64>,
    pub status: String,
    pub error: Option<String>,
    preset_name: String,
    presets: Vec<String>,
    preset_msg: Option<String>,
    shortcut_text: String,
    shortcut_msg: Option<String>,
}

/// The key a run depends on: parameters, project state, edited notes and
/// their selection.
fn inputs_key(params: &str, state: u64, target: NoteTarget, notes_rev: u64) -> String {
    format!("{params}|{state}|{target:?}|{notes_rev}")
}

/// Preset folder of one custom tool.
fn preset_dir(config: Option<&std::path::Path>, plugin: &str, tool: &str) -> Option<PathBuf> {
    // Ids are checked (a - z, 0 - 9, '.', '_', '-'): safe folder names.
    Some(
        config?
            .join("presets")
            .join("custom")
            .join(plugin)
            .join(tool),
    )
}

/// A preset name: 1 – 60 characters without path or reserved characters.
fn safe_preset_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty()
        || n.chars().count() > 60
        || n.chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || n.starts_with('.')
        || n.ends_with('.')
    {
        return Err("A preset name is 1 – 60 characters without < > : \" / \\ | ? * or a leading / trailing dot.".into());
    }
    Ok(n.to_owned())
}

/// Values as preset JSON (by parameter key).
pub fn values_to_json(decl: &ToolDecl, values: &[ToolValue]) -> Json {
    let mut m = serde_json::Map::new();
    for (p, v) in decl.params.iter().zip(values) {
        let j = match v {
            ToolValue::Int(n) => json!(n),
            ToolValue::Float(f) => json!(f),
            ToolValue::Bool(b) => json!(b),
            ToolValue::Choice(i) => match &p.kind {
                ToolParamKind::Choice(c) => json!(c.get(*i)),
                _ => json!(i),
            },
            ToolValue::Text(s) => json!(s),
            ToolValue::Color(c) => json!(c),
            ToolValue::Curve(c) => json!(c.points),
            ToolValue::Range(a, b) => json!([a, b]),
            ToolValue::Map(m) => json!(m),
            ToolValue::Filter(f) => json!({
                "keys": [f.keys.0, f.keys.1],
                "velocity": [f.velocity.0, f.velocity.1],
                "channel": f.channel,
            }),
        };
        m.insert(p.key.clone(), j);
    }
    json!({ "version": 1, "values": Json::Object(m) })
}

/// Values from preset JSON: parameters it lacks or holds wrongly keep their
/// defaults (each one named in the returned problems).
pub fn values_from_json(decl: &ToolDecl, j: &Json) -> (Vec<ToolValue>, Vec<String>) {
    let mut out = decl.defaults();
    let mut problems = Vec::new();
    let Some(vals) = j.get("values").and_then(Json::as_object) else {
        return (out, vec!["The preset holds no values.".into()]);
    };
    for (i, p) in decl.params.iter().enumerate() {
        let Some(v) = vals.get(&p.key) else { continue };
        let parsed: Option<ToolValue> = match &p.kind {
            ToolParamKind::Int { min, max } => v
                .as_i64()
                .filter(|n| n >= min && n <= max)
                .map(ToolValue::Int),
            ToolParamKind::Float { min, max, .. } => v
                .as_f64()
                .filter(|f| f >= min && f <= max)
                .map(ToolValue::Float),
            ToolParamKind::Bool => v.as_bool().map(ToolValue::Bool),
            ToolParamKind::Choice(c) => v
                .as_str()
                .and_then(|s| c.iter().position(|x| x == s))
                .map(ToolValue::Choice),
            ToolParamKind::Pitch => v
                .as_i64()
                .filter(|n| (0..=255).contains(n))
                .map(ToolValue::Int),
            ToolParamKind::Ticks { min, max } => v
                .as_u64()
                .filter(|n| n >= min && n <= max)
                .map(|n| ToolValue::Int(n as i64)),
            ToolParamKind::Seed => v.as_i64().map(ToolValue::Int),
            ToolParamKind::Text { max_len, .. } => v
                .as_str()
                .filter(|s| s.chars().count() <= *max_len)
                .map(|s| ToolValue::Text(s.to_owned())),
            ToolParamKind::Color => serde_json::from_value::<[u8; 3]>(v.clone())
                .ok()
                .map(ToolValue::Color),
            ToolParamKind::Curve => serde_json::from_value::<Vec<(f64, f64)>>(v.clone())
                .ok()
                .filter(|p| {
                    (2..=plumspirit_engine::transform::param::MAX_CURVE_POINTS).contains(&p.len())
                        && p.iter()
                            .all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
                })
                .map(|p| ToolValue::Curve(Curve::new(p))),
            ToolParamKind::Range { min, max } => serde_json::from_value::<(i64, i64)>(v.clone())
                .ok()
                .filter(|(a, b)| a <= b && a >= min && b <= max)
                .map(|(a, b)| ToolValue::Range(a, b)),
            ToolParamKind::SubchannelMap => serde_json::from_value::<Vec<(u32, u32)>>(v.clone())
                .ok()
                .filter(|m| m.len() <= crate::lua::custom::MAX_MAP)
                .map(ToolValue::Map),
            ToolParamKind::NoteFilter => {
                let keys = v
                    .get("keys")
                    .and_then(|k| serde_json::from_value::<(u8, u8)>(k.clone()).ok());
                let vel = v
                    .get("velocity")
                    .and_then(|k| serde_json::from_value::<(u8, u8)>(k.clone()).ok());
                let ch = v.get("channel").map(|c| c.as_u64().map(|c| c as u8));
                match (keys, vel, ch) {
                    (Some(k), Some(vl), ch)
                        if k.0 <= k.1 && vl.0 <= vl.1 && vl.0 >= 1 && vl.1 <= 127 =>
                    {
                        Some(ToolValue::Filter(NoteFilter {
                            keys: k,
                            velocity: vl,
                            channel: ch.flatten().filter(|c| *c <= 15),
                        }))
                    }
                    _ => None,
                }
            }
        };
        match parsed {
            Some(pv) => out[i] = pv,
            None => problems.push(format!(
                "{} kept its default (the preset's value is not valid).",
                p.label
            )),
        }
    }
    (out, problems)
}

impl CustomToolSession {
    fn new(
        plugin: String,
        plugin_name: String,
        decl: ToolDecl,
        config: Option<&std::path::Path>,
    ) -> Self {
        let values = decl.defaults();
        let mut s = CustomToolSession {
            plugin,
            plugin_name,
            decl,
            values,
            preview: true,
            stroke: None,
            stroke_overlay: Vec::new(),
            stroking: false,
            stroke_rev: 0,
            stroke_target: None,
            generation: 0,
            requested: None,
            result: None,
            applying: None,
            status: String::new(),
            error: None,
            preset_name: String::new(),
            presets: Vec::new(),
            preset_msg: None,
            shortcut_text: String::new(),
            shortcut_msg: None,
        };
        s.reload_presets(config);
        s
    }

    pub fn capture_mode(&self) -> Option<crate::piano_roll::CaptureMode> {
        self.decl
            .freehand
            .then_some(crate::piano_roll::CaptureMode::FreehandOrdered)
    }
    fn key_params(&self) -> String {
        let params = params_literal(&self.decl, &self.values);
        if self.decl.freehand {
            format!("{params}|stroke:{}", self.stroke_rev)
        } else {
            params
        }
    }
    fn clear_stroke(&mut self) {
        self.stroke = None;
        self.stroke_overlay.clear();
        self.stroking = false;
        self.stroke_target = None;
        self.stroke_rev = self.stroke_rev.wrapping_add(1);
        self.requested = None;
        self.result = None;
        self.applying = None;
        self.generation += 1;
    }
    fn reload_presets(&mut self, config: Option<&std::path::Path>) {
        self.presets.clear();
        if let Some(d) = preset_dir(config, &self.plugin, &self.decl.id)
            && let Ok(rd) = std::fs::read_dir(d)
        {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if let Some(stem) = n.strip_suffix(".json") {
                    self.presets.push(stem.to_owned());
                }
            }
        }
        self.presets.sort();
    }
}

/// What the roll draws for the open custom tool on `target`.
pub fn custom_preview(
    s: &Option<CustomToolSession>,
    target: NoteTarget,
) -> Option<crate::piano_roll::ToolPreview<'_>> {
    let s = s.as_ref()?;
    if !s.preview {
        return None;
    }
    let r = s.result.as_ref()?;
    if r.target != target || r.too_many {
        return None;
    }
    Some(crate::piano_roll::ToolPreview::Run {
        notes: &r.notes,
        extra: &[],
        max_dur: r.max_dur,
        all: true,
        rev: r.rev,
    })
}

/// Every ready plugin's custom tools: (plugin id, plugin name, [(tool id,
/// name, tooltip)]).
pub type CustomMenu = Vec<(String, String, Vec<(String, String, String)>)>;

impl PlumSpiritApp {
    /// Completed strokes use the existing roll capture and plugin worker. Partial
    /// samples are a bounded native overlay, never per-pointer Lua callbacks.
    pub(super) fn custom_stroke(
        &mut self,
        points: &[(u64, f64, f64)],
        done: bool,
        ctx: &egui::Context,
    ) {
        let Some((target, _, _, track, content)) = self.custom_target() else {
            return;
        };
        let Some(s) = self.custom_tool.as_ref() else {
            return;
        };
        let plugin = s.plugin.clone();
        let allowed = self.note_lock().is_none()
            && self.settings.lua_plugins.run_plugins
            && self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin(&plugin))
                .is_some_and(|p| {
                    let state = self.settings.lua_plugins.installed.get(&plugin);
                    state.is_some_and(|s| s.enabled)
                        && p.effective(state)
                            .contains(&crate::lua::Capability::DrawingTools)
                });
        if !allowed {
            self.cancel_custom_stroke();
            return;
        }
        let shift = self.edit_shift();
        let snap = self.snap_ticks();
        let keys = self
            .doc()
            .map_or(128, |d| u16::from(d.project().keys.max_pitch()) + 1);
        let ppq = self
            .doc()
            .and_then(|d| d.division().musical_ppqn())
            .unwrap_or(960);
        let mods = ctx.input(|i| i.modifiers);
        let Some(s) = self.custom_tool.as_mut() else {
            return;
        };
        if !s.decl.freehand || s.applying.is_some() {
            return;
        }
        if !s.stroking {
            s.clear_stroke();
            s.stroking = true;
            if let Some(m) = self.lua_plugins.as_mut() {
                m.cancel(&plugin);
            }
        }
        s.stroke_target = Some(target);
        s.stroke_overlay = points
            .iter()
            .take(plumspirit_engine::transform::MAX_STROKE_POINTS)
            .map(|&(t, p, _)| (t, p))
            .collect();
        if done {
            s.stroking = false;
            s.stroke_rev = s.stroke_rev.wrapping_add(1);
            match crate::lua::drawing::Captured::new(
                points,
                track,
                content,
                shift,
                keys,
                ppq,
                snap,
                [mods.shift, mods.ctrl, mods.alt, mods.command],
            ) {
                Ok(input) => {
                    s.stroke = Some(input);
                    s.status = "Stroke captured. Inspect Preview, then Apply.".into();
                    s.error = None;
                }
                Err(e) => {
                    s.clear_stroke();
                    s.error = Some(e);
                }
            }
        } else {
            s.status = "Capturing stroke; Lua runs after release.".into();
        }
    }
    pub(super) fn cancel_custom_stroke(&mut self) {
        if let Some(s) = self.custom_tool.as_mut()
            && s.decl.freehand
        {
            s.clear_stroke();
            s.status = "Stroke canceled; project unchanged.".into();
            if let Some(m) = self.lua_plugins.as_mut() {
                m.cancel(&s.plugin);
            }
        }
    }
    /// Tools → Custom: every ready plugin's tools.
    pub(super) fn custom_tool_menu(&self) -> CustomMenu {
        let Some(m) = &self.lua_plugins else {
            return Vec::new();
        };
        m.plugins()
            .iter()
            .filter(|p| p.status == Status::Ready && !p.decls.tools.is_empty())
            .map(|p| {
                (
                    p.id.clone(),
                    p.name(),
                    p.decls
                        .tools
                        .iter()
                        .map(|t| (t.id.clone(), t.name.clone(), t.tooltip.clone()))
                        .collect(),
                )
            })
            .collect()
    }

    /// Opens custom tool `tool` of `plugin` (Tools → Custom, the note menu,
    /// its shortcut).
    pub(super) fn open_custom_tool(&mut self, plugin: &str, tool: &str) {
        // B2-A01: custom tools change the roll's notes (their commits do not
        // pass `apply_op`): none while the roll is locked.
        if let Some(lock) = self.note_lock() {
            self.set_status(lock.hint(), StatusKind::Info);
            return;
        }
        let Some(m) = &self.lua_plugins else { return };
        let Some(p) = m.plugin(plugin) else { return };
        let Some(decl) = p.decls.tools.iter().find(|t| t.id == tool).cloned() else {
            self.set_status(
                format!("The custom tool {tool} is not available (is its plugin enabled?)."),
                StatusKind::Warning,
            );
            return;
        };
        if decl.freehand
            && !p
                .effective(self.settings.lua_plugins.installed.get(plugin))
                .contains(&crate::lua::Capability::DrawingTools)
        {
            self.set_status(
                "This tool needs the Piano Roll drawing tools permission (tools.draw).",
                StatusKind::Warning,
            );
            return;
        }
        let name = p.name();
        // Built-in tool windows and a custom one never preview at once.
        self.tool = None;
        let mut s =
            CustomToolSession::new(plugin.to_owned(), name, decl, self.config_dir.as_deref());
        let key = format!("{plugin}/{tool}");
        s.shortcut_text = self
            .settings
            .lua_plugins
            .tool_keys
            .get(&key)
            .cloned()
            .unwrap_or_default();
        self.custom_tool = Some(s);
        if self.main_view == super::track_list::MainView::TrackList {
            self.main_view = super::track_list::MainView::PianoRoll;
        }
    }

    /// The edited notes' state for run keys: (target, store revision).
    fn custom_target(&self) -> Option<(NoteTarget, u64, u64, u32, Option<u32>)> {
        let o = self.open.as_ref()?;
        let target = self.edit_target();
        let view = plumspirit_engine::editor::note_view(&o.doc, target).ok()?;
        let track = view.track.id().0;
        let content = view.content.map(|c| c.id.0);
        Some((
            target,
            view.notes.revision(),
            o.history.current_state(),
            track,
            content,
        ))
    }

    /// Project plugin settings are outside musical history. A retained preview
    /// pins its source Arc, so release it before the native settings writer needs
    /// unique access. Generation fencing also discards a late in-flight reply.
    pub(super) fn custom_project_data_staged(&mut self) {
        if let Some(s) = self.custom_tool.as_mut() {
            s.clear_stroke();
            s.status = "Project plugin settings changed; waiting for a fresh preview.".into();
        }
    }

    /// Requests a run (`preview`) for the current inputs.
    fn request_custom_run(&mut self, preview: bool, dark: bool) {
        // Do not repin the old source while project data waits for unique access.
        if !self.lua_project_data.is_empty() {
            return;
        }
        let Some((target, notes_rev, state, track, content)) = self.custom_target() else {
            return;
        };
        let keys = self
            .open
            .as_ref()
            .map_or(128, |o| u16::from(o.doc.project().keys.max_pitch()) + 1);
        let world = self.lua_world(dark);
        let Some(s) = self.custom_tool.as_mut() else {
            return;
        };
        if s.decl.freehand && (s.stroke.is_none() || s.stroking || s.stroke_target != Some(target))
        {
            return;
        }
        let params = params_literal(&s.decl, &s.values);
        let key = inputs_key(&s.key_params(), state, target, notes_rev);
        s.generation += 1;
        let generation = s.generation;
        let seed = s
            .decl
            .params
            .iter()
            .zip(&s.values)
            .find(|(p, _)| p.kind == ToolParamKind::Seed)
            .and_then(|(_, v)| match v {
                ToolValue::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(1);
        let job = ToolJob {
            stroke: s.stroke.clone(),
            tool: s.decl.id.clone(),
            params,
            track,
            content,
            preview,
            seed,
            keys,
            generation,
            world,
        };
        let plugin = s.plugin.clone();
        s.requested = Some((key, generation));
        if !preview {
            s.applying = Some(generation);
        }
        s.status = if preview {
            "Previewing…".into()
        } else {
            "Applying…".into()
        };
        let r = self.lua_plugins().run_tool(&plugin, job);
        if let Err(e) = r
            && let Some(s) = self.custom_tool.as_mut()
        {
            s.error = Some(e);
            s.applying = None;
        }
    }

    /// A tool run finished (from the plugin manager).
    pub(super) fn custom_tool_result(
        &mut self,
        plugin: &str,
        generation: u64,
        preview: bool,
        result: Result<Option<Commits>, String>,
    ) {
        let Some(s) = self.custom_tool.as_mut() else {
            return;
        };
        if s.plugin != plugin || s.requested.as_ref().is_none_or(|(_, g)| *g != generation) {
            return; // an older request: the controls moved on
        }
        let key = s
            .requested
            .as_ref()
            .map(|(k, _)| k.clone())
            .unwrap_or_default();
        match result {
            Err(e) => {
                s.error = Some(e);
                s.result = None;
                s.applying = None;
                s.status.clear();
            }
            Ok(None) => {
                s.error = None;
                s.result = None;
                s.status = "The tool changed nothing.".into();
                if s.applying == Some(generation) {
                    s.applying = None;
                }
            }
            Ok(Some(commits)) => {
                s.error = None;
                let Some((target, ..)) = self.custom_target() else {
                    return;
                };
                let slot = result_slot(key, commits, target);
                let Some(s) = self.custom_tool.as_mut() else {
                    return;
                };
                s.status = if slot.too_many {
                    format!(
                        "The result has more than {} notes: no preview is drawn; Apply works.",
                        plumspirit_engine::midi::group_digits(PREVIEW_NOTE_CAP as u64)
                    )
                } else {
                    format!(
                        "Preview: {} notes on the edited track.",
                        plumspirit_engine::midi::group_digits(slot.notes.len() as u64)
                    )
                };
                s.result = Some(slot);
                if !preview || s.applying == Some(generation) {
                    s.applying = None;
                    self.apply_custom_result();
                }
            }
        }
    }

    /// Applies the kept result (Preview = Apply) through the undo history.
    fn apply_custom_result(&mut self) {
        self.release_preview_worker();
        self.release_tool_worker();
        let Some(s) = self.custom_tool.as_mut() else {
            return;
        };
        let Some(slot) = s.result.take() else { return };
        let name = s.decl.name.clone();
        let mut commits = slot.commits;
        if commits.needs_confirmation() {
            s.error = Some(format!(
                "{name} would remove {} notes; nothing was changed. Narrow the selection, or run it in smaller parts.",
                plumspirit_engine::midi::group_digits(commits.removed_notes())
            ));
            return;
        }
        let Some(o) = self.open.as_mut() else { return };
        match commits.apply(&mut o.history, &mut o.doc) {
            Ok(_) => {
                let label = o.history.undo_label().unwrap_or("").to_owned();
                if let Some(s) = self.custom_tool.as_mut() {
                    s.status = format!("Applied ({label}). Undo takes it back.");
                    s.requested = None;
                    if s.decl.freehand {
                        s.clear_stroke();
                    }
                }
            }
            Err(crate::lua::tx::CommitError::ProjectChanged) => {
                if let Some(s) = self.custom_tool.as_mut()
                    && s.decl.freehand
                {
                    s.clear_stroke();
                    s.error = Some("The project changed after Preview; draw a new stroke.".into());
                    return;
                }
                // The project moved on since the preview: run once more.
                if let Some(s) = self.custom_tool.as_mut() {
                    s.status =
                        "The project changed since the preview; running the tool again…".into();
                }
                self.request_custom_run(false, self.dark_mode);
            }
            Err(e) => {
                if let Some(s) = self.custom_tool.as_mut() {
                    s.error = Some(e.user_message());
                }
            }
        }
    }

    /// The window (and the preview's upkeep) every frame.
    pub(super) fn custom_tool_ui(&mut self, ctx: &egui::Context) {
        let dark = ctx.theme() == egui::Theme::Dark;
        self.dark_mode = dark;
        if self.custom_tool.is_none() {
            return;
        }
        // The plugin unloaded or the tool vanished: close.
        let alive = self.custom_tool.as_ref().is_some_and(|s| {
            self.lua_plugins
                .as_ref()
                .and_then(|m| m.plugin(&s.plugin))
                .is_some_and(|p| {
                    p.status == Status::Ready
                        && p.decls.tools.iter().any(|t| t.id == s.decl.id)
                        && (!s.decl.freehand
                            || p.effective(self.settings.lua_plugins.installed.get(&s.plugin))
                                .contains(&crate::lua::Capability::DrawingTools))
                })
        });
        if !alive {
            if self.custom_tool.as_ref().is_some_and(|s| s.decl.freehand) {
                self.roll.cancel_gesture();
            }
            self.custom_tool = None;
            return;
        }
        if self
            .custom_tool
            .as_ref()
            .is_some_and(|s| s.decl.freehand && s.stroking)
            && !ctx.input(|i| i.focused)
        {
            self.cancel_custom_stroke();
            self.roll.cancel_gesture();
        }
        let current_target = self.edit_target();
        let current_shift = self.edit_shift();
        let current_keys = self
            .doc()
            .map_or(128, |d| u16::from(d.project().keys.max_pitch()) + 1);
        if let Some(s) = self.custom_tool.as_mut()
            && s.decl.freehand
            && (s.stroke_target.is_some_and(|t| t != current_target)
                || s.stroke
                    .as_ref()
                    .is_some_and(|v| v.shift != current_shift || v.keys != current_keys))
        {
            s.clear_stroke();
        }
        if self.note_lock().is_some() && self.custom_tool.as_ref().is_some_and(|s| s.decl.freehand)
        {
            if let Some(s) = self.custom_tool.take()
                && let Some(m) = self.lua_plugins.as_mut()
            {
                m.cancel(&s.plugin);
            }
            self.roll.cancel_gesture();
            return;
        }
        // Live preview: request a run when the inputs changed.
        let want_preview = self.custom_tool.as_ref().is_some_and(|s| {
            s.preview
                && s.applying.is_none()
                && (!s.decl.freehand || (s.stroke.is_some() && !s.stroking))
        });
        if want_preview && let Some((target, notes_rev, state, ..)) = self.custom_target() {
            let s = self.custom_tool.as_ref().expect("checked");
            let key = inputs_key(&s.key_params(), state, target, notes_rev);
            if s.requested.as_ref().is_none_or(|(k, _)| *k != key) {
                self.request_custom_run(true, dark);
            }
        }
        let keys = self
            .open
            .as_ref()
            .map_or(127, |o| o.doc.project().keys.max_pitch());
        let has_selection = self.selected_count() > 0;
        let config = self.config_dir.clone();
        let mut open = true;
        let mut action: Option<UiAction> = None;
        let s = self.custom_tool.as_mut().expect("checked");
        let title = format!("{} — {}", s.decl.name, s.plugin_name);
        let mut want_help = false;
        egui::Window::new(title)
            .id(egui::Id::new("plumspirit-custom-tool"))
            .open(&mut open)
            .default_width(380.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                if !s.decl.tooltip.is_empty() {
                    ui.label(&s.decl.tooltip);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Custom tool from the plugin {} (Tools → Custom).", s.plugin_name))
                            .small()
                            .weak(),
                    );
                    want_help |= super::help::help_button(ui, "lua-plugins");
                });
                if s.decl.freehand {
                    ui.label("Draw in the Piano Roll, inspect Preview, then Apply. Each Apply is one Undo step.");
                    ui.label("Escape cancels the active stroke. A new stroke replaces an unapplied stroke.");
                    if ui.add_enabled(s.stroke.is_some() || s.stroking,egui::Button::new("Clear stroke")).clicked() {action=Some(UiAction::ClearStroke);}
                }
                ui.separator();
                // Match native tools: parameters scroll, action footer remains reachable.
                let screen=ui.ctx().content_rect();
                let room=screen.bottom()-ui.cursor().top()-170.;
                egui::ScrollArea::vertical()
                    .id_salt("custom-tool-parameters")
                    .max_height(room.clamp(80.,(screen.height()*0.55).max(80.)))
                    .auto_shrink([false,true])
                    .show(ui,|ui|param_form(ui,&s.decl.params,&mut s.values,keys));
                ui.separator();
                if s.decl.needs_selection && !has_selection {
                    ui.colored_label(ui.visuals().warn_fg_color, "Select notes first: this tool works on the selection.");
                }
                if let Some(e) = &s.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                } else if !s.status.is_empty() {
                    ui.label(&s.status);
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.preview, "Preview")
                        .on_hover_text("Shows the result in the Piano Roll before you apply it (the project is not changed).");
                    let can = (!s.decl.needs_selection || has_selection) && (!s.decl.freehand || (s.stroke.is_some() && !s.stroking));
                    if ui
                        .add_enabled(can && s.applying.is_none(), egui::Button::new("Apply"))
                        .on_hover_text("Applies the result as one Undo step.")
                        .clicked()
                    {
                        action = Some(UiAction::Apply);
                    }
                    if ui.button("Cancel").on_hover_text("Closes the tool; nothing is changed.").clicked() {
                        action = Some(UiAction::Close);
                    }
                    if ui.button("Reset").on_hover_text("Every control back to its default.").clicked() {
                        s.values = s.decl.defaults();
                    }
                });
                ui.collapsing("Presets", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.preset_name).hint_text("preset name").desired_width(160.0));
                        if ui.button("Save").clicked() {
                            action = Some(UiAction::SavePreset);
                        }
                    });
                    for name in s.presets.clone() {
                        ui.horizontal(|ui| {
                            if ui.button(&name).on_hover_text("Load this preset.").clicked() {
                                action = Some(UiAction::LoadPreset(name.clone()));
                            }
                            if ui.small_button("Delete").clicked() {
                                action = Some(UiAction::DeletePreset(name.clone()));
                            }
                        });
                    }
                    if let Some(m) = &s.preset_msg {
                        ui.label(RichText::new(m).small());
                    }
                });
                ui.collapsing("Shortcut", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.shortcut_text).hint_text("e.g. Ctrl+Alt+1").desired_width(140.0))
                            .on_hover_text("A key that opens this tool. Keys PlumSpirit already uses are refused.");
                        if ui.button("Set").clicked() {
                            action = Some(UiAction::SetShortcut);
                        }
                        if ui.button("Clear").clicked() {
                            s.shortcut_text.clear();
                            action = Some(UiAction::SetShortcut);
                        }
                    });
                    if let Some(m) = &s.shortcut_msg {
                        ui.label(RichText::new(m).small());
                    }
                });
            });
        if !open {
            action = Some(UiAction::Close);
        }
        if want_help {
            self.open_help("lua-plugins");
        }
        if let Some(a) = action {
            self.custom_tool_action(a, config.as_deref(), dark);
        }
    }

    fn custom_tool_action(&mut self, a: UiAction, config: Option<&std::path::Path>, dark: bool) {
        match a {
            UiAction::ClearStroke => {
                self.roll.cancel_gesture();
                if let Some(s) = self.custom_tool.as_mut() {
                    s.clear_stroke();
                    if let Some(m) = self.lua_plugins.as_mut() {
                        m.cancel(&s.plugin);
                    }
                }
            }
            UiAction::Close => {
                if self.custom_tool.as_ref().is_some_and(|s| s.decl.freehand) {
                    self.roll.cancel_gesture();
                }
                if let Some(s) = self.custom_tool.take()
                    && let Some(m) = self.lua_plugins.as_mut()
                {
                    m.cancel(&s.plugin);
                }
            }
            UiAction::Apply => {
                if !self.lua_project_data.is_empty() {
                    self.custom_project_data_staged();
                    return;
                }
                let current = self
                    .custom_target()
                    .and_then(|(target, notes_rev, state, ..)| {
                        let s = self.custom_tool.as_ref()?;
                        let key = inputs_key(&s.key_params(), state, target, notes_rev);
                        Some(s.result.as_ref().is_some_and(|r| r.key == key))
                    });
                if current == Some(true) {
                    self.apply_custom_result();
                } else if self.custom_tool.as_ref().is_some_and(|s| s.decl.freehand) {
                    if let Some(s) = self.custom_tool.as_mut() {
                        s.error = Some(
                            "Preview is not current. Wait for a new Preview before Apply.".into(),
                        );
                    }
                    self.request_custom_run(true, dark);
                } else {
                    self.request_custom_run(false, dark);
                }
            }
            UiAction::SavePreset => {
                let Some(s) = self.custom_tool.as_mut() else {
                    return;
                };
                s.preset_msg = Some(
                    match (
                        safe_preset_name(&s.preset_name),
                        preset_dir(config, &s.plugin, &s.decl.id),
                    ) {
                        (Err(e), _) => e,
                        (_, None) => {
                            "Presets need the settings folder, which is unavailable.".into()
                        }
                        (Ok(n), Some(d)) => {
                            let text =
                                serde_json::to_string_pretty(&values_to_json(&s.decl, &s.values))
                                    .unwrap_or_default();
                            match std::fs::create_dir_all(&d)
                                .and_then(|()| std::fs::write(d.join(format!("{n}.json")), text))
                            {
                                Ok(()) => format!("Saved \u{201C}{n}\u{201D}."),
                                Err(e) => format!("The preset could not be saved ({e})."),
                            }
                        }
                    },
                );
                s.reload_presets(config);
            }
            UiAction::LoadPreset(n) => {
                let Some(s) = self.custom_tool.as_mut() else {
                    return;
                };
                let Some(d) = preset_dir(config, &s.plugin, &s.decl.id) else {
                    return;
                };
                s.preset_msg = Some(match std::fs::read_to_string(d.join(format!("{n}.json"))) {
                    Err(e) => format!("The preset could not be read ({e})."),
                    Ok(t) => match serde_json::from_str::<Json>(&t) {
                        Err(_) => "The preset file is damaged.".into(),
                        Ok(j) => {
                            let (v, problems) = values_from_json(&s.decl, &j);
                            s.values = v;
                            if problems.is_empty() {
                                format!("Loaded \u{201C}{n}\u{201D}.")
                            } else {
                                problems.join(" ")
                            }
                        }
                    },
                });
            }
            UiAction::DeletePreset(n) => {
                let Some(s) = self.custom_tool.as_mut() else {
                    return;
                };
                if let Some(d) = preset_dir(config, &s.plugin, &s.decl.id) {
                    let _ = std::fs::remove_file(d.join(format!("{n}.json")));
                }
                s.reload_presets(config);
                s.preset_msg = Some(format!("Deleted \u{201C}{n}\u{201D}."));
            }
            UiAction::SetShortcut => {
                let Some(s) = self.custom_tool.as_mut() else {
                    return;
                };
                let key = format!("{}/{}", s.plugin, s.decl.id);
                let text = s.shortcut_text.trim().to_owned();
                let msg = if text.is_empty() {
                    self.settings.lua_plugins.tool_keys.remove(&key);
                    "No shortcut.".to_owned()
                } else {
                    match check_tool_chord(&text, &key, &self.settings.lua_plugins.tool_keys) {
                        Ok(norm) => {
                            s.shortcut_text = norm.clone();
                            self.settings
                                .lua_plugins
                                .tool_keys
                                .insert(key, norm.clone());
                            format!("{norm} opens this tool.")
                        }
                        Err(e) => e,
                    }
                };
                if let Some(s) = self.custom_tool.as_mut() {
                    s.shortcut_msg = Some(msg);
                }
                self.save_settings();
            }
        }
    }

    /// Custom tool shortcuts (after PlumSpirit's own, which they never
    /// shadow: such chords are refused when set).
    pub(super) fn custom_tool_shortcuts(&mut self, ctx: &egui::Context) {
        if self.settings.lua_plugins.tool_keys.is_empty() {
            return;
        }
        let mut fired: Option<String> = None;
        let keys: Vec<(String, String)> = self
            .settings
            .lua_plugins
            .tool_keys
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        ctx.input_mut(|i| {
            for (tool, chord) in &keys {
                if let Ok((m, k)) = super::keybinds::parse_chord(chord)
                    && super::keybinds::consume(i, &egui::KeyboardShortcut::new(m, k))
                {
                    fired = Some(tool.clone());
                }
            }
        });
        if let Some(t) = fired
            && let Some((plugin, tool)) = t.split_once('/')
        {
            self.open_custom_tool(plugin, tool);
        }
    }
}

/// A shortcut for a custom tool, normalized (`Ctrl+Alt+1`), or why not:
/// PlumSpirit's own shortcuts and other custom tools' keep theirs.
pub fn check_tool_chord(
    text: &str,
    key: &str,
    others: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let (m, k) = super::keybinds::parse_chord(text)?;
    let norm = super::keybinds::chord_text(m, k);
    let table = super::keybinds::effective();
    if let Some(why) =
        super::keybinds::refusal(&table, super::commands::Command::ShowLuaPlugins, (m, k))
    {
        return Err(why);
    }
    if let Some((other, _)) = others
        .iter()
        .find(|(o, c)| o.as_str() != key && **c == norm)
    {
        return Err(format!("{norm} already opens the custom tool {other}."));
    }
    Ok(norm)
}

/// The result notes of `target` in `commits`' result as a drawn preview.
fn result_slot(key: String, commits: Commits, target: NoteTarget) -> ToolResultSlot {
    // A per-session worker generation can repeat when another tool opens. The
    // renderer identity must be unique across all custom results in this process.
    static NEXT_PREVIEW: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0x4c55_4100_0000_0000);
    let mut notes = Vec::new();
    let mut max_dur = 0;
    let mut too_many = false;
    if let Ok(view) = plumspirit_engine::editor::note_view(commits.result(), target) {
        if view.notes.len() > PREVIEW_NOTE_CAP {
            too_many = true;
        } else {
            notes.reserve(view.notes.len());
            view.notes.query(0, u64::MAX, |_, v| {
                max_dur = max_dur.max(v.duration_ticks);
                notes.push(TNote::from_record(&v.record(), 0));
            });
            notes.sort_by_key(|n| n.start);
        }
    }
    ToolResultSlot {
        key,
        commits,
        target,
        notes,
        max_dur,
        // A new identity per result (the roll caches drawings by it).
        rev: NEXT_PREVIEW.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        too_many,
    }
}

enum UiAction {
    ClearStroke,
    Apply,
    Close,
    SavePreset,
    LoadPreset(String),
    DeletePreset(String),
    SetShortcut,
}

/// One parameter's native control.
/// The native form of a parameter list (custom tools and the options of
/// plugin commands): one labeled control per parameter with a reset button.
pub(super) fn param_form(
    ui: &mut egui::Ui,
    params: &[crate::lua::custom::ToolParam],
    values: &mut [ToolValue],
    keys: u8,
) {
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .show(ui, |ui| {
            for (i, (p, v)) in params.iter().zip(values.iter_mut()).enumerate() {
                ui.horizontal(|ui| {
                    let l = ui.label(&p.label);
                    if !p.help.is_empty() {
                        l.on_hover_text(&p.help);
                    }
                    if ui
                        .small_button("↺")
                        .on_hover_text("Back to this control's default.")
                        .clicked()
                    {
                        *v = p.default.clone();
                    }
                });
                param_control(ui, i, &p.kind, v, &p.default, keys);
                ui.add_space(4.0);
            }
        });
}

fn param_control(
    ui: &mut egui::Ui,
    i: usize,
    kind: &ToolParamKind,
    v: &mut ToolValue,
    default: &ToolValue,
    max_key: u8,
) {
    match (kind, v) {
        (ToolParamKind::Int { min, max }, ToolValue::Int(n)) => {
            ui.add(crate::ui::stepper::Stepper::new(n).range(*min..=*max));
        }
        (ToolParamKind::Float { min, max, unit }, ToolValue::Float(f)) => {
            crate::ui::stepper::slider(ui, f, *min..=*max, unit);
        }
        (ToolParamKind::Bool, ToolValue::Bool(b)) => {
            ui.checkbox(b, "On");
        }
        (ToolParamKind::Choice(c), ToolValue::Choice(sel)) => {
            egui::ComboBox::from_id_salt(("custom-choice", i))
                .selected_text(c.get(*sel).map_or("", String::as_str))
                .show_ui(ui, |ui| {
                    for (k, label) in c.iter().enumerate() {
                        ui.selectable_value(sel, k, label);
                    }
                });
        }
        (ToolParamKind::Pitch, ToolValue::Int(n)) => {
            *n = (*n).min(i64::from(max_key));
            ui.add(
                crate::ui::stepper::Stepper::new(n)
                    .range(0..=i64::from(max_key))
                    .custom_formatter(|v, _| plumspirit_engine::model::note::pitch_name(v as u8)),
            );
        }
        (ToolParamKind::Ticks { min, max }, ToolValue::Int(n)) => {
            let (lo, hi) = (*min as i64, (*max).min(i64::MAX as u64) as i64);
            ui.add(
                crate::ui::stepper::Stepper::new(n)
                    .range(lo..=hi)
                    .suffix(" ticks"),
            );
        }
        (ToolParamKind::Seed, ToolValue::Int(n)) => {
            ui.horizontal(|ui| {
                ui.add(crate::ui::stepper::Stepper::new(n));
                if ui
                    .button("🎲")
                    .on_hover_text("A new random seed.")
                    .clicked()
                {
                    let t = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(1, |d| d.subsec_nanos() as i64 ^ d.as_secs() as i64);
                    *n = t.rem_euclid(1_000_000_000);
                }
            });
        }
        (ToolParamKind::Color, ToolValue::Color(c)) => {
            ui.color_edit_button_srgb(c);
        }
        (ToolParamKind::Curve, ToolValue::Curve(c)) => {
            let d = match default {
                ToolValue::Curve(d) => d.clone(),
                _ => Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
            };
            if let Some(n) = crate::ui::param_editors::curve_editor_overshoot(
                ui,
                egui::Id::new(("custom-curve", i)),
                c,
                &d,
                false,
            ) {
                *c = n;
            }
        }
        (ToolParamKind::Range { min, max }, ToolValue::Range(a, b)) => {
            ui.horizontal(|ui| {
                ui.add(
                    crate::ui::stepper::Stepper::new(a)
                        .range(*min..=*max)
                        .prefix("from "),
                );
                ui.add(
                    crate::ui::stepper::Stepper::new(b)
                        .range(*min..=*max)
                        .prefix("to "),
                );
            });
            if *a > *b {
                std::mem::swap(a, b);
            }
        }
        (ToolParamKind::SubchannelMap, ToolValue::Map(m)) => {
            let mut remove = None;
            for (k, (a, b)) in m.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(crate::ui::stepper::Stepper::new(a).prefix("subchannel "));
                    ui.label("→");
                    ui.add(crate::ui::stepper::Stepper::new(b));
                    if ui.small_button("×").clicked() {
                        remove = Some(k);
                    }
                });
            }
            if let Some(k) = remove {
                m.remove(k);
            }
            if m.len() < crate::lua::custom::MAX_MAP && ui.small_button("Add").clicked() {
                m.push((0, 0));
            }
        }
        (ToolParamKind::NoteFilter, ToolValue::Filter(f)) => {
            ui.horizontal(|ui| {
                ui.label("Keys");
                ui.add(crate::ui::stepper::Stepper::new(&mut f.keys.0).range(0..=max_key));
                ui.add(crate::ui::stepper::Stepper::new(&mut f.keys.1).range(0..=max_key));
            });
            ui.horizontal(|ui| {
                ui.label("Velocity");
                ui.add(crate::ui::stepper::Stepper::new(&mut f.velocity.0).range(1..=127));
                ui.add(crate::ui::stepper::Stepper::new(&mut f.velocity.1).range(1..=127));
            });
            ui.horizontal(|ui| {
                let mut any = f.channel.is_none();
                ui.checkbox(&mut any, "Every channel");
                if any {
                    f.channel = None;
                } else {
                    let mut c = f.channel.unwrap_or(0);
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut c)
                            .range(0..=15)
                            .custom_formatter(|v, _| format!("{}", v as u8 + 1)),
                    );
                    f.channel = Some(c);
                }
            });
            if f.keys.0 > f.keys.1 {
                std::mem::swap(&mut f.keys.0, &mut f.keys.1);
            }
            if f.velocity.0 > f.velocity.1 {
                std::mem::swap(&mut f.velocity.0, &mut f.velocity.1);
            }
        }
        (ToolParamKind::Text { max_len, multiline }, ToolValue::Text(s)) => {
            if *multiline {
                ui.add(
                    egui::TextEdit::multiline(s)
                        .id(ui.id().with(("lua_multiline", i)))
                        .desired_rows(3)
                        .desired_width(ui.available_width())
                        .char_limit(*max_len),
                );
            } else {
                ui.add(egui::TextEdit::singleline(s).char_limit(*max_len));
            }
        }
        (_, v) => {
            // A value that does not match its kind (never stored): reset.
            *v = default.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    //! The app's side: Tools → Custom lists the tool, the window previews
    //! without touching the project, Apply is one Undo step, Cancel changes
    //! nothing; presets and shortcuts.
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use super::*;
    use crate::lua::custom::{ToolDecl, ToolParam};
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::time::{Duration, Instant};

    fn decl() -> ToolDecl {
        let p = |key: &str, kind: ToolParamKind, default: ToolValue| ToolParam {
            key: key.into(),
            label: key.into(),
            help: String::new(),
            kind,
            default,
        };
        ToolDecl {
            id: "t".into(),
            name: "T".into(),
            tooltip: String::new(),
            needs_selection: true,
            freehand: false,
            params: vec![
                p(
                    "n",
                    ToolParamKind::Int { min: 0, max: 10 },
                    ToolValue::Int(3),
                ),
                p(
                    "c",
                    ToolParamKind::Choice(vec!["A".into(), "B".into()]),
                    ToolValue::Choice(1),
                ),
                p(
                    "cv",
                    ToolParamKind::Curve,
                    ToolValue::Curve(Curve::new(vec![(0.0, 0.2), (1.0, 0.9)])),
                ),
                p(
                    "r",
                    ToolParamKind::Range { min: 0, max: 127 },
                    ToolValue::Range(4, 9),
                ),
                p(
                    "m",
                    ToolParamKind::SubchannelMap,
                    ToolValue::Map(vec![(1, 2)]),
                ),
                p(
                    "f",
                    ToolParamKind::NoteFilter,
                    ToolValue::Filter(NoteFilter {
                        keys: (1, 2),
                        velocity: (3, 4),
                        channel: Some(5),
                    }),
                ),
                p("col", ToolParamKind::Color, ToolValue::Color([9, 8, 7])),
            ],
        }
    }

    #[test]
    fn presets_round_trip_and_bad_values_keep_their_defaults() {
        let d = decl();
        let mut v = d.defaults();
        v[0] = ToolValue::Int(7);
        v[1] = ToolValue::Choice(0);
        let j = values_to_json(&d, &v);
        let (back, problems) = values_from_json(&d, &j);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back, v);
        let bad = serde_json::json!({ "version": 1, "values": { "n": 99, "c": "Z" } });
        let (back, problems) = values_from_json(&d, &bad);
        assert_eq!(back, d.defaults());
        assert_eq!(problems.len(), 2);
        assert!(safe_preset_name("../evil").is_err() && safe_preset_name("Wide 2").is_ok());
    }

    #[test]
    fn a_custom_tool_shortcut_never_takes_a_chord_plumspirit_uses() {
        let none = std::collections::BTreeMap::new();
        assert!(
            check_tool_chord("Ctrl+S", "p/t", &none)
                .unwrap_err()
                .contains("already used")
        );
        assert_eq!(
            check_tool_chord("ctrl+alt+1", "p/t", &none).unwrap(),
            "Ctrl+Alt+1"
        );
        let taken: std::collections::BTreeMap<String, String> =
            [("p/other".to_owned(), "Ctrl+Alt+1".to_owned())]
                .into_iter()
                .collect();
        assert!(
            check_tool_chord("Ctrl+Alt+1", "p/t", &taken)
                .unwrap_err()
                .contains("p/other")
        );
        assert!(
            check_tool_chord("Ctrl+Alt+1", "p/other", &taken).is_ok(),
            "its own chord"
        );
    }

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..8u64)
            .map(|k| NoteRecord {
                start_tick: k * 240,
                duration_ticks: 200,
                attrs: NoteAttrs::new(60, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        d
    }

    fn frames_until(h: &mut Harness, what: &str, ok: impl Fn(&Harness) -> bool) {
        let t = Instant::now();
        while !ok(h) {
            h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "{what}: {}",
                h.status()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn tools_custom_previews_applies_once_and_cancel_changes_nothing() {
        let root =
            std::env::temp_dir().join(format!("plumspirit-app-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("plum-plugin.toml"),
            "id = \"test.vel\"\nname = \"Velocity\"\nversion = \"1.0\"\napi = \"0.1\"\ncapabilities = [\"project.read\", \"notes.edit\"]\n",
        )
        .unwrap();
        std::fs::write(
            src.join("main.lua"),
            "function register(p) p.tool{ id = 'set', name = 'Set Velocity', \
               params = { { key = 'v', kind = 'int', min = 1, max = 127, default = 33 } }, \
               run = function(ctx) ctx.tx:modify_notes(ctx.notes, { velocity = ctx.params.v }) end } end",
        )
        .unwrap();
        let mut h = Harness::new(doc());
        h.app.settings.lua_plugins.folder = Some(root.join("plugins"));
        h.app.settings.lua_plugins.enable_on_install = true;
        h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        let mut prefs = h.app.settings.lua_plugins.clone();
        h.app
            .lua_plugins()
            .install(&src, false, &mut prefs)
            .unwrap();
        h.app.settings.lua_plugins = prefs;
        frames_until(&mut h, "the tool registers", |h| {
            !h.app.custom_tool_menu().is_empty()
        });
        let menu = h.app.custom_tool_menu();
        assert_eq!(menu[0].2[0].1, "Set Velocity");
        let before = h.doc().export_fingerprint();
        h.app.open_custom_tool("test.vel", "set");
        assert!(h.app.custom_tool.is_some());
        frames_until(&mut h, "the preview arrives", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some())
        });
        assert_eq!(
            h.doc().export_fingerprint(),
            before,
            "previewing changes nothing"
        );
        let n = h
            .app
            .custom_tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .notes
            .len();
        assert_eq!(n, 8);
        // Apply: one Undo step named after the tool.
        h.app.custom_tool_action(UiAction::Apply, None, true);
        frames_until(&mut h, "applied", |h| {
            h.doc().export_fingerprint() != before
        });
        assert_eq!(
            h.app.open.as_ref().unwrap().history.undo_label(),
            Some("Set Velocity")
        );
        let mut vel = Vec::new();
        h.doc()
            .track(0)
            .unwrap()
            .notes
            .query(0, u64::MAX, |_, v| vel.push(v.attrs.velocity()));
        assert!(vel.iter().all(|&v| v == 33), "{vel:?}");
        // Undo, then preview again and Cancel: nothing changes.
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().export_fingerprint(), before);
        frames_until(&mut h, "a new preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some())
        });
        h.app.custom_tool_action(UiAction::Close, None, true);
        for _ in 0..5 {
            h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        }
        assert!(h.app.custom_tool.is_none());
        assert_eq!(
            h.doc().export_fingerprint(),
            before,
            "Cancel changes nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    include!("custom_drawing_tests.rs");
    include!("image_art_studio_tests.rs");
    include!("text_art_studio_tests.rs");
    include!("midi_input_laboratory_app_tests.rs");
}
