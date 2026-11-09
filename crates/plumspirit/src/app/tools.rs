//! v0.7 tool windows: one generic, modeless, movable window for every tool of
//! the Note Transformation Platform (`plumspirit_engine::transform`).
//!
//! * Parameters are rendered from the tool's declarative spec; the ten advanced
//!   functions are collapsible sections with their own switch.
//! * Preview is ON by default. When a parameter (or the selection) changes, the
//!   kernel is rebuilt; pointwise tools preview at once over the viewport (no
//!   copy of the notes), and the full result is computed — synchronously for
//!   small selections, on a worker otherwise (generation ids: a late result for
//!   old parameters is dropped; a running worker is cancelled).
//! * Apply commits the previewed result (the same kernel output) as one undo
//!   step; Cancel / closing the window never touches the document or history.
//! * Presets: Save, Save As, Load, Rename, Delete, Reset (files under the
//!   settings folder, `presets/<tool>/<name>.psp`).

use super::PlumSpiritApp;
use crate::app::StatusKind;
use egui::{RichText, Ui};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::transform::definition::{
    Capture, FunctionStatus, ParamDef, ToolDefinition, definition,
};
use plumspirit_engine::transform::param::{ParamKind, ParamSpec, ParamValue};
use plumspirit_engine::transform::preset;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::build_operation;
use plumspirit_engine::transform::{
    Boundary, Ctx, Kernel, Limits, ParamSet, Source, SourceTrack, TNote, ToolDef, ToolSpec,
    TrackScope, TransformError, TransformRun, run_kernel,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;

/// Selections up to this size are computed on the UI thread.
const SYNC_NOTES: usize = 20_000;
/// The most notes a single tool run may copy as its source.
const SOURCE_LIMIT: usize = 20_000_000;

/// v0.8 §196–§198: what Repeat Last Tool runs again — the recipe (tool id,
/// parameter values incl. seeds, scope, version), never a copy of the output.
#[derive(Clone, Debug)]
pub enum Recipe {
    Tool {
        id: &'static str,
        values: ParamSet,
        scope: usize,
        /// PlumSpirit version the recipe was made with (tools are versioned
        /// with the application).
        version: &'static str,
        /// Name of the one-click command that ran it (Mute Notes, …), if
        /// not the tool's own name.
        label: Option<&'static str>,
    },
    /// Edit → Scale X / Y (ordinary scale is N → N; Duplicate Notes only
    /// when it was on; the overlap prevention as it was).
    Scale {
        axis: super::dialogs::ScaleAxis,
        text: String,
        opts: plumspirit_engine::editor::ScaleY,
    },
}

/// v0.8 §209: how a tool computed as a background job is committed once its
/// result arrives on the UI thread.
#[derive(Clone, Debug)]
pub enum ToolFinish {
    /// A one-click command or Repeat Last Tool: `recipe` becomes the last
    /// recipe; `repeated` words the status line; `select` = the Select tool.
    Recipe {
        recipe: Recipe,
        repeated: bool,
        select: bool,
    },
    /// The open window's Apply without a preview (its settings fingerprint
    /// at Apply: a result for other settings is never applied).
    Window { id: &'static str, values: String },
}

impl Recipe {
    pub fn tool(id: &'static str, values: ParamSet, scope: usize) -> Recipe {
        Recipe::Tool {
            id,
            values,
            scope,
            version: env!("CARGO_PKG_VERSION"),
            label: None,
        }
    }

    /// Name for menus ("Repeat Humanize").
    pub fn name(&self) -> String {
        match self {
            Recipe::Tool { label: Some(l), .. } => (*l).to_owned(),
            Recipe::Tool { id, .. } => {
                resolve(id).map_or_else(|| (*id).to_owned(), |d| (d.spec)().name.to_owned())
            }
            Recipe::Scale { axis, .. } => axis.title().to_owned(),
        }
    }
}

/// "Name — Section": the family's section, or the marked Legacy section of a
/// tool moved to Legacy (v0.9 §113: "Shear — Legacy").
pub fn tool_window_title(spec: &plumspirit_engine::transform::ToolSpec) -> String {
    tool_window_title_in(spec, &plumspirit_engine::transform::definition::LEGACY)
}

fn tool_window_title_in(
    spec: &plumspirit_engine::transform::ToolSpec,
    legacy: &plumspirit_engine::transform::definition::LegacyTable,
) -> String {
    let section = legacy
        .category(spec.id, spec.family)
        .legacy_label()
        .unwrap_or_else(|| spec.family.label());
    format!("{} — {section}", spec.name)
}

/// Fresh values for every Seed parameter (Repeat Last Tool with New Seed).
fn reseed(spec: &plumspirit_engine::transform::ToolSpec, values: &mut ParamSet) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut any = false;
    for p in &spec.params {
        if matches!(p.kind, plumspirit_engine::transform::ParamKind::Seed) {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            let c = COUNTER.fetch_add(1, Ordering::Relaxed);
            let v = (t ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % 1_000_000_000 + 1;
            values.set(
                p.key,
                plumspirit_engine::transform::ParamValue::Int(v as i64),
            );
            any = true;
        }
    }
    any
}

/// Which notes a tool works on (tools with their own "scope" parameter, like
/// Transpose, map onto these).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolScope {
    /// Selected notes of the active track.
    Selected,
    /// Selected notes of the active track and its subtracks.
    SelectedSubtree,
    /// Selected notes of every track.
    SelectedEverywhere,
    /// Every note of the active track.
    Track,
    /// Every note of the active track and its subtracks.
    Subtree,
    /// Every note of the project.
    Project,
    /// Every note of the selected tracks (and the active one).
    SelectedTracks,
}

const SCOPE_LABELS: &[&str] = &[
    "Selected notes (active track)",
    "Selected notes (track + subtracks)",
    "Selected notes (every track)",
];

struct Worker {
    generation: u64,
    rx: Receiver<(
        u64,
        Result<TransformRun, TransformError>,
        std::time::Duration,
    )>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn stop(mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// An open tool window.
pub struct ToolSession {
    pub def: &'static ToolDef,
    pub spec: ToolSpec,
    /// The tool's definition (v0.9 §095): the window, the pointer capture
    /// and the stroke behaviour are generated from it.
    pub definition: ToolDefinition,
    pub values: ParamSet,
    pub scope: usize,
    pub preview: bool,
    key: String,
    generation: u64,
    worker: Option<Worker>,
    /// The full result for the current parameters (what Apply commits).
    pub run: Option<Arc<TransformRun>>,
    /// Result notes going to other tracks (destinations such as a Clipping
    /// Mask's base track, or a new subtrack), sorted by start: drawn over the
    /// active track as a preview.
    pub extra: Vec<plumspirit_engine::transform::TNote>,
    /// Longest result note per run slot, and of `extra` (computed once per run:
    /// the preview's visible window needs it every frame).
    max_durs: Vec<u64>,
    /// Pointwise kernel for immediate viewport preview.
    pub lazy: Option<(Arc<dyn Kernel>, Boundary, Vec<SourceTrack>)>,
    pub status: String,
    pub error: Option<String>,
    presets: PresetUi,
    /// v0.7 stroke tools: the stroke / lasso drawn in the Piano Roll.
    pub stroke: Vec<plumspirit_engine::transform::StrokePoint>,
    /// The same as screen-independent (tick, pitch) points for the overlay.
    pub stroke_overlay: Vec<(u64, f64)>,
    stroke_rev: u64,
    /// Draw / Paint: apply each stroke when the button is released (the window
    /// stays open; one undo step per stroke).
    pub auto_apply: bool,
    /// Before / after: draw the original notes instead of the result (view
    /// only; the result is kept).
    pub show_original: bool,
    /// The project's key range (v0.9): keyed parameters' ranges and defaults
    /// (`ParamSpec::range_in`, `ParamSet::defaults_in`) follow it.
    pub keys: KeyRange,
}

#[derive(Default)]
struct PresetUi {
    names: Vec<String>,
    selected: Option<String>,
    name_edit: String,
    message: Option<String>,
    loaded_dir: Option<PathBuf>,
    /// The values as the selected preset holds them (after Load / Save): the
    /// window marks the preset "(changed)" while the values differ (B2-F01:
    /// an execution-order edit too).
    saved: Option<ParamSet>,
}

impl ToolSession {
    /// Stores a computed result (and the notes it sends to other tracks).
    fn set_run(&mut self, run: TransformRun) {
        let mut extra: Vec<plumspirit_engine::transform::TNote> = run
            .notes
            .iter()
            .filter(|n| {
                run.tracks
                    .get(usize::from(n.track))
                    .is_some_and(|t| t.insert_only)
            })
            .copied()
            .collect();
        if let Some((_, v)) = &run.new_subtrack {
            extra.extend_from_slice(v);
        }
        extra.sort_by_key(|n| n.start);
        let extra_max = extra.iter().map(|n| n.dur).max().unwrap_or(0);
        self.max_durs = (0..run.tracks.len())
            .map(|slot| {
                run.track_notes(slot as u16)
                    .iter()
                    .map(|n| n.dur)
                    .max()
                    .unwrap_or(0)
                    .max(extra_max)
            })
            .collect();
        self.extra = extra;
        self.run = Some(Arc::new(run));
    }

    /// Identity of what the preview shows (never 0): the preview key and
    /// whether the full result is there (the Piano Roll caches its drawing).
    fn preview_rev(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.key.hash(&mut h);
        self.run.as_ref().map(Arc::as_ptr).hash(&mut h);
        self.lazy
            .as_ref()
            .map(|l| Arc::as_ptr(&l.0) as *const u8)
            .hash(&mut h);
        h.finish() | 1
    }

    pub fn new(def: &'static ToolDef, keys: KeyRange) -> ToolSession {
        let mut spec = (def.spec)();
        let definition = definition(def);
        // v0.9 §098: the window, presets and recipes cover the mandatory
        // functions' parameters too.
        spec.params = definition.specs();
        let values = ParamSet::defaults_in(&spec.params, keys);
        // Freehand stroke tools (Draw, Paint) apply each stroke on release.
        let auto_apply = definition.pointer.base == Some(Capture::Freehand);
        ToolSession {
            def,
            spec,
            definition,
            values,
            scope: 0,
            preview: true,
            key: String::new(),
            generation: 0,
            worker: None,
            run: None,
            max_durs: Vec::new(),
            extra: Vec::new(),
            lazy: None,
            status: String::new(),
            error: None,
            presets: PresetUi::default(),
            stroke: Vec::new(),
            stroke_overlay: Vec::new(),
            stroke_rev: 0,
            auto_apply,
            show_original: false,
            keys,
        }
    }

    /// How this tool captures the pointer in the Piano Roll (stroke tools),
    /// from its definition and the current settings.
    pub fn capture_mode(&self) -> Option<crate::piano_roll::CaptureMode> {
        use crate::piano_roll::CaptureMode;
        self.definition.capture(&self.values).map(|c| match c {
            Capture::Freehand => CaptureMode::Freehand,
            Capture::Polygon => CaptureMode::Polygon,
        })
    }

    /// Replaces the stroke (a new one, or the one being drawn).
    pub fn set_stroke(&mut self, points: &[(u64, f64, f64)]) {
        self.stroke = points
            .iter()
            .map(|&(tick, pitch, t)| plumspirit_engine::transform::StrokePoint { tick, pitch, t })
            .collect();
        self.stroke_overlay = points.iter().map(|&(t, p, _)| (t, p)).collect();
        self.stroke_rev += 1;
    }

    /// Adds a polygon corner.
    pub fn add_vertex(&mut self, tick: u64, pitch: f64) {
        let t = self.stroke.len() as f64;
        self.stroke
            .push(plumspirit_engine::transform::StrokePoint { tick, pitch, t });
        self.stroke_overlay.push((tick, pitch));
        self.stroke_rev += 1;
    }

    pub fn clear_stroke(&mut self) {
        self.stroke.clear();
        self.stroke_overlay.clear();
        self.stroke_rev += 1;
    }

    /// The stroke for the kernel context (None when nothing is drawn).
    fn stroke_arc(&self) -> Option<Arc<plumspirit_engine::transform::Stroke>> {
        (!self.stroke.is_empty()).then(|| {
            Arc::new(plumspirit_engine::transform::Stroke {
                points: self.stroke.clone(),
            })
        })
    }

    pub fn stop_worker(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop();
        }
    }

    fn effective_scope(&self) -> ToolScope {
        // Select: among the selection or every note of the track.
        if self.def.id == "select" {
            let p = plumspirit_engine::transform::Params::new(&self.spec.params, &self.values);
            return if p.choice("among") == 1 {
                ToolScope::Track
            } else {
                ToolScope::Selected
            };
        }
        // Tools with their own scope control (Transpose).
        if self.spec.params.iter().any(|p| p.key == "scope") {
            let p = plumspirit_engine::transform::Params::new(&self.spec.params, &self.values);
            return match p.choice("scope") {
                1 => ToolScope::Track,
                2 => ToolScope::Subtree,
                3 => ToolScope::Project,
                4 => ToolScope::SelectedTracks,
                _ => ToolScope::Selected,
            };
        }
        match self.scope {
            1 => ToolScope::SelectedSubtree,
            2 => ToolScope::SelectedEverywhere,
            _ => ToolScope::Selected,
        }
    }
}

/// The track scopes of a tool scope for `target` (the active track, or an
/// opened clip's content, else the track's own notes; V09-S11). The
/// project-wide scopes cover every track's own notes and the opened clip's
/// content. v1.0-beta2 B2-A02: every scoped track other than the edited
/// notes is taken as the song shows it ([`scopes_framed`]).
pub fn scopes_for(
    doc: &PlumDocument,
    target: impl Into<plumspirit_engine::editor::NoteTarget>,
    scope: ToolScope,
) -> Vec<TrackScope> {
    scopes_framed(doc, target, scope, 0).0
}

/// v1.0-beta2 B2-A02: [`scopes_for`] with the frame of each scope on the
/// edited notes' ticks (`shift` = their song tick of tick 0): every scoped
/// track other than the edited notes contributes each clip content it shows
/// (the clip the roll edits first, then the others; a content once), at the
/// place of its clip — `clip shift − shift` — so a moved clip, a clip of
/// another track's content and an owned clip all take part on one axis
/// (`Ctx::scope_frames`). With every clip at the identity and `shift` 0 the
/// scopes are the v1.0-beta1 ones and every frame is 0.
pub fn scopes_framed(
    doc: &PlumDocument,
    target: impl Into<plumspirit_engine::editor::NoteTarget>,
    scope: ToolScope,
    shift: i128,
) -> (Vec<TrackScope>, Vec<i128>) {
    let target = target.into();
    let ti = target.host_track();
    let own = |all: bool| TrackScope::of(target, all);
    let track = |index: usize, all: bool| TrackScope {
        index,
        all,
        content: None,
    };
    let subtree = |all: bool| -> Vec<TrackScope> {
        let end = doc.subtree_end(ti).min(doc.track_count());
        let mut v = vec![own(all)];
        v.extend((ti + 1..end).map(|index| track(index, all)));
        v
    };
    let everywhere = |all: bool| -> Vec<TrackScope> {
        let mut v: Vec<TrackScope> = (0..doc.track_count()).map(|i| track(i, all)).collect();
        if !matches!(target, plumspirit_engine::editor::NoteTarget::Track(_)) {
            v.push(own(all));
        }
        v
    };
    let v: Vec<TrackScope> = match scope {
        ToolScope::Selected => vec![own(false)],
        // Without the app's track selection (see `selected_tracks_framed`).
        ToolScope::Track | ToolScope::SelectedTracks => vec![own(true)],
        ToolScope::SelectedSubtree => subtree(false),
        ToolScope::Subtree => subtree(true),
        ToolScope::SelectedEverywhere => everywhere(false),
        ToolScope::Project => everywhere(true),
    };
    in_scope(doc, resolve_clips(doc, target, v, shift))
}

/// Every note of the tracks at `indices` (the selected tracks), v1.0-beta2
/// B2-A02: as the song shows each track (see [`scopes_framed`]); `own` (the
/// edited notes) keeps frame 0.
pub fn selected_tracks_framed(
    doc: &PlumDocument,
    indices: &[usize],
    own: plumspirit_engine::editor::NoteTarget,
    shift: i128,
) -> (Vec<TrackScope>, Vec<i128>) {
    let v = indices
        .iter()
        .filter(|&&i| doc.track(i).is_some())
        .map(|&index| TrackScope {
            index,
            all: true,
            content: None,
        })
        .collect();
    in_scope(doc, resolve_clips(doc, own, v, shift))
}

/// B2-A02: each scope of a track's own notes (other than the edited notes
/// `own`) becomes the clip contents the track shows — the clip the roll
/// edits first ([`super::clip_session::default_pick`]), then the others in
/// placement order, each content once — with its frame on the edited
/// notes' ticks; the edited notes keep frame 0.
fn resolve_clips(
    doc: &PlumDocument,
    own: plumspirit_engine::editor::NoteTarget,
    raw: Vec<TrackScope>,
    shift: i128,
) -> (Vec<TrackScope>, Vec<i128>) {
    use plumspirit_engine::editor::NoteTarget;
    let mut out: Vec<TrackScope> = Vec::with_capacity(raw.len());
    let mut frames: Vec<i128> = Vec::with_capacity(raw.len());
    let mut taken: Vec<NoteTarget> = Vec::new();
    let mut push = |sc: TrackScope, frame: i128| {
        if !taken.contains(&sc.target()) {
            taken.push(sc.target());
            out.push(sc);
            frames.push(frame);
        }
    };
    for s in raw {
        if s.target() == own {
            push(s, 0);
            continue;
        }
        if s.content.is_some() || doc.track(s.index).is_none() {
            push(s, 0);
            continue;
        }
        let mut clips: Vec<plumspirit_engine::model::ClipPlacement> = doc
            .placements_on(s.index)
            .into_iter()
            .filter(|p| plumspirit_engine::editor::clips::is_clip(doc, p))
            .collect();
        if let Some(pick) = super::clip_session::default_pick(doc, s.index)
            && let Some(k) = clips.iter().position(|p| p.id == pick.id)
        {
            let pick = clips.remove(k);
            clips.insert(0, pick);
        }
        if clips.is_empty() {
            // No clip: its own notes where they are (none, or at the identity).
            push(s, -shift);
            continue;
        }
        for p in clips {
            let Some(v) = super::clip_session::view_of(doc, &p) else {
                continue;
            };
            let frame = if v.target == own { 0 } else { v.shift - shift };
            push(TrackScope::of(v.target, s.all), frame);
        }
    }
    (out, frames)
}

/// The scopes with notes in scope (every note, or the selected ones), with
/// their frames.
fn in_scope(
    doc: &PlumDocument,
    (v, f): (Vec<TrackScope>, Vec<i128>),
) -> (Vec<TrackScope>, Vec<i128>) {
    v.into_iter()
        .zip(f)
        .filter(|(s, _)| {
            plumspirit_engine::editor::note_view(doc, s.target()).is_ok_and(|t| {
                if s.all {
                    !t.notes.is_empty()
                } else {
                    t.notes.selected_count() > 0
                }
            })
        })
        .unzip()
}

/// The kernel context of a document.
#[cfg(test)]
pub fn ctx_for(
    doc: &PlumDocument,
    meter: Option<&plumspirit_engine::view::grid::MeterMap>,
    snap: u64,
    cursor: u64,
) -> Ctx {
    ctx_at(doc, meter, snap, cursor, 0)
}

/// The decoded map picture (path, size and time stamp, result).
type PictureCache = Option<(
    PathBuf,
    Option<(u64, Option<std::time::SystemTime>)>,
    Result<Arc<plumspirit_engine::transform::image_midi::Rgba>, String>,
)>;

/// V09-S47: the map picture a tool's `picture` parameter names when its
/// `source` chooses a picture (Displacement Map) — or V09-S50 Tumours' mask
/// picture when its mask is a picture —, decoded once per path, size and
/// modification time (the engine has no image decoders).
pub fn picture_for(
    id: &str,
    values: &ParamSet,
) -> Option<Result<Arc<plumspirit_engine::transform::image_midi::Rgba>, String>> {
    static CACHE: std::sync::Mutex<PictureCache> = std::sync::Mutex::new(None);
    let key = match id {
        "displacement_map" if values.get_raw("source") == Some(&ParamValue::Choice(3)) => "picture",
        "tumours"
            if values.get_raw("c24_mask_boundary_constraint") == Some(&ParamValue::Bool(true))
                && values.get_raw("c24_mask_boundary_constraint.mode")
                    == Some(&ParamValue::Choice(3)) =>
        {
            "c24_mask_boundary_constraint.picture"
        }
        _ => return None,
    };
    let path = match values.get_raw(key) {
        Some(ParamValue::Text(t)) if !t.trim().is_empty() => PathBuf::from(t.trim()),
        _ => return None,
    };
    let stamp = std::fs::metadata(&path)
        .ok()
        .map(|m| (m.len(), m.modified().ok()));
    if let Ok(c) = CACHE.lock()
        && let Some((p, s, r)) = c.as_ref()
        && *p == path
        && *s == stamp
    {
        return Some(r.clone());
    }
    let r = super::image_midi::decode(&path).map(Arc::new);
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((path, stamp, r.clone()));
    }
    Some(r)
}

/// The ruler's time selection (song ticks) on the axis of notes whose tick
/// 0 is song tick `shift` (none when it lies wholly before them).
fn local_range(sel: Option<(u64, u64)>, shift: i128) -> Option<(u64, u64)> {
    let (a, b) = sel?;
    let at = |t: u64| (i128::from(t) - shift).clamp(0, i128::from(u64::MAX)) as u64;
    let (a, b) = (at(a.min(b)), at(a.max(b)));
    (b > a).then_some((a, b))
}

/// [`ctx_for`] for notes whose tick 0 is song tick `shift` (an opened clip;
/// `cursor` on the same axis).
pub fn ctx_at(
    doc: &PlumDocument,
    meter: Option<&plumspirit_engine::view::grid::MeterMap>,
    snap: u64,
    cursor: u64,
    shift: i128,
) -> Ctx {
    let ppq = doc.division().musical_ppqn().unwrap_or(960);
    let mut c = Ctx::simple(ppq);
    if let Some(m) = meter {
        c.meter = m.clone();
    }
    c.snap = snap.max(1);
    c.cursor = cursor;
    c.shift = shift;
    c.keys = doc.project().keys;
    // V09-S39: Delete n08 crops to the loop.
    c.loop_range = local_range(doc.project().looping.range, shift);
    if !doc.project().groups.is_empty() {
        c.groups = Arc::new(doc.project().groups.clone());
    }
    c.tempo = Some(Arc::new(
        plumspirit_engine::model::TempoMap::for_document_at(
            doc,
            plumspirit_engine::transform::TEMPO_RATE,
            plumspirit_engine::model::Speed::NORMAL,
        ),
    ));
    let markers = doc.conductor().markers();
    if !markers.is_empty() {
        let mut m: Vec<u64> = markers.iter().map(|m| m.start).collect();
        m.sort_unstable();
        c.markers = Arc::new(m);
        let mut ch: Vec<(u64, u64)> = markers
            .iter()
            .filter_map(|m| m.end.map(|e| (m.start, e)))
            .collect();
        ch.sort_unstable();
        c.chapters = Arc::new(ch);
    }
    c
}

/// A run's summary with the time it took and its memory estimate (contract
/// §067, §069).
fn timed_summary(run: &TransformRun, took: std::time::Duration) -> String {
    let mb = run.estimate.temp_bytes as f64 / (1024.0 * 1024.0);
    format!(
        "{} Computed in {} (working memory ≤ {:.1} MB).",
        run.summary(),
        super::tasks::format_task_duration(took),
        mb
    )
}

pub(super) fn compute(
    doc: &PlumDocument,
    def: &'static ToolDef,
    values: &ParamSet,
    scopes: &[TrackScope],
    ctx: &Ctx,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<TransformRun, TransformError> {
    // The context reaches the tool's source preparation (v0.9: Draw reads
    // the notes around its stroke).
    let src = plumspirit_engine::transform::run::collect_in(
        def,
        doc,
        scopes,
        values,
        SOURCE_LIMIT,
        Some(ctx),
    )?;
    let b = BuildCtx {
        ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(values, &b)?;
    let limits = Limits::for_system_memory(crate::platform::total_memory_bytes());
    let mut run = run_kernel(k.as_ref(), &src, def.boundary(values), &limits, cancel)?;
    // B1-0117: the General Note Overlap Prevention, when on — in the run, so
    // the preview shows (and its summary counts) what Apply removes.
    if let Some(g) = ctx.general_overlap {
        plumspirit_engine::transform::overlap::prevent_in_run(doc, &src, &mut run, g);
    }
    Ok(run)
}

/// B1-0117: why a run that changed nothing changed nothing, when the Note
/// Overlap Prevention removed what it would have added.
fn prevented_note(run: &TransformRun) -> String {
    let st = &run.stats;
    if st.overlaps_removed + st.overlaps_trimmed == 0 {
        return String::new();
    }
    format!(
        " (Overlaps: {} removed, {} shortened.)",
        group_digits(st.overlaps_removed),
        group_digits(st.overlaps_trimmed)
    )
}

/// A tool, or the kernel of a one-click command (Mute Notes, Slice Notes),
/// by id.
pub(super) fn resolve(id: &str) -> Option<&'static plumspirit_engine::transform::ToolDef> {
    plumspirit_engine::transform::tool(id)
        .or_else(|| plumspirit_engine::transform::command_tool(id))
}

/// v0.9 §118: where Slice Notes cuts — a ruler time selection's start and
/// end when one exists, else the edit cursor (ticks, as the kernel's `at`).
pub(super) fn slice_cuts(time_selection: Option<(u64, u64)>, cursor: u64) -> Vec<i64> {
    let tick = |t: u64| t.min(u64::MAX >> 3) as i64;
    match time_selection {
        Some((a, b)) => vec![tick(a), tick(b)],
        None => vec![tick(cursor)],
    }
}

impl PlumSpiritApp {
    /// Runs tool `id` once on the active track's selection with its defaults
    /// changed by `set`, and commits it (one undo step) — keyboard commands such
    /// as Group (Ctrl+G) that need no window.
    pub(super) fn quick_tool(
        &mut self,
        id: &str,
        set: &[(&str, plumspirit_engine::transform::ParamValue)],
        label: Option<&'static str>,
        ctx: &egui::Context,
    ) {
        let Some(def) = resolve(id) else {
            return;
        };
        self.release_preview_worker();
        let ti = self.edit_target();
        let snap = self.snap_ticks();
        let (cursor, shift) = (self.edit_cursor(), self.edit_shift());
        let meter = self.meter.as_ref().map(|m| m.1.clone());
        let spec = (def.spec)();
        let name = label.unwrap_or(spec.name);
        let keys = self.doc().map(|d| d.project().keys).unwrap_or_default();
        let mut values = ParamSet::defaults_in(&spec.params, keys);
        for (k, v) in set {
            values.set(k, v.clone());
        }
        let Some(doc) = self.doc() else {
            return;
        };
        let scopes = scopes_for(doc, ti, ToolScope::Selected);
        if scopes.is_empty() {
            self.set_status("Select one or more notes first.", StatusKind::Info);
            return;
        }
        let mut kctx = ctx_at(doc, meter.as_ref(), snap, cursor, shift);
        kctx.active = Some(ti.host_track());
        kctx.host = Some(self.active_track());
        kctx.general_overlap = self.general_overlap();
        kctx.picture = picture_for(def.id, &values);
        let mut recipe = Recipe::tool(def.id, values.clone(), 0);
        if let Recipe::Tool { label: l, .. } = &mut recipe {
            *l = label;
        }
        let finish = ToolFinish::Recipe {
            recipe,
            repeated: false,
            select: false,
        };
        self.run_tool(def, values, scopes, kctx, name, finish, ctx);
    }

    /// v0.9 §118: Slice Notes (Ctrl+K, Edit menu, note menu) — cuts the
    /// active track's selected notes at the ruler time selection's start and
    /// end, or at the edit cursor; one undo step.
    pub(super) fn slice_notes(&mut self, ctx: &egui::Context) {
        // On the roll's axis (content ticks of an opened clip).
        let sel = self.time_selection.map(|r| self.range_to_content(r));
        let at = slice_cuts(sel, self.edit_cursor());
        self.quick_tool("slice_notes", &[("at", ParamValue::List(at))], None, ctx);
    }

    /// v0.8 §209: runs `def` now when at most `SYNC_NOTES` notes are in
    /// scope, otherwise as a cancellable background job (progress in the
    /// status bar, edits disabled meanwhile); [`finish_tool`] commits it.
    #[allow(clippy::too_many_arguments)]
    fn run_tool(
        &mut self,
        def: &'static ToolDef,
        values: ParamSet,
        scopes: Vec<TrackScope>,
        kctx: Ctx,
        name: &'static str,
        finish: ToolFinish,
        ctx: &egui::Context,
    ) {
        let Some(o) = &self.open else {
            return;
        };
        if self.job.is_some() {
            self.set_status(
                "Another operation is still running; try again when it has finished.",
                StatusKind::Info,
            );
            return;
        }
        let notes = Source::scan(&o.doc, &scopes).1.count;
        let started = std::time::Instant::now();
        if notes <= SYNC_NOTES {
            let r = compute(&o.doc, def, &values, &scopes, &kctx, None);
            self.finish_tool(name, finish, r, started);
            return;
        }
        let doc = o.doc.clone();
        let id = self.next_id();
        let work: super::jobs::ToolWork =
            Box::new(move |d, cancel| compute(d, def, &values, &scopes, &kctx, Some(cancel)));
        match super::jobs::spawn_tool(id, doc, name, finish, notes as u64, work, ctx) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.show_error("Could not start the operation.", e.to_string()),
        }
    }

    /// Commits a tool result computed by [`run_tool`] (now or on a worker).
    pub(super) fn finish_tool(
        &mut self,
        name: &'static str,
        finish: ToolFinish,
        result: Result<TransformRun, TransformError>,
        started: std::time::Instant,
    ) {
        let verb_failed = match &finish {
            ToolFinish::Recipe { repeated: true, .. } => "was not repeated",
            _ => "was not applied",
        };
        let run = match result {
            Ok(r) => r,
            Err(TransformError::Cancelled) => return,
            Err(e) => {
                self.show_error(&format!("{name} {verb_failed}."), e.user_message());
                return;
            }
        };
        match finish {
            ToolFinish::Window { id, values } => {
                match self.tool.as_mut() {
                    Some(s) if s.def.id == id && s.values.fingerprint() == values => {
                        s.set_run(run);
                    }
                    _ => {
                        self.set_status(
                            format!("{name}: the settings changed while it was computed; press Apply again."),
                            StatusKind::Info,
                        );
                        return;
                    }
                }
                self.apply_tool_from(Some(started), None);
            }
            ToolFinish::Recipe {
                recipe,
                repeated,
                select,
            } => {
                if select {
                    let n = self.select_run_notes(&run);
                    if let Recipe::Tool { values, .. } = &recipe {
                        self.store_selection_slot(values, &run);
                    }
                    self.set_status(
                        format!("Selected {} note(s).", group_digits(n as u64)),
                        StatusKind::Info,
                    );
                    self.last_recipe = Some(recipe);
                    return;
                }
                let op = match self.doc_mut() {
                    Some(doc) => build_operation(doc, &run, name),
                    None => return,
                };
                match op {
                    Ok(op) => {
                        if self.apply_op(op).is_some() {
                            self.tasks.record(
                                name,
                                started.elapsed(),
                                super::tasks::TaskOutcome::Done,
                            );
                            let what = if repeated {
                                format!("Repeated {name}")
                            } else {
                                name.to_owned()
                            };
                            self.set_status(format!("{what}: {}", run.summary()), StatusKind::Info);
                            self.last_recipe = Some(recipe);
                        }
                    }
                    Err(TransformError::Unchanged) => {
                        // A one-click command says why nothing changed ("No
                        // selected note crosses the cut position.").
                        let why = match (&recipe, &run.message) {
                            (Recipe::Tool { id, .. }, Some(m))
                                if plumspirit_engine::transform::command_tool(id).is_some() =>
                            {
                                m.clone()
                            }
                            _ => TransformError::Unchanged.user_message(),
                        };
                        let why = why + &prevented_note(&run);
                        self.set_status(format!("{name}: {why}"), StatusKind::Info);
                    }
                    Err(e) => self.show_error(&format!("{name} {verb_failed}."), e.user_message()),
                }
            }
        }
    }

    /// Repeat Last Tool (F5) / with new seeds (Ctrl+F5): runs the last recipe
    /// again on the current selection, without opening a window (§196–§198).
    pub(super) fn repeat_last_tool(&mut self, new_seed: bool, ctx: &egui::Context) {
        let Some(recipe) = self.last_recipe.clone() else {
            self.set_status("No tool has been applied yet.", StatusKind::Info);
            return;
        };
        match recipe {
            Recipe::Scale { axis, text, opts } => {
                self.stop_preview();
                self.transient = None;
                if let Err(msg) = self.apply_scale(axis, &text, opts, ctx) {
                    self.set_status(msg, StatusKind::Warning);
                }
            }
            Recipe::Tool {
                id,
                mut values,
                scope,
                version,
                label,
            } => {
                let Some(def) = resolve(id) else {
                    return;
                };
                // A recipe is only valid for the tool version that made it.
                if version != env!("CARGO_PKG_VERSION") {
                    self.last_recipe = None;
                    self.set_status(
                        format!(
                            "{} was applied with PlumSpirit {version}; open it again to repeat it.",
                            (def.spec)().name
                        ),
                        StatusKind::Warning,
                    );
                    return;
                }
                let spec = (def.spec)();
                if new_seed && !reseed(&spec, &mut values) {
                    self.set_status(
                        format!("{} has no seed; repeating it as it was.", spec.name),
                        StatusKind::Info,
                    );
                }
                self.run_recipe(def, values, scope, label, ctx);
            }
        }
    }

    /// Runs tool `def` with `values` / `scope` on the current selection
    /// (now, or as a background job for large scopes).
    fn run_recipe(
        &mut self,
        def: &'static plumspirit_engine::transform::ToolDef,
        values: ParamSet,
        scope: usize,
        label: Option<&'static str>,
        ctx: &egui::Context,
    ) {
        self.release_preview_worker();
        if let Some(mut s) = self.tool.take() {
            s.stop_worker();
            self.tool = Some(s);
        }
        let ti = self.edit_target();
        let snap = self.snap_ticks();
        let (cursor, shift) = (self.edit_cursor(), self.edit_shift());
        let meter = self.meter.as_ref().map(|m| m.1.clone());
        let sel_tracks = self.selected_track_indices();
        let keys = self.doc().map(|d| d.project().keys).unwrap_or_default();
        let mut session = ToolSession::new(def, keys);
        session.values = values.clone();
        session.scope = scope;
        let spec = (def.spec)();
        let name = label.unwrap_or(spec.name);
        let Some(doc) = self.doc() else {
            return;
        };
        let (scopes, frames) = match session.effective_scope() {
            ToolScope::SelectedTracks => selected_tracks_framed(doc, &sel_tracks, ti, shift),
            sc => scopes_framed(doc, ti, sc, shift),
        };
        if scopes.is_empty() {
            self.set_status("Select one or more notes first.", StatusKind::Info);
            return;
        }
        let mut kctx = ctx_at(doc, meter.as_ref(), snap, cursor, shift);
        kctx.active = Some(ti.host_track());
        kctx.host = Some(self.active_track());
        kctx.scope_frames = Arc::new(frames);
        kctx.general_overlap = self.general_overlap();
        kctx.selection_slots = self.selection_slots.clone();
        kctx.picture = picture_for(def.id, &values);
        let mut recipe = Recipe::tool(def.id, values.clone(), scope);
        if let Recipe::Tool { label: l, .. } = &mut recipe {
            *l = label;
        }
        let finish = ToolFinish::Recipe {
            recipe,
            repeated: true,
            select: def.id == "select",
        };
        self.run_tool(def, values, scopes, kctx, name, finish, ctx);
    }

    /// Opens (or focuses) the window of tool `id`.
    pub(super) fn open_tool(&mut self, id: &str) {
        let Some(def) = plumspirit_engine::transform::tool(id) else {
            // v0.9 §238: a removed tool is named, never replaced by another.
            if let Some(notice) = plumspirit_engine::transform::removed_tool(id) {
                self.set_status(notice, StatusKind::Warning);
            }
            return;
        };
        let id = def.id; // Canonicalize historical visible-name aliases.
        self.last_tool = Some(def.id);
        self.recent_tools.retain(|t| *t != def.id);
        self.recent_tools.insert(0, def.id);
        self.recent_tools.truncate(8);
        if let Some(s) = &self.tool
            && s.def.id == id
        {
            return;
        }
        if let Some(mut s) = self.tool.take() {
            s.stop_worker();
        }
        let keys = self.doc().map(|d| d.project().keys).unwrap_or_default();
        let mut s = ToolSession::new(def, keys);
        // The last settings used in this session (Reset restores defaults).
        if let Some((v, scope)) = self.tool_memory.get(id) {
            s.values = v.clone();
            s.scope = *scope;
        }
        self.refresh_presets(&mut s);
        self.tool = Some(s);
    }

    /// Stops the tool's worker (it shares the document, which must be unique
    /// for an edit) and forces a recompute next frame.
    pub(super) fn release_tool_worker(&mut self) {
        if let Some(s) = self.tool.as_mut()
            && s.worker.is_some()
        {
            s.stop_worker();
            s.key.clear();
        }
    }

    pub(super) fn snap_ticks(&self) -> u64 {
        let Some(div) = self.doc().map(|d| d.division()) else {
            return 240;
        };
        let ppq = div.musical_ppqn().unwrap_or(960);
        self.roll.snap.unit_ticks(div).unwrap_or(ppq / 4).max(1)
    }

    /// Keeps the tool's preview current (once per frame).
    /// Indices of the selected tracks plus the active one, in list order.
    fn selected_track_indices(&self) -> Vec<usize> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let ti = self.active_track();
        (0..doc.track_count())
            .filter(|&i| {
                i == ti
                    || doc
                        .track(i)
                        .is_some_and(|t| self.selected_tracks.contains(&t.id()))
            })
            .collect()
    }

    pub(super) fn update_tool(&mut self, ctx: &egui::Context) {
        let target = self.edit_target();
        let ti = target.host_track();
        let host = self.active_track();
        let sel_tracks = self.selected_track_indices();
        let snap = self.snap_ticks();
        let (cursor, shift) = (self.edit_cursor(), self.edit_shift());
        let meter = self.meter.as_ref().map(|m| m.1.clone());
        let Some(o) = self.open.as_ref() else {
            if let Some(mut s) = self.tool.take() {
                s.stop_worker();
            }
            return;
        };
        let doc = o.doc.clone();
        let conductor = o.conductor_selected;
        let Some(s) = self.tool.as_mut() else {
            return;
        };
        // Poll a running computation.
        if let Some(w) = &s.worker {
            match w.rx.try_recv() {
                Ok((g, r, took)) => {
                    let done = s.worker.take().expect("checked");
                    done.stop();
                    if g == s.generation {
                        match r {
                            Ok(run) => {
                                s.status = timed_summary(&run, took);
                                s.error = None;
                                s.set_run(run);
                            }
                            Err(TransformError::Cancelled) => {}
                            Err(e) => {
                                s.error = Some(e.user_message());
                                s.run = None;
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(40));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => s.worker = None,
            }
        }
        let scope = s.effective_scope();
        let (mut scopes, mut frames) = if conductor {
            (Vec::new(), Vec::new())
        } else if scope == ToolScope::SelectedTracks {
            selected_tracks_framed(&doc, &sel_tracks, target, shift)
        } else {
            scopes_framed(&doc, target, scope, shift)
        };
        // Generators work without a selection: they write into the active
        // track (the opened clip's content).
        if scopes.is_empty() && !s.spec.needs_selection && !conductor && ti < doc.track_count() {
            scopes.push(TrackScope::of(target, false));
            frames.push(0);
        }
        let mut key = s.values.fingerprint();
        // B2-A02: the edited clip's song shift too — the kernels read the
        // song at the clip's place (tempo, markers, loop, time selection).
        key.push_str(&format!(
            "|{scope:?}|{}|{target:?}|{cursor}|{shift}|st{}|",
            s.preview, s.stroke_rev
        ));
        // B2-F16: a tool that reads other tracks follows what they show —
        // their notes and every clip placement (a moved clip).
        if plumspirit_engine::transform::tools::reads_other_tracks(s.def.id) {
            key.push_str(&format!("x{};", doc.analysis_revision()));
        }
        // Group changes (lock, names, …) change some tools' results.
        let g = &doc.project().groups;
        key.push_str(&format!(
            "g{}:{}:{};",
            g.len(),
            g.next_id(),
            g.entries().iter().filter(|e| e.locked).count()
        ));
        for sc in &scopes {
            if let Ok(t) = plumspirit_engine::editor::note_view(&doc, sc.target()) {
                key.push_str(&format!(
                    "{:?}:{}:{};",
                    t.host(),
                    t.notes.revision(),
                    sc.all
                ));
            }
        }
        if key == s.key {
            return;
        }
        s.key = key;
        s.stop_worker();
        s.generation += 1;
        s.run = None;
        s.lazy = None;
        s.error = None;
        ctx.request_repaint();
        if scopes.is_empty() {
            s.status.clear();
            s.error = Some(if s.spec.needs_selection {
                "Select one or more notes in the active track first.".into()
            } else {
                "Nothing in scope.".into()
            });
            return;
        }
        let mut kctx = ctx_at(&doc, meter.as_ref(), snap, cursor, shift);
        kctx.active = Some(ti);
        // B2-F16: the track the edited clip sits on (the relative tracks of
        // the cross-track tools count from it).
        kctx.host = Some(host);
        // B2-A02: every scope on the edited notes' ticks.
        kctx.scope_frames = Arc::new(frames.clone());
        kctx.general_overlap = self.settings.overlap.active();
        kctx.stroke = s.stroke_arc();
        kctx.time_range = local_range(self.time_selection, shift);
        kctx.selection_slots = self.selection_slots.clone();
        kctx.picture = picture_for(s.def.id, &s.values);
        // Immediate lazy preview for pointwise kernels (no copy of the notes).
        let (tracks, stats) = Source::scan_framed(&doc, &scopes, &frames);
        {
            let b = BuildCtx {
                ctx: &kctx,
                stats: &stats,
                tracks: &tracks,
            };
            match s.def.kernel(&s.values, &b) {
                Ok(k) => {
                    let k: Arc<dyn Kernel> = Arc::from(k);
                    // Locked group notes pass through unchanged: only the full
                    // run knows them, so no lazy preview then.
                    if k.as_point().is_some()
                        && !plumspirit_engine::transform::source::any_locked(&doc)
                    {
                        s.lazy = Some((k, s.def.boundary(&s.values), tracks.clone()));
                    }
                }
                Err(e) => {
                    s.error = Some(e.user_message());
                    return;
                }
            }
        }
        if !s.preview && stats.count > SYNC_NOTES {
            s.status = format!(
                "{} note(s) in scope. Preview is off; Apply computes the result.",
                group_digits(stats.count as u64)
            );
            return;
        }
        if stats.count <= SYNC_NOTES {
            let started = std::time::Instant::now();
            match compute(&doc, s.def, &s.values, &scopes, &kctx, None) {
                Ok(run) => {
                    s.status = timed_summary(&run, started.elapsed());
                    s.set_run(run);
                }
                Err(e) => s.error = Some(e.user_message()),
            }
            return;
        }
        s.status = format!(
            "Computing the preview for {} notes…",
            group_digits(stats.count as u64)
        );
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let generation = s.generation;
        let def = s.def;
        let values = s.values.clone();
        let ctx2 = ctx.clone();
        let handle = std::thread::Builder::new()
            .name("plumspirit-tool".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let r = compute(&doc, def, &values, &scopes, &kctx, Some(c2));
                let _ = tx.send((generation, r, started.elapsed()));
                ctx2.request_repaint();
            })
            .ok();
        s.worker = Some(Worker {
            generation,
            rx,
            cancel,
            handle,
        });
    }

    /// A stroke drawn in the Piano Roll for the open stroke tool.
    pub(super) fn tool_stroke(
        &mut self,
        points: &[(u64, f64, f64)],
        done: bool,
        ctx: &egui::Context,
    ) {
        if self.custom_tool.as_ref().is_some_and(|s| s.decl.freehand) {
            self.custom_stroke(points, done, ctx);
            return;
        }
        let Some(s) = self.tool.as_mut() else {
            return;
        };
        if s.capture_mode().is_none() {
            return;
        }
        s.set_stroke(points);
        if done && s.auto_apply && matches!(s.def.id, "draw" | "paint") && !points.is_empty() {
            // The preview of the finished stroke, then Apply (one undo step).
            self.update_tool(ctx);
            self.apply_tool(ctx);
        }
    }

    /// A polygon-lasso corner clicked in the Piano Roll.
    pub(super) fn tool_vertex(&mut self, tick: u64, pitch: f64) {
        if let Some(s) = self.tool.as_mut()
            && s.capture_mode().is_some()
        {
            s.add_vertex(tick, pitch);
        }
    }

    /// Applies the previewed result (computing it first when needed).
    pub(super) fn apply_tool(&mut self, ctx: &egui::Context) {
        self.apply_tool_from(None, Some(ctx));
    }

    /// Apply; `computed_since`: when a background job computed the result
    /// (the task time then counts the computation too).
    fn apply_tool_from(
        &mut self,
        computed_since: Option<std::time::Instant>,
        ctx: Option<&egui::Context>,
    ) {
        let Some(mut s) = self.tool.take() else {
            return;
        };
        s.stop_worker();
        let ti = self.edit_target();
        let snap = self.snap_ticks();
        let (cursor, shift) = (self.edit_cursor(), self.edit_shift());
        let meter = self.meter.as_ref().map(|m| m.1.clone());
        let label = s.spec.name;
        let sel_tracks = self.selected_track_indices();
        let run = match s.run.clone() {
            Some(r) => r,
            None => {
                let Some(doc) = self.doc() else {
                    self.tool = Some(s);
                    return;
                };
                let (mut scopes, mut frames) = match s.effective_scope() {
                    ToolScope::SelectedTracks => {
                        selected_tracks_framed(doc, &sel_tracks, ti, shift)
                    }
                    sc => scopes_framed(doc, ti, sc, shift),
                };
                if scopes.is_empty() && !s.spec.needs_selection {
                    scopes.push(TrackScope::of(ti, false));
                    frames.push(0);
                }
                let mut kctx = ctx_at(doc, meter.as_ref(), snap, cursor, shift);
                kctx.active = Some(ti.host_track());
                kctx.host = Some(self.active_track());
                kctx.scope_frames = Arc::new(frames);
                kctx.general_overlap = self.general_overlap();
                kctx.stroke = s.stroke_arc();
                kctx.time_range = local_range(self.time_selection, shift);
                kctx.selection_slots = self.selection_slots.clone();
                kctx.picture = picture_for(s.def.id, &s.values);
                // §209: a large scope is computed as a background job; its
                // result comes back through `finish_tool`, which applies it.
                if Source::scan(doc, &scopes).1.count > SYNC_NOTES
                    && let Some(ctx) = ctx
                {
                    let finish = ToolFinish::Window {
                        id: s.def.id,
                        values: s.values.fingerprint(),
                    };
                    let (def, values) = (s.def, s.values.clone());
                    s.status = "Computing the result…".into();
                    self.tool = Some(s);
                    self.run_tool(def, values, scopes, kctx, label, finish, ctx);
                    return;
                }
                match compute(doc, s.def, &s.values, &scopes, &kctx, None) {
                    Ok(r) => Arc::new(r),
                    Err(e) => {
                        s.error = Some(e.user_message());
                        self.tool = Some(s);
                        return;
                    }
                }
            }
        };
        let started = computed_since.unwrap_or_else(std::time::Instant::now);
        if s.def.id == "select" {
            // Select changes only the selection (view state, no undo step).
            let n = self.select_run_notes(&run);
            self.store_selection_slot(&s.values, &run);
            self.set_status(
                format!("Selected {} note(s).", group_digits(n as u64)),
                StatusKind::Info,
            );
            self.tool_memory
                .insert(s.def.id, (s.values.clone(), s.scope));
            self.last_recipe = Some(Recipe::tool(s.def.id, s.values.clone(), s.scope));
            return;
        }
        let op = match self.doc_mut() {
            Some(doc) => build_operation(doc, &run, label),
            None => Err(TransformError::Invalid(
                "The project is busy; try again in a moment.".into(),
            )),
        };
        match op {
            Ok(op) => {
                if self.apply_op(op).is_some() {
                    self.tasks
                        .record(label, started.elapsed(), super::tasks::TaskOutcome::Done);
                    self.set_status(format!("{label}: {}", run.summary()), StatusKind::Info);
                    // Like FL's Accept: apply and close (settings remembered).
                    self.tool_memory
                        .insert(s.def.id, (s.values.clone(), s.scope));
                    // Stroke tools need a stroke: they are not repeatable recipes.
                    if s.capture_mode().is_none() {
                        self.last_recipe = Some(Recipe::tool(s.def.id, s.values.clone(), s.scope));
                    }
                    if s.capture_mode().is_some() && s.def.id != "select" {
                        // Stroke tools stay open for the next stroke.
                        s.clear_stroke();
                        s.run = None;
                        s.key.clear();
                        self.tool = Some(s);
                    }
                    return;
                }
            }
            // Nothing would change: no undo step, the preview stays valid.
            Err(TransformError::Unchanged) => {
                self.set_status(
                    format!(
                        "{label}: {}{}",
                        TransformError::Unchanged.user_message(),
                        prevented_note(&run)
                    ),
                    StatusKind::Info,
                );
            }
            Err(e) => {
                s.error = Some(e.user_message());
                s.key.clear();
            }
        }
        self.tool = Some(s);
    }

    /// Selects exactly the notes of `run` in its tracks (multiset match).
    /// V09-S39: Select n10 "Store" keeps the notes it selected in its slot
    /// (this session; another run of Select combines its result with it).
    fn store_selection_slot(&mut self, values: &ParamSet, run: &TransformRun) {
        let Some(def) = plumspirit_engine::transform::tool("select") else {
            return;
        };
        let spec = (def.spec)();
        let p = plumspirit_engine::transform::Params::new(&spec.params, values);
        if !p.flag("n10") || p.choice("n10.op") != 4 {
            return;
        }
        let slot = p.int("n10.slot").clamp(1, 9) as usize - 1;
        let notes: Vec<plumspirit_engine::transform::SlotNote> = run
            .notes
            .iter()
            .filter_map(|n| {
                let t = run.tracks.get(usize::from(n.track))?;
                Some(plumspirit_engine::transform::SlotNote {
                    track: t.id,
                    start: n.start,
                    dur: n.dur,
                    pitch: n.pitch,
                    vel: n.vel,
                    sub: n.sub,
                    ch: n.ch,
                })
            })
            .collect();
        let mut slots = (*self.selection_slots).clone();
        slots.resize(9, Vec::new());
        slots[slot] = notes;
        self.selection_slots = Arc::new(slots);
    }

    fn select_run_notes(&mut self, run: &TransformRun) -> usize {
        self.release_preview_worker();
        let Some(doc) = self.doc_mut() else {
            return 0;
        };
        let mut total = 0;
        for (slot, t) in run.tracks.iter().enumerate() {
            let mut want: std::collections::HashMap<(u64, u64, u8, u8, u32, u8), usize> =
                Default::default();
            for n in run.track_notes(slot as u16) {
                *want
                    .entry((n.start, n.dur, n.pitch, n.vel, n.sub, n.ch))
                    .or_default() += 1;
            }
            let Ok(notes) = plumspirit_engine::editor::notes_mut(doc, t.target()) else {
                continue;
            };
            notes.clear_selection();
            let locs: Vec<_> = notes
                .iter_with_loc()
                .filter_map(|(loc, v)| {
                    let k = (
                        v.start_tick,
                        v.duration_ticks,
                        v.attrs.pitch(),
                        v.attrs.velocity(),
                        v.sub.0,
                        v.attrs.channel(),
                    );
                    match want.get_mut(&k) {
                        Some(c) if *c > 0 => {
                            *c -= 1;
                            Some(loc)
                        }
                        _ => None,
                    }
                })
                .collect();
            for loc in locs {
                notes.set_selected(loc, true);
                total += 1;
            }
        }
        total
    }

    // ------------------------------------------------------------- presets

    fn preset_dir(&self, tool: &str) -> Option<PathBuf> {
        self.config_dir
            .as_ref()
            .map(|d| d.join("presets").join(tool))
    }

    fn refresh_presets(&self, s: &mut ToolSession) {
        s.presets.names.clear();
        let Some(dir) = self.preset_dir(s.def.id) else {
            return;
        };
        s.presets.names = preset_files::list(&dir);
        s.presets.loaded_dir = Some(dir);
    }

    fn save_preset(&self, s: &mut ToolSession, name: &str) {
        let Some(dir) = self.preset_dir(s.def.id) else {
            s.presets.message = Some("Presets need a settings folder.".into());
            return;
        };
        match preset_files::save(&dir, s.def.id, name, &s.values) {
            Ok(name) => {
                s.presets.message = Some(format!("Saved preset \"{name}\"."));
                s.presets.selected = Some(name);
                s.presets.saved = Some(s.values.clone());
            }
            Err(e) => s.presets.message = Some(e),
        }
        self.refresh_presets(s);
    }

    fn load_preset(&self, s: &mut ToolSession, name: &str) {
        let Some(dir) = self.preset_dir(s.def.id) else {
            return;
        };
        match preset_files::load(&dir, name, s.def.id, &s.spec.params) {
            Ok((p, skipped)) => {
                s.values = ParamSet::defaults_in(&s.spec.params, s.keys);
                for (k, v) in p.values.iter() {
                    s.values.set(k, v.clone());
                }
                s.presets.selected = Some(name.to_string());
                s.presets.saved = Some(s.values.clone());
                s.presets.message = Some(if skipped.is_empty() {
                    format!("Loaded preset \"{name}\".")
                } else {
                    format!(
                        "Loaded preset \"{name}\" ({} setting(s) from a newer version ignored).",
                        skipped.len()
                    )
                });
            }
            Err(e) => s.presets.message = Some(e),
        }
    }

    fn rename_preset(&self, s: &mut ToolSession, from: &str, to: &str) {
        let Some(dir) = self.preset_dir(s.def.id) else {
            return;
        };
        match preset_files::rename(&dir, from, to) {
            Ok(to) => {
                s.presets.message = Some(format!("Renamed to \"{to}\"."));
                s.presets.selected = Some(to);
            }
            Err(e) => s.presets.message = Some(e),
        }
        self.refresh_presets(s);
    }

    fn delete_preset(&self, s: &mut ToolSession, name: &str) {
        let Some(dir) = self.preset_dir(s.def.id) else {
            return;
        };
        match preset_files::delete(&dir, name) {
            Ok(()) => {
                s.presets.message = Some(format!("Deleted preset \"{name}\"."));
                s.presets.selected = None;
            }
            Err(e) => s.presets.message = Some(e),
        }
        self.refresh_presets(s);
    }

    // ------------------------------------------------------------- window

    /// The tool window (modeless, movable, resizable, no backdrop).
    pub(super) fn tool_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.tool.take() else {
            return;
        };
        let mut apply = false;
        let mut close = false;
        let mut preset_action: Option<PresetAction> = None;
        // v0.9 §082: Duplicate's one-click octave copies (+12 / −12).
        let mut octave: Option<i64> = None;
        let title = tool_window_title(&s.spec);
        // §082: the time selection limits a tool through the note selection
        // (one range state; every tool already scopes by selected notes).
        let time_sel = self.time_selection_text();
        let mut select_time = false;
        let mut open = true;
        // v0.8 semantic editors: the active track's subchannel colours (by
        // position) and the project's track labels.
        let dark = ctx.theme() == egui::Theme::Dark;
        let palette = crate::ui::theme::note_palette(ctx);
        let (sub_colors, track_names): (Vec<egui::Color32>, Vec<String>) = match self.doc() {
            Some(d) => (
                d.track(self.active_track())
                    .map(|t| {
                        t.subchannels()
                            .entries()
                            .iter()
                            .map(|e| crate::ui::theme::subchannel_color(&palette, e.id.0, dark))
                            .collect()
                    })
                    .unwrap_or_default(),
                d.track_labels()
                    .into_iter()
                    .enumerate()
                    .map(|(i, l)| format!("{} · {l}", i + 1))
                    .collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        // v0.9: Reset and the editors follow the project's key range.
        if let Some(d) = self.doc() {
            s.keys = d.project().keys;
        }
        let ectx = crate::ui::param_editors::EditorCtx {
            sub_colors: &sub_colors,
            tracks: &track_names,
            keys: s.keys,
        };
        let mut want_help = false;
        let help_topic =
            if s.spec.family == plumspirit_engine::transform::registry::Family::Generator {
                "generators"
            } else {
                "tools"
            };
        egui::Window::new(title)
            .id(egui::Id::new(("plumspirit-tool", s.def.id)))
            .open(&mut open)
            .resizable(true)
            .collapsible(true)
            .default_width(360.0)
            .max_width((ctx.content_rect().width() - 24.0).max(240.0))
            .max_height((ctx.content_rect().height() - 16.0).max(80.0))
            .vscroll(true)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-400.0, 80.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(s.spec.summary).weak()).wrap());
                    want_help |= super::help::help_button(ui, help_topic);
                });
                if let Some(credit) = super::plugins::tool_credit(s.def.id) {
                    ui.label(RichText::new(credit).small());
                }
                ui.add_space(4.0);
                preset_bar(ui, &mut s, &mut preset_action);
                ui.separator();
                // The parameters scroll; the footer (stroke hint, Preview,
                // status, Apply / Reset / Cancel) always stays on screen.
                let screen = ui.ctx().content_rect();
                let footer = if s.capture_mode().is_some() { 200.0 } else { 150.0 }
                    + if order_line(&s.definition, &s.values).is_some() {
                        18.0
                    } else {
                        0.0
                    };
                let room = screen.bottom() - ui.cursor().top() - footer;
                egui::ScrollArea::vertical()
                    .max_height(room.clamp(120.0, (screen.height() * 0.6).max(120.0)))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if !s.spec.params.iter().any(|p| p.key == "scope") {
                            egui::ComboBox::from_label("Notes")
                                .selected_text(SCOPE_LABELS[s.scope.min(SCOPE_LABELS.len() - 1)])
                                .show_ui(ui, |ui| {
                                    for (i, l) in SCOPE_LABELS.iter().enumerate() {
                                        ui.selectable_value(&mut s.scope, i, *l);
                                    }
                                });
                        }
                        if let Some(text) = &time_sel {
                            let by_selection = matches!(
                                s.effective_scope(),
                                ToolScope::Selected
                                    | ToolScope::SelectedSubtree
                                    | ToolScope::SelectedEverywhere
                            );
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("Time selection {text}")).weak());
                                if ui
                                    .add_enabled(by_selection, egui::Button::new("Select its notes"))
                                    .on_hover_text("The notes that start in the time selection (on this tool's tracks) become the selection, so the tool works on that time only.")
                                    .on_disabled_hover_text("This scope takes every note of its tracks; choose a “Selected notes” scope to limit the tool to the time selection.")
                                    .clicked()
                                {
                                    select_time = true;
                                }
                            });
                        }
                        if s.def.id == "duplicate" {
                            ui.horizontal_wrapped(|ui| {
                                ui.label("One click:");
                                for (label, semis) in [
                                    ("Duplicate Up One Octave", 12),
                                    ("Duplicate Down One Octave", -12),
                                ] {
                                    if ui
                                        .button(label)
                                        .on_hover_text("Adds a copy of every selected note an octave away at the same time (one undo step); the settings below are not used.")
                                        .clicked()
                                    {
                                        octave = Some(semis);
                                    }
                                }
                            });
                            ui.separator();
                        }
                        params_ui(ui, &mut s, &ectx);
                    });
                ui.separator();
                if let Some(mode) = s.capture_mode() {
                    let hint = match mode {
                        crate::piano_roll::CaptureMode::Freehand | crate::piano_roll::CaptureMode::FreehandOrdered => {
                            "Drag in the Piano Roll to draw (the window stays open)."
                        }
                        crate::piano_roll::CaptureMode::Polygon => {
                            "Click in the Piano Roll to add the lasso's corners."
                        }
                    };
                    ui.label(RichText::new(hint).weak());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!s.stroke.is_empty(), egui::Button::new("Clear"))
                            .clicked()
                        {
                            s.clear_stroke();
                        }
                        if matches!(s.def.id, "draw" | "paint") {
                            ui.checkbox(&mut s.auto_apply, "Apply each stroke on release")
                                .on_hover_text("Each stroke becomes notes when the mouse button is released (one undo step per stroke). Off: Preview the stroke and press Apply.");
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.preview, "Preview");
                    ui.add_enabled(
                        s.preview,
                        egui::Checkbox::new(&mut s.show_original, "Show the original"),
                    )
                    .on_hover_text("Before / after: shows the notes as they are now; the preview stays computed.");
                    if let Some(w) = &s.worker {
                        ui.spinner();
                        ui.label(RichText::new(format!("computing #{}", w.generation)).weak());
                    }
                });
                order_ui(ui, &s.definition, &s.values);
                if let Some(e) = &s.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                } else if !s.status.is_empty() {
                    ui.label(&s.status);
                }
                ui.horizontal(|ui| {
                    let can = s.error.is_none() && s.worker.is_none();
                    if ui.add_enabled(can, egui::Button::new("Apply")).clicked() {
                        apply = true;
                    }
                    if ui.button("Reset").clicked() {
                        s.values = ParamSet::defaults_in(&s.spec.params, s.keys);
                        s.presets.selected = None;
                    }
                    if ui
                        .button("Cancel")
                        .on_hover_text("Close without changing anything")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        if !open {
            close = true;
        }
        if want_help {
            self.open_help(help_topic);
        }
        if let Some(a) = preset_action {
            match a {
                PresetAction::Load(n) => self.load_preset(&mut s, &n),
                PresetAction::Save(n) => self.save_preset(&mut s, &n),
                PresetAction::Rename(from, to) => self.rename_preset(&mut s, &from, &to),
                PresetAction::Delete(n) => self.delete_preset(&mut s, &n),
            }
        }
        if close {
            s.stop_worker();
            self.tool_memory
                .insert(s.def.id, (s.values.clone(), s.scope));
            ctx.request_repaint();
            return;
        }
        if select_time && let Some(doc) = self.doc() {
            let scopes = scopes_for(doc, self.edit_target(), s.effective_scope());
            let mut indices: Vec<usize> = scopes.iter().map(|t| t.index).collect();
            indices.dedup();
            self.select_in_time_selection(&indices);
        }
        self.tool = Some(s);
        if apply {
            self.apply_tool(ctx);
        }
        if let Some(semis) = octave {
            let cmd = if semis > 0 {
                super::commands::Command::DuplicateOctaveUp
            } else {
                super::commands::Command::DuplicateOctaveDown
            };
            self.quick_tool(
                "duplicate_octave",
                &[("semis", ParamValue::Int(semis))],
                Some(cmd.label()),
                ctx,
            );
        }
    }
}

