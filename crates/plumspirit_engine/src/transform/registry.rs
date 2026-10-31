//! Every tool of the platform: its declarative spec and its kernel builder.

use super::param::{ParamSet, ParamSpec, Params};
use super::{Ctx, Kernel, SourceStats, SourceTrack, TransformError};

/// Tool families (menus, catalog sections).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// FL Studio-inspired Piano Roll tools.
    Fl,
    /// PlumSpirit note editors (properties, humanize, transpose, …).
    Editor,
    /// Mouse tools with batch modes (draw, paint, delete, select, …).
    Interactive,
    /// Continuous geometry (skew, twirl, wave, clipping mask, …).
    Geometry,
    /// Iterations (v0.9 §087 / §213, the owner's section): Paint V2 (the
    /// owner's "Paint Extra") and — v1.0-beta1 B1-0099 — Duplicate V2 (the
    /// former Paste Repeat (notes), id `paste_repeat_notes`).
    Iterations,
    /// The retained experimental tools.
    Experimental,
    /// v0.8 built-in plugins that run as tools (HZ-Chop, SAFOR): no
    /// advanced-function catalogue; credited in `crate::plugins`.
    Plugin,
    /// v0.9 §088 (V09-S13): Paste Repeat. Since B1-0099 no registered tool
    /// has this family (Duplicate V2 moved to Iterations); kept for the
    /// catalog's history and old references.
    PasteRepeat,
    /// v0.9 §151 (V09-S51): the Note Generators (5 Simple, 5 Advanced —
    /// `transform::generator`), in the Tools menu's Generator section.
    Generator,
}

impl Family {
    pub fn label(self) -> &'static str {
        match self {
            Family::Fl => "Piano Roll Tools",
            Family::Editor => "Note Editors",
            Family::Interactive => "Brush & Selection Tools",
            Family::Geometry => "Geometry",
            Family::Iterations => "Iterations",
            Family::Experimental => "Experimental",
            Family::Plugin => "Plugins",
            Family::PasteRepeat => "Paste Repeat",
            Family::Generator => "Generator",
        }
    }
}

/// The declarative description of a tool.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    /// One sentence shown at the top of the window.
    pub summary: &'static str,
    pub params: Vec<ParamSpec>,
    /// Names of the advanced functions (index 0 = function 1): ten for
    /// the v0.7 tools, up to [`super::param::ADV_KEYS`]`.len()` (Paint has
    /// fourteen); none for plugins.
    pub advanced: Vec<&'static str>,
    /// v0.9 (V09-S33, §112, §237): functions declared by stable id — the
    /// V09-S32 designs `n01 …`. Their switch is the id itself and their
    /// controls are `<id>.<name>` (declared with `adv` 0: the definition
    /// numbers them), so inserting, hiding or demoting any function never
    /// moves another's settings. The definition places them after the frozen
    /// [`ToolSpec::advanced`] functions and before the mandatory ones.
    pub functions: Vec<FunctionSpec>,
    /// Whether the tool needs selected notes (generators may not).
    pub needs_selection: bool,
    /// Whether the source should include the unselected notes around the
    /// selection (`Source::context`).
    pub context: bool,
}

/// A tool function declared by its stable id (v0.9 V09-S33): switch key =
/// `id`, control keys `<id>.<name>`; never addressed by its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionSpec {
    /// Stable id (`n01` … `n99`, never reused within the tool).
    pub id: &'static str,
    /// Display name (unique in the tool; a rename keeps the id).
    pub name: &'static str,
    /// Composition slot from the tool catalog (§235: 110–490 core
    /// behaviour, 500–790 output shaping; unique among the tool's new
    /// functions).
    pub order: u16,
}

impl ToolSpec {
    /// How many functions the tool itself declares (frozen `aN` ones plus
    /// id-keyed ones), without the platform's mandatory functions.
    pub fn own_functions(&self) -> usize {
        self.advanced.len() + self.functions.len()
    }
}

/// What a kernel builder may use.
pub struct BuildCtx<'a> {
    pub ctx: &'a Ctx,
    pub stats: &'a SourceStats,
    pub tracks: &'a [SourceTrack],
}

pub type BuildFn = fn(&Params, &BuildCtx) -> Result<Box<dyn Kernel>, TransformError>;

/// A registered tool.
pub struct ToolDef {
    pub id: &'static str,
    pub spec: fn() -> ToolSpec,
    pub build: BuildFn,
}

