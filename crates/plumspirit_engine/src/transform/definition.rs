//! The Tool Definition System (v0.9 contract §095, §208, §209).
//!
//! One data-driven description of every registered tool and one-click
//! command: stable id, name, family, category, base parameters, advanced
//! functions by stable id with their status (Active / Legacy), parameter
//! schemas with their semantic editors, Control Sweep bindings, the Preview
//! recipe, the growth kinds the preflight sees, and the Subchannel, boundary,
//! seed, Undo and pointer policies.
//!
//! A definition is assembled from two sources and never duplicates either:
//! the tool's own declarative [`ToolSpec`] (parameters, advanced-function
//! names — written next to the tool's unique mathematics) and the declared
//! table below (what the kernels do with the platform: preview path, growth,
//! subchannels, pointer input). The table was measured from the kernels on a
//! minimal fixture and is kept honest by `tests/v09_tool_definitions.rs`: a
//! kernel that starts behaving differently fails until its row is changed
//! on purpose. The app generates every tool window from the definition, and
//! the schema tests (`problems`, the integration test) run on every
//! definition, so a new tool gets its UI, presets and tests from its
//! metadata.

use super::editors::{self, Editor};
use super::param::{
    ADV_KEYS, Curve, Keyed, MAX_CURVE_POINTS, ParamKind, ParamSet, ParamSpec, ParamValue, Params,
};
use super::preflight::Growth;
use super::registry::{Family, ToolDef, ToolSpec};
use crate::model::project::KeyRange;
use std::collections::HashSet;

/// Where a tool belongs (the catalog's categories, `docs/v0_9/TOOL_CATALOG.toml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// A Piano Roll tool (FL-style tools, note editors, brush and selection
    /// tools, geometry).
    PianoRoll,
    /// One of the original experimental tools.
    Experimental,
    /// A built-in plugin run as a tool (no advanced functions, upstream
    /// behaviour).
    Plugin,
    /// A tool that became a one-click action; its window stays for old
    /// presets and its functions are Legacy.
    OneClick,
    /// A kernel behind a one-click command (no window, never listed).
    Command,
    /// v0.9 §088 (V09-S13): Paste Repeat (its own functions, §092 – §094;
    /// not one of the mandatory-function categories).
    PasteRepeat,
    /// v0.9 §109 / §113 (V09-S33): a Piano Roll tool that was replaced but
    /// stays usable (old presets and projects), in a clearly marked Legacy
    /// section. Moving a tool here changes its category only (§112).
    Legacy,
    /// v0.9 §113 (V09-S33): an Experimental tool moved to Legacy (S42).
    LegacyExperimental,
    /// v0.9 §151 (V09-S51): a Simple Note Generator (its 25 functions are
    /// the contract's; no mandatory functions).
    GeneratorSimple,
    /// v0.9 §151 (V09-S51): an Advanced Note Generator.
    GeneratorAdvanced,
}

impl Category {
    /// The catalog key (`category = "…"`).
    pub fn key(self) -> &'static str {
        match self {
            Category::PianoRoll => "piano-roll",
            Category::Experimental => "experimental",
            Category::Plugin => "plugin",
            Category::OneClick => "one-click",
            Category::Command => "command",
            Category::PasteRepeat => "paste-repeat",
            Category::Legacy => "legacy",
            Category::LegacyExperimental => "legacy-experimental",
            Category::GeneratorSimple => "generator-simple",
            Category::GeneratorAdvanced => "generator-advanced",
        }
    }

    /// The label of a marked Legacy section ("Legacy" / "Legacy
    /// Experimental", §113); `None` for the other categories, which are
    /// listed by family.
    pub fn legacy_label(self) -> Option<&'static str> {
        match self {
            Category::Legacy => Some("Legacy"),
            Category::LegacyExperimental => Some("Legacy Experimental"),
            _ => None,
        }
    }

    /// Whether this is one of the Legacy tool categories.
    pub fn is_legacy(self) -> bool {
        self.legacy_label().is_some()
    }

    /// Whether tools of this category get the platform's mandatory
    /// functions (§096). A tool moved to Legacy keeps them: moving changes
    /// the category only (§112), so its presets keep every setting.
    pub fn has_mandatory(self) -> bool {
        matches!(
            self,
            Category::PianoRoll
                | Category::Experimental
                | Category::Legacy
                | Category::LegacyExperimental
        )
    }
}

/// The declared Legacy table (v0.9 §109–§114, §237): whole tools moved to
/// Legacy and single functions demoted to Legacy Advanced, by stable id —
/// never a menu index, order or display name. It agrees with
/// `docs/v0_9/LEGACY_MAP.toml` (`tests/v09_tool_definitions.rs`): whole-tool
/// `[[legacy]]` entries are the One-Click tools plus [`LegacyTable::tools`],
/// entries naming a function are [`LegacyTable::functions`].
///
/// Moving changes the category (or the function's status) only: the id,
/// kernel, parameters, switches, preset keys and Preview stay what they
/// were.
#[derive(Clone, Copy, Debug)]
pub struct LegacyTable {
    /// Whole tools (registry ids): an Experimental tool becomes
    /// [`Category::LegacyExperimental`], any other Piano Roll tool
    /// [`Category::Legacy`].
    pub tools: &'static [&'static str],
    /// Single functions as (tool id, function id): shown under "Legacy
    /// Advanced"; their switch and keys never change.
    pub functions: &'static [(&'static str, &'static str)],
}

/// PlumSpirit's Legacy table. V09-S71 removes the two tools moved in S42;
/// no active whole-tool moves or function demotions remain.
pub const LEGACY: LegacyTable = LegacyTable {
    tools: &[],
    functions: &[],
};

impl LegacyTable {
    /// The category of tool `id` of `family` with this table.
    pub fn category(&self, id: &str, family: Family) -> Category {
        if super::tools::COMMANDS.iter().any(|c| c.id == id) {
            return Category::Command;
        }
        if let Some(c) = category_override(id) {
            return c;
        }
        let moved = self.tools.contains(&id);
        match family {
            Family::Experimental if moved => Category::LegacyExperimental,
            Family::Fl
            | Family::Editor
            | Family::Interactive
            | Family::Geometry
            | Family::Iterations
                if moved =>
            {
                Category::Legacy
            }
            Family::Fl
            | Family::Editor
            | Family::Interactive
            | Family::Geometry
            | Family::Iterations => Category::PianoRoll,
            Family::Experimental => Category::Experimental,
            Family::Plugin => Category::Plugin,
            Family::PasteRepeat => Category::PasteRepeat,
            Family::Generator if super::generator::is_advanced(id) => Category::GeneratorAdvanced,
            Family::Generator => Category::GeneratorSimple,
        }
    }

    /// The status of the own function `function` (stable id) of tool `id`
    /// in `category`: a One-Click tool's functions are all Legacy, others
    /// only when the table names them.
    pub fn function_status(&self, id: &str, function: &str, category: Category) -> FunctionStatus {
        if category == Category::OneClick || self.functions.contains(&(id, function)) {
            FunctionStatus::Legacy
        } else {
            FunctionStatus::Active
        }
    }

    /// Everything wrong with this table: entries that name no registered
    /// Piano Roll / Experimental tool or no own function of it, duplicates,
    /// a function of a tool that is Legacy as a whole (redundant). Empty
    /// when it is sound.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let base = |id: &str| {
            super::registry::tool(id).map(|t| (t, NO_LEGACY.category(id, (t.spec)().family)))
        };
        let mut seen = HashSet::new();
        for id in self.tools {
            if !seen.insert(*id) {
                out.push(format!("legacy tool {id} twice"));
            }
            match base(id) {
                Some((_, Category::PianoRoll | Category::Experimental)) => {}
                Some((_, c)) => out.push(format!("{id} ({}) cannot move to Legacy", c.key())),
                None => out.push(format!("legacy tool {id} is not registered")),
            }
        }
        let mut seen = HashSet::new();
        for &(id, f) in self.functions {
            if !seen.insert((id, f)) {
                out.push(format!("legacy function {id}/{f} twice"));
            }
            let Some((t, c)) = base(id) else {
                out.push(format!("legacy function {id}/{f}: tool not registered"));
                continue;
            };
            if c == Category::OneClick || self.tools.contains(&id) {
                out.push(format!(
                    "legacy function {id}/{f}: the whole tool is Legacy"
                ));
            }
            let spec = (t.spec)();
            let frozen = spec.advanced.len().min(FUNCTION_IDS.len());
            if !FUNCTION_IDS[..frozen].contains(&f) && !spec.functions.iter().any(|g| g.id == f) {
                out.push(format!("legacy function {id}/{f}: no such own function"));
            }
        }
        out
    }
}

/// No tool or function moved (the base categories).
const NO_LEGACY: LegacyTable = LegacyTable {
    tools: &[],
    functions: &[],
};

/// The category of tool `id` of `family` (cheap: no definition is built).
pub fn category(id: &str, family: Family) -> Category {
    LEGACY.category(id, family)
}

/// Whether an advanced function is part of the current tool or kept only so
/// old presets and projects keep working (contract §109–§116, §236).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FunctionStatus {
    Active,
    Legacy,
}

impl FunctionStatus {
    /// The catalog key (`status = "…"`).
    pub fn key(self) -> &'static str {
        match self {
            FunctionStatus::Active => "active",
            FunctionStatus::Legacy => "legacy",
        }
    }
}

pub use super::order::{Group, GroupSpec, ORDER_KEY, Phase, Plan};

/// Stable advanced-function ids, by function number (index 0 = function 1).
/// A function keeps its id for ever; moving it to Legacy changes only its
/// status (`docs/v0_9/LEGACY_MAP.toml`).
pub const FUNCTION_IDS: [&str; ADV_KEYS.len()] = [
    "a01", "a02", "a03", "a04", "a05", "a06", "a07", "a08", "a09", "a10", "a11", "a12", "a13",
    "a14", "a15", "a16",
];

/// One advanced function of a tool.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionDef {
    /// Function number, 1-based: its place in the window only — never a
    /// key (switches and controls are spelled by the stable id).
    pub n: u8,
    /// Stable id: `a01` … (the frozen v0.7 / v0.8 functions), `n01` …
    /// (functions declared by id, V09-S33) or `m_…` (mandatory).
    pub id: &'static str,
    /// Key of its on / off switch in a [`ParamSet`]: `a1` … (the frozen
    /// spelling of `a01` …), the id itself for `n01` …, `m.…`.
    pub switch: &'static str,
    pub name: &'static str,
    pub status: FunctionStatus,
    /// Keys of its own controls (without the switch).
    pub params: Vec<&'static str>,
    /// One of the platform's four mandatory functions (§098; ids `m_…`,
    /// switches `m.…`, after the tool's own functions).
    pub mandatory: bool,
    /// Composition slot (the catalog's `order`, §235): 10 × n for the
    /// frozen functions (they run inside the tool's kernel in that order),
    /// 110–790 for functions declared by id, 900–930 for the mandatory
    /// ones. [`ToolDefinition::execution_order`] is the full rule.
    pub order: u16,
}

/// Whether `s` is the id of a function declared by id (`n01` … `n99`).
pub fn is_function_id(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() == 3 && b[0] == b'n' && b[1].is_ascii_digit() && b[2].is_ascii_digit() && s != "n00")
        || contract_number(s).is_some()
}

/// The number of a generator's contract function id (`c01_free_angle` → 1;
/// V09-S52, §152 – §161: `c` + two digits + `_` + a lower-case name), if
/// `s` is spelled like one. Its composition order is ten times the number.
pub fn contract_number(s: &str) -> Option<u16> {
    let b = s.as_bytes();
    let ok = b.len() > 4
        && b[0] == b'c'
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3] == b'_'
        && b[4].is_ascii_lowercase()
        && b[4..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_');
    if !ok {
        return None;
    }
    let n = u16::from(b[1] - b'0') * 10 + u16::from(b[2] - b'0');
    (n > 0).then_some(n)
}

/// The id-keyed function a parameter key belongs to by its spelling (`n01`
/// for `n01` and `n01.…`), if it is spelled like one.
pub fn function_of_key(key: &str) -> Option<&str> {
    let head = key.split_once('.').map_or(key, |(h, _)| h);
    is_function_id(head).then_some(head)
}

/// The order range of functions declared by id (§235: core behaviour
/// 110–490, output shaping 500–790).
pub const NEW_FUNCTION_ORDERS: std::ops::RangeInclusive<u16> = 110..=790;

/// One parameter with the editor the tool window shows for it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub spec: ParamSpec,
    /// The semantic editor of a list ([`Editor::Auto`]: chosen from the kind).
    pub editor: Editor,
}

/// What a Control Sweep graph runs along (contract §099, §234): the shared
/// driver inputs (time, pitch, note index, velocity, subchannel, pattern
/// phase — one bar).
pub use super::drivers::Input as SweepDomain;

/// How a sweep's value meets the parameter's own value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Combine {
    Replace,
    Add,
    Multiply,
}

/// A Control Sweep binding: an editable graph over `domain` drives the
/// numeric parameter `param` within `range` (the engine that evaluates it is
/// shared, V09-S31; each eligible tool declares ten meaningful targets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepTarget {
    /// Stable id, unique within the tool.
    pub id: &'static str,
    /// Key of its on / off switch (`m.sweep.<id>`).
    pub on: &'static str,
    /// Key of its graph (`m.sweep.<id>.graph`).
    pub graph: &'static str,
    pub label: &'static str,
    /// The graph's label ("<label> graph").
    pub graph_label: &'static str,
    /// Key of the driven parameter (Int, Float or Ticks).
    pub param: &'static str,
    pub domain: SweepDomain,
    /// Values the graph's 0 … 1 maps to, inside the parameter's range.
    pub range: (f64, f64),
    pub combine: Combine,
}

/// Most Control Sweeps one tool declares (contract §098 function 4).
pub const MAX_SWEEPS: usize = 10;

/// Declares a Control Sweep target:
/// `sweep!("id", "Label", "param", Domain, lo, hi, Combine)`.
macro_rules! sweep {
    ($id:literal, $label:literal, $param:literal, $domain:ident, $lo:expr, $hi:expr, $how:ident) => {
        SweepTarget {
            id: $id,
            on: concat!("m.sweep.", $id),
            graph: concat!("m.sweep.", $id, ".graph"),
            label: $label,
            graph_label: concat!($label, " graph"),
            param: $param,
            domain: SweepDomain::$domain,
            range: ($lo as f64, $hi as f64),
            combine: Combine::$how,
        }
    };
}

/// How the window previews the tool (`docs/PREVIEW_ARCHITECTURE.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreviewRecipe {
    /// A per-note rule for every setting: the visible notes are previewed
    /// lazily, the full run happens on Apply.
    Viewport,
    /// A rule over the whole source: the preview is computed on a worker.
    Worker,
    /// Per-note for some settings, whole-source for others (decided when
    /// the kernel is built).
    ParamDependent,
}

impl PreviewRecipe {
    /// Whether a kernel that is (or is not) pointwise fits this recipe.
    pub fn allows(self, pointwise: bool) -> bool {
        match self {
            PreviewRecipe::Viewport => pointwise,
            PreviewRecipe::Worker => !pointwise,
            PreviewRecipe::ParamDependent => true,
        }
    }
}

/// The shape of a [`Growth`] bound (the preflight's estimate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GrowthKind {
    Map,
    Thin,
    Expand,
    Add,
    Generate,
}

impl GrowthKind {
    pub fn of(g: Growth) -> GrowthKind {
        match g {
            Growth::Map => GrowthKind::Map,
            Growth::Thin => GrowthKind::Thin,
            Growth::Expand(_) => GrowthKind::Expand,
            Growth::Add(_) => GrowthKind::Add,
            Growth::Generate(_) => GrowthKind::Generate,
        }
    }
}

/// What a tool does with the notes' subchannels (`SourceTrack::sub_offset`,
/// `docs/TRANSFORM_ENGINE.md` "Subchannel policy").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubchannelPolicy {
    /// Every result keeps its source note's subchannel, whatever the settings.
    Keep,
    /// Keeps them by default; some functions or settings route notes to
    /// other subchannels.
    KeepUnlessAsked,
    /// Assigns subchannels as its purpose (paint tools), also by default.
    Assign,
}

/// The standard boundary controls a tool exposes (`b.pitch`, `b.tick`, `b.len`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundaryControls {
    pub pitch: bool,
    pub start: bool,
    pub length: bool,
}

/// Where a tool's randomness comes from (`transform/rng.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedPolicy {
    /// No randomness: the same notes and settings give the same result.
    Deterministic,
    /// Seeded by these Seed parameters (per note, never by evaluation order).
    Seeded(Vec<&'static str>),
}

/// How an Apply enters the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoPolicy {
    /// One undo step per Apply, none when nothing changed.
    OneStepWhenChanged,
}

/// How a stroke tool captures the pointer in the Piano Roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capture {
    /// A drag records a freehand stroke.
    Freehand,
    /// Each click adds a corner.
    Polygon,
}

/// The pointer input a tool reads (`Ctx::stroke`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerInput {
    /// The capture with no function asking for another.
    pub base: Option<Capture>,
    /// Functions that capture the pointer (first switched-on entry wins).
    pub by_function: &'static [(u8, Capture)],
}

/// Everything PlumSpirit knows about one tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    pub category: Category,
    pub summary: &'static str,
    pub needs_selection: bool,
    pub context: bool,
    /// Every parameter in declaration order (base, function, boundary).
    pub params: Vec<ParamDef>,
    pub functions: Vec<FunctionDef>,
    pub sweeps: &'static [SweepTarget],
    pub preview: PreviewRecipe,
    /// The growth kinds its kernels report, in any setting.
    pub growth: &'static [GrowthKind],
    pub subchannels: SubchannelPolicy,
    pub boundary: BoundaryControls,
    pub seed: SeedPolicy,
    pub undo: UndoPolicy,
    pub pointer: PointerInput,
}

/// The declared part of a definition (measured from the kernels).
struct Declared {
    preview: PreviewRecipe,
    growth: &'static [GrowthKind],
    subchannels: SubchannelPolicy,
}