/// What the piano roll draws for the open tool on the active track `ti`.
pub fn tool_preview(
    tool: &Option<ToolSession>,
    ti: plumspirit_engine::editor::NoteTarget,
) -> Option<crate::piano_roll::ToolPreview<'_>> {
    let s = tool.as_ref()?;
    if !s.preview || s.show_original || s.error.is_some() {
        return None;
    }
    if let Some(run) = &s.run {
        let slot = run.tracks.iter().position(|t| t.target() == ti)?;
        let notes = run.track_notes(slot as u16);
        return Some(crate::piano_roll::ToolPreview::Run {
            notes,
            extra: &s.extra,
            max_dur: s.max_durs.get(slot).copied().unwrap_or(u64::MAX),
            all: run.tracks[slot].all,
            rev: s.preview_rev(),
        });
    }
    let (k, b, tracks) = s.lazy.as_ref()?;
    let slot = tracks.iter().position(|t| t.target() == ti)?;
    Some(crate::piano_roll::ToolPreview::Lazy {
        kernel: k.as_point()?,
        boundary: *b,
        slot: slot as u16,
        all: tracks[slot].all,
        rev: s.preview_rev(),
    })
}

enum PresetAction {
    Load(String),
    Save(String),
    Rename(String, String),
    Delete(String),
}