impl ToolDef {
    /// Builds the kernel for `set` (missing values = defaults).
    ///
    /// v0.9 §098: an eligible tool's mandatory functions (offset drivers,
    /// overlaps, Control Sweep) wrap its kernel when one is switched on.
    ///
    /// v1.0-beta2 B2-F01: values holding an execution order are checked
    /// first; an invalid order refuses the build with its sentence (never
    /// the declared order instead).
    pub fn kernel(&self, set: &ParamSet, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
        if set.get_raw(super::order::ORDER_KEY).is_some() {
            super::definition::definition(self)
                .plan(set)
                .map_err(TransformError::Invalid)?;
        }
        let spec = (self.spec)();
        let p = Params::new(&spec.params, set).with_keys(b.ctx.keys);
        let inner = (self.build)(&p, b)?;
        super::functions::wrap(self.id, self.build, &spec, set, b, inner)
    }

    /// The boundary policy chosen in `set` (standard `b.*` controls).
    pub fn boundary(&self, set: &ParamSet) -> super::Boundary {
        let spec = (self.spec)();
        super::Boundary::from_params(&Params::new(&spec.params, set))
    }
}

/// All tools, in menu order.
pub fn registry() -> &'static [ToolDef] {
    super::tools::ALL
}

/// The tool with this id (active tools only: a removed tool is not found —
/// see [`removed_tool`] for its notice).
pub fn tool(id: &str) -> Option<&'static ToolDef> {
    // B2-F14: historical visible names resolve to the unchanged stable id.
    let id = match id {
        "Note Repeat" | "Notes Repeat" | "Chopper V2" => "repeat",
        _ => id,
    };
    registry().iter().find(|t| t.id == id)
}

/// A kernel behind a one-click command (v0.9 §117: Mute Notes / Unmute
/// Notes): run through the platform (Preview, Undo, background jobs) but
/// never listed, opened or saved as a tool.
pub fn command_tool(id: &str) -> Option<&'static ToolDef> {
    super::tools::COMMANDS.iter().find(|t| t.id == id)
}

