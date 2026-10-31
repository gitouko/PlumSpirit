//! The Piano Roll's right-click note menu (v0.8 §156–§157): the tools in
//! meaningful categories (never one flat list), Repeat Last Tool, Recent
//! Tools, Mute and Delete. Built once; the roll draws it.

use plumspirit_engine::transform::registry;
use std::sync::OnceLock;

/// (category, [(tool id, tool name)]).
pub type Categories = Vec<(&'static str, Vec<(&'static str, &'static str)>)>;

/// Tools of each category (ids); a tool may appear in two.
const TABLE: &[(&str, &[&str])] = &[
    (
        "Common",
        &[
            "note_props",
            "transpose",
            "quantize",
            "duplicate",
            "legato",
            "humanize",
            "join",
            "slide",
        ],
    ),
    (
        "Advanced Transform",
        &["flip", "stretch", "slice", "chop", "snap_scale", "limit"],
    ),
    (
        "Rhythm",
        &["arpeggiate", "articulate", "strum", "flam", "randomize"],
    ),
    ("Harmony", &["chord_progression", "chord_stamp", "riff"]),
    (
        "Geometry",
        &[
            "skew",
            "distort",
            "perspective",
            "warp",
            "shear",
            "twirl",
            "wave",
            "clip_mask",
            "warp_transform",
            "trails",
            "note_outline",
            "displacement_map",
            "tiny_planet",
            "ripple",
        ],
    ),
    ("Generate", &["claw", "riff", "chord_stamp"]),
    ("Properties", &["note_props", "paint_color", "group"]),
    // v0.9 §087 / §213: the owner's Iterations section — Paint V2 (the
    // owner's "Paint Extra"); B1-0099: Duplicate V2 (the former Paste
    // Repeat (notes), which left Common).
    ("Iterations", &["repeat", "subpaint", "paste_repeat_notes"]),
    (
        "Experimental",
        &[
            "pulse_field",
            "particle_burst",
            "flow_field",
            "gravity_well",
            "kaleidoscope",
            "orbit",
            "vortex_trails",
            "tool3d",
            "tumours",
        ],
    ),
    // V09-S51 – S61: the Note Generators after Experimental, Simple then
    // Advanced, like Tools → Generator (they fell into "Other" here).
    (
        "Generator",
        &[
            "gen.lines",
            "gen.rect",
            "gen.circles",
            "gen.arcs",
            "gen.polygons",
            "gen.custom",
            "gen.parametric",
            "gen.lsystem",
            "gen.spirograph",
            "gen.growth",
        ],
    ),
    ("Plugins", &["hz_chop", "safor"]),
    ("Selection", &["select"]),
];

/// Stroke tools need a drawn stroke (Draw / Paint); Delete has its own
/// submenu (Mute / Unmute are one-click commands, not tools).
const OWN_PLACE: &[&str] = &["draw", "paint", "delete"];

pub fn categories() -> &'static Categories {
    static C: OnceLock<Categories> = OnceLock::new();
    C.get_or_init(|| build(&plumspirit_engine::transform::definition::LEGACY))
}

/// The categories with Legacy table `legacy`: a tool moved to Legacy leaves
/// the active categories for the marked "Legacy" / "Legacy Experimental"
/// sections at the end (v0.9 §109, §113; same id, same window).
fn build(legacy: &plumspirit_engine::transform::definition::LegacyTable) -> Categories {
    use plumspirit_engine::transform::definition::Category;
    let category = |d: &registry::ToolDef| legacy.category(d.id, (d.spec)().family);
    let active = |id: &str| registry::tool(id).is_some_and(|d| !category(d).is_legacy());
    let name = |id: &str| registry::tool(id).map(|d| (d.id, (d.spec)().name));
    let mut out: Categories = TABLE
        .iter()
        .map(|(cat, ids)| {
            let tools = ids
                .iter()
                .filter(|id| active(id))
                .filter_map(|id| name(id))
                .collect();
            (*cat, tools)
        })
        .collect();
    // Anything new lands in "Other" instead of disappearing.
    let listed =
        |id: &str| TABLE.iter().any(|(_, ids)| ids.contains(&id)) || OWN_PLACE.contains(&id);
    let other: Vec<_> = registry::registry()
        .iter()
        .filter(|d| !listed(d.id) && !category(d).is_legacy())
        .map(|d| (d.id, (d.spec)().name))
        .collect();
    out.push(("Other", other));
    for cat in [Category::Legacy, Category::LegacyExperimental] {
        let tools = registry::registry()
            .iter()
            .filter(|d| category(d) == cat)
            .map(|d| (d.id, (d.spec)().name))
            .collect();
        out.push((cat.legacy_label().unwrap_or_default(), tools));
    }
    out.retain(|(_, v)| !v.is_empty());
    out
}