/// Preset files: `<settings>/presets/<tool>/<name>.psp`, one tool per folder.
/// Free of UI state so save / load / rename / delete are unit-tested.
mod preset_files {
    use super::{ParamSet, ParamSpec, preset, sanitize_file_name};
    use std::path::{Path, PathBuf};

    fn path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.psp"))
    }

    /// The file name a typed preset name is stored under ("" = invalid).
    pub fn file_name(typed: &str) -> String {
        sanitize_file_name(&preset::clean_name(typed))
    }

    /// Preset names in the folder, case-insensitively sorted.
    pub fn list(dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "psp")
                    && let Some(stem) = p.file_stem().and_then(|x| x.to_str())
                {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Writes `values` as preset `typed` (Save As replaces a preset of the same
    /// name, as its label says). Returns the stored name.
    pub fn save(dir: &Path, tool: &str, typed: &str, values: &ParamSet) -> Result<String, String> {
        let name = file_name(typed);
        if name.is_empty() {
            return Err("Type a name for the preset first.".into());
        }
        let p = preset::Preset {
            tool: tool.to_string(),
            name: name.clone(),
            values: values.clone(),
        };
        std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(path(dir, &name), preset::to_text(&p)))
            .map_err(|e| format!("Could not save the preset: {e}."))?;
        Ok(name)
    }

    /// Reads preset `name` for `tool` (size-checked before reading).
    pub fn load(
        dir: &Path,
        name: &str,
        tool: &str,
        specs: &[ParamSpec],
    ) -> Result<(preset::Preset, Vec<String>), String> {
        let path = path(dir, name);
        let len = std::fs::metadata(&path)
            .map_err(|e| format!("Could not open the preset: {e}."))?
            .len();
        if len > preset::MAX_BYTES as u64 {
            return Err("The preset file is too large.".into());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Could not read the preset: {e}."))?;
        preset::parse(&text, tool, specs)
    }

    /// Renames preset `from` to the typed name. Never replaces another preset
    /// (a change of letter case only is allowed). Returns the new name.
    pub fn rename(dir: &Path, from: &str, typed: &str) -> Result<String, String> {
        let to = file_name(typed);
        if to.is_empty() {
            return Err("Type the new name for the preset first.".into());
        }
        if to == from {
            return Ok(to);
        }
        if !to.eq_ignore_ascii_case(from) && path(dir, &to).exists() {
            return Err(format!(
                "A preset named \"{to}\" already exists. Choose another name or delete it first."
            ));
        }
        std::fs::rename(path(dir, from), path(dir, &to))
            .map_err(|e| format!("Could not rename the preset: {e}."))?;
        Ok(to)
    }

    /// Deletes preset `name`.
    pub fn delete(dir: &Path, name: &str) -> Result<(), String> {
        std::fs::remove_file(path(dir, name))
            .map_err(|e| format!("Could not delete the preset: {e}."))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use plumspirit_engine::transform::param::ParamValue;

        fn dir(tag: &str) -> PathBuf {
            let d = std::env::temp_dir()
                .join(format!("plumspirit-presets-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            d
        }

        #[test]
        fn presets_save_load_rename_and_delete_through_their_folder() {
            let tool = plumspirit_engine::transform::tool("strum").unwrap();
            let specs = (tool.spec)().params;
            let d = dir("rt");
            let mut v = ParamSet::defaults(&specs);
            v.set(
                plumspirit_engine::transform::param::ADV_KEYS[0],
                ParamValue::Bool(true),
            );
            assert!(save(&d, "strum", "  ", &v).is_err());
            assert_eq!(save(&d, "strum", "Soft: up", &v).unwrap(), "Soft_ up");
            assert_eq!(save(&d, "strum", "alpha", &v).unwrap(), "alpha");
            assert_eq!(list(&d), vec!["alpha", "Soft_ up"]);
            let loaded = |name: &str| {
                let (p, skipped) = load(&d, name, "strum", &specs).unwrap();
                assert!(skipped.is_empty());
                let mut got = ParamSet::defaults(&specs);
                for (k, x) in p.values.iter() {
                    got.set(k, x.clone());
                }
                got
            };
            assert_eq!(loaded("Soft_ up"), v);
            // Rename onto another preset is refused and changes nothing.
            let e = rename(&d, "alpha", "Soft_ up").unwrap_err();
            assert!(e.ends_with('.') && e.contains("already exists"), "{e}");
            assert_eq!(list(&d), vec!["alpha", "Soft_ up"]);
            assert_eq!(loaded("Soft_ up"), v);
            // A real rename, and a case-only rename.
            assert_eq!(rename(&d, "alpha", "beta").unwrap(), "beta");
            assert_eq!(rename(&d, "beta", "Beta").unwrap(), "Beta");
            assert_eq!(list(&d), vec!["Beta", "Soft_ up"]);
            delete(&d, "Beta").unwrap();
            assert_eq!(list(&d), vec!["Soft_ up"]);
            let e = load(&d, "Beta", "strum", &specs).unwrap_err();
            assert!(e.starts_with("Could not open the preset"), "{e}");
            // A preset of another tool is refused with a sentence.
            let other = (plumspirit_engine::transform::tool("chop").unwrap().spec)().params;
            let e = load(&d, "Soft_ up", "chop", &other).unwrap_err();
            assert!(e.ends_with('.'), "{e}");
            // An oversized file is refused before reading.
            std::fs::write(d.join("huge.psp"), vec![b'x'; preset::MAX_BYTES + 1]).unwrap();
            assert_eq!(
                load(&d, "huge", "strum", &specs).unwrap_err(),
                "The preset file is too large."
            );
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

fn sanitize_file_name(n: &str) -> String {
    n.chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim_matches('.')
        .trim()
        .to_string()
}

fn preset_bar(ui: &mut Ui, s: &mut ToolSession, action: &mut Option<PresetAction>) {
    ui.horizontal(|ui| {
        ui.label("Preset");
        let changed = s.presets.selected.is_some()
            && s.presets.saved.as_ref().is_some_and(|v| *v != s.values);
        let current = match &s.presets.selected {
            Some(n) if changed => format!("{n} (changed)"),
            Some(n) => n.clone(),
            None => "(none)".into(),
        };
        egui::ComboBox::from_id_salt(("preset", s.def.id))
            .selected_text(current)
            .width(150.0)
            .show_ui(ui, |ui| {
                if s.presets.names.is_empty() {
                    ui.label(RichText::new("No saved presets").weak());
                }
                for n in s.presets.names.clone() {
                    if ui
                        .selectable_label(s.presets.selected.as_deref() == Some(&n), &n)
                        .clicked()
                    {
                        *action = Some(PresetAction::Load(n));
                    }
                }
            });
        let sel = s.presets.selected.clone();
        if ui
            .add_enabled(sel.is_some(), egui::Button::new("Save"))
            .on_hover_text("Overwrite the selected preset with the current settings")
            .clicked()
            && let Some(n) = sel.clone()
        {
            *action = Some(PresetAction::Save(n));
        }
        if ui
            .add_enabled(sel.is_some(), egui::Button::new("Delete"))
            .clicked()
            && let Some(n) = sel.clone()
        {
            *action = Some(PresetAction::Delete(n));
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.presets.name_edit)
                .hint_text("Preset name")
                .desired_width(150.0),
        );
        if ui.button("Save As").clicked() {
            *action = Some(PresetAction::Save(s.presets.name_edit.clone()));
        }
        let sel = s.presets.selected.clone();
        if ui
            .add_enabled(
                sel.is_some() && !s.presets.name_edit.trim().is_empty(),
                egui::Button::new("Rename"),
            )
            .clicked()
            && let Some(n) = sel
        {
            *action = Some(PresetAction::Rename(n, s.presets.name_edit.clone()));
        }
    });
    if let Some(m) = &s.presets.message {
        ui.label(RichText::new(m).small().weak());
    }
}

/// The tool window's controls, generated from the tool's definition (v0.9
/// §208): base controls, the Active advanced functions, the Legacy ones
/// (kept for presets of earlier versions), the boundary policies.
fn params_ui(ui: &mut Ui, s: &mut ToolSession, ectx: &crate::ui::param_editors::EditorCtx) {
    let tool_id = s.def.id;
    let def = &s.definition;
    let open_all = all_sections_open(ui);
    for p in def.base_params() {
        param_row(ui, tool_id, p, &mut s.values, ectx);
    }
    for (status, heading, note) in [
        (FunctionStatus::Active, "Advanced", None),
        (
            FunctionStatus::Legacy,
            "Legacy Advanced",
            Some("Kept so presets from earlier versions keep working."),
        ),
    ] {
        let functions: Vec<_> = def
            .functions
            .iter()
            .filter(|f| f.status == status)
            .collect();
        if functions.is_empty() {
            continue;
        }
        ui.add_space(4.0);
        let h = ui.label(RichText::new(heading).strong());
        if let Some(n) = note {
            h.on_hover_text(n);
        }
        rendered(ui, heading);
        for f in functions {
            let mut on = matches!(s.values.get_raw(f.switch), Some(ParamValue::Bool(true)));
            let id = ui.make_persistent_id(("adv", tool_id, f.n));
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                open_all,
            )
            .show_header(ui, |ui| {
                if ui
                    .checkbox(&mut on, format!("{} · {}", f.n, f.name))
                    .changed()
                {
                    s.values.set(f.switch, ParamValue::Bool(on));
                }
            })
            .body(|ui| {
                // §210: one sweep graph at a time, chosen from the targets.
                if f.mandatory && f.switch == plumspirit_engine::transform::functions::SWEEP {
                    ui.add_enabled_ui(on, |ui| {
                        sweep_section(ui, tool_id, def, &mut s.values, ectx)
                    });
                    return;
                }
                let mut any = false;
                for p in def.function_params(f.n) {
                    any = true;
                    ui.add_enabled_ui(on, |ui| param_row(ui, tool_id, p, &mut s.values, ectx));
                }
                if !any {
                    ui.label(RichText::new("No extra settings.").weak());
                }
            });
        }
    }
    order_section(ui, s, open_all);
    let def = &s.definition;
    // Boundary policy.
    if def.boundary_params().next().is_some() {
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Boundaries")
            .id_salt(("bounds", tool_id))
            .default_open(open_all)
            .show(ui, |ui| {
                for p in def.boundary_params() {
                    param_row(ui, tool_id, p, &mut s.values, ectx);
                }
            });
    }
}

/// v1.0-beta2 B2-F01: the Execution Order section — every reorderable
/// group with its members numbered in run order (off ones greyed in their
/// slots), Move Up / Move Down buttons and Alt+Up / Alt+Down on a focused
/// row, the fixed functions that are on with the reason they cannot move,
/// Reset Order, and the sentence of an invalid order. A move writes every
/// group's order into the values (`ToolDefinition::moved_order`).
fn order_section(ui: &mut Ui, s: &mut ToolSession, open_all: bool) {
    use plumspirit_engine::transform::definition::{ORDER_KEY, Phase};
    let def = &s.definition;
    if def.groups().is_empty() {
        return;
    }
    let tool_id = s.def.id;
    let plan = def.plan(&s.values);
    let custom = s.values.get_raw(ORDER_KEY).is_some();
    let on = |v: &ParamSet, f: &plumspirit_engine::transform::definition::FunctionDef| {
        matches!(v.get_raw(f.switch), Some(ParamValue::Bool(true)))
    };
    // Open by default when an order matters: two members of a group on.
    let matters = def.groups().iter().any(|g| {
        g.members
            .iter()
            .filter(|m| def.function(m).is_some_and(|f| on(&s.values, f)))
            .count()
            >= 2
    });
    let mut moved: Option<String> = None;
    let mut reset = false;
    let refocus_id = egui::Id::new(("order-refocus", tool_id));
    let pending: Option<egui::Id> = ui.data(|d| d.get_temp(refocus_id));
    ui.add_space(4.0);
    egui::CollapsingHeader::new("Execution Order")
        .id_salt(("order", tool_id))
        .default_open(open_all || matters || custom)
        .show(ui, |ui| {
            rendered(ui, "Execution Order");
            ui.label(
                RichText::new(
                    "A group's functions run top to bottom — move them to change the result. The others run at fixed places.",
                )
                .small()
                .weak(),
            );
            if let Err(e) = &plan {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            for phase in def.phases(&s.values) {
                match phase {
                    Phase::Fixed { functions, reason } => {
                        let list: Vec<String> = functions
                            .iter()
                            .filter(|f| on(&s.values, f))
                            .map(|f| format!("{} · {}", f.n, f.name))
                            .collect();
                        if !list.is_empty() {
                            ui.label(
                                RichText::new(format!("Fixed: {}", list.join(", "))).small(),
                            )
                            .on_hover_text(format!("Fixed in place: {reason}."));
                        }
                    }
                    Phase::Group { group, members } => {
                        ui.label(RichText::new(group.name).strong())
                            .on_hover_text("Runs in the order listed; each function works on what the ones above it made.");
                        let last = members.len().saturating_sub(1);
                        for (i, f) in members.iter().enumerate() {
                            ui.push_id(("order-row", f.id), |ui| ui.horizontal(|ui| {
                                let up = ui
                                    .add_enabled(i > 0, egui::Button::new("↑").small())
                                    .on_hover_text("Move up (Alt+Up)");
                                let down = ui
                                    .add_enabled(i < last, egui::Button::new("↓").small())
                                    .on_hover_text("Move down (Alt+Down)");
                                let text = format!("{}. {} · {}", i + 1, f.n, f.name);
                                let text = if on(&s.values, f) {
                                    RichText::new(text)
                                } else {
                                    RichText::new(text).weak()
                                };
                                // A stable id per function: the row keeps its focus
                                // when it moves (auto ids follow the position).
                                let label = ui.label(text);
                                let row = ui
                                    .interact(
                                        label.rect,
                                        egui::Id::new(("order-row", tool_id, f.id)),
                                        egui::Sense::click(),
                                    )
                                    .on_hover_text(if on(&s.values, f) {
                                        "On. Focus the row (click, Tab) and press Alt+Up / Alt+Down to move it."
                                    } else {
                                        "Off: it keeps its place for when it is switched on."
                                    });
                                #[cfg(test)]
                                ui.data_mut(|d| {
                                    d.insert_temp(egui::Id::new(("order-row-id", f.id)), row.id)
                                });
                                if row.clicked() {
                                    row.request_focus();
                                }
                                let mut delta = 0isize;
                                // Focused, or moved last frame (the focus comes
                                // back this frame).
                                let focused = row.has_focus() || pending == Some(row.id);
                                if up.clicked() {
                                    delta = -1;
                                } else if down.clicked() {
                                    delta = 1;
                                } else if focused {
                                    ui.input_mut(|inp| {
                                        if inp.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp) {
                                            delta = -1;
                                        } else if inp
                                            .consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown)
                                        {
                                            delta = 1;
                                        }
                                    });
                                }
                                if delta != 0 {
                                    moved = def.moved_order(&s.values, f.id, delta);
                                    if moved.is_some() && focused {
                                        // Keep the moved row focused for the next press
                                        // (egui's arrow-key focus move would take it).
                                        ui.data_mut(|d| d.insert_temp(refocus_id, row.id));
                                    }
                                } else if pending == Some(row.id) {
                                    row.request_focus();
                                    ui.data_mut(|d| d.remove::<egui::Id>(refocus_id));
                                }
                                if delta != 0 && pending == Some(row.id) {
                                    row.request_focus();
                                }
                            }));
                        }
                    }
                }
            }
            if ui
                .add_enabled(custom, egui::Button::new("Reset Order"))
                .on_hover_text("Back to the tool's own order")
                .clicked()
            {
                reset = true;
            }
        });
    if let Some(o) = moved {
        s.values.set(ORDER_KEY, ParamValue::Text(o));
    } else if reset {
        s.values.remove(ORDER_KEY);
    }
}

/// The Preview's order line (v0.9 §235, §236): with two or more functions
/// switched on, their numbers in the order they are applied
/// ([`ToolDefinition::execution_order`]), Legacy ones marked — one short
/// line ("Order: 14 → 1 → 3 (Legacy) → 12"), so the window's footer keeps
/// its height; [`order_details`] names them (the hover text).
pub(crate) fn order_line(def: &ToolDefinition, values: &ParamSet) -> Option<String> {
    let on = def.execution_order(values);
    if on.len() < 2 {
        return None;
    }
    let steps: Vec<String> = on
        .iter()
        .map(|f| format!("{}{}", f.n, legacy_mark(f.status)))
        .collect();
    Some(format!("Order: {}", steps.join(" → ")))
}

/// The functions of [`order_line`] by number and name, one per line.
pub(crate) fn order_details(def: &ToolDefinition, values: &ParamSet) -> String {
    let mut out = String::from("Applied in this order:");
    for f in def.execution_order(values) {
        out.push_str(&format!("\n{} · {}{}", f.n, f.name, legacy_mark(f.status)));
    }
    out
}

fn legacy_mark(status: FunctionStatus) -> &'static str {
    match status {
        FunctionStatus::Legacy => " (Legacy)",
        FunctionStatus::Active => "",
    }
}

/// Draws [`order_line`] under the Preview switch (nothing with fewer than
/// two functions on): one line, truncated when the window is narrow; the
/// hover lists the functions by name.
fn order_ui(ui: &mut Ui, def: &ToolDefinition, values: &ParamSet) {
    if let Some(line) = order_line(def, values) {
        ui.add(egui::Label::new(RichText::new(line).small().weak()).truncate())
            .on_hover_text(format!(
                "{}\n\nThe functions that are on run in this order. The groups under Execution Order run in the order you give them there; the other functions run at fixed places (the Control Sweep chooses the settings first; the tool's own functions run at their stages of its pipeline). The preview shows the combined result.",
                order_details(def, values)
            ));
        rendered(ui, "Order");
    }
}

/// The Track List Paste Repeat dialog's ten functions (V09-S15, §094):
/// each with its switch as the section header and its controls drawn by the
/// tool windows' rows ([`param_row`]: the same numbers, menus, curve editor
/// and semantic list editors), greyed while it is off.
pub(super) fn function_sections(
    ui: &mut Ui,
    tool_id: &'static str,
    names: &[&str],
    defs: &[ParamDef],
    values: &mut ParamSet,
    ectx: &crate::ui::param_editors::EditorCtx,
) {
    let open_all = all_sections_open(ui);
    for (i, name) in names.iter().enumerate() {
        let n = (i + 1) as u8;
        let switch = plumspirit_engine::transform::param::ADV_KEYS[i];
        let mut on = matches!(values.get_raw(switch), Some(ParamValue::Bool(true)));
        let id = ui.make_persistent_id(("adv", tool_id, n));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open_all)
            .show_header(ui, |ui| {
                if ui.checkbox(&mut on, format!("{n} · {name}")).changed() {
                    values.set(switch, ParamValue::Bool(on));
                }
            })
            .body(|ui| {
                for p in defs
                    .iter()
                    .filter(|p| p.spec.adv == n && p.spec.key != switch)
                {
                    ui.add_enabled_ui(on, |ui| param_row(ui, tool_id, p, values, ectx));
                }
            });
    }
}

/// The Control Sweep section (v0.9 §210): a target menu (● marks the sweeps
/// that are on) instead of ten graphs at once, what the chosen target
/// drives, its switch and its graph (the shared curve editor: free ends,
/// numeric points, Smoothness, Reset), and which sweeps are on.
fn sweep_section(
    ui: &mut Ui,
    tool_id: &'static str,
    def: &ToolDefinition,
    values: &mut ParamSet,
    ectx: &crate::ui::param_editors::EditorCtx,
) {
    use plumspirit_engine::transform::definition::{Combine, SweepTarget};
    let targets = def.sweeps;
    if targets.is_empty() {
        return;
    }
    let is_on =
        |v: &ParamSet, t: &SweepTarget| matches!(v.get_raw(t.on), Some(ParamValue::Bool(true)));
    let text = |v: &ParamSet, t: &SweepTarget| {
        format!("{} {}", if is_on(v, t) { "●" } else { "○" }, t.label)
    };
    // One window per tool: the chosen target is kept per tool.
    let sel_id = egui::Id::new(("sweep-target", tool_id));
    let mut sel = ui
        .data(|d| d.get_temp::<usize>(sel_id))
        .unwrap_or(0)
        .min(targets.len() - 1);
    ui.horizontal(|ui| {
        ui.label("Target");
        let before = sel;
        egui::ComboBox::from_id_salt(("sweep-combo", tool_id))
            .selected_text(text(values, &targets[sel]))
            .width(220.0)
            .show_ui(ui, |ui| {
                for (i, t) in targets.iter().enumerate() {
                    ui.selectable_value(&mut sel, i, text(values, t));
                }
            });
        if sel != before {
            ui.data_mut(|d| d.insert_temp(sel_id, sel));
        }
    });
    rendered(ui, "Sweep target");
    let t = &targets[sel];
    // V09-0143: a tool that sweeps per unit (Group: per group, measured at
    // its first note) says so.
    let per_group = plumspirit_engine::transform::definition::sweeps_per_group(tool_id);
    let along = match t.domain {
        plumspirit_engine::transform::drivers::Input::Time if per_group => {
            "the selection's time, per group (at its first note)"
        }
        plumspirit_engine::transform::drivers::Input::Time => "the selection's time",
        plumspirit_engine::transform::drivers::Input::Pitch if per_group => {
            "the selection's pitch range, per group (at its first note)"
        }
        plumspirit_engine::transform::drivers::Input::Pitch => "the selection's pitch range",
        plumspirit_engine::transform::drivers::Input::NoteIndex if per_group => {
            "the groups' order in time"
        }
        plumspirit_engine::transform::drivers::Input::NoteIndex => "the notes' order",
        plumspirit_engine::transform::drivers::Input::Velocity if per_group => {
            "velocity, per group (its first note)"
        }
        plumspirit_engine::transform::drivers::Input::Velocity => "velocity",
        plumspirit_engine::transform::drivers::Input::Subchannel => "the subchannels",
        plumspirit_engine::transform::drivers::Input::PatternPhase => "each bar",
        plumspirit_engine::transform::drivers::Input::Tool => "the tool's own dimension",
    };
    let how = match t.combine {
        Combine::Replace => "replaces",
        Combine::Add => "is added to",
        Combine::Multiply => "multiplies",
    };
    let target = def.param(t.param).map_or(t.param, |p| p.spec.label);
    ui.label(
        RichText::new(format!(
            "The graph runs along {along}; its value ({} … {}) {how} “{target}”.",
            t.range.0, t.range.1
        ))
        .weak(),
    );
    for key in [t.on, t.graph] {
        if let Some(p) = def.param(key) {
            param_row(ui, tool_id, p, values, ectx);
        }
    }
    let on: Vec<&str> = targets
        .iter()
        .filter(|t| is_on(values, t))
        .map(|t| t.label)
        .collect();
    ui.label(
        RichText::new(if on.is_empty() {
            "No sweep is on.".to_owned()
        } else {
            format!("On: {}.", on.join(", "))
        })
        .weak(),
    );
}

/// Test hook: whether every collapsible section of a tool window starts
/// open (the schema UI test renders every control).
fn all_sections_open(ui: &Ui) -> bool {
    #[cfg(test)]
    return ui.data(|d| d.get_temp::<bool>(egui::Id::new(OPEN_ALL)).unwrap_or(false));
    #[cfg(not(test))]
    {
        let _ = ui;
        false
    }
}

#[cfg(test)]
const OPEN_ALL: &str = "tool-window-open-all";
#[cfg(test)]
const RENDERED: &str = "tool-window-rendered";

/// Test hook: records a control (parameter key) or heading the window drew.
#[inline]
fn rendered(ui: &Ui, what: &'static str) {
    #[cfg(test)]
    ui.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<&'static str>>(egui::Id::new(RENDERED))
            .push(what)
    });
    #[cfg(not(test))]
    let _ = (ui, what);
}