/// Tools removed in v0.9 (§115, §116, §117, §238), by stable id, with the
/// notice shown when an old preset or setting names one — never run another
/// tool instead. The same entries as `docs/v0_9/LEGACY_MAP.toml` `[[removed]]`
/// (a test compares them).
pub const REMOVED_TOOLS: &[(&str, &str)] = &[
    (
        "interpolate",
        "Interpolate was removed in PlumSpirit 0.9 (DAW-oriented tool). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "lfo",
        "LFO was removed in PlumSpirit 0.9 (DAW-oriented tool). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "level_scale",
        "Level Scaling was removed in PlumSpirit 0.9 (DAW-oriented tool). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "mute",
        "Advanced Mute was removed in PlumSpirit 0.9; use Mute / Unmute (one click). This preset cannot be applied.",
    ),
    (
        "magnetic_field",
        "Magnetic Field was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "fractal_echo",
        "Fractal Echo was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "contour_trace",
        "Contour Trace was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "skeletonize",
        "Skeletonize was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "voronoi_scatter",
        "Voronoi Scatter was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "morph_bridge",
        "Morph Bridge was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "density_sculptor",
        "Density Sculptor was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "ribbon",
        "Ribbon was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "harmonic_prism",
        "Harmonic Prism was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "time_fold",
        "Time Fold was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "pitch_terrain",
        "Pitch Terrain was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "cellular_growth",
        "Cellular Growth was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
    (
        "recursive_tile",
        "Recursive Tile was removed in PlumSpirit 0.9 (Experimental tool retired by the owner). This preset cannot be applied; no other tool is run instead.",
    ),
];

/// The notice of a removed tool (`None`: not a removed tool).
pub fn removed_tool(id: &str) -> Option<&'static str> {
    REMOVED_TOOLS
        .iter()
        .find(|(t, _)| *t == id)
        .map(|(_, notice)| *notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// v0.9 §115–§117, §238: removed tools are gone from the active registry,
    /// keep their notices (the same as docs/v0_9/LEGACY_MAP.toml), and the
    /// one-click command kernels are separate from the tools.
    #[test]
    fn removed_tools_are_gone_and_keep_their_notices() {
        for (id, notice) in REMOVED_TOOLS {
            assert!(tool(id).is_none(), "{id} is still an active tool");
            assert!(command_tool(id).is_none(), "{id}");
            assert_eq!(removed_tool(id), Some(*notice));
            assert!(notice.contains("removed"), "{notice}");
        }
        assert_eq!(removed_tool("transpose"), None);
        assert_eq!(removed_tool("slice"), None, "Slice stays (one-click)");
        let ids: HashSet<&str> = registry().iter().map(|t| t.id).collect();
        for c in super::super::tools::COMMANDS {
            assert!(!ids.contains(c.id), "{} is both a tool and a command", c.id);
            assert!(
                (c.spec)().advanced.is_empty(),
                "{}: a command has no advanced functions",
                c.id
            );
            assert_eq!((c.spec)().id, c.id);
        }
        let mute = command_tool("mute_notes").expect("Mute Notes");
        assert_eq!((mute.spec)().name, "Mute Notes");
        // The notices are the legacy map's, word for word.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/v0_9/LEGACY_MAP.toml"
        );
        let text = std::fs::read_to_string(path).expect("LEGACY_MAP.toml");
        // Table headers start a line; `# [[removed]] …` comments do not count.
        let mut map = Vec::new();
        let mut blocks: Vec<(bool, Vec<&str>)> = Vec::new();
        for l in text.lines() {
            let t = l.trim();
            if t.starts_with("[[") {
                blocks.push((t == "[[removed]]", Vec::new()));
            } else if let Some(b) = blocks.last_mut() {
                b.1.push(t);
            }
        }
        for (_, lines) in blocks.iter().filter(|b| b.0) {
            let field = |k: &str| {
                lines.iter().find_map(|l| {
                    l.strip_prefix(k)
                        .and_then(|r| r.trim_start().strip_prefix('='))
                        .map(|v| v.trim().trim_matches('"').to_owned())
                })
            };
            map.push((field("tool").unwrap(), field("notice").unwrap()));
        }
        let ours: Vec<(String, String)> = REMOVED_TOOLS
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(map, ours);
    }

    #[test]
    fn tool_ids_and_parameter_keys_are_unique_and_specs_are_complete() {
        let mut ids = HashSet::new();
        for t in registry() {
            assert!(ids.insert(t.id), "duplicate tool id {}", t.id);
            let s = (t.spec)();
            assert_eq!(s.id, t.id);
            let mut keys = HashSet::new();
            for p in &s.params {
                assert!(keys.insert(p.key), "{}: duplicate key {}", t.id, p.key);
                assert!(
                    usize::from(p.adv) <= s.advanced.len(),
                    "{}: {}",
                    t.id,
                    p.key
                );
            }
            if s.family == Family::Plugin {
                // Plugins have no advanced functions, and are credited.
                assert!(s.advanced.is_empty(), "{}", t.id);
                assert!(s.params.iter().all(|p| p.adv == 0), "{}", t.id);
                assert!(
                    crate::plugins::for_tool(t.id).is_some(),
                    "{} uncredited",
                    t.id
                );
                continue;
            }
            assert!(
                s.advanced.len() <= super::super::param::ADV_KEYS.len(),
                "{}: too many advanced functions",
                t.id
            );
            // Every advanced function is named and has its switch.
            for (i, name) in s.advanced.iter().enumerate() {
                assert!(
                    !name.is_empty(),
                    "{}: advanced function {} unnamed",
                    t.id,
                    i + 1
                );
                assert!(
                    s.params.iter().any(
                        |p| p.key == super::super::param::ADV_KEYS[i] && p.adv == (i + 1) as u8
                    ),
                    "{}: advanced function {} has no switch",
                    t.id,
                    i + 1
                );
            }
            let mut names = HashSet::new();
            for n in s.advanced.iter().chain(s.functions.iter().map(|f| &f.name)) {
                assert!(
                    names.insert(*n),
                    "{}: advanced function named twice: {n}",
                    t.id
                );
            }
            // V09-S33: functions declared by id have their switch (the id)
            // and their controls are not numbered by position in the spec.
            for f in &s.functions {
                assert!(
                    s.params
                        .iter()
                        .any(|p| p.key == f.id && p.kind == super::super::param::ParamKind::Bool),
                    "{}: function {} has no switch",
                    t.id,
                    f.id
                );
            }
            for p in &s.params {
                if super::super::definition::function_of_key(p.key).is_some() {
                    assert_eq!(p.adv, 0, "{}: {} is numbered by position", t.id, p.key);
                }
            }
        }
    }
}