const fn d(
    preview: PreviewRecipe,
    growth: &'static [GrowthKind],
    subchannels: SubchannelPolicy,
) -> Declared {
    Declared {
        preview,
        growth,
        subchannels,
    }
}

use GrowthKind as G;
use PreviewRecipe::{ParamDependent as PD, Viewport as VP, Worker as WK};
use SubchannelPolicy::{Assign, Keep, KeepUnlessAsked as Ask};

const M: &[GrowthKind] = &[G::Map];
const T: &[GrowthKind] = &[G::Thin];
const TE: &[GrowthKind] = &[G::Thin, G::Expand];
const E: &[GrowthKind] = &[G::Expand];
const EM: &[GrowthKind] = &[G::Map, G::Expand];
const MTE: &[GrowthKind] = &[G::Map, G::Thin, G::Expand];
const MT: &[GrowthKind] = &[G::Map, G::Thin];
const A: &[GrowthKind] = &[G::Add];
const GEN: &[GrowthKind] = &[G::Generate];
/// V09-S47 Displacement Map: Map / Expand (the shared raster), Add (Vice
/// Versa keeps the selection and adds the moved map notes), Generate (View
/// Map).
const MEAG: &[GrowthKind] = &[G::Map, G::Expand, G::Add, G::Generate];

/// The declared behaviour of every tool and command, by id.
fn declared(id: &str) -> Option<Declared> {
    Some(match id {
        // ----- Piano Roll tools
        "arpeggiate" => d(WK, GEN, Ask),
        "articulate" => d(WK, M, Keep),
        // V09-S34: per-note while only per-note functions are on; n04, n05,
        // n06, n08, n10, n11 (and n01 / n07 by note or chord) read the
        // other notes.
        "chop" => d(PD, GEN, Ask),
        // V09-S34: n07 (trash refill) and n11 (dry layer) add notes; n11
        // moves the dry layer to another subchannel.
        "claw" => d(WK, TE, Ask),
        // V09-S34: per-note while only per-note functions are on; n03, n04,
        // n07, n09, n10 read the other notes.
        "quantize" => d(PD, M, Keep),
        // V09-S34: n08 echoes add notes (the generator's bound covers them).
        "randomize" => d(WK, GEN, Ask),
        // V09-S34: n04 adds run notes (Expand).
        "strum" => d(WK, EM, Keep),
        "flam" => d(PD, E, Ask),
        // V09-S35: n03 / n04 / n05 / n07 / n08 / n11 (and n02's removal)
        // remove notes, n02 splits long ones; n09 moves subchannels.
        "limit" => d(PD, MTE, Ask),
        "chord_progression" => d(WK, A, Ask),
        "chord_stamp" => d(PD, E, Ask),
        // V09-S35: n08 stacks variants in subchannels (when asked); every
        // function generates (the riff's bound covers them).
        "riff" => d(WK, GEN, Ask),
        "delete" => d(WK, T, Keep),
        "select" => d(WK, T, Keep),
        "draw" => d(WK, A, Ask),
        "paint" => d(WK, A, Ask),
        "slice" => d(VP, GEN, Ask),
        "snap_scale" => d(WK, M, Ask),
        "flip" => d(WK, EM, Ask),
        // V09-S39 step 7: per note while only per-note functions are on
        // (n02 but its inversion, n03 but the reference, n05, n07, n08
        // notes only, n11, n12); n01's destinations, n03's reference, n08's
        // controllers, n02's inversion, n04, n09, n10 and the sweeps read
        // the whole selection (Worker).
        "duplicate" => d(PD, E, Ask),
        // V09-S39 step 8: n04 thins the result (Thin); n07 / n08 change
        // keys, no function routes subchannels.
        "stretch" => d(WK, MT, Keep),
        // V09-S36: n06 adds a copy per player on its subchannel step; n01
        // with phrase anchors, n03, n05, n07, n10 and n11 read the notes
        // around each note.
        "humanize" => d(PD, EM, Ask),
        // V09-S36: n10 adds a note per Shepard layer; n02 from the first or
        // lowest note, n03, n04, n05 per chord, n06 by onset and n07 read
        // the other notes.
        "transpose" => d(PD, EM, Keep),
        // V09-S36: n07 ties and n09 (removing the other voices) remove
        // notes; every function reads the notes around each note.
        "legato" => d(WK, MT, Keep),
        // V09-S36: n02 / n03 / n06 / n07 / n04 read the other notes; n10,
        // n08 and n11 may set subchannels.
        "note_props" => d(WK, M, Ask),
        // V09-S36: every function reads the chains around each note; n07
        // removes duplicates across tracks, n06 writes controller points.
        "join" => d(WK, T, Keep),
        // V09-S36: n07 and n09 read the chords and places of the notes; n02
        // echoes past them; n12 writes controller points.
        "repeat" => d(WK, GEN, Ask),
        // V09-S36: n01 / n06 regroup the selection, n07 / n09 change the
        // notes, n04 adds repeats; n10 may set subchannels.
        "group" => d(WK, GEN, Ask),
        // V09-S35: n10 moves glided notes to free MIDI channels (never
        // their subchannels); n07 plays a chain's notes on its first pitch.
        "slide" => d(WK, GEN, Keep),
        "skew" | "distort" | "perspective" | "warp" | "shear" | "twirl" | "wave" => d(WK, EM, Ask),
        // V09-S44: Warp Transform — Transform Existing maps (N → N, the
        // Source Region's other notes unchanged); Fill / Duplicate expands
        // (split, filled rows, tiles); the tiling reads the whole box.
        "warp_transform" => d(WK, EM, Ask),
        // V09-S45: Trails — generations of the selection (Expand; the
        // source alone, or nothing with Trails Only and no trails).
        "trails" => d(WK, MTE, Ask),
        // V09-S46: Note Outline — the art (kept or not) and its outline:
        // generated from the art's geometry, like Contour Trace.
        "note_outline" => d(WK, GEN, Ask),
        // V09-S47: Displacement Map.
        "displacement_map" => d(WK, MEAG, Ask),
        // V09-S48: Tiny Planet (the shared raster: Map, or Expand when it
        // follows the arcs / fills the rings).
        "tiny_planet" => d(WK, EM, Ask),
        // V09-S49: Ripple (the shared raster).
        "ripple" => d(WK, EM, Ask),
        // V09-S38 step 3: n08's outside parts move by a subchannel offset
        // and n11 takes the base notes' subchannels (when asked).
        "clip_mask" => d(WK, E, Ask),
        "paint_color" => d(WK, M, Assign),
        "subpaint" => d(WK, M, Assign),
        // ----- Experimental tools
        // V09-S37: the shared geometry Fill with copies cycling through the
        // subchannels routes them when asked (measured once the definition
        // test tries that setting on every geometry tool).
        // V09-S40 step 1: n01, n07, n08 read the whole selection (Worker);
        // no function adds notes beyond a9's fill (EM as v0.8).
        "gravity_well" => d(WK, EM, Ask),
        "kaleidoscope" | "orbit" | "vortex_trails" => d(WK, E, Ask),
        "pulse_field" => d(WK, EM, Ask),
        "particle_burst" => d(WK, E, Ask),
        // V09-S43: the 3D tool reads the whole selection (the depth range,
        // occlusion); split / fill expand like the geometry tools.
        "tool3d" => d(WK, EM, Ask),
        // V09-S50: Tumours (growth notes beside the kept art).
        "tumours" => d(WK, GEN, Ask),
        "flow_field" => d(WK, EM, Ask),
        // ----- Plugins
        "hz_chop" => d(VP, E, Keep),
        "safor" => d(WK, E, Keep),
        // ----- Generators (V09-S51): whole-selection-free shapes, drawn on a
        // worker; the subchannel control routes them (when asked).
        "gen.lines" | "gen.rect" | "gen.circles" | "gen.arcs" | "gen.polygons" | "gen.custom"
        | "gen.parametric" | "gen.lsystem" | "gen.spirograph" | "gen.growth" => d(WK, GEN, Ask),
        // ----- Paste Repeat (V09-S13)
        "paste_repeat_notes" => d(PD, E, Ask),
        // ----- One-click commands
        "mute_notes" => d(WK, M, Keep),
        "slice_notes" => d(WK, E, Keep),
        "duplicate_octave" => d(VP, E, Keep),
        _ => return None,
    })
}

/// Tools whose category is not the one their family implies.
fn category_override(id: &str) -> Option<Category> {
    match id {
        // §118: Slice is a one-click action; the v0.8 tool stays for presets.
        "slice" => Some(Category::OneClick),
        // B1-0099: Duplicate V2 is listed under Iterations but keeps the
        // Paste Repeat category (its own functions, no mandatory functions).
        "paste_repeat_notes" => Some(Category::PasteRepeat),
        _ => None,
    }
}

/// Pointer input of the stroke tools.
fn pointer(id: &str) -> PointerInput {
    match id {
        "draw" | "paint" => PointerInput {
            base: Some(Capture::Freehand),
            by_function: &[],
        },
        // Select: function 2 (polygon lasso) wins over function 1 (lasso).
        "select" => PointerInput {
            base: None,
            by_function: &[(2, Capture::Polygon), (1, Capture::Freehand)],
        },
        _ => PointerInput::default(),
    }
}

/// Tools whose kernel evaluates its Control Sweeps itself, once per unit
/// (V09-0143: their output does not come note by note from the selection,
/// so the platform must not split the source by levels — see
/// [`super::sweep::UnitSweeps`]). Group Notes measures each group at its
/// first note (Note index = the group's place in time order); Wave per note
/// (V09-S38 step 2); Clipping Mask per note (V09-S38 step 3: the art's box
/// and span, a1's inversion and a9's animation read every note; its mask
/// parameters by levels, a mask per level); Paint Color per note (V09-S38
/// step 4: a2's palette by source index, the gradients' box, a8's
/// densities, a10's regions, n02's outline, n09's depths and n10's blur
/// read the whole selection).
pub fn kernel_sweeps(id: &str) -> bool {
    matches!(
        id,
        "group"
            | "wave"
            | "clip_mask"
            | "paint_color"
            | "subpaint"
            | "skew"
            | "distort"
            | "perspective"
            | "shear"
            | "twirl"
            | "warp"
            // V09-S39 step 0 (E0422): the S34 – S36 tools whose passes read
            // across notes take their sweeps per note (per level, the
            // platform's values) while seeing the whole selection.
            | "humanize"
            | "transpose"
            | "legato"
            | "note_props"
            | "join"
            | "repeat"
            | "arpeggiate"
            | "articulate"
            | "chop"
            | "quantize"
            | "strum"
            | "flam"
            | "limit"
            | "claw"
            | "chord_stamp"
            | "slide"
            | "chord_progression"
            | "randomize"
            | "riff"
            // V09-S39 step 1: Delete per note from the start (n01, n05,
            // n06, n07, n09, n10 and n11 read the whole selection).
            | "delete"
            // V09-S39 step 2: Select per note from the start (n01, n02,
            // n04 – n07, n09 and n11 read the whole pool).
            | "select"
            // V09-S39 step 3: Draw per drawn position (its output comes
            // from the pointer stroke, not from the selected notes).
            | "draw"
            // V09-S39 step 4: the Note Brush per painted cell (its output
            // comes from the pointer stroke, not from the selected notes).
            | "paint"
            // V09-S39 step 5: Snap to Scale per note from the start (n01's
            // windows, n04 / n11's chords, n08's next notes and n10's lines
            // read across the selection).
            | "snap_scale"
            // V09-S39 step 6: Flip per note from the start (the groups' boxes,
            // n02's halves, n08's region and n10's grid pairs read across the
            // selection).
            | "flip"
            // V09-S39 step 7: Duplicate per note from the start (n04's
            // sequences, n09's ranks, n10's fragment and n08's controllers
            // read across the selection).
            | "duplicate"
            // V09-S39 step 8: Score Stretch per note from the start (n05's
            // phrases, n08's end, n11's next onsets and n04's windows read
            // across the selection).
            | "stretch"
            // V09-S40 step 1: Gravity Well per note from the start (n01's
            // masses, n07's windows and n08's chords read the whole
            // selection).
            | "gravity_well"
            // V09-S40 step 4: Kaleidoscope per note from the start (n04's
            // outlines, n09's seams and n11's units read the whole
            // selection).
            | "kaleidoscope"
            // V09-S40 step 5: Orbit per note from the start (n07's ranks
            // and n11's trail read the whole selection).
            | "orbit"
            // V09-S40 step 6: Vortex Trails per note from the start (n07's
            // windows, n11's box and n03's core read the whole selection).
            | "vortex_trails"
            // V09-S43: the 3D tool's depth range and occlusion read the
            // whole selection; its sweeps act per note.
            | "tool3d"
            // V09-S50: Tumours' growth per nearest art note.
            | "tumours"
            // V09-S44: Warp Transform's sweeps act per note (each note its
            // own homography and tiling).
            | "warp_transform"
            // V09-S46: Note Outline's thickness / offset per art note (the
            // outline of the whole art), attributes per piece's nearest note.
            | "note_outline"
            // V09-S47: Displacement Map's amounts and levels per note.
            | "displacement_map"
            // V09-S48: Tiny Planet's projection per note.
            | "tiny_planet"
            // V09-S49: Ripple's waves per note.
            | "ripple"
            // V09-S41: Pulse Field's pulse per note (n02's onsets read the
            // reference notes).
            | "pulse_field"
            // V09-S41: Particle Burst's flight per note (n05's targets read
            // the reference notes).
            | "particle_burst"
            // V09-S41: Flow Field's field per note (n01's melody and n05's
            // obstacles read the reference notes; n04's streamlines and n11's
            // cells the whole output).
            | "flow_field"
    )
}

/// Kernel-swept tools whose unit is a group (measured at its first note);
/// the others' unit is the note (Wave: n03's phrases / chords / onsets and
/// n10's track ranks need every note in one run).
pub fn sweeps_per_group(id: &str) -> bool {
    matches!(id, "group")
}

/// A tool's own driver dimension (§098 "tool-specific dimension", §232):
/// the ordinal its kernel gives each note it emits through
/// `Output::set_dim` (0 for notes outside it, e.g. the kept source), offered
/// as the seventh "Driven by" input of the Subchannel / Pitch Offset Drivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverDim {
    /// What the ordinal counts ("Copy", "Arpeggio step" …).
    pub name: &'static str,
    /// What its 0 is and how it counts, for the window's help.
    pub help: &'static str,
    /// The "Driven by" menu: the six generic inputs, then `name`.
    pub menu: &'static [&'static str],
}

macro_rules! dim {
    ($name:literal, $help:literal) => {
        Some(&DriverDim {
            name: $name,
            help: $help,
            menu: &[
                "Time",
                "Pitch",
                "Note index",
                "Velocity",
                "Subchannel",
                "Pattern phase",
                $name,
            ],
        })
    };
}

/// The driver dimension of tool `id` (`None`: the generic inputs only —
/// tools with no ordinal of their own: they move, filter or recolour the
/// notes they are given one for one).
pub fn driver_dimension(id: &str) -> Option<&'static DriverDim> {
    match id {
        "duplicate" => dim!(
            "Copy",
            "Copy: 0 = the original note, 1 = its first copy, 2 = the second …"
        ),
        "repeat" => dim!(
            "Repeat",
            "Repeat: 0 = a note's first repeat, 1 = the next … (nested repeats count within their slot)"
        ),
        "flam" => dim!(
            "Stroke",
            "Stroke: 0 = the note itself, 1 = the grace stroke nearest it, 2 = the next … (each side counts from the note)"
        ),
        "strum" => dim!(
            "Strum position",
            "Strum position: 0 = the first note struck in its chord, 1 = the next … (a lead-in run is 0)"
        ),
        "arpeggiate" => dim!(
            "Arpeggio step",
            "Arpeggio step: 0 = a chord's first step, 1 = the next … (copies of a step share it; pedal anchors are 0)"
        ),
        "chop" => dim!(
            "Piece",
            "Piece: 0 = a note's first piece, 1 = the next … (a piece cut deeper keeps its piece's number)"
        ),
        "chord_progression" => dim!(
            "Chord",
            "Chord: 0 = the progression's first chord, 1 = the next … (kept selected notes are 0)"
        ),
        "chord_stamp" => dim!(
            "Chord tone",
            "Chord tone: 0 = the stamp's first tone, 1 = the next … (its copies share it; unstamped notes are 0)"
        ),
        "riff" => dim!(
            "Variant",
            "Variant: 0 = the riff's first variant (n08), 1 = the next … (one variant: all 0; kept notes 0)"
        ),
        "draw" => dim!(
            "Step",
            "Step: 0 = the stroke's first drawn note, 1 = the next … (motif copies and mirrors share their step; kept notes 0)"
        ),
        "paint" => dim!(
            "Cell",
            "Cell: 0 = the first cell painted along the stroke, 1 = the next … (every note of a cell shares it; kept notes 0)"
        ),
        "group" => dim!(
            "Group",
            "Group: 1 = the earliest group (by its first note), 2 = the next …; ungrouped notes 0."
        ),
        "humanize" => dim!(
            "Player",
            "Player: 0 = the note itself, 1 … = n06's ensemble players (without n06 all 0)"
        ),
        "flip" => dim!(
            "Copy",
            "Copy: 0 = the note or its flipped image, 1 = n01's appended mirror, k = n04's glide copy k."
        ),
        "distort" => dim!(
            "Shard",
            "Shard: the n08 shard the note flies with (0 … shards − 1, numbered along time; without n08 all 0)"
        ),
        "slide" => dim!(
            "Glide step",
            "Glide step: 0 = the first note of a glide drawn as steps, 1 = the next … (the note itself last; notes without stepped glides 0)"
        ),
        "skew" => dim!(
            "Trail copy",
            "Trail copy: 0 = the note where it was, 1 … = its trail copies toward the full skew (a10; without it all 0)"
        ),
        "perspective" => dim!(
            "Echo",
            "Echo: 0 = the note itself, 1 = its first echo toward the far edge, 2 = the next …"
        ),
        "shear" => dim!(
            "Copy",
            "Copy: 0 = the sheared note, 1 = n09's first copy back toward the original, 2 = the next …"
        ),
        "twirl" => dim!(
            "Trail copy",
            "Trail copy: 0 = the note where it was, 1 … = its trail copies toward the full twirl (without copies all 0)"
        ),
        "kaleidoscope" => dim!(
            "Sector",
            "Sector: 0 = the original sector's notes, 1 = the next sector around the center … (mirrored and nested copies keep their sector)"
        ),
        "orbit" => dim!(
            "Orbit copy",
            "Orbit copy: 0 = a note's first copy on the orbit, 1 = the next … (ring by ring; the orbit line is 0)"
        ),
        "vortex_trails" => dim!(
            "Trail",
            "Trail: 0 = the original, 1 = the nearest trail, 2 = the next … (reversed trails count from the note too; the core is 0)"
        ),
        "pulse_field" => dim!(
            "Echo",
            "Echo: 0 = the note itself, 1 = its first echo, 2 = the next … (without echoes all 0)"
        ),
        "particle_burst" => dim!(
            "Particle",
            "Particle: 0 = a note's first particle, 1 = the next … (a fountain's later emissions count on; fragments, bounces and trails take their particle's; kept notes 0)"
        ),
        "flow_field" => dim!(
            "Trail copy",
            "Trail copy: 0 = the least advected copy, … the last = the note's whole flow (without trails all 0; grid streamlines 0)"
        ),
        "tumours" => dim!(
            "Stage",
            "Stage: 0 = the first evolution stage (c25), 1 = the next … (one stage: all 0; the kept art's copy beside each stage has its number)"
        ),
        "trails" => dim!(
            "Generation",
            "Generation: 0 = the source, 1 = the nearest generation before or after it, 2 = the next …"
        ),
        "note_outline" => dim!("Outline", "Outline: 0 = the kept art, 1 = the outline."),
        "warp_transform" => dim!(
            "Tile ring",
            "Tile ring: 0 = the original tile, 1 = the tiles next to it, 2 = the ring beyond … (Fill / Duplicate with Wrap; otherwise all 0)"
        ),
        _ => None,
    }
}