/// What the note menu shows besides the categories.
#[derive(Clone, Debug, Default)]
pub struct NoteMenu {
    /// "Repeat Humanize" when there is a recipe.
    pub repeat: Option<String>,
    /// (tool id, name), newest first.
    pub recent: Vec<(&'static str, &'static str)>,
    pub can_mute: bool,
    pub can_unmute: bool,
    /// v0.9 §118 / §082: one-click Slice Notes and Duplicate One Octave (a
    /// selection exists).
    pub can_slice: bool,
    /// v0.9 V09-S10: Delete Notes, Copy and Cut (notes are selected; they
    /// were enabled and did nothing without a selection).
    pub can_copy: bool,
    /// B1-0047: ready plugins' custom tools (plugin id, plugin name,
    /// [(tool id, name, tooltip)]).
    pub custom: crate::app::custom_tools::CustomMenu,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.9 §087 / §213: the note menu has the owner's Iterations section
    /// holding Paint V2 (the owner's "Paint Extra"), which left Properties,
    /// and — B1-0099 — Duplicate V2, which left Common; every other category
    /// keeps its tools.
    #[test]
    fn paint_v2_and_duplicate_v2_are_the_iterations_section() {
        let c = categories();
        let it = c.iter().find(|(name, _)| *name == "Iterations").unwrap();
        assert_eq!(
            it.1,
            vec![
                ("repeat", "Chopper V2"),
                ("subpaint", "Paint V2"),
                ("paste_repeat_notes", "Duplicate V2")
            ]
        );
        for id in ["repeat", "subpaint", "paste_repeat_notes"] {
            let where_: Vec<&str> = c
                .iter()
                .filter(|(_, v)| v.iter().any(|(t, _)| *t == id))
                .map(|(name, _)| *name)
                .collect();
            assert_eq!(where_, vec!["Iterations"], "{id}");
        }
        let props = c.iter().find(|(name, _)| *name == "Properties").unwrap();
        let ids: Vec<&str> = props.1.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["note_props", "paint_color", "group"]);
        // It comes after the other Piano Roll sections, before Experimental
        // (as in the Tools menu).
        let order: Vec<&str> = c.iter().map(|(n, _)| *n).collect();
        let at = |n: &str| order.iter().position(|x| *x == n).unwrap();
        assert_eq!(at("Iterations"), at("Properties") + 1);
        assert_eq!(at("Experimental"), at("Iterations") + 1);
    }

    /// v0.9 §109 / §113 (V09-S33): nothing is moved today; a tool moved
    /// through the Legacy table leaves every active category for the marked
    /// Legacy section (Experimental → Legacy Experimental), under its name.
    #[test]
    fn moved_tools_leave_the_active_categories_for_the_marked_legacy_sections() {
        use plumspirit_engine::transform::definition::{LEGACY, LegacyTable};
        assert_eq!(&build(&LEGACY), categories());
        // V09-S71: retired tools are removed, with no Legacy fallback.
        let today = categories();
        assert!(today.iter().all(|(n, _)| !n.starts_with("Legacy")));
        let experimental = &today.iter().find(|(n, _)| *n == "Experimental").unwrap().1;
        let actual: std::collections::BTreeSet<_> =
            experimental.iter().map(|(id, _)| *id).collect();
        let expected = std::collections::BTreeSet::from([
            "gravity_well",
            "kaleidoscope",
            "orbit",
            "vortex_trails",
            "pulse_field",
            "particle_burst",
            "flow_field",
            "tool3d",
            "tumours",
        ]);
        assert_eq!(actual, expected);
        assert_eq!(experimental.len(), 9);
        static MOVED: LegacyTable = LegacyTable {
            tools: &["riff", "orbit"],
            functions: &[],
        };
        let c = build(&MOVED);
        let where_ = |id: &str| -> Vec<&str> {
            c.iter()
                .filter(|(_, v)| v.iter().any(|(t, _)| *t == id))
                .map(|(n, _)| *n)
                .collect()
        };
        // Riff was in Harmony and Generate; now only in Legacy.
        assert_eq!(where_("riff"), vec!["Legacy"]);
        assert_eq!(where_("orbit"), vec!["Legacy Experimental"]);
        assert_eq!(where_("chord_stamp"), vec!["Harmony", "Generate"]);
        let n = c.len();
        assert_eq!((c[n - 2].0, c[n - 1].0), ("Legacy", "Legacy Experimental"));
        assert_eq!(c[n - 2].1, vec![("riff", "Riff Machine")]);
    }

    /// v0.9 §115–§117, §238: removed tools appear in no menu; the one-click
    /// Mute command is not a tool.
    #[test]
    fn removed_tools_and_commands_are_in_no_tool_menu() {
        let c = categories();
        for (id, _) in plumspirit_engine::transform::REMOVED_TOOLS {
            assert!(
                c.iter().all(|(_, v)| v.iter().all(|(t, _)| t != id)),
                "{id} is still in a menu"
            );
            assert!(registry::registry().iter().all(|d| d.id != *id), "{id}");
        }
        for cmd in plumspirit_engine::transform::tools::COMMANDS {
            assert!(
                c.iter().all(|(_, v)| v.iter().all(|(t, _)| *t != cmd.id)),
                "{} is a command, not a tool",
                cmd.id
            );
        }
        assert!(
            TABLE
                .iter()
                .all(|(_, ids)| ids.iter().all(|id| registry::tool(id).is_some()))
        );
        assert_eq!(
            crate::app::tools::Recipe::tool("mute_notes", Default::default(), 0).name(),
            "Mute Notes"
        );
        assert_eq!(
            crate::app::tools::Recipe::tool("slice_notes", Default::default(), 0).name(),
            "Slice Notes"
        );
    }

    #[test]
    fn every_note_tool_is_in_a_category_and_no_menu_is_a_flat_dump() {
        let c = categories();
        for d in registry::registry() {
            if OWN_PLACE.contains(&d.id) {
                continue;
            }
            assert!(
                c.iter().any(|(_, v)| v.iter().any(|(id, _)| *id == d.id)),
                "{} is in no category",
                d.id
            );
        }
        assert!(
            c.iter().all(|(_, v)| v.len() <= 20),
            "no category is a flat dump"
        );
        assert!(
            c.iter().all(|(cat, _)| *cat != "Other"),
            "every tool has a meaningful category: {:?}",
            c.iter().find(|(cat, _)| *cat == "Other")
        );
    }
}