/// The range a tool window offers for an integer, pitch or list parameter
/// in a project of key range `keys` (v0.9: keyed parameters reach ± the top
/// key, pitches 0–255 in 256-Key Mode); other kinds: the full `i64` range.
fn value_range(p: &ParamSpec, keys: KeyRange) -> (i64, i64) {
    p.range_in(keys).unwrap_or((i64::MIN, i64::MAX))
}

fn param_row(
    ui: &mut Ui,
    tool_id: &str,
    pd: &ParamDef,
    values: &mut ParamSet,
    ectx: &crate::ui::param_editors::EditorCtx,
) {
    let p = &pd.spec;
    rendered(ui, p.key);
    let cur = values
        .get_raw(p.key)
        .cloned()
        .unwrap_or_else(|| p.default.clone());
    // v0.8 §185–§188: lists get the editor of their meaning (comma text is the
    // secondary "Raw" view); curves the curve editor.
    match (&p.kind, &cur) {
        (ParamKind::List { max_len, .. }, ParamValue::List(l)) => {
            let editor = pd.editor;
            let raw_id = ui.make_persistent_id(("raw", tool_id, p.key));
            let mut raw = ui.data(|d| d.get_temp::<bool>(raw_id)).unwrap_or(false);
            ui.horizontal(|ui| {
                let label = ui.label(p.label);
                if !p.help.is_empty() {
                    label.on_hover_text(p.help);
                }
                if ui
                    .selectable_label(raw, RichText::new("Raw").small())
                    .on_hover_text("Edit Raw Values: the numbers as text (comma-separated)")
                    .clicked()
                {
                    raw = !raw;
                    ui.data_mut(|d| d.insert_temp(raw_id, raw));
                }
                if ui
                    .small_button("Reset")
                    .on_hover_text("Back to the default values")
                    .clicked()
                {
                    values.set(p.key, p.default.clone());
                }
            });
            if !raw && editor != plumspirit_engine::transform::editors::Editor::Auto {
                let id = ui.make_persistent_id(("sem", tool_id, p.key));
                let (min, max) = value_range(p, ectx.keys);
                if let Some(v) = crate::ui::param_editors::list_editor(
                    ui, id, editor, l, min, max, *max_len, ectx,
                ) {
                    values.set(p.key, ParamValue::List(v));
                }
                return;
            }
            raw_list_row(ui, p, l, *max_len, values);
            return;
        }
        // B1-0078: Note Properties' range rows have their own editor (a
        // block of its own, like a graph).
        (ParamKind::Text { .. }, ParamValue::Text(t)) if p.key == "n13.rows" => {
            let label = ui.label(p.label);
            if !p.help.is_empty() {
                label.on_hover_text(p.help);
            }
            let id = ui.make_persistent_id(("ranges", tool_id, p.key));
            if let Some(nt) = crate::ui::ranges_editor::ranges_editor(ui, id, t) {
                values.set(p.key, ParamValue::Text(nt));
            }
            return;
        }
        (ParamKind::Curve, ParamValue::Curve(c)) => {
            let label = ui.label(p.label);
            if !p.help.is_empty() {
                label.on_hover_text(p.help);
            }
            let def = match &p.default {
                ParamValue::Curve(d) => d.clone(),
                _ => plumspirit_engine::transform::Curve::identity(),
            };
            let id = ui.make_persistent_id(("curve", tool_id, p.key));
            // B1-0113: the graphs whose kernel allows it can overshoot.
            if let Some(c2) =
                crate::ui::param_editors::curve_editor_overshoot(ui, id, c, &def, p.overshoot)
            {
                values.set(p.key, ParamValue::Curve(c2));
            }
            return;
        }
        _ => {}
    }
    ui.horizontal(|ui| {
        let label = ui.label(p.label);
        if !p.help.is_empty() {
            label.on_hover_text(p.help);
        }
        match (&p.kind, cur) {
            (ParamKind::Int { .. }, ParamValue::Int(mut v)) => {
                // v0.9: keyed ranges reach the project's top key.
                let (min, max) = value_range(p, ectx.keys);
                if ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut v)
                            .range(min..=max)
                            .speed(drag_speed(min, max))
                            .what(p.label),
                    )
                    .changed()
                {
                    values.set(p.key, ParamValue::Int(v));
                }
            }
            (ParamKind::Ticks { min, max }, ParamValue::Int(mut v)) => {
                let hi = (*max).min(i64::MAX as u64) as i64;
                if ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut v)
                            .range(*min as i64..=hi)
                            .speed(4.0)
                            .suffix(" ticks")
                            .what(p.label),
                    )
                    .changed()
                {
                    values.set(p.key, ParamValue::Int(v));
                }
            }
            (ParamKind::Pitch, ParamValue::Int(mut v)) => {
                let top = ectx.keys.max_pitch();
                let (lo, hi) = value_range(p, ectx.keys);
                if ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut v)
                            .range(lo..=hi)
                            .what(p.label)
                            .custom_formatter(move |n, _| {
                                plumspirit_engine::model::note::pitch_name(
                                    n.clamp(0.0, f64::from(top)) as u8,
                                )
                            }),
                    )
                    .changed()
                {
                    values.set(p.key, ParamValue::Int(v));
                }
            }
            (ParamKind::Seed, ParamValue::Int(mut v)) => {
                let mut changed = ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut v)
                            .range(0..=i64::MAX)
                            .what("the seed"),
                    )
                    .changed();
                if ui.button("🎲").on_hover_text("New seed").clicked() {
                    v = (plumspirit_engine::transform::rng::mix(v as u64) >> 2) as i64;
                    changed = true;
                }
                if changed {
                    values.set(p.key, ParamValue::Int(v));
                }
            }
            (ParamKind::Float { min, max, unit }, ParamValue::Float(mut v)) => {
                let changed = crate::ui::stepper::slider(ui, &mut v, *min..=*max, unit).changed();
                if changed {
                    values.set(p.key, ParamValue::Float(v));
                }
            }
            (ParamKind::Bool, ParamValue::Bool(mut v)) => {
                if ui.checkbox(&mut v, "").changed() {
                    values.set(p.key, ParamValue::Bool(v));
                }
            }
            (ParamKind::Choice(opts), ParamValue::Choice(mut c)) => {
                let before = c;
                egui::ComboBox::from_id_salt(("choice", p.key))
                    .selected_text(opts.get(c as usize).copied().unwrap_or("?"))
                    .show_ui(ui, |ui| {
                        for (i, o) in opts.iter().enumerate() {
                            ui.selectable_value(&mut c, i as u32, *o);
                        }
                    });
                if c != before {
                    values.set(p.key, ParamValue::Choice(c));
                }
            }

            (ParamKind::Text { max_len }, ParamValue::Text(mut t)) => {
                // B1-0075: Note Properties' expression fields are wide, and an
                // empty one says that it keeps each note's value.
                let formula = p.key.starts_with("n12.");
                let hint = match p.key {
                    "n12.consts" => "a = 12; b = a * 2",
                    "n12.cc" => "no controller",
                    _ if formula => "unchanged",
                    _ => "",
                };
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut t)
                            .char_limit(*max_len)
                            .hint_text(hint)
                            .desired_width(if formula { 340.0 } else { 180.0 }),
                    )
                    .changed()
                {
                    values.set(p.key, ParamValue::Text(t));
                }
                // V09-S47: a picture file parameter (Displacement Map;
                // V09-S50 Tumours' mask picture).
                if (p.key == "picture" || p.key.ends_with(".picture"))
                    && ui
                        .small_button("Browse…")
                        .on_hover_text("Choose a PNG, JPEG, BMP, GIF, WebP or TIFF picture.")
                        .clicked()
                    && let Some(path) = super::files::pick_picture()
                {
                    values.set(p.key, ParamValue::Text(path.display().to_string()));
                }
            }

            _ => {
                ui.label(RichText::new("(invalid value — reset)").weak());
                if ui.small_button("Reset").clicked() {
                    values.set(p.key, p.default.clone());
                }
            }
        }
    });
}