/// Control Sweep targets by tool (§099: ten semantically chosen per eligible
/// tool, each driving one numeric parameter along a domain).
pub fn sweep_targets(id: &str) -> &'static [SweepTarget] {
    match id {
        // V09-S36 step 7: evaluated per group by the kernel
        // ([`kernel_sweeps`]).
        "group" => &[
            sweep!(
                "stagger",
                "Stagger step",
                "n03.shift",
                NoteIndex,
                0,
                960,
                Replace
            ),
            sweep!("fill_gap", "Fill gap", "n04.gap", Time, 0, 960, Replace),
            sweep!(
                "fit_len",
                "Fit length",
                "n05.len",
                NoteIndex,
                960,
                3840,
                Replace
            ),
            sweep!(
                "fit_span",
                "Fit pitch span",
                "n05.span",
                NoteIndex,
                0,
                24,
                Replace
            ),
            sweep!(
                "merge",
                "Merge closer than",
                "n06.merge",
                Time,
                0,
                960,
                Replace
            ),
            sweep!(
                "degree",
                "Degree offset",
                "n08.offset",
                NoteIndex,
                0,
                7,
                Replace
            ),
            sweep!("thin", "Notes per group", "n09.count", Time, 1, 32, Replace),
            sweep!(
                "scale",
                "Scale per group",
                "n11.factor",
                NoteIndex,
                50,
                150,
                Replace
            ),
            sweep!(
                "iso",
                "Semitones per group",
                "a8.step",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!(
                "pivot",
                "Local time scale",
                "a6.factor",
                Pitch,
                50,
                200,
                Replace
            ),
        ],
        "humanize" => &[
            sweep!("rubato", "Rubato depth", "n01.depth", Time, 0, 200, Replace),
            sweep!("split", "Hand split", "n02.split", Time, 36, 84, Replace),
            sweep!("arc", "Phrase arc depth", "n03.depth", Time, 0, 64, Replace),
            sweep!(
                "accent_spread",
                "Accent spread by velocity",
                "n04.spread",
                Velocity,
                0,
                32,
                Replace
            ),
            sweep!(
                "fatigue",
                "Velocity loss per repeat by pitch",
                "n05.loss",
                Pitch,
                0,
                20,
                Replace
            ),
            sweep!(
                "ensemble",
                "Ensemble timing spread",
                "n06.spread",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "leap",
                "Leap error per octave by pitch",
                "n07.per",
                Pitch,
                0,
                120,
                Replace
            ),
            sweep!(
                "off_grid",
                "Off-grid amount",
                "n08.off",
                Time,
                0,
                200,
                Replace
            ),
            sweep!("swing", "Highest swing", "n09.hi", Time, 50, 80, Replace),
            sweep!(
                "roll",
                "Chord roll by velocity",
                "n11.spread",
                Velocity,
                0,
                120,
                Replace
            ),
        ],
        "legato" => &[
            sweep!(
                "rest_max",
                "Largest rest filled",
                "n01.max",
                Time,
                0,
                3840,
                Replace
            ),
            sweep!(
                "metric_offset",
                "Metric boundary offset",
                "n02.offset",
                Time,
                -240,
                240,
                Replace
            ),
            sweep!("repedal", "Re-pedal gap", "n03.gap", Time, 0, 480, Replace),
            sweep!(
                "poly",
                "Most sounding notes by pitch",
                "n04.most",
                Pitch,
                1,
                64,
                Replace
            ),
            sweep!("meet", "Meeting point", "n05.split", Time, 0, 100, Replace),
            sweep!(
                "retrigger",
                "Retrigger gap by velocity",
                "n06.gap",
                Velocity,
                0,
                120,
                Replace
            ),
            sweep!(
                "shorter",
                "Only notes shorter than",
                "n08.short",
                Time,
                60,
                3840,
                Replace
            ),
            sweep!(
                "range",
                "Directional range by pitch",
                "n10.range",
                Pitch,
                1,
                24,
                Replace
            ),
            sweep!("hold", "Hold over onsets", "n11.n", Time, 1, 16, Replace),
            sweep!(
                "overlap",
                "Fixed overlap",
                "a2.ticks",
                Time,
                1,
                480,
                Replace
            ),
        ],
        "note_props" => &[
            sweep!("cross_lo", "Mapping low", "n01.lo", Time, 0, 127, Replace),
            sweep!(
                "cross_hi",
                "Mapping high by pitch",
                "n01.hi",
                Pitch,
                0,
                127,
                Replace
            ),
            sweep!(
                "accent",
                "Accent amount",
                "n02.amount",
                Time,
                -64,
                64,
                Replace
            ),
            sweep!("leap", "Accent leap", "n02.leap", Time, 1, 24, Replace),
            sweep!(
                "bridge",
                "Ends bridge strength",
                "n04.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "length_ms",
                "Real-time length",
                "n05.ms",
                Time,
                10,
                2000,
                Replace
            ),
            sweep!(
                "smooth",
                "Smoothing strength by velocity",
                "n06.strength",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "deviations",
                "Outlier deviations",
                "n07.k",
                Time,
                0.5,
                6,
                Replace
            ),
            sweep!(
                "cc_to",
                "Controller gradient end",
                "n09.to",
                Time,
                0,
                127,
                Replace
            ),
            sweep!(
                "cond_to",
                "Conditional value by note",
                "n10.to",
                NoteIndex,
                0,
                127,
                Replace
            ),
        ],
        "join" => &[
            sweep!(
                "window",
                "Window ticks (custom)",
                "n01.ticks",
                Time,
                60,
                3840,
                Replace
            ),
            sweep!(
                "longest",
                "Longest result",
                "n02.max",
                Time,
                120,
                7680,
                Replace
            ),
            sweep!(
                "similar",
                "Velocity difference by velocity",
                "n03.diff",
                Velocity,
                0,
                64,
                Replace
            ),
            sweep!(
                "nps",
                "Target notes per second",
                "n04.nps",
                Time,
                100,
                100000,
                Replace
            ),
            sweep!("ties", "Tie tolerance", "n05.tol", Time, 0, 120, Replace),
            sweep!(
                "expression",
                "Expression scale by velocity",
                "n06.scale",
                Velocity,
                0,
                200,
                Replace
            ),
            sweep!(
                "short",
                "Absorb shorter than by pitch",
                "n08.short",
                Pitch,
                15,
                480,
                Replace
            ),
            sweep!(
                "chord_gap",
                "Repeated chord gap",
                "n09.gap",
                Time,
                0,
                960,
                Replace
            ),
            sweep!("rest", "Largest rest", "n10.rest", Time, 0, 3840, Replace),
            sweep!(
                "per_bar",
                "Fragments per bar by pitch",
                "n11.most",
                Pitch,
                1,
                16,
                Replace
            ),
        ],
        // V09-S37 step 1 (note-level: each level segment is skewed with
        // its value).
        "skew" => &[
            sweep!("amount", "X skew", "amount", Time, -100, 100, Replace),
            sweep!(
                "stair",
                "Staircase strength",
                "n01.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "stair_rows",
                "Rows per step by pitch",
                "n01.rows",
                Pitch,
                1,
                12,
                Replace
            ),
            sweep!(
                "zigzag_phase",
                "Zigzag phase",
                "n02.phase",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "end",
                "End amount by pitch",
                "n03.amount",
                Pitch,
                -100,
                100,
                Replace
            ),
            sweep!(
                "rt_slope",
                "Milliseconds per semitone",
                "n06.ms",
                Time,
                -50,
                50,
                Replace
            ),
            sweep!(
                "depth",
                "Depth velocity by velocity",
                "n07.hi",
                Velocity,
                0,
                150,
                Replace
            ),
            sweep!(
                "overshoot",
                "Elastic overshoot",
                "n09.overshoot",
                Time,
                0,
                200,
                Replace
            ),
            sweep!(
                "stagger",
                "Track stagger by note",
                "n10.shift",
                NoteIndex,
                -960,
                960,
                Replace
            ),
            sweep!(
                "mirror_vel",
                "Mirror copy velocity",
                "n11.vel",
                Velocity,
                0,
                150,
                Replace
            ),
        ],
        // V09-S37 step 2 (note-level).
        "distort" => &[
            sweep!(
                "amount",
                "Bulge / pinch",
                "amount",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!("focus", "Lens focus", "n01.focus", Time, 0, 100, Replace),
            sweep!(
                "magnification",
                "Lens magnification by pitch",
                "n01.mag",
                Pitch,
                50,
                400,
                Replace
            ),
            sweep!("region", "Region size", "n03.size", Time, 10, 150, Replace),
            sweep!("sag", "Sag", "n04.sag", Time, -24, 24, Replace),
            sweep!(
                "attract",
                "Attractor strength by pitch",
                "n06.strength",
                Pitch,
                -100,
                100,
                Replace
            ),
            sweep!("taper", "Taper at end", "n07.end", Time, 0, 100, Replace),
            sweep!(
                "explosion",
                "Shatter explosion",
                "n08.explosion",
                Time,
                0,
                200,
                Replace
            ),
            sweep!("bend", "Bend angle", "n09.angle", Time, -180, 180, Replace),
            sweep!(
                "split_x",
                "Quadrant split X by pitch",
                "n11.x",
                Pitch,
                10,
                90,
                Replace
            ),
        ],
        // V09-S37 step 3 (note-level; Horizon guides are drawn once per
        // run, so their line count is not swept).
        "perspective" => &[
            sweep!("top", "Top edge width", "top", Time, 20, 200, Replace),
            sweep!("fog", "Fog far velocity", "n01.far", Time, 0, 100, Replace),
            sweep!(
                "bands",
                "Depth bands by pitch",
                "n02.bands",
                Pitch,
                2,
                16,
                Replace
            ),
            sweep!("thin", "Far share kept", "n03.keep", Time, 0, 100, Replace),
            sweep!(
                "reflection",
                "Reflection fade",
                "n04.fade",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "curvature",
                "Horizon curvature",
                "n07.curvature",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!(
                "stereo",
                "Eye separation by pitch",
                "n09.sep",
                Pitch,
                0,
                480,
                Replace
            ),
            sweep!(
                "shadow_x",
                "Shadow offset X",
                "n10.x",
                Time,
                -480,
                480,
                Replace
            ),
            sweep!(
                "shadow_vel",
                "Shadow velocity by velocity",
                "n10.vel",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!("focus", "Focal depth", "n11.focus", Time, 0, 100, Replace),
        ],
        // V09-S37 step 4 (note-level).
        "warp" => &[
            sweep!("push_x", "Push X", "dx", Time, -50, 50, Replace),
            sweep!("push_y", "Push Y by pitch", "dy", Pitch, -50, 50, Replace),
            sweep!(
                "smooth",
                "Mesh smoothing amount",
                "n01.amount",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "conformal",
                "Conformal strength",
                "n04.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "noise",
                "Mesh noise amount",
                "n05.amount",
                Time,
                0,
                50,
                Replace
            ),
            sweep!(
                "polar_x",
                "Polar center X by pitch",
                "n07.x",
                Pitch,
                0,
                100,
                Replace
            ),
            sweep!("fold", "Fold angle", "n08.angle", Time, -180, 180, Replace),
            sweep!("brush", "Path push", "n09.push", Time, -50, 50, Replace),
            sweep!(
                "iterate",
                "Scale per iteration",
                "n10.scale",
                Time,
                10,
                150,
                Replace
            ),
            sweep!("smear", "Smear width", "n11.width", Time, 0, 50, Replace),
        ],
        // V09-S38 step 3: evaluated by the kernel ([`kernel_sweeps`]) — the
        // mask's parameters by levels (a mask per level), the others per
        // note.
        "clip_mask" => &[
            sweep!(
                "shape_size",
                "Shape size",
                "n01.size",
                Time,
                10,
                200,
                Replace
            ),
            sweep!(
                "base_velocity",
                "Lowest base velocity",
                "n02.lo",
                Time,
                1,
                127,
                Replace
            ),
            sweep!(
                "offset_x",
                "Mask offset X",
                "n04.dx",
                Time,
                -1920,
                1920,
                Replace
            ),
            sweep!(
                "offset_y",
                "Mask offset Y",
                "n04.dy",
                Time,
                -24,
                24,
                Replace
            ),
            sweep!(
                "stroke",
                "Stroke width",
                "n05.width",
                Time,
                15,
                960,
                Replace
            ),
            sweep!(
                "period",
                "Tile period",
                "n06.period",
                Time,
                240,
                7680,
                Replace
            ),
            sweep!(
                "density",
                "Density threshold",
                "n07.threshold",
                Time,
                1,
                32,
                Replace
            ),
            sweep!("fade", "Fade width", "n03.width", Time, 0, 1920, Replace),
            sweep!(
                "outside_velocity",
                "Outside velocity",
                "n08.vel",
                Time,
                0,
                150,
                Replace
            ),
            sweep!(
                "transfer",
                "Transfer amount",
                "n11.amount",
                Time,
                0,
                100,
                Replace
            ),
        ],
        // V09-S38 step 4: evaluated per note by the kernel
        // ([`kernel_sweeps`]).
        // V09-S38 step 5: evaluated per note by the kernel
        // ([`kernel_sweeps`]: the automaton, the reaction-diffusion, the
        // chains and the phrases read the whole selection).
        "subpaint" => &[
            sweep!("gens", "Generations", "n01.gens", Time, 1, 16, Replace),
            sweep!(
                "iters",
                "L-system iterations",
                "n03.iters",
                Time,
                1,
                8,
                Replace
            ),
            sweep!("feed", "Feed", "n04.feed", Time, 2, 6, Replace),
            sweep!("kill", "Kill", "n04.kill", Pitch, 5, 7, Replace),
            sweep!("steps", "Steps", "n04.steps", Time, 50, 500, Replace),
            sweep!("flash", "Flash offset", "n05.flash", Time, 0, 8, Replace),
            sweep!("decay", "Decay", "n05.decay", Time, 0, 1920, Replace),
            sweep!("walk_step", "Largest step", "n06.max", Pitch, 1, 8, Replace),
            sweep!("corr", "Row correlation", "n06.corr", Time, 0, 100, Replace),
            sweep!(
                "rest",
                "Phrase split rest",
                "n07.rest",
                Time,
                0,
                1920,
                Replace
            ),
        ],
        // V09-S39 step 1: evaluated per note by the kernel
        // ([`kernel_sweeps`]); n05's per chord at its first note, n11's
        // per distinct value.
        "delete" => &[
            sweep!(
                "probability",
                "Erase probability",
                "a6.prob",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "cover_tol",
                "Covered start tolerance",
                "n01.tol",
                Time,
                0,
                480,
                Replace
            ),
            sweep!(
                "shortest",
                "Shortest length",
                "n02.lo",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "longest",
                "Longest length",
                "n02.hi",
                Time,
                0,
                1920,
                Replace
            ),
            sweep!("stack_keep", "Stack keep", "n05.keep", Time, 1, 8, Replace),
            sweep!("hollow_n", "Interior N", "n06.n", Time, 2, 16, Replace),
            sweep!(
                "collision_tol",
                "Collision tolerance",
                "n07.tol",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "radius",
                "Isolation radius",
                "n09.radius",
                Time,
                0,
                1920,
                Replace
            ),
            sweep!(
                "rows",
                "Isolation rows by pitch",
                "n09.rows",
                Pitch,
                0,
                12,
                Replace
            ),
            sweep!(
                "budget",
                "Most notes",
                "n11.most",
                Time,
                1000,
                1000000,
                Replace
            ),
        ],
        // V09-S39 step 2: evaluated per note by the kernel
        // ([`kernel_sweeps`]); n01 at each result note, n02 / n09 per chord
        // at its first note, n07 at each note of the line.
        "select" => &[
            sweep!("radius", "Grow radius", "n01.radius", Time, 0, 960, Replace),
            sweep!(
                "rows",
                "Grow rows by pitch",
                "n01.rows",
                Pitch,
                0,
                12,
                Replace
            ),
            sweep!("rank", "Voice rank", "n02.rank", Time, 1, 8, Replace),
            sweep!(
                "metric_tol",
                "Metric tolerance",
                "n03.tol",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "largest_interval",
                "Largest interval",
                "n04.hi",
                Time,
                0,
                24,
                Replace
            ),
            sweep!(
                "deviations",
                "Outlier deviations",
                "n06.k",
                Time,
                0.5,
                6,
                Replace
            ),
            sweep!(
                "window",
                "Outlier window",
                "n06.window",
                Time,
                480,
                15360,
                Replace
            ),
            sweep!("gap", "Line gap", "n07.gap", Time, 0, 1920, Replace),
            sweep!("bpm", "Region BPM", "n08.bpm", Time, 60, 240, Replace),
            sweep!(
                "smallest_chord",
                "Smallest chord",
                "n09.lo",
                Time,
                1,
                16,
                Replace
            ),
        ],
        // V09-S39 step 3: evaluated per drawn position by the kernel
        // ([`kernel_sweeps`]), measured at the position's start among the
        // drawn positions.
        "draw" => &[
            sweep!("gate", "Gate", "gate", Time, 25, 200, Replace),
            sweep!("velocity", "Velocity", "vel", Time, 40, 127, Replace),
            sweep!(
                "tension",
                "Chord-follow tension",
                "n01.tension",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "smoothing",
                "Smoothing",
                "n02.smooth",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "stair_rows",
                "Rows per stair step",
                "n03.rows",
                Time,
                1,
                12,
                Replace
            ),
            sweep!(
                "shape_tolerance",
                "Shape tolerance",
                "n07.tol",
                Time,
                1,
                100,
                Replace
            ),
            sweep!(
                "tremolo",
                "Tremolo interval",
                "n10.interval",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!(
                "pitch_jitter",
                "Pitch jitter",
                "a9.pitch",
                Time,
                0,
                12,
                Replace
            ),
            sweep!(
                "time_jitter",
                "Time jitter",
                "a9.ticks",
                Time,
                0,
                480,
                Replace
            ),
            sweep!(
                "mirror_axis",
                "Mirror axis",
                "a7.axis",
                Time,
                36,
                96,
                Replace
            ),
        ],
        // V09-S39 step 5: evaluated per note by the kernel
        // ([`kernel_sweeps`]); a chord (n04, n11) takes its first note's.
        "snap_scale" => &[
            sweep!(
                "detect_bars",
                "Detection window (bars)",
                "n01.bars",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "passing",
                "Passing notes shorter than",
                "n02.short",
                Time,
                0,
                480,
                Replace
            ),
            sweep!(
                "bend_range",
                "Bend range",
                "n03.range",
                Time,
                1,
                12,
                Replace
            ),
            sweep!(
                "voice_tolerance",
                "Voice-aware chord tolerance",
                "n04.tol",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "reference_least",
                "Reference occurrences",
                "n05.least",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "approach",
                "Approach window",
                "n08.window",
                Time,
                0,
                960,
                Replace
            ),
            sweep!(
                "chord_length",
                "Progression chord length",
                "n09.len",
                Time,
                480,
                7680,
                Replace
            ),
            sweep!(
                "shift",
                "Largest chord shift",
                "n11.shift",
                Time,
                1,
                12,
                Replace
            ),
            sweep!(
                "shift_tolerance",
                "Chord-shift tolerance",
                "n11.tol",
                Time,
                0,
                240,
                Replace
            ),
            sweep!("mark", "Mark subchannel", "a10.sub", Time, 0, 3, Replace),
        ],
        // V09-S39 step 4: evaluated per painted cell by the kernel
        // ([`kernel_sweeps`]), measured at the cell's start among the
        // painted cells; an edited note takes its covering cell's values.
        // V09-S39 step 6: evaluated per note by the kernel
        // ([`kernel_sweeps`]); a flipping set's n02 half takes its first
        // note's.
        "flip" => &[
            sweep!(
                "append_velocity",
                "Mirror append velocity",
                "n01.vel",
                Time,
                0,
                150,
                Replace
            ),
            sweep!(
                "glide_period",
                "Glide period",
                "n04.period",
                Time,
                240,
                7680,
                Replace
            ),
            sweep!(
                "glide_offset",
                "Glide pitch offset",
                "n04.offset",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!(
                "mirror_angle",
                "Mirror angle",
                "n05.angle",
                Time,
                -90,
                90,
                Replace
            ),
            sweep!(
                "mirror_offset",
                "Mirror line offset",
                "n05.offset",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!(
                "region_x",
                "Region center X",
                "n08.cx",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "region_y",
                "Region center Y",
                "n08.cy",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "region_size",
                "Region size",
                "n08.size",
                Time,
                1,
                200,
                Replace
            ),
            sweep!(
                "recursion_depth",
                "Recursion depth",
                "n09.depth",
                Time,
                1,
                6,
                Replace
            ),
            sweep!(
                "axis_range",
                "Moving axis range",
                "n11.range",
                Time,
                1,
                48,
                Replace
            ),
        ],
        // V09-S39 step 7: evaluated per note by the kernel
        // ([`kernel_sweeps`]): each note's copies take its own values; the
        // controllers (n08) follow each track's first note.
        "duplicate" => &[
            sweep!(
                "layers",
                "Copies on each track",
                "n01.count",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "canon_factor",
                "Canon factor",
                "n02.factor",
                Time,
                50,
                200,
                Replace
            ),
            sweep!(
                "entry_delay",
                "Canon entry delay",
                "n02.delay",
                Time,
                240,
                3840,
                Replace
            ),
            sweep!(
                "entries",
                "Canon entries",
                "n02.entries",
                Time,
                1,
                4,
                Replace
            ),
            sweep!(
                "rotation",
                "Pitch rotation per copy",
                "n04.rotate",
                Time,
                -4,
                4,
                Replace
            ),
            sweep!(
                "path_range",
                "Path pitch range",
                "n05.range",
                Time,
                1,
                24,
                Replace
            ),
            sweep!(
                "path_spacing",
                "Path spacing",
                "n05.spacing",
                Time,
                240,
                3840,
                Replace
            ),
            sweep!(
                "path_copies",
                "Path copies",
                "n05.copies",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "keep",
                "Thinning: keep per copy",
                "n09.keep",
                Time,
                25,
                100,
                Replace
            ),
            sweep!(
                "stutter_times",
                "Stutter times",
                "n10.times",
                Time,
                1,
                4,
                Replace
            ),
        ],
        // V09-S39 step 8: evaluated per note by the kernel
        // ([`kernel_sweeps`]); a phrase's rest takes its first note's
        // factor.
        "stretch" => &[
            sweep!(
                "meter_to",
                "Meter: to beats per bar",
                "n01.to",
                Time,
                2,
                8,
                Replace
            ),
            sweep!(
                "short_keep",
                "Short-note keep below",
                "n02.short",
                Time,
                30,
                960,
                Replace
            ),
            sweep!(
                "nps_limit",
                "NPS limit",
                "n04.limit",
                Time,
                100,
                10000,
                Replace
            ),
            sweep!(
                "rest_factor",
                "Rest factor",
                "n05.factor",
                Time,
                0,
                400,
                Replace
            ),
            sweep!(
                "groove_keep",
                "Groove keep",
                "n06.keep",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "tape_semitones",
                "Tape semitones per doubling",
                "n07.semis",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!(
                "stop_length",
                "Tape stop length",
                "n08.len",
                Time,
                480,
                7680,
                Replace
            ),
            sweep!(
                "stop_drop",
                "Tape stop pitch drop",
                "n08.drop",
                Time,
                0,
                24,
                Replace
            ),
            sweep!(
                "factor_before",
                "Factor before the pivot",
                "n10.before",
                Time,
                50,
                200,
                Replace
            ),
            sweep!(
                "factor_after",
                "Factor after the pivot",
                "n10.after",
                Time,
                50,
                200,
                Replace
            ),
        ],
        "paint" => &[
            sweep!("gate", "Gate", "gate", Time, 25, 200, Replace),
            sweep!("velocity", "Velocity", "vel", Time, 40, 127, Replace),
            sweep!(
                "soft_radius",
                "Soft brush radius",
                "n03.radius",
                Time,
                1,
                12,
                Replace
            ),
            sweep!(
                "hardness",
                "Soft brush hardness",
                "n03.hard",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "smudge_strength",
                "Smudge strength",
                "n05.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "recolour",
                "Recolor color",
                "n06.colour",
                Time,
                0,
                3,
                Replace
            ),
            sweep!(
                "stamp_rotation",
                "Stamp rotation step",
                "n07.rot",
                Time,
                -45,
                45,
                Replace
            ),
            sweep!(
                "stamp_scale",
                "Stamp scale step",
                "n07.scale",
                Time,
                -20,
                20,
                Replace
            ),
            sweep!(
                "dither_level",
                "Dither level",
                "n08.level",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "brush_velocity",
                "Velocity brush amount",
                "n11.vel",
                Time,
                1,
                127,
                Replace
            ),
        ],
        "paint_color" => &[
            sweep!("subchannel", "Subchannel", "sub", Time, 0, 3, Replace),
            sweep!("polar_x", "Polar center X", "n01.cx", Time, 0, 100, Replace),
            sweep!(
                "polar_band",
                "Polar band width",
                "n01.band",
                Time,
                2,
                40,
                Replace
            ),
            sweep!("edge", "Edge color", "n02.edge", Time, 0, 3, Replace),
            sweep!("dither_a", "Dither color A", "n03.a", Time, 0, 3, Replace),
            sweep!(
                "hatch_angle",
                "Hatch angle",
                "n05.angle",
                Time,
                -90,
                90,
                Replace
            ),
            sweep!(
                "hatch_spacing",
                "Hatch spacing",
                "n05.spacing",
                Time,
                2,
                24,
                Replace
            ),
            sweep!("glyph_size", "Glyph size", "n06.size", Time, 7, 56, Replace),
            sweep!(
                "glyph_x",
                "Glyph position X",
                "n06.x",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "cycle_period",
                "Cycle period",
                "n08.period",
                Time,
                240,
                7680,
                Replace
            ),
        ],
        // V09-S38 step 2: evaluated per note by the kernel
        // ([`kernel_sweeps`]).
        "wave" => &[
            sweep!("sx", "Time displacement", "sx", Time, 0, 200, Replace),
            sweep!("sy", "Pitch displacement", "sy", Time, 0, 200, Replace),
            sweep!(
                "sync",
                "Synced wavelength",
                "n01.len",
                Time,
                0.25,
                8,
                Replace
            ),
            sweep!("wavy", "Wavy amplitude", "n02.amp", Time, 1, 12, Replace),
            sweep!(
                "depth",
                "Modulation depth",
                "n04.depth",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "gate",
                "Gate threshold",
                "n05.threshold",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!(
                "fold",
                "Fold threshold",
                "n07.threshold",
                Time,
                1,
                100,
                Replace
            ),
            sweep!("hold", "Hold length", "n08.hold", Time, 30, 1920, Replace),
            sweep!("travel", "Travel speed", "n09.speed", Time, -8, 8, Replace),
            sweep!(
                "distortion",
                "Phase distortion",
                "n11.amount",
                Time,
                0,
                100,
                Replace
            ),
        ],
        // V09-S38 step 1 (note-level).
        // V09-S40 step 1: evaluated per note by the kernel
        // ([`kernel_sweeps`]).
        // V09-S40 step 7: evaluated by the kernel per cell, from the
        // level of the note that represents it ([`kernel_sweeps`]; the
        // grid-wide hull, shapes and walk direction are not swept).
        // V09-S40 step 6: evaluated per note by the kernel
        // ([`kernel_sweeps`]; per level, the platform's values; n03's core
        // is added once per run, so its controls are not swept).
        "trails" => &[
            sweep!("count", "Trail Count", "count", Pitch, 0, 16, Replace),
            sweep!(
                "separation",
                "Trail Separation",
                "separation",
                Pitch,
                0,
                960,
                Replace
            ),
            sweep!(
                "intensity",
                "Trail Intensity",
                "intensity",
                Time,
                20,
                100,
                Replace
            ),
            sweep!(
                "scale_x",
                "Scale Trails X",
                "scale_x",
                Time,
                50,
                150,
                Replace
            ),
            sweep!(
                "pos_x",
                "Position X Trails",
                "pos_x",
                Pitch,
                -20,
                20,
                Replace
            ),
            sweep!(
                "pos_y",
                "Position Y Trails",
                "pos_y",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!("rotate", "Rotate Trails", "rotate", Time, -45, 45, Replace),
            sweep!("tumble", "Tumble Trails", "tumble", Time, -45, 45, Replace),
            sweep!(
                "fb_disp",
                "Displacement Feedback",
                "fb.disp",
                Time,
                50,
                150,
                Replace
            ),
            sweep!("gate", "Gate per trail", "gate", Velocity, 50, 100, Replace),
        ],
        // V09-S41: evaluated per note by the kernel ([`kernel_sweeps`]).
        // V09-S41: evaluated per note by the kernel ([`kernel_sweeps`]).
        // V09-S41: evaluated per note by the kernel ([`kernel_sweeps`]).
        "flow_field" => &[
            sweep!("strength", "Strength", "strength", Time, 0, 100, Replace),
            sweep!("turns", "Field turns", "n02.turns", Time, -2, 2, Replace),
            sweep!("drift", "Field drift", "n02.drift", Time, 0, 60, Replace),
            sweep!(
                "colour_steps",
                "Color steps",
                "n03.steps",
                Time,
                1,
                32,
                Replace
            ),
            sweep!(
                "obstacle_radius",
                "Obstacle radius",
                "n05.radius",
                Time,
                1,
                12,
                Replace
            ),
            sweep!("streak", "Streak", "n06.streak", Time, 0, 300, Replace),
            sweep!("layers", "Shear layers", "n08.layers", Time, 2, 12, Replace),
            sweep!("curl", "Layer curl", "n08.curl", Time, 0, 100, Replace),
            sweep!(
                "fade_radius",
                "Fade radius",
                "n09.radius",
                Time,
                20,
                150,
                Replace
            ),
            sweep!(
                "path_width",
                "Path width",
                "n10.width",
                Time,
                5,
                50,
                Replace
            ),
        ],
        "particle_burst" => &[
            sweep!("distance", "Distance", "dist", Time, 2, 40, Replace),
            sweep!(
                "duration",
                "Emission duration",
                "n02.duration",
                Time,
                0,
                1920,
                Replace
            ),
            sweep!("rate", "Emission rate", "n02.rate", Time, 1, 16, Replace),
            sweep!(
                "restitution",
                "Restitution",
                "n03.restitution",
                Time,
                0,
                100,
                Replace
            ),
            sweep!("trail", "Trail length", "n04.length", Time, 0, 960, Replace),
            sweep!(
                "strength",
                "Attraction strength",
                "n05.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "wind_time",
                "Wind in time",
                "n06.time",
                Time,
                -50,
                50,
                Replace
            ),
            sweep!(
                "wind_pitch",
                "Wind in pitch",
                "n06.pitch",
                Time,
                -50,
                50,
                Replace
            ),
            sweep!("size", "Shape size", "n10.size", Time, 2, 40, Replace),
            sweep!(
                "turbulence",
                "Turbulence",
                "n11.amount",
                Time,
                0,
                100,
                Replace
            ),
        ],
        "pulse_field" => &[
            sweep!("amplitude", "Amplitude", "amp", Time, 0, 100, Replace),
            sweep!("cycles", "Cycles", "cycles", Time, 1, 16, Replace),
            sweep!(
                "unit_cycles",
                "Cycles per unit",
                "n01.cycles",
                Time,
                0.25,
                4,
                Replace
            ),
            sweep!("decay", "Decay", "n02.decay", Time, 60, 1920, Replace),
            sweep!(
                "depth",
                "Dynamics depth",
                "n03.depth",
                Time,
                0,
                100,
                Replace
            ),
            sweep!("split", "Split radius", "n12.split", Time, 10, 90, Replace),
            sweep!(
                "counter",
                "Counter phase",
                "n12.phase",
                Time,
                0,
                360,
                Replace
            ),
            sweep!(
                "pitch_offset",
                "Pitch phase offset",
                "n07.offset",
                Time,
                0,
                360,
                Replace
            ),
            sweep!(
                "growth",
                "Growth per cycle",
                "n08.growth",
                Time,
                -25,
                25,
                Replace
            ),
            sweep!("duty", "Strobe duty", "n10.duty", Time, 10, 90, Replace),
        ],
        "ripple" => &[
            sweep!("height", "Height", "height", Time, 0, 20, Replace),
            sweep!("perp", "Perpendicular Height", "perp", Time, 0, 20, Replace),
            sweep!("radius", "Radius", "radius", Time, 10, 60, Replace),
            sweep!("width", "Wave Width", "width", Time, 2, 30, Replace),
            sweep!("speed", "Speed", "speed", Time, 0, 8, Replace),
            sweep!("phase", "Phase", "phase", Time, 0, 360, Replace),
            sweep!("falloff", "Falloff", "falloff", Time, 0, 100, Replace),
            sweep!("cx", "Center Point: time", "cx", Time, 25, 75, Replace),
            sweep!("cy", "Center Point: pitch", "cy", Time, 25, 75, Replace),
            sweep!("light", "Light", "light", Time, 0, 100, Replace),
        ],
        "tiny_planet" => &[
            sweep!("latitude", "Latitude", "latitude", Time, -90, 90, Replace),
            sweep!("longitude", "Longitude", "longitude", Time, 0, 360, Replace),
            sweep!("zoom", "Z (zoom distance)", "zoom", Time, 1, 3, Replace),
            sweep!("cx", "Center in time", "cx", Time, 25, 75, Replace),
            sweep!("cy", "Center in pitch", "cy", Time, 25, 75, Replace),
            sweep!("radius", "Radius scale", "radius", Time, 50, 150, Replace),
            sweep!("seam", "Seam position", "seam", Time, 0, 100, Replace),
            sweep!("spiral", "Spiral", "spiral", Time, 0, 50, Replace),
            sweep!(
                "phase_steps",
                "Subchannel steps",
                "phase.steps",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "turn_n",
                "Note values a turn",
                "turn.n",
                Time,
                1,
                8,
                Replace
            ),
        ],
        "displacement_map" => &[
            sweep!(
                "master",
                "Master displacement",
                "master",
                Time,
                0,
                200,
                Replace
            ),
            sweep!(
                "h_max",
                "Maximum time displacement",
                "h.max",
                Time,
                0,
                960,
                Replace
            ),
            sweep!(
                "v_max",
                "Maximum pitch displacement",
                "v.max",
                Time,
                0,
                24,
                Replace
            ),
            sweep!(
                "h_ref",
                "Time reference level",
                "h.ref",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "v_ref",
                "Pitch reference level",
                "v.ref",
                Time,
                0,
                100,
                Replace
            ),
            sweep!("black", "Map black level", "black", Time, 0, 50, Replace),
            sweep!("white", "Map white level", "white", Time, 50, 100, Replace),
            sweep!("gamma", "Map gamma", "gamma", Time, 1, 4, Replace),
            sweep!(
                "map_dx",
                "Map offset in time",
                "map.dx",
                Time,
                0,
                3840,
                Replace
            ),
            sweep!(
                "map_dy",
                "Map offset in pitch",
                "map.dy",
                Time,
                -12,
                12,
                Replace
            ),
        ],
        "note_outline" => &[
            sweep!(
                "thick_x",
                "Thickness along time",
                "thick_x",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "thick_y",
                "Thickness across pitch",
                "thick_y",
                Time,
                0,
                4,
                Replace
            ),
            sweep!("offset", "Offset", "offset", Time, 0, 8, Replace),
            sweep!(
                "vel_scale",
                "Velocity (of the nearest note)",
                "vel.scale",
                Time,
                20,
                150,
                Replace
            ),
            sweep!("vel", "Fixed velocity", "vel", Pitch, 20, 127, Replace),
            sweep!(
                "sub_step",
                "Subchannel steps",
                "sub_step",
                Pitch,
                0,
                8,
                Replace
            ),
            sweep!("delay", "Outline delay", "delay", Time, 0, 960, Replace),
            sweep!("gate", "Outline gate", "gate", Time, 25, 100, Replace),
            sweep!("dash", "Dash", "dash", Time, 60, 960, Replace),
            sweep!("gap", "Dash gap", "gap", Time, 30, 480, Replace),
        ],
        "warp_transform" => &[
            sweep!("scale_x", "Scale X", "scale_x", Time, 25, 200, Replace),
            sweep!("scale_y", "Scale Y", "scale_y", Time, 25, 200, Replace),
            sweep!("shift_x", "Shift X", "shift_x", Pitch, -960, 960, Replace),
            sweep!("shift_y", "Shift Y", "shift_y", Time, -12, 12, Replace),
            sweep!("z_dist", "Z Distance", "z_dist", Time, 0.5, 3, Replace),
            sweep!("rotate", "Rotate", "rotate", Time, -180, 180, Replace),
            sweep!("swivel", "Swivel", "swivel", Time, -60, 60, Replace),
            sweep!("tilt", "Tilt", "tilt", Time, -60, 60, Replace),
            sweep!(
                "persp",
                "Perspective Amount",
                "persp",
                Time,
                0.25,
                4,
                Replace
            ),
            sweep!("shear_x", "Shear X", "shear_x", Pitch, -100, 100, Replace),
        ],
        "tumours" => &[
            sweep!("scale", "Growth scale", "scale", Time, 50, 200, Replace),
            sweep!("ring", "Generation ring", "ring", Time, 1, 6, Replace),
            sweep!(
                "lobes",
                "Lobe amount",
                "c02_lobe_count.amount",
                Time,
                0,
                200,
                Replace
            ),
            sweep!(
                "branch",
                "Branch chance",
                "c05_branch_probability.probability",
                Time,
                0,
                60,
                Replace
            ),
            sweep!(
                "branch_len",
                "Branch length",
                "c05_branch_probability.length",
                Time,
                4,
                24,
                Replace
            ),
            sweep!(
                "nodules",
                "Nodule frequency",
                "c07_nodule_frequency.frequency",
                Time,
                0,
                20,
                Replace
            ),
            sweep!(
                "rough",
                "Edge roughness",
                "c09_edge_roughness.amount",
                Time,
                0,
                4,
                Replace
            ),
            sweep!(
                "sub",
                "Subchannel step",
                "c19_subchannel_mutation.step",
                Time,
                -4,
                4,
                Replace
            ),
            sweep!(
                "vel",
                "Velocity per generation",
                "c20_velocity_mutation.decay",
                Time,
                50,
                120,
                Replace
            ),
            sweep!(
                "gate",
                "Gate of the first generation",
                "c21_gate_mutation.start",
                Time,
                20,
                100,
                Replace
            ),
        ],
        "tool3d" => &[
            sweep!("yaw", "Camera yaw", "cam.yaw", Time, -90, 90, Replace),
            sweep!("pitch", "Camera pitch", "cam.pitch", Time, -60, 60, Replace),
            sweep!("roll", "Camera roll", "cam.roll", Time, -90, 90, Replace),
            sweep!("rot_x", "Rotation X", "rot.x", Time, -180, 180, Replace),
            sweep!("rot_y", "Rotation Y", "rot.y", Time, -180, 180, Replace),
            sweep!("rot_z", "Rotation Z", "rot.z", Pitch, -180, 180, Replace),
            sweep!("focal", "Focal depth", "focal", Time, 40, 600, Replace),
            sweep!("z_scale", "Z scale", "z.scale", Time, -200, 200, Replace),
            sweep!(
                "z_offset", "Z offset", "z.offset", Pitch, -100, 100, Replace
            ),
            sweep!(
                "far_velocity",
                "Farthest velocity",
                "d.vel.far",
                Velocity,
                0,
                100,
                Replace
            ),
        ],
        "vortex_trails" => &[
            sweep!(
                "delay",
                "Delay per trail",
                "n01.delay",
                Time,
                0,
                960,
                Replace
            ),
            sweep!(
                "semis",
                "Semitones per trail",
                "n02.semis",
                Time,
                -12,
                12,
                Replace
            ),
            sweep!("merge_at", "Merge at", "n04.at", Time, 0, 100, Replace),
            sweep!(
                "final_strength",
                "Final strength",
                "n04.final",
                Time,
                0,
                200,
                Replace
            ),
            sweep!(
                "per_beat",
                "Trails per beat",
                "n05.per_beat",
                Time,
                1,
                8,
                Replace
            ),
            sweep!("tilt", "Tilt per trail", "n06.tilt", Time, -60, 60, Replace),
            sweep!(
                "window",
                "Window per trail",
                "n07.window",
                Time,
                10,
                100,
                Replace
            ),
            sweep!(
                "depth",
                "Depth per trail",
                "n08.depth",
                Time,
                0,
                50,
                Replace
            ),
            sweep!("every", "Keep every", "n10.every", Time, 1, 4, Replace),
            sweep!(
                "progress",
                "Progress per trail",
                "n11.progress",
                Time,
                0,
                100,
                Replace
            ),
        ],
        // V09-S40 step 5: evaluated per note by the kernel
        // ([`kernel_sweeps`]; per level, the platform's values).
        "orbit" => &[
            sweep!("tilt", "Tilt", "n02.tilt", Time, -60, 60, Replace),
            sweep!("depth", "Depth scale", "n02.depth", Time, 0, 100, Replace),
            sweep!(
                "eccentricity",
                "Eccentricity",
                "n03.e",
                Time,
                0,
                90,
                Replace
            ),
            sweep!(
                "periapsis",
                "Periapsis angle",
                "n03.peri",
                Time,
                -180,
                180,
                Replace
            ),
            sweep!("spin", "Spin per copy", "n04.spin", Time, -90, 90, Replace),
            sweep!("mass", "Mass ratio", "n05.mass", Time, 1, 50, Replace),
            sweep!(
                "spacing",
                "Arc spacing",
                "n06.spacing",
                Time,
                5,
                50,
                Replace
            ),
            sweep!("body", "Body radius", "n08.radius", Time, 0, 50, Replace),
            sweep!(
                "precession",
                "Precession per ring",
                "n09.angle",
                Time,
                -90,
                90,
                Replace
            ),
            sweep!(
                "moon_radius",
                "Moon radius",
                "n10.radius",
                Time,
                5,
                50,
                Replace
            ),
        ],
        // V09-S40 step 4: evaluated per note by the kernel
        // ([`kernel_sweeps`]; per level, the platform's values).
        "kaleidoscope" => &[
            sweep!(
                "cell_width",
                "Cell width",
                "n01.width",
                Time,
                480,
                7680,
                Replace
            ),
            sweep!(
                "cell_height",
                "Cell height",
                "n01.height",
                Time,
                2,
                24,
                Replace
            ),
            sweep!("repeats", "Repeats in time", "n01.rt", Time, 1, 8, Replace),
            sweep!("turns", "Spin turns", "n02.turns", Time, -4, 4, Replace),
            sweep!(
                "sector_delay",
                "Delay per sector",
                "n03.delay",
                Time,
                0,
                960,
                Replace
            ),
            sweep!("rings", "Rings", "n06.rings", Time, 1, 4, Replace),
            sweep!(
                "ring_spacing",
                "Ring spacing",
                "n06.spacing",
                Time,
                5,
                50,
                Replace
            ),
            sweep!("broken", "Broken sectors", "n08.count", Time, 0, 8, Replace),
            sweep!(
                "perturbation",
                "Perturbation",
                "n08.amount",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "local_sectors",
                "Local sectors",
                "n11.sectors",
                Time,
                2,
                12,
                Replace
            ),
        ],
        // V09-S40 step 3: evaluated per note by the kernel
        // ([`kernel_sweeps`]; per level, the platform's values).
        // V09-S40 step 2: evaluated per note by the kernel
        // ([`kernel_sweeps`]).
        "gravity_well" => &[
            sweep!("strength", "Strength", "strength", Time, 0, 100, Replace),
            sweep!("radius", "Radius", "radius", Time, 1, 200, Replace),
            sweep!(
                "einstein",
                "Einstein radius",
                "n02.radius",
                Time,
                1,
                100,
                Replace
            ),
            sweep!(
                "tidal",
                "Tidal strength",
                "n03.tidal",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "turns",
                "Accretion turns",
                "n04.turns",
                Time,
                0,
                32,
                Replace
            ),
            sweep!(
                "escape",
                "Escape velocity",
                "n05.vel",
                Time,
                1,
                127,
                Replace
            ),
            sweep!(
                "gain",
                "Potential gain",
                "n06.gain",
                Time,
                -100,
                200,
                Replace
            ),
            sweep!(
                "separation",
                "Binary separation",
                "n09.sep",
                Time,
                1,
                100,
                Replace
            ),
            sweep!("reach", "Force reach", "n10.reach", Time, 1, 200, Replace),
            sweep!(
                "dilation",
                "Time dilation",
                "n11.dilation",
                Time,
                0,
                100,
                Replace
            ),
        ],
        "twirl" => &[
            sweep!("angle", "Twirl angle", "angle", Time, -720, 720, Replace),
            sweep!(
                "path_speed",
                "Path speed",
                "n01.speed",
                Time,
                10,
                400,
                Replace
            ),
            sweep!(
                "local_angle",
                "Local vortex angle",
                "n02.angle",
                Time,
                -720,
                720,
                Replace
            ),
            sweep!("arm_width", "Arm width", "n03.width", Time, 1, 180, Replace),
            sweep!("void", "Void radius", "n04.radius", Time, 0, 100, Replace),
            sweep!(
                "sector",
                "Sector start",
                "n05.start",
                Time,
                -180,
                180,
                Replace
            ),
            sweep!(
                "street",
                "Vortex spacing",
                "n06.spacing",
                Time,
                1,
                100,
                Replace
            ),
            sweep!(
                "snap",
                "Ring snap strength",
                "n08.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "divergence",
                "Phyllotaxis divergence",
                "n09.divergence",
                Time,
                1,
                359,
                Replace
            ),
            sweep!("noise", "Turbulence", "n10.noise", Time, 0, 180, Replace),
        ],
        // V09-S37 step 5 (note-level).
        "shear" => &[
            sweep!("amount", "Shear amount", "amount", Time, -200, 200, Replace),
            sweep!(
                "contour",
                "Contour scale",
                "n01.scale",
                Time,
                -200,
                200,
                Replace
            ),
            sweep!(
                "slope_len",
                "Slope lengths amount",
                "n02.amount",
                Time,
                0,
                200,
                Replace
            ),
            sweep!("noise", "Noise amplitude", "n03.amp", Time, 0, 200, Replace),
            sweep!("axis", "Split axis", "n04.axis", Time, 0, 127, Replace),
            sweep!(
                "slope",
                "Slope scale",
                "n05.scale",
                Time,
                -200,
                200,
                Replace
            ),
            sweep!(
                "tempo",
                "Tempo scale",
                "n06.scale",
                Time,
                -200,
                200,
                Replace
            ),
            sweep!("band_low", "Band low", "n07.lo", Time, 0, 127, Replace),
            sweep!(
                "pulse",
                "Pulse amplitude",
                "n08.amp",
                Time,
                -24,
                24,
                Replace
            ),
            sweep!(
                "copies",
                "Strength copies",
                "n09.copies",
                Time,
                1,
                16,
                Replace
            ),
        ],
        "repeat" => &[
            sweep!("count", "Repeats per note", "n01.n", Time, 1, 64, Replace),
            sweep!("delay", "Echo delay", "n02.delay", Time, 30, 1920, Replace),
            sweep!(
                "feedback",
                "Echo feedback by velocity",
                "n02.feedback",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "restitution",
                "Bounce restitution",
                "n03.restitution",
                Time,
                0.1,
                0.95,
                Replace
            ),
            sweep!("grain", "Grain length", "n05.len", Time, 10, 480, Replace),
            sweep!(
                "density",
                "Grains per beat by pitch",
                "n05.density",
                Pitch,
                1,
                64,
                Replace
            ),
            sweep!("peak", "Doppler peak", "n13.peak", Time, 0, 100, Replace),
            sweep!("depth", "Doppler depth", "n13.depth", Time, 0, 400, Replace),
            sweep!("hold", "Hold before", "n10.share", Time, 0, 95, Replace),
            sweep!(
                "overlap",
                "Repeat overlap by velocity",
                "n14.overlap",
                Velocity,
                100,
                400,
                Replace
            ),
        ],
        "transpose" => &[
            sweep!("semis", "Semitones", "semis", Time, -24, 24, Add),
            sweep!("octaves", "Octaves", "octaves", Time, -2, 2, Add),
            sweep!("degrees", "Scale degrees", "a1.deg", Time, -7, 7, Add),
            sweep!(
                "diatonic",
                "Diatonic degrees by pitch",
                "a2.deg",
                Pitch,
                -7,
                7,
                Add
            ),
            sweep!("axis", "Inversion axis", "a3.axis", Time, 36, 84, Replace),
            sweep!("fold_lo", "Fold floor", "a4.lo", Time, 24, 72, Replace),
            sweep!("fold_hi", "Fold ceiling", "a4.hi", Time, 60, 108, Replace),
            sweep!(
                "depth",
                "Envelope depth by note",
                "a6.depth",
                NoteIndex,
                -24,
                24,
                Replace
            ),
            sweep!("roots", "Chord-root degrees", "a7.deg", Time, -7, 7, Add),
            sweep!(
                "ceiling",
                "Register ceiling by velocity",
                "a8.hi",
                Velocity,
                60,
                108,
                Replace
            ),
        ],
        "arpeggiate" => &[
            sweep!("gate", "Gate", "gate", Time, 10, 100, Replace),
            sweep!("range", "Octave range", "range", Time, 1, 4, Replace),
            sweep!(
                "pulses",
                "Euclidean pulses",
                "n01.pulses",
                Time,
                0,
                16,
                Replace
            ),
            sweep!(
                "rotation",
                "Euclidean rotation",
                "n01.rot",
                Time,
                -8,
                8,
                Add
            ),
            sweep!(
                "burst_gate",
                "Burst gate by velocity",
                "n02.gate",
                Velocity,
                5,
                100,
                Replace
            ),
            sweep!("rate", "Start rate", "n03.from", Time, 1, 64, Replace),
            sweep!("stride", "Stride", "n04.stride", Time, 1, 8, Replace),
            sweep!(
                "passing",
                "Passing notes by pitch",
                "n05.most",
                Pitch,
                0,
                6,
                Replace
            ),
            sweep!(
                "anchor_vel",
                "Anchor velocity",
                "n07.vel",
                Velocity,
                10,
                150,
                Replace
            ),
            sweep!(
                "cycles",
                "Cycles per chord",
                "n08.cycles",
                Time,
                0.5,
                8,
                Replace
            ),
        ],
        "articulate" => &[
            sweep!("gap", "Real-time gap", "n01.gap", Time, 0, 250, Replace),
            sweep!(
                "phrase_end",
                "Phrase-end length",
                "n03.last",
                Time,
                50,
                400,
                Replace
            ),
            sweep!(
                "phrase_rest",
                "Phrase split rest",
                "n03.rest",
                Time,
                120,
                1920,
                Replace
            ),
            sweep!("dense", "Dense gate", "n04.dense", Time, 10, 200, Replace),
            sweep!(
                "legato",
                "Legato interval by pitch",
                "n05.within",
                Pitch,
                0,
                12,
                Replace
            ),
            sweep!(
                "fast",
                "Gate at the fast pivot",
                "n06.at_fast",
                Time,
                10,
                200,
                Replace
            ),
            sweep!(
                "strength",
                "Palette strength by velocity",
                "n07.strength",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "top",
                "Top voice gate by pitch",
                "n08.top",
                Pitch,
                25,
                200,
                Replace
            ),
            sweep!(
                "decay",
                "Change per repeat",
                "n09.change",
                Time,
                -50,
                50,
                Replace
            ),
            sweep!(
                "polyphony",
                "Most sounding notes",
                "n11.most",
                Time,
                1,
                256,
                Replace
            ),
        ],
        "chop" => &[
            sweep!(
                "pulses",
                "Euclidean pulses",
                "n01.pulses",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "rotation",
                "Euclidean rotation by pitch",
                "n01.rot",
                Pitch,
                -8,
                8,
                Add
            ),
            sweep!("rate", "Start rate", "n02.from", Time, 1, 64, Replace),
            sweep!(
                "interval",
                "Glissando interval",
                "n03.interval",
                Time,
                -24,
                24,
                Replace
            ),
            sweep!(
                "voices",
                "Voices per slice",
                "n04.voices",
                Time,
                1,
                8,
                Replace
            ),
            sweep!("nps", "Target NPS", "n05.nps", Time, 100, 20000, Replace),
            sweep!(
                "bridge",
                "Velocity bridge by velocity",
                "n06.amount",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "phase",
                "Phase per step by pitch",
                "n07.phase",
                Pitch,
                -50,
                50,
                Replace
            ),
            sweep!(
                "fill",
                "Longest fill",
                "n08.longest",
                Time,
                1,
                1920,
                Replace
            ),
            sweep!(
                "merge",
                "Merge cuts within",
                "n10.merge",
                Time,
                0,
                480,
                Replace
            ),
        ],
        "claw" => &[
            sweep!("end", "End period", "n01.end", Time, 240, 7680, Replace),
            sweep!(
                "warp_vel",
                "Warp velocity amount",
                "n02.vel",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!(
                "warp_len",
                "Warp length amount",
                "n02.len",
                Time,
                -100,
                100,
                Replace
            ),
            sweep!(
                "slices",
                "Slices per period",
                "n05.slices",
                Time,
                2,
                16,
                Replace
            ),
            sweep!("keep", "Slices kept", "n06.keep", Time, 1, 8, Replace),
            sweep!(
                "fill_vel",
                "Fill velocity by velocity",
                "n07.vel",
                Velocity,
                1,
                150,
                Replace
            ),
            sweep!(
                "drift",
                "Drift per period",
                "n08.drift",
                Time,
                -960,
                960,
                Replace
            ),
            sweep!(
                "new_period",
                "New period",
                "n09.period",
                Time,
                960,
                7680,
                Replace
            ),
            sweep!(
                "strength",
                "Equalizer strength",
                "n10.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "wet",
                "Wet velocity by velocity",
                "n11.wet",
                Velocity,
                0,
                150,
                Replace
            ),
        ],
        "quantize" => &[
            sweep!("swing", "Swing", "n01.swing", Time, 50, 80, Replace),
            sweep!(
                "frame_ms",
                "Frame grid milliseconds",
                "n02.ms",
                Time,
                1,
                100,
                Replace
            ),
            sweep!(
                "radius",
                "Search radius by pitch",
                "n03.radius",
                Pitch,
                1,
                16,
                Replace
            ),
            sweep!(
                "cluster",
                "Cluster window",
                "n04.window",
                Time,
                0,
                240,
                Replace
            ),
            sweep!(
                "spacing_from",
                "Elastic start spacing",
                "n05.from",
                Time,
                60,
                1920,
                Replace
            ),
            sweep!(
                "spacing_to",
                "Elastic end spacing",
                "n05.to",
                Time,
                60,
                1920,
                Replace
            ),
            sweep!(
                "groove",
                "Self-groove strength by velocity",
                "n07.strength",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "magnet",
                "Magnet distance",
                "n09.dist",
                Time,
                0,
                480,
                Replace
            ),
            sweep!("unit", "IOI unit", "n10.unit", Time, 30, 960, Replace),
            sweep!(
                "levels",
                "Velocity levels",
                "n11.levels",
                Time,
                2,
                32,
                Replace
            ),
        ],
        "randomize" => &[
            sweep!("note_note", "Note → note", "n02.nn", Time, 0, 100, Replace),
            sweep!("rest_note", "Rest → note", "n02.rn", Time, 0, 100, Replace),
            sweep!(
                "tension",
                "Tension notes",
                "n03.tension",
                Time,
                0,
                100,
                Replace
            ),
            sweep!("bursts", "Bursts per bar", "n04.rate", Time, 0, 16, Replace),
            sweep!(
                "burst_notes",
                "Notes per burst",
                "n04.notes",
                Time,
                1,
                64,
                Replace
            ),
            sweep!(
                "density",
                "Shape density",
                "n05.density",
                Time,
                0.1,
                8,
                Replace
            ),
            sweep!(
                "mutation",
                "Mutation per repeat",
                "n06.mutation",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "scatter",
                "Scatter probability by pitch",
                "n07.prob",
                Pitch,
                0,
                100,
                Replace
            ),
            sweep!(
                "echo",
                "Echo probability by velocity",
                "n08.prob",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!("decay", "Echo decay", "n08.decay", Time, 0, 90, Replace),
        ],
        "strum" => &[
            sweep!("real", "Real-time strum", "n01.ms", Time, 0, 200, Replace),
            sweep!("up", "Up-stroke time", "n02.up", Time, 10, 200, Replace),
            sweep!(
                "share",
                "Share of the chord",
                "n03.share",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "octaves",
                "Run octaves by pitch",
                "n04.octaves",
                Pitch,
                0,
                4,
                Replace
            ),
            sweep!(
                "run_vel",
                "Run velocity by velocity",
                "n04.vel",
                Velocity,
                10,
                150,
                Replace
            ),
            sweep!("leap", "Leap threshold", "n05.leap", Time, 1, 24, Replace),
            sweep!(
                "voices",
                "Voices per group",
                "n06.voices",
                Time,
                1,
                8,
                Replace
            ),
            sweep!("scale", "Time scale", "n08.scale", Time, 0, 400, Replace),
            sweep!("window", "Wall window", "n09.window", Time, 0, 960, Replace),
            sweep!(
                "per_semitone",
                "Ticks per semitone",
                "n10.per",
                Time,
                0,
                60,
                Replace
            ),
        ],
        // V09-S35: the Flam's numeric controls of n01 – n11 (a swept n11
        // budget applies within each band of the sweep's groups).
        "flam" => &[
            sweep!(
                "interval",
                "Approach interval by pitch",
                "n01.interval",
                Pitch,
                -12,
                12,
                Replace
            ),
            sweep!(
                "threshold",
                "Accent threshold",
                "n03.threshold",
                Time,
                1,
                127,
                Replace
            ),
            sweep!(
                "accent_strokes",
                "Strokes at 127",
                "n03.strokes",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "cluster",
                "Cluster size by pitch",
                "n05.size",
                Pitch,
                1,
                12,
                Replace
            ),
            sweep!(
                "spread",
                "Mirror spread",
                "n07.spread",
                Time,
                1,
                12,
                Replace
            ),
            sweep!(
                "twin_vel",
                "Twin velocity by velocity",
                "n07.vel",
                Velocity,
                10,
                150,
                Replace
            ),
            sweep!("lead", "Roll lead-in", "n08.lead", Time, 10, 2000, Replace),
            sweep!(
                "roll_to",
                "Roll end rate by velocity",
                "n08.to",
                Velocity,
                1,
                200,
                Replace
            ),
            sweep!(
                "grid",
                "Stroke grid strength",
                "n09.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "budget",
                "NPS budget over time",
                "n11.nps",
                Time,
                100,
                20000,
                Replace
            ),
        ],
        // V09-S35: the Limit's numeric controls of n01 – n11 (a swept
        // n03 / n04 / n07 / n11 limit applies within each band of the
        // sweep's groups).
        "limit" => &[
            sweep!(
                "vel_hi",
                "Velocity ceiling over time",
                "n01.hi",
                Time,
                1,
                127,
                Replace
            ),
            sweep!(
                "longest",
                "Longest length by pitch",
                "n02.longest",
                Pitch,
                30,
                3840,
                Replace
            ),
            sweep!(
                "poly",
                "Polyphony cap over time",
                "n03.most",
                Time,
                1,
                64,
                Replace
            ),
            sweep!(
                "onsets",
                "Onset limit over time",
                "n04.most",
                Time,
                1,
                2000,
                Replace
            ),
            sweep!(
                "leap",
                "Largest leap over time",
                "n05.leap",
                Time,
                1,
                24,
                Replace
            ),
            sweep!(
                "span",
                "Chord span over time",
                "n06.span",
                Time,
                7,
                48,
                Replace
            ),
            sweep!(
                "retrigger",
                "Retrigger distance by pitch",
                "n07.dist",
                Pitch,
                1,
                480,
                Replace
            ),
            sweep!(
                "knee",
                "Compression knee over time",
                "n10.knee",
                Time,
                0,
                24,
                Replace
            ),
            sweep!(
                "ratio",
                "Compression ratio over time",
                "n10.ratio",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "copies",
                "Octave copies over time",
                "n11.most",
                Time,
                1,
                4,
                Replace
            ),
        ],
        // V09-S35: Slide / Portamento's numeric controls of n01 – n11 (a
        // contextual tool: each group of the sweep finds its glide sources
        // among its own notes and the context).
        "slide" => &[
            sweep!(
                "glide_time",
                "Glide time over time",
                "n01.ms",
                Time,
                0,
                5000,
                Replace
            ),
            sweep!(
                "per_semitone",
                "Glide time per semitone by pitch",
                "n01.per",
                Pitch,
                0,
                500,
                Replace
            ),
            sweep!(
                "largest",
                "Largest glided interval over time",
                "n02.hi",
                Time,
                0,
                48,
                Replace
            ),
            sweep!(
                "depth",
                "Release depth by velocity",
                "n03.depth",
                Velocity,
                1,
                48,
                Replace
            ),
            sweep!(
                "release_time",
                "Release time over time",
                "n03.time",
                Time,
                1,
                100,
                Replace
            ),
            sweep!(
                "trill_rate",
                "Trill rate over time",
                "n04.rate",
                Time,
                0.25,
                64,
                Replace
            ),
            sweep!(
                "trill_interval",
                "Trill interval by pitch",
                "n04.interval",
                Pitch,
                -12,
                12,
                Replace
            ),
            sweep!(
                "env_high",
                "Envelope high by velocity",
                "n05.hi",
                Velocity,
                0,
                127,
                Replace
            ),
            sweep!(
                "window",
                "Source search window over time",
                "n06.window",
                Time,
                0,
                3840,
                Replace
            ),
            sweep!(
                "error",
                "Thinning error over time",
                "n11.cents",
                Time,
                1,
                100,
                Replace
            ),
        ],
        // V09-S35: the Riff Machine's numeric controls of n01 – n11 (a
        // generator: with nothing selected it runs once with its base
        // settings; over a selection each group generates its own riff).
        "riff" => &[
            sweep!(
                "interval",
                "Answer interval over time",
                "n01.interval",
                Time,
                -24,
                24,
                Replace
            ),
            sweep!(
                "displacement",
                "Answer displacement over time",
                "n01.shift",
                Time,
                -16,
                16,
                Replace
            ),
            sweep!(
                "layers",
                "Layers over time",
                "n02.layers",
                Time,
                1,
                8,
                Replace
            ),
            sweep!(
                "variation",
                "Variation per repeat over time",
                "n03.vary",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "build",
                "Build bars over time",
                "n04.bars",
                Time,
                1,
                64,
                Replace
            ),
            sweep!(
                "rise",
                "Velocity rise by velocity",
                "n04.rise",
                Velocity,
                0,
                100,
                Replace
            ),
            sweep!(
                "offset",
                "Counter register by pitch",
                "n06.offset",
                Pitch,
                -24,
                24,
                Replace
            ),
            sweep!(
                "steps",
                "Accent steps per bar over time",
                "n09.steps",
                Time,
                1,
                64,
                Replace
            ),
            sweep!(
                "shift",
                "Bar displacement over time",
                "n10.shift",
                Time,
                -16,
                16,
                Replace
            ),
            sweep!(
                "ending",
                "Ending length over time",
                "n11.beats",
                Time,
                1,
                8,
                Replace
            ),
        ],
        // V09-S35: the Chord Progression's numeric controls of n01 – n11
        // (a generator: with nothing selected it runs once with its base
        // settings; over a selection each group runs on its notes).
        "chord_progression" => &[
            sweep!(
                "borrow",
                "Borrow share over time",
                "n02.share",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "dominant_share",
                "Secondary dominant share",
                "n03.share",
                Time,
                10,
                90,
                Replace
            ),
            sweep!(
                "phrase",
                "Phrase length over time",
                "n04.phrase",
                Time,
                2,
                8,
                Replace
            ),
            sweep!(
                "cadence",
                "Cadence strength over time",
                "n04.strength",
                Time,
                0,
                100,
                Replace
            ),
            sweep!(
                "passing",
                "Passing length over time",
                "n06.len",
                Time,
                60,
                960,
                Replace
            ),
            sweep!(
                "resolve",
                "Suspension resolve point",
                "n07.resolve",
                Time,
                10,
                90,
                Replace
            ),
            sweep!(
                "adventure",
                "Harmonizing adventure by pitch",
                "n08.adventure",
                Pitch,
                0,
                100,
                Replace
            ),
            sweep!(
                "start",
                "Walk start degree",
                "n09.start",
                Time,
                1,
                7,
                Replace
            ),
            sweep!(
                "voices",
                "Chord size over time (build-ups)",
                "n10.voices",
                Time,
                3,
                32,
                Replace
            ),
            sweep!(
                "repeat",
                "Chain repeats",
                "n11.repeat",
                Time,
                1,
                16,
                Replace
            ),
        ],
        // V09-S35: the Chord Stamp's numeric controls of n01 – n11 (a swept
        // n11 budget applies within each band of the sweep's groups).
        "chord_stamp" => &[
            sweep!(
                "voices_soft",
                "Voices at velocity 1",
                "n02.lo",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "voices_loud",
                "Voices at velocity 127",
                "n02.hi",
                Time,
                1,
                16,
                Replace
            ),
            sweep!(
                "width",
                "Cluster width by pitch",
                "n03.width",
                Pitch,
                2,
                24,
                Replace
            ),
            sweep!(
                "partials",
                "Partials by velocity",
                "n05.partials",
                Velocity,
                2,
                32,
                Replace
            ),
            sweep!(
                "rolloff",
                "Partial rolloff over time",
                "n05.rolloff",
                Time,
                0,
                50,
                Replace
            ),
            sweep!(
                "drop_by",
                "Drop by octaves over time",
                "n06.by",
                Time,
                1,
                3,
                Replace
            ),
            sweep!(
                "range_lo",
                "Range low over time",
                "n08.lo",
                Time,
                24,
                72,
                Replace
            ),
            sweep!(
                "range_hi",
                "Range high over time",
                "n08.hi",
                Time,
                48,
                108,
                Replace
            ),
            sweep!(
                "gate",
                "Polyrhythm gate by velocity",
                "n09.gate",
                Velocity,
                5,
                100,
                Replace
            ),
            sweep!(
                "budget",
                "Voice budget over time",
                "n11.most",
                Time,
                4,
                256,
                Replace
            ),
        ],
        _ => &[],
    }
}

/// Whether tool `id` of `family` gets the four mandatory functions (§096:
/// Piano Roll and Experimental tools, also once moved to Legacy; not
/// plugins, one-click tools or commands).
pub fn eligible(id: &str, family: Family) -> bool {
    category(id, family).has_mandatory()
}

/// Builds the definition of a registered tool or command.
///
/// # Panics
/// When the tool has no row in the declared table (a test keeps every
/// registered tool and command declared).
pub fn definition(def: &ToolDef) -> ToolDefinition {
    definition_in(def, &LEGACY)
}

/// [`definition`] with another Legacy table (tests move fixture tools and
/// functions through it).
pub fn definition_in(def: &ToolDef, legacy: &LegacyTable) -> ToolDefinition {
    from_spec(def.id, (def.spec)(), legacy)
}

fn from_spec(id: &'static str, spec: ToolSpec, legacy: &LegacyTable) -> ToolDefinition {
    let decl = declared(id).unwrap_or_else(|| panic!("tool {id} is not declared"));
    let category = legacy.category(id, spec.family);
    let functions = spec
        .advanced
        .iter()
        .enumerate()
        .take(ADV_KEYS.len())
        .map(|(i, &name)| {
            let n = (i + 1) as u8;
            FunctionDef {
                n,
                id: FUNCTION_IDS[i],
                switch: ADV_KEYS[i],
                name,
                status: legacy.function_status(id, FUNCTION_IDS[i], category),
                params: spec
                    .params
                    .iter()
                    .filter(|p| p.adv == n && p.key != ADV_KEYS[i])
                    .map(|p| p.key)
                    .collect(),
                mandatory: false,
                order: 10 * u16::from(n),
            }
        })
        .collect::<Vec<_>>();
    let mut all_params = spec.params.clone();
    let mut functions = functions;
    // V09-S33: functions declared by stable id follow the frozen ones; their
    // switch is the id and their controls are `<id>.…`, numbered here (the
    // place in the window), so no key depends on a position.
    for f in &spec.functions {
        let n = (functions.len() + 1) as u8;
        let mut params = Vec::new();
        for p in all_params.iter_mut() {
            if function_of_key(p.key) == Some(f.id) {
                p.adv = n;
                if p.key != f.id {
                    params.push(p.key);
                }
            }
        }
        functions.push(FunctionDef {
            n,
            id: f.id,
            switch: f.id,
            name: f.name,
            status: legacy.function_status(id, f.id, category),
            params,
            mandatory: false,
            order: f.order,
        });
    }
    // v0.9 §098: the platform's mandatory functions follow the tool's own.
    if category.has_mandatory() {
        let n0 = functions.len() as u8;
        let extra = super::functions::specs(n0, sweep_targets(id), driver_dimension(id));
        for (k, (fid, switch, name)) in super::functions::MANDATORY.iter().enumerate() {
            let n = n0 + 1 + k as u8;
            if !extra.iter().any(|p| p.key == *switch) {
                continue; // no Control Sweep without declared targets
            }
            functions.push(FunctionDef {
                n,
                id: fid,
                switch,
                name,
                status: FunctionStatus::Active,
                params: extra
                    .iter()
                    .filter(|p| p.adv == n && p.key != *switch)
                    .map(|p| p.key)
                    .collect(),
                mandatory: true,
                order: 900 + 10 * k as u16,
            });
        }
        all_params.extend(extra);
    }
    let has = |k: &str| spec.params.iter().any(|p| p.key == k);
    let boundary = BoundaryControls {
        pitch: has("b.pitch"),
        start: has("b.tick"),
        length: has("b.len"),
    };
    let seeds: Vec<&'static str> = spec
        .params
        .iter()
        .filter(|p| p.kind == ParamKind::Seed)
        .map(|p| p.key)
        .collect();
    let params = all_params
        .iter()
        .map(|p| ParamDef {
            editor: editors::editor(id, p),
            spec: p.clone(),
        })
        .collect();
    ToolDefinition {
        id,
        name: spec.name,
        family: spec.family,
        category,
        summary: spec.summary,
        needs_selection: spec.needs_selection,
        context: spec.context,
        params,
        functions,
        sweeps: sweep_targets(id),
        preview: decl.preview,
        growth: decl.growth,
        subchannels: decl.subchannels,
        boundary,
        seed: if seeds.is_empty() {
            SeedPolicy::Deterministic
        } else {
            SeedPolicy::Seeded(seeds)
        },
        undo: UndoPolicy::OneStepWhenChanged,
        pointer: pointer(id),
    }
}

/// Every registered tool (menu order), then every one-click command.
pub fn definitions() -> Vec<ToolDefinition> {
    super::registry::registry()
        .iter()
        .chain(super::tools::COMMANDS.iter())
        .map(definition)
        .collect()
}

/// The definition of the tool or command `id`.
pub fn find(id: &str) -> Option<ToolDefinition> {
    super::registry::tool(id)
        .or_else(|| super::registry::command_tool(id))
        .map(definition)
}

impl ToolDefinition {
    /// The parameter `key`.
    pub fn param(&self, key: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.spec.key == key)
    }

    /// Base controls (no function, no boundary policy), in order.
    pub fn base_params(&self) -> impl Iterator<Item = &ParamDef> {
        self.params
            .iter()
            .filter(|p| p.spec.adv == 0 && !p.spec.key.starts_with("b."))
    }

    /// The controls of function `n` (without its switch), in order.
    pub fn function_params(&self, n: u8) -> impl Iterator<Item = &ParamDef> {
        let switch = self.functions.iter().find(|f| f.n == n).map(|f| f.switch);
        self.params
            .iter()
            .filter(move |p| p.spec.adv == n && n > 0 && Some(p.spec.key) != switch)
    }

    /// The boundary-policy controls (`b.*`), in order.
    pub fn boundary_params(&self) -> impl Iterator<Item = &ParamDef> {
        self.params.iter().filter(|p| p.spec.key.starts_with("b."))
    }

    /// The function with stable id `id` (`a01` …).
    pub fn function(&self, id: &str) -> Option<&FunctionDef> {
        self.functions.iter().find(|f| f.id == id)
    }

    /// The functions switched on in `set`, in the order they are applied
    /// (§235, §236; the tool window's Preview shows it):
    ///
    /// 1. the Control Sweep (it chooses each group's parameters before the
    ///    tool runs);
    /// 2. the frozen v0.7 / v0.8 functions (`a01 …`) inside the tool's
    ///    kernel, in kernel order (orders 10, 20, …) — a function demoted to
    ///    Legacy keeps its place there, so it still runs before every new
    ///    function;
    /// 3. the functions declared by id (`n01 …`): Legacy ones first, then
    ///    Active ones, each by `order`;
    /// 4. Pitch Offset, Subchannel Offset, Remove Note Overlaps.
    ///
    /// v1.0-beta2 B2-F01: each reorderable group
    /// ([`ToolDefinition::groups`]) runs as one chain where its first member
    /// would, its members in the order the values ask for
    /// ([`ToolDefinition::plan`]; with no order, or an invalid one — the run
    /// refuses it, `plan` gives the sentence — the group's declared order,
    /// which for the tools above is the order of the rule; a generator's
    /// group gives its pipeline's order, its catalog order being the
    /// contract's numbering).
    pub fn execution_order(&self, set: &ParamSet) -> Vec<&FunctionDef> {
        let mut all = self.execution_order_all();
        let plan = self.plan(set).ok();
        for g in self.groups() {
            let seq: Vec<&'static str> = plan
                .as_ref()
                .and_then(|p| p.sequence(g.key))
                .map_or_else(|| g.members.clone(), <[&str]>::to_vec);
            let Some(at) = all.iter().position(|f| seq.contains(&f.id)) else {
                continue;
            };
            all.retain(|f| !seq.contains(&f.id));
            let fs: Vec<&FunctionDef> = seq.iter().filter_map(|id| self.function(id)).collect();
            all.splice(at..at, fs);
        }
        all.retain(|f| matches!(set.get_raw(f.switch), Some(ParamValue::Bool(true))));
        all
    }

    /// Every function in the declared order (on or off).
    pub(crate) fn execution_order_all(&self) -> Vec<&FunctionDef> {
        let mut all: Vec<&FunctionDef> = self.functions.iter().collect();
        all.sort_by_key(|f| run_rank(f));
        all
    }

    /// The parameter specs, in order (what [`Params`] and presets read).
    pub fn specs(&self) -> Vec<ParamSpec> {
        self.params.iter().map(|p| p.spec.clone()).collect()
    }

    /// How the pointer is captured with these settings (stroke tools).
    pub fn capture(&self, set: &ParamSet) -> Option<Capture> {
        let specs = self.specs();
        let p = Params::new(&specs, set);
        self.pointer
            .by_function
            .iter()
            .find(|(n, _)| p.adv(*n))
            .map(|&(_, c)| c)
            .or(self.pointer.base)
    }

    /// Everything wrong with this definition's schema (contract §209:
    /// stable ids, defaults and ranges in both key ranges, advanced
    /// functions, editors, sweeps, policies). Empty when it is sound.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut bad = |s: String| out.push(format!("{}: {s}", self.id));
        if !is_id(self.id) {
            bad("tool id is not lower-case ASCII".into());
        }
        if self.name.trim().is_empty() {
            bad("no name".into());
        }
        // Parameters: unique, well-formed keys that belong somewhere.
        let mut keys = HashSet::new();
        for p in &self.params {
            let s = &p.spec;
            if !keys.insert(s.key) {
                bad(format!("parameter key {} twice", s.key));
            }
            if s.label.trim().is_empty() {
                bad(format!("{} has no label", s.key));
            }
            if usize::from(s.adv) > self.functions.len() {
                bad(format!("{} belongs to a missing function {}", s.key, s.adv));
            }
            if s.key.starts_with("b.") && !matches!(s.key, "b.pitch" | "b.tick" | "b.len") {
                bad(format!("{} is not a boundary policy", s.key));
            }
            for k in [KeyRange::Standard, KeyRange::Extended256] {
                for why in value_problems(s, k) {
                    bad(format!("{} ({k:?}): {why}", s.key));
                }
            }
            let fits = match (&s.kind, p.editor) {
                (ParamKind::List { .. }, e) => editors::misfit(e, s),
                (_, Editor::Auto) => None,
                _ => Some("a list editor on a non-list parameter".into()),
            };
            if let Some(why) = fits {
                bad(format!("{}: editor {:?}: {why}", s.key, p.editor));
            }
        }
        // Functions: stable ids, switches, names.
        // The frozen `a01 …` functions come first (their switch `aN` is a
        // position, frozen for ever), then the ones declared by id.
        let frozen = self
            .functions
            .iter()
            .take_while(|f| !f.mandatory && FUNCTION_IDS.contains(&f.id))
            .count();
        if frozen > ADV_KEYS.len() {
            bad(format!("{frozen} frozen advanced functions"));
        }
        let mut new_ids = HashSet::new();
        let mut orders = HashSet::new();
        for f in self.functions.iter().filter(|f| is_function_id(f.id)) {
            if !new_ids.insert(f.id) {
                bad(format!("function id {} twice", f.id));
            }
            // A generator's contract functions keep the contract's order
            // (10 × their number); the others take §235's slots.
            let placed = match contract_number(f.id) {
                Some(n) => f.order == 10 * n,
                None => NEW_FUNCTION_ORDERS.contains(&f.order),
            };
            if !placed || !orders.insert(f.order) {
                bad(format!(
                    "function {} order {} outside its slots or shared",
                    f.id, f.order
                ));
            }
        }
        // A key spelled like an id-keyed function belongs to that function.
        for p in &self.params {
            if let Some(owner) = function_of_key(p.spec.key) {
                match self.functions.iter().find(|f| f.id == owner) {
                    Some(f) if p.spec.adv == f.n => {}
                    Some(f) => bad(format!(
                        "{} is numbered {}, its function {} is {}",
                        p.spec.key, p.spec.adv, f.id, f.n
                    )),
                    None => bad(format!("{} belongs to no declared function", p.spec.key)),
                }
            }
        }
        if matches!(self.category, Category::Plugin | Category::Command)
            && (!self.functions.is_empty() || self.params.iter().any(|p| p.spec.adv != 0))
        {
            bad("a plugin or command has advanced functions".into());
        }
        let mut names = HashSet::new();
        let own = self.functions.iter().filter(|f| !f.mandatory).count();
        let eligible = self.category.has_mandatory();
        let mandatory = self.functions.iter().filter(|f| f.mandatory).count();
        if eligible && !(3..=4).contains(&mandatory) || !eligible && mandatory > 0 {
            bad(format!("{mandatory} mandatory functions"));
        }
        if mandatory == 4 && self.sweeps.is_empty() {
            bad("a Control Sweep without targets".into());
        }
        for (i, f) in self.functions.iter().enumerate() {
            let placed = if f.mandatory {
                i >= own
                    && super::functions::MANDATORY
                        .get(i - own)
                        .is_some_and(|(id, sw, name)| (*id, *sw, *name) == (f.id, f.switch, f.name))
                    && f.order == 900 + 10 * (i - own) as u16
            } else if i < frozen {
                f.id == FUNCTION_IDS[i] && f.switch == ADV_KEYS[i] && f.order == 10 * (i as u16 + 1)
            } else {
                i < own && is_function_id(f.id) && f.switch == f.id
            };
            if usize::from(f.n) != i + 1 || !placed {
                bad(format!("function {} is out of place", f.n));
            }
            if f.name.trim().is_empty() || !names.insert(f.name) {
                bad(format!("function {} is unnamed or named twice", f.n));
            }
            let switch = self.param(f.switch);
            if !switch.is_some_and(|p| p.spec.kind == ParamKind::Bool && p.spec.adv == f.n) {
                bad(format!("function {} has no on / off switch", f.n));
            }
            // Mandatory functions are never Legacy; a One-Click tool's own
            // functions all are; others are Legacy only when the Legacy
            // table names them (its agreement with LEGACY_MAP is tested).
            let legacy = f.status == FunctionStatus::Legacy;
            if f.mandatory && legacy || self.category == Category::OneClick && !legacy {
                bad(format!("function {} status {:?}", f.n, f.status));
            }
        }
        // Control Sweeps.
        if self.sweeps.len() > MAX_SWEEPS {
            bad(format!("{} Control Sweeps", self.sweeps.len()));
        }
        let mut sweep_ids = HashSet::new();
        for w in self.sweeps {
            if !is_id(w.id) || !sweep_ids.insert(w.id) {
                bad(format!("sweep id {} invalid or twice", w.id));
            }
            if w.label.trim().is_empty() {
                bad(format!("sweep {} has no label", w.id));
            }
            if w.on != format!("m.sweep.{}", w.id) || w.graph != format!("m.sweep.{}.graph", w.id) {
                bad(format!("sweep {} keys {} / {}", w.id, w.on, w.graph));
            }
            let (lo, hi) = w.range;
            let Some(target) = self.param(w.param) else {
                bad(format!(
                    "sweep {} drives a missing parameter {}",
                    w.id, w.param
                ));
                continue;
            };
            let range = match target.spec.kind {
                ParamKind::Int { .. } | ParamKind::Pitch => target
                    .spec
                    .range_in(KeyRange::Standard)
                    .map(|(a, b)| (a as f64, b as f64)),
                ParamKind::Ticks { min, max } => Some((min as f64, max as f64)),
                ParamKind::Float { min, max, .. } => Some((min, max)),
                _ => None,
            };
            match range {
                None => bad(format!("sweep {} drives a non-numeric parameter", w.id)),
                Some((min, max)) => {
                    if !(lo.is_finite() && hi.is_finite() && lo < hi && lo >= min && hi <= max) {
                        bad(format!(
                            "sweep {} range {lo} … {hi} outside {min} … {max}",
                            w.id
                        ));
                    }
                }
            }
        }
        // Policies.
        if self.growth.is_empty() {
            bad("no growth kind declared".into());
        }
        for (n, _) in self.pointer.by_function {
            if usize::from(*n) == 0 || usize::from(*n) > self.functions.len() {
                bad(format!("pointer capture by a missing function {n}"));
            }
        }
        if let SeedPolicy::Seeded(k) = &self.seed
            && k.iter().any(|k| self.param(k).is_none())
        {
            bad("seed parameter missing".into());
        }
        // B2-F01: reorderable groups.
        for p in self.group_problems() {
            bad(p);
        }
        out
    }
}

/// Sort key of [`ToolDefinition::execution_order`]: (stage, Legacy first
/// among the new functions, slot).
fn run_rank(f: &FunctionDef) -> (u8, u8, u16) {
    use super::functions::{OVERLAPS, PITCH, SUB, SWEEP};
    if f.mandatory {
        return match f.switch {
            s if s == SWEEP => (0, 0, 0),
            s if s == PITCH => (3, 0, 0),
            s if s == SUB => (3, 0, 1),
            s if s == OVERLAPS => (3, 0, 2),
            _ => (3, 0, f.order),
        };
    }
    if is_function_id(f.id) {
        (2, u8::from(f.status != FunctionStatus::Legacy), f.order)
    } else {
        (1, 0, f.order)
    }
}

/// Lower-case ASCII letters, digits, `_` and `.`, starting with a letter.
fn is_id(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// What is wrong with a parameter's range or default in key range `keys`.
fn value_problems(s: &ParamSpec, keys: KeyRange) -> Vec<String> {
    let mut out = Vec::new();
    let def = s.default_in(keys);
    let range = s.range_in(keys);
    let in_range = |v: i64| range.is_none_or(|(lo, hi)| (lo..=hi).contains(&v));
    match (&s.kind, def.as_ref()) {
        (ParamKind::Int { .. } | ParamKind::Pitch, ParamValue::Int(v)) => {
            let (lo, hi) = range.unwrap_or((0, 0));
            if lo > hi {
                out.push(format!("empty range {lo} … {hi}"));
            }
            if !in_range(*v) {
                out.push(format!("default {v} outside {lo} … {hi}"));
            }
        }
        (ParamKind::Ticks { min, max }, ParamValue::Int(v)) => {
            if min > max || *v < 0 || (*v as u64) < *min || (*v as u64) > *max {
                out.push(format!("default {v} outside {min} … {max} ticks"));
            }
        }
        (ParamKind::Float { min, max, .. }, ParamValue::Float(v)) => {
            if !(min.is_finite() && max.is_finite() && min < max) {
                out.push(format!("range {min} … {max}"));
            }
            if !(v.is_finite() && (*min..=*max).contains(v)) {
                out.push(format!("default {v} outside {min} … {max}"));
            }
        }
        (ParamKind::Bool, ParamValue::Bool(_)) => {}
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            if opts.is_empty() || opts.iter().any(|o| o.trim().is_empty()) {
                out.push("empty choice".into());
            }
            if *c as usize >= opts.len() {
                out.push(format!("default choice {c} of {}", opts.len()));
            }
            let set: HashSet<&&str> = opts.iter().collect();
            if set.len() != opts.len() {
                out.push("a choice listed twice".into());
            }
        }
        (ParamKind::Curve, ParamValue::Curve(c)) => {
            if c.sanitized() != *c || c.points.len() > MAX_CURVE_POINTS || c.points.len() < 2 {
                out.push("default curve is not a sound curve".into());
            }
            if Curve::from_text(&c.to_text()).as_ref() != Some(c) {
                out.push("default curve does not survive its text form".into());
            }
        }
        (ParamKind::List { max_len, .. }, ParamValue::List(l)) => {
            let (lo, hi) = range.unwrap_or((0, 0));
            if lo > hi || *max_len == 0 {
                out.push(format!("empty list range {lo} … {hi} × {max_len}"));
            }
            if l.len() > *max_len || l.iter().any(|&v| !in_range(v)) {
                out.push(format!("default list outside {lo} … {hi} × {max_len}"));
            }
        }
        (ParamKind::Seed, ParamValue::Int(v)) => {
            if *v < 0 {
                out.push("negative seed".into());
            }
        }
        (ParamKind::Text { max_len }, ParamValue::Text(t)) => {
            if *max_len == 0 || t.chars().count() > *max_len {
                out.push(format!("default text longer than {max_len}"));
            }
        }
        (k, v) => out.push(format!("default {v:?} does not fit the kind {k:?}")),
    }
    // Every default survives its preset text form.
    if ParamValue::parse(&s.kind, &def.to_text()).as_ref() != Some(def.as_ref()) {
        out.push("default does not survive its preset text form".into());
    }
    // Keyed parameters only make sense on numbers.
    if s.keyed != Keyed::No
        && !matches!(
            s.kind,
            ParamKind::Int { .. } | ParamKind::List { .. } | ParamKind::Pitch
        )
    {
        out.push("keyed range on a non-integer parameter".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_and_command_has_a_sound_definition() {
        let defs = definitions();
        let problems: Vec<String> = defs.iter().flat_map(|d| d.problems()).collect();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        assert_eq!(
            defs.len(),
            super::super::registry::registry().len() + super::super::tools::COMMANDS.len()
        );
        // Stable ids: unique across tools, commands and removed tools.
        let mut ids = HashSet::new();
        for d in &defs {
            assert!(ids.insert(d.id), "{} twice", d.id);
        }
        for (id, _) in super::super::registry::REMOVED_TOOLS {
            assert!(!ids.contains(id), "{id} is removed but defined");
        }
        for d in &defs {
            assert_eq!(find(d.id).as_ref(), Some(d));
        }
        assert!(find("interpolate").is_none());
    }

    #[test]
    fn categories_functions_and_policies_follow_the_declarations() {
        let slice = find("slice").unwrap();
        assert_eq!(slice.category, Category::OneClick);
        assert!(
            slice
                .functions
                .iter()
                .all(|f| f.status == FunctionStatus::Legacy)
        );
        assert_eq!(slice.functions[0].id, "a01");
        let shear = find("shear").unwrap();
        assert_eq!(shear.category, Category::PianoRoll);
        let f = shear.function("a02").unwrap();
        assert_eq!((f.n, f.switch, f.status), (2, "a2", FunctionStatus::Active));
        assert_eq!(f.params, vec!["a2.curve", "a2.amount"]);
        assert!(
            shear
                .function_params(2)
                .all(|p| p.spec.adv == 2 && p.spec.key != "a2")
        );
        assert!(
            shear
                .base_params()
                .all(|p| p.spec.adv == 0 && !p.spec.key.starts_with("b."))
        );
        assert_eq!(find("mute_notes").unwrap().category, Category::Command);
        assert_eq!(find("hz_chop").unwrap().category, Category::Plugin);
        assert_eq!(find("hz_chop").unwrap().preview, PreviewRecipe::Viewport);
        assert_eq!(
            find("gravity_well").unwrap().category,
            Category::Experimental
        );
        assert_eq!(
            find("subpaint").unwrap().subchannels,
            SubchannelPolicy::Assign
        );
        // Every tool previews and undoes through the platform.
        assert!(
            definitions()
                .iter()
                .all(|d| d.undo == UndoPolicy::OneStepWhenChanged)
        );
    }

    fn tool(id: &str) -> &'static ToolDef {
        super::super::registry::tool(id).unwrap()
    }

    /// §109 / §112 / §113: a whole tool moved through the Legacy table gets
    /// the marked Legacy category (Experimental → Legacy Experimental) and
    /// nothing else changes — id, parameters, functions with their ids,
    /// switches and statuses, mandatory functions, sweeps, policies.
    #[test]
    fn moving_a_tool_to_legacy_changes_its_category_only() {
        static MOVED: LegacyTable = LegacyTable {
            tools: &["shear", "gravity_well"],
            functions: &[],
        };
        assert!(MOVED.problems().is_empty(), "{:?}", MOVED.problems());
        for (id, want) in [
            ("shear", Category::Legacy),
            ("gravity_well", Category::LegacyExperimental),
        ] {
            let base = definition(tool(id));
            let mut moved = definition_in(tool(id), &MOVED);
            assert_eq!(moved.category, want);
            assert!(moved.category.is_legacy() && moved.category.has_mandatory());
            assert!(moved.problems().is_empty(), "{:?}", moved.problems());
            assert!(moved.functions.iter().any(|f| f.mandatory), "{id}");
            moved.category = base.category;
            assert_eq!(moved, base, "{id}: only the category may change");
        }
        // Everything else keeps its category.
        assert_eq!(
            MOVED.category("twirl", Family::Geometry),
            Category::PianoRoll
        );
        assert_eq!(MOVED.category("slice", Family::Editor), Category::OneClick);
        assert_eq!(
            MOVED.category("mute_notes", Family::Editor),
            Category::Command
        );
        assert_eq!(category("shear", Family::Geometry), Category::PianoRoll);
        assert_eq!(Category::Legacy.key(), "legacy");
        assert_eq!(Category::LegacyExperimental.key(), "legacy-experimental");
        assert_eq!(
            Category::LegacyExperimental.legacy_label(),
            Some("Legacy Experimental")
        );
        assert_eq!(Category::Experimental.legacy_label(), None);
    }

    /// §109 / §114: a single function demoted through the table becomes
    /// Legacy (the window's Legacy Advanced section) with its id, switch,
    /// controls and place unchanged; the tool stays Active.
    #[test]
    fn a_demoted_function_keeps_its_id_switch_and_controls() {
        static DEMOTED: LegacyTable = LegacyTable {
            tools: &[],
            functions: &[("transpose", "a03")],
        };
        assert!(DEMOTED.problems().is_empty(), "{:?}", DEMOTED.problems());
        let base = definition(tool("transpose"));
        let mut moved = definition_in(tool("transpose"), &DEMOTED);
        assert_eq!(moved.category, Category::PianoRoll);
        assert!(moved.problems().is_empty(), "{:?}", moved.problems());
        let f = moved.function("a03").unwrap();
        assert_eq!((f.n, f.switch, f.status), (3, "a3", FunctionStatus::Legacy));
        assert_eq!(f.params, base.function("a03").unwrap().params);
        let legacy: Vec<&str> = moved
            .functions
            .iter()
            .filter(|f| f.status == FunctionStatus::Legacy)
            .map(|f| f.id)
            .collect();
        assert_eq!(legacy, vec!["a03"]);
        moved.functions[2].status = FunctionStatus::Active;
        assert_eq!(moved, base);
    }

    /// Transpose's spec without its own functions declared by id (V09-S36
    /// gave it eleven): the frozen functions only, the fixtures' base.
    fn frozen_transpose() -> ToolSpec {
        let mut spec = (tool("transpose").spec)();
        spec.functions.clear();
        spec.params.retain(|p| function_of_key(p.key).is_none());
        spec
    }

    /// Transpose's spec with fixture functions declared by id (V09-S33
    /// gap 3): `n01` (order 110, one control) and `n02` (order 120, two
    /// controls), declared in `order` unless `swapped`.
    fn with_id_keyed(swapped: bool) -> ToolSpec {
        use super::super::param::{flag, int};
        use super::super::registry::FunctionSpec;
        let mut spec = frozen_transpose();
        let n01 = FunctionSpec {
            id: "n01",
            name: "Fixture fold",
            order: 110,
        };
        let n02 = FunctionSpec {
            id: "n02",
            name: "Fixture spread",
            order: 120,
        };
        let p01 = vec![
            flag("n01", "Fixture fold", false),
            int("n01.span", "Span", 1, 24, 12),
        ];
        let p02 = vec![
            flag("n02", "Fixture spread", false),
            int("n02.width", "Width", 0, 48, 4),
            flag("n02.up", "Upwards", true),
        ];
        // Base and frozen-function parameters keep their place; the new
        // ones may sit anywhere (here before the boundary policies).
        let at = spec
            .params
            .iter()
            .position(|p| p.key.starts_with("b."))
            .unwrap_or(spec.params.len());
        let (fs, ps) = if swapped {
            (vec![n02, n01], [p02, p01].concat())
        } else {
            (vec![n01, n02], [p01, p02].concat())
        };
        spec.params.splice(at..at, ps);
        spec.functions = fs;
        spec
    }

    /// The (id, switch, controls, status, order) of every function — what a
    /// preset or a project depends on (the place `n` is window layout).
    fn keys_of(
        d: &ToolDefinition,
    ) -> Vec<(
        &'static str,
        &'static str,
        Vec<&'static str>,
        FunctionStatus,
        u16,
    )> {
        d.functions
            .iter()
            .map(|f| (f.id, f.switch, f.params.clone(), f.status, f.order))
            .collect()
    }

    /// V09-S33 (§112, §114, §237): functions declared by stable id are
    /// built after the frozen `a01 …` ones and before the mandatory ones,
    /// with the id as switch and `<id>.…` controls; adding them, reordering
    /// their declarations or demoting any function never moves another
    /// function's settings, and their presets round-trip by key.
    #[test]
    fn id_keyed_functions_never_move_another_functions_settings() {
        let base = from_spec("transpose", frozen_transpose(), &LEGACY);
        let fx = from_spec("transpose", with_id_keyed(false), &LEGACY);
        assert!(fx.problems().is_empty(), "{:?}", fx.problems());
        let frozen = base.functions.iter().filter(|f| !f.mandatory).count();
        assert_eq!(fx.functions[..frozen], base.functions[..frozen]);
        let n01 = fx.function("n01").unwrap();
        assert_eq!(
            (n01.n as usize, n01.switch, n01.order, n01.status),
            (frozen + 1, "n01", 110, FunctionStatus::Active)
        );
        assert_eq!(n01.params, vec!["n01.span"]);
        assert_eq!(
            fx.function("n02").unwrap().params,
            vec!["n02.width", "n02.up"]
        );
        assert!(fx.function_params(n01.n).all(|p| p.spec.key == "n01.span"));
        // The mandatory functions keep ids, switches, controls and orders;
        // only their place in the window moves by two.
        let tail = |d: &ToolDefinition| -> Vec<_> {
            keys_of(d)
                .into_iter()
                .filter(|k| k.0.starts_with("m_"))
                .collect()
        };
        assert_eq!(tail(&fx), tail(&base));
        assert_eq!(fx.functions.len(), base.functions.len() + 2);
        for f in fx.functions.iter().filter(|f| f.mandatory) {
            assert_eq!(f.n, base.function(f.id).unwrap().n + 2, "{}", f.id);
        }
        // Every earlier parameter keeps its spec (mandatory ones renumbered).
        for p in &base.params {
            let mut q = fx.param(p.spec.key).unwrap().clone();
            if p.spec.adv as usize > frozen {
                assert_eq!(q.spec.adv, p.spec.adv + 2, "{}", p.spec.key);
                q.spec.adv = p.spec.adv;
            }
            assert_eq!(&q, p, "{}", p.spec.key);
        }
        // Declaring them in another order changes only their places.
        let swapped = from_spec("transpose", with_id_keyed(true), &LEGACY);
        assert!(swapped.problems().is_empty(), "{:?}", swapped.problems());
        let mut a = keys_of(&fx);
        let mut b = keys_of(&swapped);
        a.sort_by_key(|k| k.0);
        b.sort_by_key(|k| k.0);
        assert_eq!(a, b);
        // Demoting a frozen and an id-keyed function changes their status
        // only.
        static DEMOTED: LegacyTable = LegacyTable {
            tools: &[],
            functions: &[("transpose", "a03"), ("transpose", "n01")],
        };
        let demoted = from_spec("transpose", with_id_keyed(false), &DEMOTED);
        assert!(demoted.problems().is_empty(), "{:?}", demoted.problems());
        for (f, g) in demoted.functions.iter().zip(&fx.functions) {
            let want = if matches!(f.id, "a03" | "n01") {
                FunctionStatus::Legacy
            } else {
                FunctionStatus::Active
            };
            assert_eq!(f.status, want, "{}", f.id);
            let mut f = f.clone();
            f.status = g.status;
            assert_eq!(&f, g);
        }
        assert_eq!(demoted.params, fx.params);
        // A preset with every function on round-trips by key through the
        // demoted definition (the settings map to the same functions).
        let specs = fx.specs();
        let mut set = ParamSet::defaults_in(&specs, KeyRange::Standard);
        for f in &fx.functions {
            set.set(f.switch, ParamValue::Bool(true));
        }
        set.set("n01.span", ParamValue::Int(5));
        set.set("a3.axis", ParamValue::Int(64));
        let text = super::super::preset::to_text(&super::super::preset::Preset {
            tool: "transpose".into(),
            name: "fixture".into(),
            values: set.clone(),
        });
        assert!(text.contains("\np.n01=1\n") && text.contains("\np.n01.span=5\n"));
        let (back, skipped) =
            super::super::preset::parse(&text, "transpose", &demoted.specs()).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(back.values, set);
    }

    /// §235 / §236: the switched-on functions run in a declared order — the
    /// Control Sweep, the frozen functions in kernel order (a demoted one
    /// keeps its place), the id-keyed ones Legacy first then by order, then
    /// the drivers and overlaps. B2-S16: members of a reorderable group (here
    /// Transpose's Pitch maps, which the fixture's n01 / n02 belong to, and
    /// the platform's Output group) run in the group's sequence — by default
    /// the kernel's own order — so a demotion inside a group changes the
    /// function's category, never its place.
    #[test]
    fn the_execution_order_is_declared_with_legacy_before_active() {
        static DEMOTED: LegacyTable = LegacyTable {
            tools: &[],
            functions: &[("transpose", "a03"), ("transpose", "n02")],
        };
        let d = from_spec("transpose", with_id_keyed(true), &DEMOTED);
        let mut set = ParamSet::new();
        for k in [
            "n01", "n02", "a3", "a1", "m.ovl", "m.sub", "m.pitch", "m.sweep",
        ] {
            set.set(k, ParamValue::Bool(true));
        }
        let ids =
            |set: &ParamSet| -> Vec<&str> { d.execution_order(set).iter().map(|f| f.id).collect() };
        assert_eq!(
            ids(&set),
            [
                "m_sweep",
                "a01",
                "a03",
                "n01",
                "n02",
                "m_pitch_offset",
                "m_sub_offset",
                "m_overlaps"
            ]
        );
        // Outside a group the rank holds: Legacy first, then by order.
        let ranked: Vec<&str> = d
            .execution_order_all()
            .iter()
            .map(|f| f.id)
            .filter(|id| ["n01", "n02"].contains(id))
            .collect();
        assert_eq!(ranked, ["n02", "n01"]);
        // Off functions are left out; with n02 Active the new ones follow
        // their orders.
        set.set("m.sweep", ParamValue::Bool(false));
        set.set("a1", ParamValue::Bool(false));
        let active = from_spec("transpose", with_id_keyed(true), &LEGACY);
        let order: Vec<&str> = active.execution_order(&set).iter().map(|f| f.id).collect();
        assert_eq!(
            order,
            [
                "a03",
                "n01",
                "n02",
                "m_pitch_offset",
                "m_sub_offset",
                "m_overlaps"
            ]
        );
        assert!(d.execution_order(&ParamSet::new()).is_empty());
    }

    /// Bad id-keyed declarations are described: a control of an undeclared
    /// function, an order outside 110 … 790 or shared, a frozen-position
    /// number on an id-keyed control.
    #[test]
    fn bad_id_keyed_functions_are_described() {
        use super::super::param::int;
        let mut spec = with_id_keyed(false);
        spec.params.push(int("n07.x", "Stray", 0, 1, 0));
        spec.functions[1].order = 110;
        let d = from_spec("transpose", spec, &LEGACY);
        let p = d.problems().join("\n");
        assert!(p.contains("n07.x belongs to no declared function"), "{p}");
        assert!(p.contains("function n02 order 110"), "{p}");
        let mut spec = with_id_keyed(false);
        spec.functions[0].order = 100;
        let p = from_spec("transpose", spec, &LEGACY).problems().join("\n");
        assert!(p.contains("function n01 order 100"), "{p}");
        assert!(is_function_id("n01") && is_function_id("n99"));
        assert!(!is_function_id("n00") && !is_function_id("a01") && !is_function_id("n1"));
        assert_eq!(function_of_key("n12.width"), Some("n12"));
        assert_eq!(function_of_key("n12"), Some("n12"));
        assert_eq!(function_of_key("a1.n01"), None);
        // Generators' contract functions (V09-S52).
        assert!(
            is_function_id("c01_free_angle") && is_function_id("c25_recursive_line_subdivision")
        );
        assert_eq!(contract_number("c17_crosshatch"), Some(17));
        assert!(!is_function_id("c00_none") && !is_function_id("c1_x") && !is_function_id("c01_"));
        assert!(!is_function_id("c01_Free") && !is_function_id("cc1_x"));
        assert_eq!(
            function_of_key("c04_line_thickness.rows"),
            Some("c04_line_thickness")
        );
    }

    /// The declared table is sound, and a bad one is described.
    #[test]
    fn the_legacy_table_names_only_movable_tools_and_own_functions() {
        assert!(LEGACY.problems().is_empty(), "{:?}", LEGACY.problems());
        static BAD: LegacyTable = LegacyTable {
            tools: &["ghost", "slice", "hz_chop", "shear", "shear"],
            functions: &[
                ("shear", "a02"),
                ("transpose", "a99"),
                ("transpose", "m_sweep"),
                ("slice", "a01"),
                ("nobody", "a01"),
                ("twirl", "a01"),
                ("twirl", "a01"),
            ],
        };
        let p = BAD.problems().join("\n");
        for want in [
            "legacy tool ghost is not registered",
            "slice (one-click) cannot move to Legacy",
            "hz_chop (plugin) cannot move to Legacy",
            "legacy tool shear twice",
            "legacy function shear/a02: the whole tool is Legacy",
            "legacy function transpose/a99: no such own function",
            "legacy function transpose/m_sweep: no such own function",
            "legacy function slice/a01: the whole tool is Legacy",
            "legacy function nobody/a01: tool not registered",
            "legacy function twirl/a01 twice",
        ] {
            assert!(p.contains(want), "{want}: {p}");
        }
        // A mandatory function can never be Legacy; a One-Click tool's own
        // functions always are.
        let mut d = definition(tool("shear"));
        let last = d.functions.len() - 1;
        d.functions[last].status = FunctionStatus::Legacy;
        let mut s = definition(tool("slice"));
        s.functions[0].status = FunctionStatus::Active;
        let p = [d.problems(), s.problems()].concat().join("\n");
        assert!(
            p.contains(&format!("shear: function {} status Legacy", last + 1)),
            "{p}"
        );
        assert!(p.contains("slice: function 1 status Active"), "{p}");
    }

    #[test]
    fn stroke_tools_capture_the_pointer_by_their_settings() {
        let draw = find("draw").unwrap();
        assert_eq!(draw.capture(&ParamSet::new()), Some(Capture::Freehand));
        let select = find("select").unwrap();
        let mut set = ParamSet::new();
        assert_eq!(select.capture(&set), None);
        set.set("a1", ParamValue::Bool(true));
        assert_eq!(select.capture(&set), Some(Capture::Freehand));
        set.set("a2", ParamValue::Bool(true));
        assert_eq!(select.capture(&set), Some(Capture::Polygon), "2 wins");
        assert_eq!(find("shear").unwrap().capture(&set), None);
    }

    #[test]
    fn schema_problems_are_found() {
        let mut d = find("shear").unwrap();
        d.params[0].spec.default = ParamValue::Int(3);
        d.functions[1].name = d.functions[0].name;
        d.params.push(d.params[1].clone());
        static SW: [SweepTarget; 2] = [
            sweep!("amount", "Amount", "amount", Time, -300, 10, Add),
            sweep!("curve", "Curve", "curve", Pitch, 0, 1, Replace),
        ];
        d.sweeps = &SW;
        let p = d.problems().join("\n");
        for want in [
            "does not fit the kind",
            "named twice",
            "twice",
            "sweep amount range",
            "sweep curve drives a non-numeric parameter",
        ] {
            assert!(p.contains(want), "{want}: {p}");
        }
        // A sound sweep passes.
        static OK: [SweepTarget; 1] = [sweep!(
            "amount", "Amount", "amount", NoteIndex, -50, 50, Multiply
        )];
        let mut d = find("shear").unwrap();
        d.sweeps = &OK;
        assert!(d.problems().is_empty(), "{:?}", d.problems());
    }
}