/// The comma-separated text view of a list ("Edit Raw Values").
fn raw_list_row(ui: &mut Ui, p: &ParamSpec, l: &[i64], max_len: usize, values: &mut ParamSet) {
    ui.horizontal(|ui| {
        let id = ui.make_persistent_id(("list", p.key));
        let mut text = ui
            .data(|d| d.get_temp::<String>(id))
            .unwrap_or_else(|| ParamValue::List(l.to_vec()).to_text());
        let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(220.0));
        if r.changed() {
            if let Some(v) = plumspirit_engine::transform::param::parse_list(&text, max_len) {
                values.set(p.key, ParamValue::List(v));
            }
            ui.data_mut(|d| d.insert_temp(id, text.clone()));
        }
        if r.lost_focus() {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        if plumspirit_engine::transform::param::parse_list(&text, max_len).is_none() {
            ui.colored_label(ui.visuals().error_fg_color, "numbers separated by commas");
        }
    });
}

fn drag_speed(min: i64, max: i64) -> f64 {
    let span = (max as f64 - min as f64).abs();
    (span / 500.0).clamp(0.05, 50.0)
}

/// Every output note of a run visible in `[t0, t1]` (helper for tests).
#[allow(dead_code)]
pub fn visible_run_notes(
    notes: &[TNote],
    max_dur: u64,
    t0: u64,
    t1: u64,
) -> impl Iterator<Item = &TNote> {
    let a = notes.partition_point(|n| n.start < t0.saturating_sub(max_dur));
    notes[a..]
        .iter()
        .take_while(move |n| n.start <= t1)
        .filter(move |n| n.end() >= t0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};

    /// V09-S38 step 3: the kernels' context lists the chapters (Clipping
    /// Mask n10) beside every marker start.
    #[test]
    fn the_tool_context_lists_the_chapters_beside_the_marker_starts() {
        let mut d = PlumDocument::new_project("ctx".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        for (s, e) in [(960, Some(4800)), (100, None), (7680, Some(9600))] {
            let edit = d.conductor().prepare_set_marker(None, s, e, "M").unwrap();
            h.apply(
                &mut d,
                Operation::Conductor {
                    edit,
                    label: "Add Marker",
                },
            )
            .unwrap();
        }
        let c = ctx_at(&d, None, 240, 0, 0);
        assert_eq!(*c.markers, vec![100, 960, 7680]);
        assert_eq!(*c.chapters, vec![(960, 4800), (7680, 9600)]);
    }

    /// v0.9 §113 (V09-S33): the window names the family's section; a tool
    /// moved to Legacy names its marked Legacy section instead.
    #[test]
    fn a_legacy_tool_window_names_its_marked_legacy_section() {
        use plumspirit_engine::transform::definition::LegacyTable;
        let spec = |id: &str| (plumspirit_engine::transform::tool(id).unwrap().spec)();
        assert_eq!(
            tool_window_title(&spec("subpaint")),
            "Paint V2 — Iterations"
        );
        assert_eq!(tool_window_title(&spec("shear")), "Shear — Geometry");
        static MOVED: LegacyTable = LegacyTable {
            tools: &["shear", "orbit"],
            functions: &[],
        };
        assert_eq!(
            tool_window_title_in(&spec("shear"), &MOVED),
            "Shear — Legacy"
        );
        let orbit = spec("orbit");
        assert_eq!(
            tool_window_title_in(&orbit, &MOVED),
            format!("{} — Legacy Experimental", orbit.name)
        );
    }

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 2,
            },
        )
        .unwrap();
        d.add_track().unwrap();
        for i in 0..d.track_count() {
            let r = NoteRecord {
                start_tick: 100 * i as u64,
                duration_ticks: 50,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(0),
                group: 0,
            };
            d.track_mut(i).unwrap().notes.insert(r, i != 1).unwrap();
        }
        d
    }

    /// V09-S35: the ruler's time selection reaches the tools (Riff Machine
    /// n07) on their own axis — an opened clip's content ticks — ordered,
    /// cut at the clip's tick 0, none when empty or wholly before it.
    #[test]
    fn the_time_selection_reaches_tools_on_their_axis() {
        assert_eq!(local_range(None, 0), None);
        assert_eq!(local_range(Some((960, 3840)), 0), Some((960, 3840)));
        assert_eq!(local_range(Some((3840, 960)), 480), Some((480, 3360)));
        assert_eq!(local_range(Some((960, 3840)), 1920), Some((0, 1920)));
        assert_eq!(local_range(Some((960, 3840)), -960), Some((1920, 4800)));
        assert_eq!(local_range(Some((960, 1920)), 1920), None);
        assert_eq!(local_range(Some((960, 960)), 0), None);
    }

    /// v0.9 §118: Slice Notes cuts at a time selection's start and end, else
    /// at the edit cursor, and runs the one-click kernel on the active
    /// track's selection the way `quick_tool` does — a top-level track or a
    /// subtrack alike; a track without selected notes has nothing in scope.
    #[test]
    fn slice_notes_cuts_at_the_time_selection_or_the_cursor_on_any_track() {
        assert_eq!(slice_cuts(None, 225), vec![225]);
        assert_eq!(slice_cuts(Some((110, 330)), 225), vec![110, 330]);
        assert_eq!(slice_cuts(None, u64::MAX), vec![(u64::MAX >> 3) as i64]);
        let mut d = doc();
        let def = resolve("slice_notes").expect("Slice Notes kernel");
        let spec = (def.spec)();
        let run = |d: &PlumDocument, ti: usize, at: Vec<i64>| {
            let mut values = ParamSet::defaults_in(&spec.params, d.project().keys);
            values.set("at", ParamValue::List(at));
            let scopes = scopes_for(d, ti, ToolScope::Selected);
            let kctx = ctx_for(d, None, 240, 0);
            (scopes.len(), compute(d, def, &values, &scopes, &kctx, None))
        };
        // Track 0 (parent, note 0–50) and subtrack 2 (note 200–250).
        for (ti, at) in [(0usize, 25u64), (2, 225)] {
            let (n, r) = run(&d, ti, slice_cuts(None, at));
            assert_eq!(n, 1);
            let r = r.unwrap();
            assert_eq!(r.notes.len(), 2, "track {ti}");
            let mut h = History::default();
            let op = build_operation(&mut d, &r, "Slice Notes").unwrap();
            h.apply(&mut d, op).unwrap();
            assert_eq!(d.track(ti).unwrap().notes.len(), 2);
            h.undo(&mut d).unwrap();
            assert_eq!(d.track(ti).unwrap().notes.len(), 1);
        }
        // Subtrack 1 has no selected note: nothing is in scope.
        assert_eq!(run(&d, 1, slice_cuts(None, 125)).0, 0);
        // A time selection around the note: it is not crossed, unchanged.
        let (_, r) = run(&d, 2, slice_cuts(Some((150, 300)), 0));
        let r = r.unwrap();
        assert!(matches!(
            build_operation(&mut d, &r, "Slice Notes"),
            Err(TransformError::Unchanged)
        ));
        assert_eq!(
            r.message.as_deref(),
            Some("No selected note crosses the cut position.")
        );
    }

    /// v0.9 §082: Duplicate Up / Down One Octave runs the one-click kernel on
    /// the active track's selection the way `quick_tool` does — a top-level
    /// track or a subtrack alike — and commits in one undo step.
    #[test]
    fn duplicate_one_octave_runs_on_any_tracks_selection() {
        let mut d = doc();
        let def = resolve("duplicate_octave").expect("Duplicate One Octave kernel");
        let spec = (def.spec)();
        for (ti, semis, want) in [(0usize, 12i64, 72u8), (2, -12, 48)] {
            let mut values = ParamSet::defaults_in(&spec.params, d.project().keys);
            values.set("semis", ParamValue::Int(semis));
            let scopes = scopes_for(&d, ti, ToolScope::Selected);
            assert_eq!(scopes.len(), 1);
            let kctx = ctx_for(&d, None, 240, 0);
            let r = compute(&d, def, &values, &scopes, &kctx, None).unwrap();
            let mut h = History::default();
            let op = build_operation(&mut d, &r, "Duplicate Up One Octave").unwrap();
            h.apply(&mut d, op).unwrap();
            let mut pitches: Vec<u8> = d
                .track(ti)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.attrs.pitch())
                .collect();
            pitches.sort_unstable();
            let mut expect = vec![60, want];
            expect.sort_unstable();
            assert_eq!(pitches, expect, "track {ti}");
            h.undo(&mut d).unwrap();
            assert_eq!(d.track(ti).unwrap().notes.len(), 1);
        }
        // Subtrack 1 has no selected note: nothing is in scope.
        assert!(scopes_for(&d, 1, ToolScope::Selected).is_empty());
    }

    #[test]
    fn precise_tool_preset_preview_matches_apply_and_one_undo() {
        let mut d = doc();
        let def = resolve("stretch").unwrap();
        let spec = (def.spec)();
        let mut values = ParamSet::defaults_in(&spec.params, d.project().keys);
        let factor = crate::ui::numeric::float("123.456789012345").unwrap();
        values.set("factor", ParamValue::Float(factor));
        let dir = std::env::temp_dir().join(format!("plum-b2-precision-{}", std::process::id()));
        preset_files::save(&dir, "stretch", "precise", &values).unwrap();
        let (reopened, skipped) =
            preset_files::load(&dir, "precise", "stretch", &spec.params).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(
            reopened.values.get_raw("factor"),
            Some(&ParamValue::Float(factor))
        );
        let scopes = scopes_for(&d, 0, ToolScope::Selected);
        let kctx = ctx_for(&d, None, 240, 0);
        let preview = compute(&d, def, &reopened.values, &scopes, &kctx, None).unwrap();
        let before: Vec<_> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect();
        let expected: Vec<_> = preview
            .track_notes(0)
            .iter()
            .map(|n| (n.start, n.dur))
            .collect();
        let op = build_operation(&mut d, &preview, "Stretch").unwrap();
        let mut h = History::default();
        h.apply(&mut d, op).unwrap();
        let applied: Vec<_> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.duration_ticks))
            .collect();
        assert_eq!(applied, expected);
        assert_eq!(
            applied,
            vec![(0, 62)],
            "time output rounds to whole MIDI ticks"
        );
        h.undo(&mut d).unwrap();
        assert_eq!(
            d.track(0)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect::<Vec<_>>(),
            before
        );
        assert!(!matches!(h.undo(&mut d), Ok(Some(_))));
        h.redo(&mut d).unwrap();
        assert_eq!(
            d.track(0)
                .unwrap()
                .notes
                .iter()
                .next()
                .unwrap()
                .duration_ticks,
            62
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B1-0078: Note Properties' Ranges preview with the kernel they apply
    /// with and commit in one undo step; Undo restores every note.
    #[test]
    fn ranges_preview_then_apply_in_one_undo_step() {
        use plumspirit_engine::editor::ranges::{self, Blend, RangeRow, Target};
        let mut d = doc();
        let def = resolve("note_props").expect("Note Properties");
        let spec = (def.spec)();
        let mut values = ParamSet::defaults_in(&spec.params, d.project().keys);
        let rows = vec![
            RangeRow {
                from: 30,
                to: 30,
                ..RangeRow::new(Target::Velocities)
            },
            RangeRow {
                from: 5,
                to: 5,
                blend: Blend::Add,
                ..RangeRow::new(Target::Transpose)
            },
        ];
        values.set("n13", ParamValue::Bool(true));
        values.set("n13.rows", ParamValue::Text(ranges::to_text(&rows)));
        let notes = |d: &PlumDocument| -> Vec<(u64, u8, u8)> {
            d.track(0)
                .unwrap()
                .notes
                .iter()
                .map(|n| (n.start_tick, n.attrs.pitch(), n.attrs.velocity()))
                .collect()
        };
        let before = notes(&d);
        let scopes = scopes_for(&d, 0, ToolScope::Selected);
        let kctx = ctx_for(&d, None, 240, 0);
        let preview = compute(&d, def, &values, &scopes, &kctx, None).unwrap();
        let again = compute(&d, def, &values, &scopes, &kctx, None).unwrap();
        assert_eq!(preview.notes, again.notes);
        assert_eq!(notes(&d), before, "Preview never mutates");
        assert!(
            preview
                .summary()
                .contains("Ranges: 1 note(s) changed by 2 range(s)."),
            "{}",
            preview.summary()
        );
        let mut h = History::default();
        let op = build_operation(&mut d, &preview, "Note Properties").unwrap();
        h.apply(&mut d, op).unwrap();
        assert_eq!(notes(&d), vec![(0, 65, 30)]);
        h.undo(&mut d).unwrap();
        assert_eq!(notes(&d), before);
        assert!(!matches!(h.undo(&mut d), Ok(Some(_))), "one undo step");
    }

    /// B1-0077: Note Properties' math expressions preview with the kernel
    /// they apply with (the same result again, the document untouched) and
    /// commit in one undo step; Undo restores every note.
    #[test]
    fn math_expressions_preview_then_apply_in_one_undo_step() {
        let mut d = doc();
        let def = resolve("note_props").expect("Note Properties");
        let spec = (def.spec)();
        let mut values = ParamSet::defaults_in(&spec.params, d.project().keys);
        values.set("n12", ParamValue::Bool(true));
        values.set("n12.v", ParamValue::Text("v / 2 + index".into()));
        values.set("n12.k", ParamValue::Text("math.max(k + 12, 0)".into()));
        values.set("n12.c", ParamValue::Text("(k % 16) + 1".into()));
        let notes = |d: &PlumDocument| -> Vec<(u64, u8, u8, u8)> {
            d.track(0)
                .unwrap()
                .notes
                .iter()
                .map(|n| {
                    (
                        n.start_tick,
                        n.attrs.pitch(),
                        n.attrs.velocity(),
                        n.attrs.channel(),
                    )
                })
                .collect()
        };
        let before = notes(&d);
        let scopes = scopes_for(&d, 0, ToolScope::Selected);
        let kctx = ctx_for(&d, None, 240, 0);
        let preview = compute(&d, def, &values, &scopes, &kctx, None).unwrap();
        let again = compute(&d, def, &values, &scopes, &kctx, None).unwrap();
        assert_eq!(preview.notes, again.notes);
        assert_eq!(notes(&d), before, "Preview never mutates");
        assert!(
            preview
                .summary()
                .contains("Math expressions: 1 note(s) changed."),
            "{}",
            preview.summary()
        );
        let mut h = History::default();
        let op = build_operation(&mut d, &preview, "Note Properties").unwrap();
        h.apply(&mut d, op).unwrap();
        assert_eq!(notes(&d), vec![(0, 72, 50, 12)]);
        h.undo(&mut d).unwrap();
        assert_eq!(notes(&d), before);
        assert!(!matches!(h.undo(&mut d), Ok(Some(_))), "one undo step");
    }

    #[test]
    fn scopes_follow_the_choice_and_skip_tracks_without_notes_in_scope() {
        let d = doc();
        let idx = |v: Vec<TrackScope>| v.iter().map(|s| (s.index, s.all)).collect::<Vec<_>>();
        assert_eq!(
            idx(scopes_for(&d, 0, ToolScope::Selected)),
            vec![(0, false)]
        );
        // Track 1 (a subtrack) has no selected note.
        assert_eq!(
            idx(scopes_for(&d, 0, ToolScope::SelectedSubtree)),
            vec![(0, false), (2, false)]
        );
        assert_eq!(
            idx(scopes_for(&d, 0, ToolScope::Subtree)),
            vec![(0, true), (1, true), (2, true)]
        );
        assert_eq!(idx(scopes_for(&d, 3, ToolScope::Track)), vec![(3, true)]);
        assert_eq!(scopes_for(&d, 0, ToolScope::Project).len(), 4);
        assert_eq!(scopes_for(&d, 0, ToolScope::SelectedEverywhere).len(), 3);
    }

    /// v0.9 256-Key Mode (S21 step 4b): a tool window offers the project's
    /// keys — Transpose ±255 semitones and its Range low / high pitches 0–255
    /// in a 256-key project, ±127 / 0–127 in a standard one — and its
    /// defaults come from `ParamSet::defaults_in`.
    #[test]
    fn tool_windows_offer_the_projects_key_range() {
        let def = plumspirit_engine::transform::tool("transpose").unwrap();
        let spec = (def.spec)();
        let p = |k: &str| spec.params.iter().find(|p| p.key == k).unwrap();
        for (keys, top) in [(KeyRange::Standard, 127i64), (KeyRange::Extended256, 255)] {
            assert_eq!(value_range(p("semis"), keys), (-top, top));
            assert_eq!(value_range(p("a8.lo"), keys), (0, top));
            assert_eq!(value_range(p("a6.depth"), keys), (-top, top));
            let s = ToolSession::new(def, keys);
            assert_eq!(s.keys, keys);
            // The tool's own parameters and the mandatory functions' (§098).
            assert_eq!(s.values, ParamSet::defaults_in(&s.definition.specs(), keys));
            assert!(
                spec.params
                    .iter()
                    .all(|p| s.values.get_raw(p.key).is_some())
            );
        }
        // Non-keyed ranges never move.
        let r = value_range(p("a7.deg"), KeyRange::Extended256);
        assert_eq!(r, (-70, 70));
        // A transpose of 150 runs in a 256-key project (60 → 210).
        let mut d = doc();
        d.project_mut().keys = KeyRange::Extended256;
        let mut v = ParamSet::defaults_in(&spec.params, KeyRange::Extended256);
        v.set("semis", ParamValue::Int(150));
        let c = ctx_for(&d, None, 240, 0);
        let sc = scopes_for(&d, 0, ToolScope::Selected);
        let r = compute(&d, def, &v, &sc, &c, None).unwrap();
        assert!(r.notes.iter().all(|n| n.pitch > 127), "{:?}", r.notes);
    }

    #[test]
    fn tools_compute_through_the_app_context_and_names_are_sanitized() {
        let d = doc();
        let def = plumspirit_engine::transform::tool("transpose").unwrap();
        let mut v = ParamSet::defaults(&(def.spec)().params);
        v.set("semis", ParamValue::Int(5));
        let c = ctx_for(&d, None, 240, 0);
        assert_eq!(c.ppq, 960);
        let sc = scopes_for(&d, 0, ToolScope::Selected);
        let r = compute(&d, def, &v, &sc, &c, None).unwrap();
        assert_eq!(r.notes.len(), 1);
        assert_eq!(r.notes[0].pitch, 65);
        assert_eq!(sanitize_file_name("a/b:c?.."), "a_b_c_");
    }

    /// Renders a tool window's controls headlessly (every section open) at
    /// `ppp` and returns what it drew, in order.
    fn render_window(s: &mut ToolSession, ppp: f32) -> Vec<&'static str> {
        render_window_with(s, ppp, |_| {})
    }

    /// [`render_window`] after `setup` prepared the context.
    fn render_window_with(
        s: &mut ToolSession,
        ppp: f32,
        setup: impl Fn(&egui::Context),
    ) -> Vec<&'static str> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN_ALL), true));
        setup(&ctx);
        let colors = [egui::Color32::RED, egui::Color32::GREEN];
        let tracks = ["1 · Piano".to_owned(), "2 · Bass".to_owned()];
        let ectx = crate::ui::param_editors::EditorCtx {
            sub_colors: &colors,
            tracks: &tracks,
            keys: s.keys,
        };
        let mut drawn = Vec::new();
        // The second frame draws the opened sections' bodies.
        for _ in 0..2 {
            ctx.data_mut(|d| d.remove::<Vec<&'static str>>(egui::Id::new(RENDERED)));
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 4000.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    params_ui(ui, s, &ectx);
                    order_ui(ui, &s.definition, &s.values);
                });
            });
            out.textures_delta.clear();
            drawn = ctx
                .data(|d| d.get_temp::<Vec<&'static str>>(egui::Id::new(RENDERED)))
                .unwrap_or_default();
        }
        drawn
    }

    /// v0.9 §208: every tool window is generated from its definition — with
    /// every function switched on, each parameter gets exactly one control,
    /// in the definition's order (base, functions, boundaries), at two
    /// display scales and in both key ranges; Legacy functions sit under
    /// "Legacy Advanced", Active ones under "Advanced".
    #[test]
    fn every_tool_window_is_generated_from_its_definition() {
        for def in plumspirit_engine::transform::registry() {
            for (keys, ppp) in [(KeyRange::Standard, 1.0), (KeyRange::Extended256, 1.5)] {
                let mut s = ToolSession::new(def, keys);
                for f in &s.definition.functions {
                    s.values.set(f.switch, ParamValue::Bool(true));
                }
                let drawn = render_window(&mut s, ppp);
                let d = &s.definition;
                let mut want: Vec<&'static str> = d.base_params().map(|p| p.spec.key).collect();
                for status in [FunctionStatus::Active, FunctionStatus::Legacy] {
                    let fs: Vec<_> = d.functions.iter().filter(|f| f.status == status).collect();
                    if !fs.is_empty() {
                        want.push(match status {
                            FunctionStatus::Active => "Advanced",
                            FunctionStatus::Legacy => "Legacy Advanced",
                        });
                    }
                    for f in fs {
                        if f.switch == plumspirit_engine::transform::functions::SWEEP {
                            // One target at a time (the first, until another is chosen).
                            want.push("Sweep target");
                            want.extend([d.sweeps[0].on, d.sweeps[0].graph]);
                            continue;
                        }
                        want.extend(d.function_params(f.n).map(|p| p.spec.key));
                    }
                }
                // B2-F01: the Execution Order section (tools with a group).
                if !d.groups().is_empty() {
                    want.push("Execution Order");
                }
                want.extend(d.boundary_params().map(|p| p.spec.key));
                // §236: with two or more functions on, the Preview's order line.
                if d.functions.len() >= 2 {
                    want.push("Order");
                }
                assert_eq!(drawn, want, "{} at {ppp}", def.id);
                // Every parameter except the function switches (drawn in the
                // section headers) and the other sweep targets appears.
                let hidden = 2 * d.sweeps.len().saturating_sub(1);
                let all: usize = d.params.len() - d.functions.len() - hidden;
                let headings = drawn
                    .iter()
                    .filter(|w| {
                        w.contains("Advanced")
                            || **w == "Sweep target"
                            || **w == "Order"
                            || **w == "Execution Order"
                    })
                    .count();
                assert_eq!(drawn.len() - headings, all, "{}", def.id);
            }
        }
        // Slice (one click since v0.9): its v0.8 functions are Legacy.
        let mut s = ToolSession::new(
            plumspirit_engine::transform::tool("slice").unwrap(),
            KeyRange::Standard,
        );
        let drawn = render_window(&mut s, 1.0);
        assert!(drawn.contains(&"Legacy Advanced"));
        assert!(!drawn.contains(&"Advanced"));
    }

    /// v0.9 §235 / §236 (V09-S33): the Preview's order line names the
    /// functions that are on in the order they run — the Control Sweep, the
    /// tool's own functions in kernel order (a demoted function keeps its
    /// place and is marked Legacy), then the drivers — and is absent with
    /// fewer than two on.
    #[test]
    fn the_preview_shows_the_order_of_the_functions_that_are_on() {
        use plumspirit_engine::transform::definition::{LegacyTable, definition_in};
        static DEMOTED: LegacyTable = LegacyTable {
            tools: &[],
            functions: &[("transpose", "a03")],
        };
        let tool = plumspirit_engine::transform::tool("transpose").unwrap();
        let mut s = ToolSession::new(tool, KeyRange::Standard);
        s.definition = definition_in(tool, &DEMOTED);
        let pitch = s.definition.function("m_pitch_offset").unwrap().n;
        let sweep = s.definition.function("m_sweep").unwrap().n;
        s.values.set("a3", ParamValue::Bool(true));
        assert_eq!(order_line(&s.definition, &s.values), None);
        assert!(!render_window(&mut s, 1.0).contains(&"Order"));
        for k in ["m.pitch", "a1", "m.sweep"] {
            s.values.set(k, ParamValue::Bool(true));
        }
        assert_eq!(
            order_line(&s.definition, &s.values).unwrap(),
            format!("Order: {sweep} → 1 → 3 (Legacy) → {pitch}")
        );
        assert_eq!(
            order_details(&s.definition, &s.values),
            format!(
                "Applied in this order:\n{sweep} · Control Sweep\n1 · Scale-degree transpose\n\
                 3 · Pitch-axis inversion (Legacy)\n{pitch} · Pitch Offset Driver"
            )
        );
        let drawn = render_window(&mut s, 1.0);
        assert_eq!(drawn.last(), Some(&"Order"));
        assert!(drawn.contains(&"Legacy Advanced"));
    }

    /// v1.0-beta2 B2-F01: the Execution Order section lists Transpose's
    /// groups; a move writes every group's order into the values, the order
    /// line and the run follow it, the selected preset reads "(changed)",
    /// Reset Order brings back the declared order.
    #[test]
    fn the_execution_order_section_moves_functions_and_marks_the_preset_changed() {
        use plumspirit_engine::transform::definition::ORDER_KEY;
        let tool = plumspirit_engine::transform::tool("transpose").unwrap();
        let mut s = ToolSession::new(tool, KeyRange::Standard);
        for k in ["n06", "n08"] {
            s.values.set(k, ParamValue::Bool(true));
        }
        let n06 = s.definition.function("n06").unwrap().n;
        let n08 = s.definition.function("n08").unwrap().n;
        assert!(render_window(&mut s, 1.0).contains(&"Execution Order"));
        assert_eq!(
            order_line(&s.definition, &s.values).unwrap(),
            format!("Order: {n08} → {n06}")
        );
        // A preset as loaded: unchanged.
        s.presets.selected = Some("Mine".into());
        s.presets.saved = Some(s.values.clone());
        // n06 moves up past n09 and n08 (two presses).
        for _ in 0..2 {
            let o = s.definition.moved_order(&s.values, "n06", -1).unwrap();
            s.values.set(ORDER_KEY, ParamValue::Text(o));
        }
        assert_eq!(
            s.values.get_raw(ORDER_KEY),
            Some(&ParamValue::Text(
                "n01,n02,n06,n08,n09,m_pitch_offset,m_sub_offset,m_overlaps".into()
            ))
        );
        assert_eq!(
            order_line(&s.definition, &s.values).unwrap(),
            format!("Order: {n06} → {n08}")
        );
        assert!(s.presets.saved.as_ref() != Some(&s.values));
        render_window(&mut s, 1.0);
        // The first member cannot move up; an id outside the groups never.
        let first = s.definition.moved_order(&s.values, "n01", -1);
        assert_eq!(first, None);
        assert_eq!(s.definition.moved_order(&s.values, "n04", 1), None);
        // Switching a member off and on keeps its slot.
        s.values.set("n06", ParamValue::Bool(false));
        s.values.set("n06", ParamValue::Bool(true));
        assert_eq!(
            order_line(&s.definition, &s.values).unwrap(),
            format!("Order: {n06} → {n08}")
        );
        s.values.remove(ORDER_KEY);
        assert_eq!(
            order_line(&s.definition, &s.values).unwrap(),
            format!("Order: {n08} → {n06}")
        );
    }

    /// B2-F01: Alt+Up / Alt+Down on a focused Execution Order row move its
    /// function; the row keeps the focus, so the next press moves it again.
    #[test]
    fn alt_up_and_down_move_the_focused_execution_order_row() {
        use plumspirit_engine::transform::definition::ORDER_KEY;
        let tool = plumspirit_engine::transform::tool("transpose").unwrap();
        let mut s = ToolSession::new(tool, KeyRange::Standard);
        for k in ["n06", "n08"] {
            s.values.set(k, ParamValue::Bool(true));
        }
        let ctx = egui::Context::default();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN_ALL), true));
        let colors = [egui::Color32::RED];
        let tracks = ["1 · Piano".to_owned()];
        let ectx = crate::ui::param_editors::EditorCtx {
            sub_colors: &colors,
            tracks: &tracks,
            keys: s.keys,
        };
        let key = |k: egui::Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::ALT,
        };
        let frame = |s: &mut ToolSession, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 4000.0),
                )),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| params_ui(ui, s, &ectx));
            });
            out.textures_delta.clear();
        };
        frame(&mut s, Vec::new());
        frame(&mut s, Vec::new());
        let row = ctx
            .data(|d| d.get_temp::<egui::Id>(egui::Id::new(("order-row-id", "n06"))))
            .expect("the n06 row is drawn");
        ctx.memory_mut(|m| m.request_focus(row));
        frame(&mut s, Vec::new());
        frame(&mut s, vec![key(egui::Key::ArrowUp)]);
        frame(&mut s, vec![key(egui::Key::ArrowUp)]);
        let seq = s.definition.plan(&s.values).unwrap();
        assert_eq!(
            seq.sequence("pitch").unwrap(),
            ["n01", "n02", "n06", "n08", "n09"],
            "{:?}",
            s.values.get_raw(ORDER_KEY)
        );
        frame(&mut s, vec![key(egui::Key::ArrowDown)]);
        let seq = s.definition.plan(&s.values).unwrap();
        assert_eq!(
            seq.sequence("pitch").unwrap(),
            ["n01", "n02", "n08", "n06", "n09"]
        );
    }

    /// V09-S15 (§094): the Track List Paste Repeat dialog's ten functions
    /// are drawn by the tool windows' rows — with every switch on, each
    /// control of `editor::clip_repeat::params()` exactly once, in order, at
    /// two display scales and in both key ranges (the lists with their
    /// semantic editors).
    #[test]
    fn the_clip_paste_repeat_functions_are_drawn_with_the_tool_window_rows() {
        use plumspirit_engine::editor::clip_repeat::{FUNCTIONS, TOOL_ID};
        use plumspirit_engine::transform::editors::Editor;
        let defs = crate::app::clip_repeat_ui::param_defs();
        let editor = |k: &str| defs.iter().find(|p| p.spec.key == k).unwrap().editor;
        assert!(matches!(editor("a5.seq"), Editor::Intervals(_)));
        assert!(matches!(
            editor("a6.list"),
            Editor::Subchannels { set: false }
        ));
        for (keys, ppp) in [(KeyRange::Standard, 1.0), (KeyRange::Extended256, 1.5)] {
            let mut values = ParamSet::new();
            for k in &plumspirit_engine::transform::param::ADV_KEYS[..10] {
                values.set(k, ParamValue::Bool(true));
            }
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN_ALL), true));
            let colors = [egui::Color32::RED, egui::Color32::GREEN];
            let ectx = crate::ui::param_editors::EditorCtx {
                sub_colors: &colors,
                tracks: &[],
                keys,
            };
            let mut drawn = Vec::new();
            for _ in 0..2 {
                ctx.data_mut(|d| d.remove::<Vec<&'static str>>(egui::Id::new(RENDERED)));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(420.0, 6000.0),
                    )),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        function_sections(ui, TOOL_ID, &FUNCTIONS, defs, &mut values, &ectx)
                    });
                });
                out.textures_delta.clear();
                drawn = ctx
                    .data(|d| d.get_temp::<Vec<&'static str>>(egui::Id::new(RENDERED)))
                    .unwrap_or_default();
            }
            // Every control (`aN.…`) once; the switches are the headers.
            let want: Vec<&'static str> = defs
                .iter()
                .map(|p| p.spec.key)
                .filter(|k| k.contains('.'))
                .collect();
            assert_eq!(drawn, want, "at {ppp}");
        }
    }

    /// §210: the Control Sweep shows one target's switch and graph; another
    /// target chosen in the menu shows that one's.
    #[test]
    fn the_control_sweep_section_shows_one_chosen_target_at_a_time() {
        let def = plumspirit_engine::transform::tool("transpose").unwrap();
        let mut s = ToolSession::new(def, KeyRange::Standard);
        for f in &s.definition.functions {
            s.values.set(f.switch, ParamValue::Bool(true));
        }
        let targets = s.definition.sweeps;
        assert_eq!(targets.len(), 10);
        let drawn = render_window(&mut s, 1.0);
        assert!(drawn.contains(&targets[0].graph));
        assert!(!drawn.contains(&targets[3].graph));
        // The menu stores its choice per tool; the fourth target is shown.
        let drawn = render_window_with(&mut s, 1.0, |ctx| {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(("sweep-target", "transpose")), 3usize));
        });
        assert!(drawn.contains(&targets[3].on) && drawn.contains(&targets[3].graph));
        assert!(!drawn.contains(&targets[0].graph));
    }

    /// The stroke tools capture the pointer as their definitions say.
    #[test]
    fn stroke_capture_and_auto_apply_follow_the_definition() {
        use crate::piano_roll::CaptureMode;
        let open = |id| {
            ToolSession::new(
                plumspirit_engine::transform::tool(id).unwrap(),
                KeyRange::Standard,
            )
        };
        for id in ["draw", "paint"] {
            let s = open(id);
            assert_eq!(s.capture_mode(), Some(CaptureMode::Freehand), "{id}");
            assert!(s.auto_apply, "{id}");
        }
        let mut s = open("select");
        assert!(!s.auto_apply);
        assert_eq!(s.capture_mode(), None);
        s.values.set("a1", ParamValue::Bool(true));
        assert_eq!(s.capture_mode(), Some(CaptureMode::Freehand));
        s.values.set("a2", ParamValue::Bool(true));
        assert_eq!(s.capture_mode(), Some(CaptureMode::Polygon));
        let s = open("shear");
        assert_eq!((s.capture_mode(), s.auto_apply), (None, false));
    }

    /// V09-S14 (§092, §093): Ctrl+B in the Piano Roll opens Paste Repeat
    /// (notes) on the selected notes; with a function switched on (2,
    /// Cumulative Pitch Vector) Apply is one undo step whose copies climb by
    /// the step, and Undo restores the document exactly.
    #[test]
    fn paste_repeat_notes_with_a_function_applies_in_one_undo_step() {
        use crate::app::commands::Command;
        use crate::app::test_support::Harness;
        let mut d = PlumDocument::new_project("chord".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = [60u8, 64, 67]
            .iter()
            .map(|&p| NoteRecord {
                start_tick: 0,
                duration_ticks: 960,
                attrs: NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(0),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut h = Harness::new(d);
        h.set_notes(0);
        let fp = h.doc().state_fingerprint();
        h.chord(Command::PasteRepeat);
        let s = h.app.tool.as_mut().expect("the tool window opens");
        assert_eq!(s.def.id, "paste_repeat_notes");
        let f2 = s
            .definition
            .functions
            .iter()
            .find(|f| f.n == 2)
            .unwrap()
            .switch;
        s.values.set("count", ParamValue::Int(2));
        s.values.set(f2, ParamValue::Bool(true));
        s.values.set("a2.step", ParamValue::Int(12));
        let ctx = h.ctx.clone();
        h.app.apply_tool(&ctx);
        let mut got: Vec<(u64, u8)> = h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.attrs.pitch()))
            .collect();
        got.sort_unstable();
        let mut want = Vec::new();
        for k in 0..3u64 {
            for p in [60u8, 64, 67] {
                want.push((k * 960, p + 12 * k as u8));
            }
        }
        want.sort_unstable();
        assert_eq!(got, want, "{}", h.status());
        assert_eq!(
            h.app.open.as_ref().unwrap().history.undo_label(),
            Some("Duplicate V2")
        );
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;

    #[test]
    fn displacement_maps_decode_tiff_and_png_pictures_once_and_name_bad_files() {
        let dir = std::env::temp_dir().join(format!("ps-dmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::from_raw(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]).unwrap();
        let tif = dir.join("map.tif");
        img.save_with_format(&tif, image::ImageFormat::Tiff)
            .unwrap();
        let png = dir.join("map.png");
        img.save_with_format(&png, image::ImageFormat::Png).unwrap();
        let def = plumspirit_engine::transform::tool("displacement_map").unwrap();
        let mut v = ParamSet::defaults(&(def.spec)().params);
        // A track source: nothing is decoded.
        v.set("picture", ParamValue::Text(tif.display().to_string()));
        assert!(picture_for("displacement_map", &v).is_none());
        v.set("source", ParamValue::Choice(3));
        for path in [&tif, &png] {
            v.set("picture", ParamValue::Text(path.display().to_string()));
            let pic = picture_for("displacement_map", &v).unwrap().unwrap();
            assert_eq!(
                (pic.w, pic.h, pic.at(0, 0), pic.at(1, 0)),
                (2, 1, [0, 0, 0, 255], [255, 255, 255, 255])
            );
            // Decoded once: the cached picture comes back.
            let again = picture_for("displacement_map", &v).unwrap().unwrap();
            assert!(Arc::ptr_eq(&pic, &again));
        }
        let bad = dir.join("map.xyz");
        std::fs::write(&bad, b"not a picture").unwrap();
        v.set("picture", ParamValue::Text(bad.display().to_string()));
        let e = picture_for("displacement_map", &v).unwrap().unwrap_err();
        assert!(e.contains("PNG, JPEG, BMP, GIF, WebP and TIFF"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tumours_mask_pictures_are_decoded_only_for_the_picture_mask() {
        let dir = std::env::temp_dir().join(format!("ps-tum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::from_raw(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]).unwrap();
        let png = dir.join("mask.png");
        img.save_with_format(&png, image::ImageFormat::Png).unwrap();
        let def = plumspirit_engine::transform::tool("tumours").unwrap();
        let mut v = ParamSet::defaults(&(def.spec)().params);
        v.set(
            "c24_mask_boundary_constraint.picture",
            ParamValue::Text(png.display().to_string()),
        );
        // The mask off, or another mask mode: nothing is decoded.
        assert!(picture_for("tumours", &v).is_none());
        v.set("c24_mask_boundary_constraint", ParamValue::Bool(true));
        assert!(picture_for("tumours", &v).is_none());
        v.set("c24_mask_boundary_constraint.mode", ParamValue::Choice(3));
        let pic = picture_for("tumours", &v).unwrap().unwrap();
        assert_eq!((pic.w, pic.h, pic.at(0, 0)), (2, 1, [255, 255, 255, 255]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B2-F16 (B2-S13): Track 1's clip moved onto Track 2 and edited there —
    /// Clipping Mask's "track below" is Track 3 (counted from the clip's
    /// track, never from its content's); moving the mask clip changes the
    /// preview's key (it is recomputed); Apply writes the clipped notes into
    /// the mask track's clip where the song shows the mask, in one undo step.
    #[test]
    fn clipping_mask_counts_from_the_clips_track_and_follows_a_mask_clip_move() {
        use crate::app::test_support::Harness;
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::editor::NoteTarget;
        use plumspirit_engine::model::{
            Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlacedLane, PlacedNotes, PlumDocument,
            SubchannelId,
        };
        let rec = |s: u64, d: u64, p: u8| NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        };
        let mut d = PlumDocument::new_project("mask".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        d.add_track().unwrap();
        let art = [rec(0, 2000, 60), rec(0, 2000, 61), rec(0, 2000, 62)];
        let mask = [rec(500, 1000, 60), rec(500, 300, 61), rec(1200, 300, 61)];
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&art, false)
            .unwrap();
        d.track_mut(2)
            .unwrap()
            .notes
            .insert_sorted_batch(&mask, false)
            .unwrap();
        let mut h = Harness::new(d);
        let (t0, t2) = (h.id(0), h.id(2));
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 1, 0)]));
        h.set_notes(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        h.app.open_tool("clip_mask");
        let ctx = h.ctx.clone();
        h.app.update_tool(&ctx);
        let before = h.app.tool.as_ref().expect("the tool window").key.clone();
        // The mask clip moves to song 1,000 (it covers [1,500, 2,500)).
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t2), 2, 1_000)]));
        h.set_notes(1);
        h.app.sync_clip_session();
        h.app.update_tool(&ctx);
        assert_ne!(
            h.app.tool.as_ref().expect("the tool window").key,
            before,
            "a moved mask clip invalidates the preview"
        );
        let fp = h.doc().state_fingerprint();
        h.app.apply_tool(&ctx);
        let song = |h: &Harness, i: usize| -> Vec<(u64, u64, u8)> {
            let lanes: Vec<PlacedLane> = h
                .doc()
                .placed_contents(i)
                .iter()
                .map(PlacedLane::of)
                .collect();
            let mut v: Vec<(u64, u64, u8)> = PlacedNotes::new(&lanes)
                .map(|r| (r.start_tick, r.duration_ticks, r.attrs.pitch()))
                .collect();
            v.sort_unstable();
            v
        };
        let got = song(&h, 2);
        for k in [(1_500, 500, 60), (1_500, 300, 61)] {
            assert!(got.contains(&k), "{k:?} in {got:?} ({})", h.status());
        }
        assert_eq!(got.len(), 3 + 2, "{got:?}");
        assert!(song(&h, 1).len() == 3, "the art stays");
        assert!(h.command(crate::app::commands::Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
    }

    /// B2-A02: an open tool window recomputes its preview when the clip it
    /// edits moves — the kernels read the song at the clip's place (tempo
    /// for millisecond parameters, markers, the loop and the time
    /// selection), so the edited clip's song shift is part of the key.
    #[test]
    fn a_tool_preview_follows_a_move_of_the_clip_it_edits() {
        use crate::app::test_support::Harness;
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::model::{
            Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
        };
        let mut d = PlumDocument::new_project("move".into(), Bpm::DEFAULT, 960);
        let rec = NoteRecord {
            start_tick: 0,
            duration_ticks: 960,
            attrs: NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        };
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec], false)
            .unwrap();
        let mut h = Harness::new(d);
        let t0 = h.id(0);
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 960)]));
        h.set_notes(0);
        h.app.sync_clip_session();
        h.app.open_tool("transpose");
        let ctx = h.ctx.clone();
        h.app.update_tool(&ctx);
        let before = h.app.tool.as_ref().expect("the tool window").key.clone();
        // Only the clip moves (the selection stays as it is).
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 7_680)]));
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_shift(), 7_680);
        assert!(h.app.selected_count() > 0);
        h.app.update_tool(&ctx);
        assert_ne!(
            h.app.tool.as_ref().expect("the tool window").key,
            before,
            "the moved clip's preview is recomputed"
        );
    }

    /// B2-A02: Transpose over the Subtree from a parent clip moved to song
    /// 7,680 reaches the subtrack's owned clip at song 8,000 (before, the
    /// subtrack's empty raw store left it out of the scope) on the edited
    /// notes' axis (frame 320), each result stored in its own clip's ticks,
    /// one undo step.
    #[test]
    fn subtree_scope_from_a_moved_clip_reaches_a_subtracks_owned_clip() {
        use crate::app::test_support::Harness;
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::editor::NoteTarget;
        use plumspirit_engine::editor::clips::add_clip;
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::{
            Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
        };
        let rec = |s: u64, p: u8| NoteRecord {
            start_tick: s,
            duration_ticks: 200,
            attrs: NoteAttrs::new(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        };
        let mut d = PlumDocument::new_project("sub".into(), Bpm::DEFAULT, 960);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(0, 60)], false)
            .unwrap();
        let mut hist = History::default();
        hist.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 1,
            },
        )
        .unwrap();
        let op = add_clip(&d, 1, 8_000, 960).unwrap();
        hist.apply(&mut d, op).unwrap();
        let c = d.clips().contents().last().expect("the owned clip").id;
        d.notes_of_mut(ContentRef::Owned(c))
            .unwrap()
            .insert_sorted_batch(&[rec(100, 50)], false)
            .unwrap();
        let mut h = Harness::new(d);
        let t0 = h.id(0);
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 7_680)]));
        h.set_track(0);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_shift(), 7_680);
        let (scopes, frames) =
            scopes_framed(h.doc(), h.app.edit_target(), ToolScope::Subtree, 7_680);
        assert_eq!(frames, vec![0, 320]);
        assert_eq!(scopes[0].target(), NoteTarget::Track(0));
        assert!(matches!(
            scopes[1].target(),
            NoteTarget::Content { host: 1, .. }
        ));
        let fp = h.doc().state_fingerprint();
        h.app.open_tool("transpose");
        let s = h.app.tool.as_mut().expect("the tool window");
        s.values.set("scope", ParamValue::Choice(2));
        s.values.set("semis", ParamValue::Int(1));
        let ctx = h.ctx.clone();
        h.app.apply_tool(&ctx);
        let pitches = |h: &Harness, host: ContentRef| -> Vec<(u64, u8)> {
            h.doc()
                .notes_of(host)
                .unwrap()
                .iter()
                .map(|n| (n.start_tick, n.attrs.pitch()))
                .collect()
        };
        assert_eq!(
            pitches(&h, ContentRef::Track(t0)),
            vec![(0, 61)],
            "{}",
            h.status()
        );
        assert_eq!(pitches(&h, ContentRef::Owned(c)), vec![(100, 51)]);
        assert!(h.command(crate::app::commands::Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
    }
}
