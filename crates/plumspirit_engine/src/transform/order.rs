//! v1.0-beta2 B2-F01: the user-chosen execution order of a tool's functions
//! (contract §005; design: `docs/TRANSFORM_ENGINE.md` "User-chosen execution
//! order").
//!
//! A tool's run is a list of phases. Most functions are fixed at their stage
//! of the tool's pipeline; a **group** is a chain of stages with the same
//! input and output (per-note pitch maps, the platform's offsets and
//! overlaps) whose members run in the user's order. Groups never cross
//! phases, so every permutation of a group is valid.
//!
//! The order is stored in the tool's values under [`ORDER_KEY`] as stable
//! function ids separated by commas; no key = the declared order (each
//! group's members as listed here = what the kernels ran before B2-F01), so
//! existing presets and results are unchanged. Unknown ids are left out
//! with a notice; members missing from the list follow their nearest
//! declared predecessor that is listed (catalog changes never reorder an
//! older sequence); duplicates, fixed functions and malformed lists refuse
//! the run with a sentence.

use super::definition::ToolDefinition;
use super::param::{ParamSet, ParamValue};

/// The reserved key of a tool's values holding the user's execution order.
pub const ORDER_KEY: &str = "order";

/// A reorderable group as declared (members in their declared order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupSpec {
    /// Stable key (kernels ask for their group's sequence by it).
    pub key: &'static str,
    /// What the window calls it.
    pub name: &'static str,
    /// Stable function ids, in the declared (default) order.
    pub members: &'static [&'static str],
}

/// The platform's Output group of every eligible tool: the offset drivers
/// and Remove Note Overlaps on the kernel's result (declared order = §235's
/// pitch offset → subchannel offset → overlaps).
pub const OUTPUT: GroupSpec = GroupSpec {
    key: "output",
    name: "Output",
    members: &["m_pitch_offset", "m_sub_offset", "m_overlaps"],
};

/// Transpose's per-note pitch maps (key-to-key map, interval scaling,
/// frequency shift, overtone snap, octave spread): each takes a pitch and
/// gives a pitch.
pub const TRANSPOSE_PITCH: GroupSpec = GroupSpec {
    key: "pitch",
    name: "Pitch maps",
    members: &["n01", "n02", "n08", "n09", "n06"],
};

/// Note Properties' value passes over the notes' working values (each
/// reads what the passes before it wrote; the lock mask, the clamps and the
/// controller points stay after them).
pub const NOTE_PROPS_PASSES: GroupSpec = GroupSpec {
    key: "values",
    name: "Value passes",
    members: &[
        "n03", "n01", "n10", "n08", "n11", "n05", "n04", "n06", "n07", "n02", "n13", "n12",
    ],
};

/// Humanize's time maps on the performed note: swing (n09) and the rubato
/// warp (n01); both keep the order of notes. (Its offsets and velocity
/// shapes are sums: no order could change them.)
pub const HUMANIZE_TIME: GroupSpec = GroupSpec {
    key: "time",
    name: "Time maps",
    members: &["n09", "n01"],
};

/// Paint Color's colour functions: n01 – n09 each override the colour so
/// far (wholly or where their pattern lies), n10 blurs the colours, n11
/// repaints them by its table — each on what the ones before it left.
pub const PAINT_COLOR_PASSES: GroupSpec = GroupSpec {
    key: "colours",
    name: "Color functions",
    members: &[
        "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
    ],
};

/// Select's steps n01 – n09 over the result so far: grow / shrink and the
/// line trace read it, the criteria keep the notes they match (the
/// criteria's mutual order changes nothing; their order against grow /
/// shrink and the trace does). n10 (set operations with a stored slot) and
/// n11 (outline over the pool and the slot's notes) stay after them.
pub const SELECT_STEPS: GroupSpec = GroupSpec {
    key: "steps",
    name: "Selection steps",
    members: &[
        "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09",
    ],
};

/// Duplicate's per-copy pitch maps after the copy's pitch source (n04's
/// rotation, n02's inversion, v0.8's steps, n05's path): n12 harmonic
/// sequence and n07 progression fit.
pub const DUPLICATE_PITCH: GroupSpec = GroupSpec {
    key: "pitch",
    name: "Copy pitch maps",
    members: &["n12", "n07"],
};

/// Limit's output shaping on the notes it wrote: n07 retrigger spacing,
/// n03 polyphony cap, n04 onset rate limit — each thins what the ones
/// before it kept.
pub const LIMIT_OUTPUT: GroupSpec = GroupSpec {
    key: "shaping",
    name: "Output shaping",
    members: &["n07", "n03", "n04"],
};

/// Paint V2's colour stages after the v0.8 colours: L-system, Markov,
/// phrase colours, reaction-diffusion, the colour automaton, row walks,
/// onset flashes — each on the colours the ones before it left.
pub const SUBPAINT_PASSES: GroupSpec = GroupSpec {
    key: "colours",
    name: "Color stages",
    members: &["n03", "n02", "n07", "n04", "n01", "n06", "n05"],
};

/// Clipping Mask's edits of the finished mask: n04 transform (scale and
/// offset) and n05 outline stroke. (n09's time gate belongs to the mask's
/// source, built once for every sweep level; n06's tiles are evaluated per
/// query, last.)
pub const CLIP_MASK_EDITS: GroupSpec = GroupSpec {
    key: "mask",
    name: "Mask edits",
    members: &["n04", "n05"],
};

/// Lines' variants: affine maps composed for every copy of the path
/// (crosshatch, radial fan, parallel copies, mirror X, mirror Y — the
/// order the kernel nested them before B2-F01; a generator's catalog order
/// is its contract numbering, not its pipeline, `docs/GENERATORS.md`).
pub const LINES_VARIANTS: GroupSpec = GroupSpec {
    key: "variants",
    name: "Variants",
    members: &[
        "c17_crosshatch",
        "c16_radial_fan",
        "c05_parallel_copies",
        "c14_mirror_x",
        "c15_mirror_y",
    ],
};

/// Rectangle / Square's affine maps of the shape: c10 shear and c09
/// rotation (declared: the shear first, as the kernel ran them).
pub const RECT_MAPS: GroupSpec = GroupSpec {
    key: "maps",
    name: "Shape maps",
    members: &["c10_shear", "c09_rotation"],
};

/// Arcs / Curves' copy expansions, each composed onto every copy so far
/// (after c24's branches): c18 tangent-aligned chain, c19 normal offsets,
/// c17 mirror.
pub const ARCS_COPIES: GroupSpec = GroupSpec {
    key: "copies",
    name: "Copies",
    members: &[
        "c18_tangent_aligned_duplication",
        "c19_normal_offset_duplication",
        "c17_mirror",
    ],
};

/// Custom Shape's symmetry: c17's mirrors (about an axis that may sit off
/// the centre) and c18's turned copies, each over the instances so far.
pub const CUSTOM_SYMMETRY: GroupSpec = GroupSpec {
    key: "symmetry",
    name: "Symmetry",
    members: &["c17_mirror_symmetry", "c18_radial_symmetry"],
};

/// Custom Shape's point maps after the shape is built: c23 mesh warp over
/// the box and c16 full transform.
pub const CUSTOM_MAPS: GroupSpec = GroupSpec {
    key: "maps",
    name: "Point maps",
    members: &["c23_mesh_warp", "c16_full_transform"],
};

/// The groups inside tool `id`'s kernel (before the Output group).
pub fn kernel_groups(id: &str) -> &'static [GroupSpec] {
    match id {
        "transpose" => &[TRANSPOSE_PITCH],
        "note_props" => &[NOTE_PROPS_PASSES],
        "humanize" => &[HUMANIZE_TIME],
        "paint_color" => &[PAINT_COLOR_PASSES],
        "select" => &[SELECT_STEPS],
        "duplicate" => &[DUPLICATE_PITCH],
        "limit" => &[LIMIT_OUTPUT],
        "subpaint" => &[SUBPAINT_PASSES],
        "clip_mask" => &[CLIP_MASK_EDITS],
        "gen.lines" => &[LINES_VARIANTS],
        "gen.rect" => &[RECT_MAPS],
        "gen.arcs" => &[ARCS_COPIES],
        "gen.custom" => &[CUSTOM_SYMMETRY, CUSTOM_MAPS],
        _ => &[],
    }
}

/// Why the Control Sweep cannot move.
pub const SWEEP_REASON: &str = "it chooses each group's parameters before the tool runs";
/// Why a function outside every group cannot move.
pub const STAGE_REASON: &str = "it runs at its own stage of the tool's pipeline";

/// A group of one tool: the members it has, in their declared order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub key: &'static str,
    pub name: &'static str,
    pub members: Vec<&'static str>,
}

/// A stretch of a tool's run as the window lists it ([`ToolDefinition::phases`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Phase<'a> {
    /// Functions that run at fixed places, and why they cannot move.
    Fixed {
        functions: Vec<&'a super::definition::FunctionDef>,
        reason: &'static str,
    },
    /// A reorderable group, its members in run order.
    Group {
        group: Group,
        members: Vec<&'a super::definition::FunctionDef>,
    },
}

/// The order a run follows: each group's members in run order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Whether the values hold an order of their own (else the declared one).
    pub custom: bool,
    sequences: Vec<(&'static str, Vec<&'static str>)>,
    /// What was left out (ids this version does not have).
    pub notices: Vec<String>,
}

impl Plan {
    /// The members of group `key` in run order.
    pub fn sequence(&self, key: &str) -> Option<&[&'static str]> {
        self.sequences
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.as_slice())
    }

    /// Every group's sequence.
    pub fn sequences(&self) -> impl Iterator<Item = (&'static str, &[&'static str])> {
        self.sequences.iter().map(|(k, s)| (*k, s.as_slice()))
    }
}

fn damaged(detail: Option<&str>) -> String {
    match detail {
        Some(t) => format!(
            "The execution order is damaged (\"{t}\" is not a function id); reset the order."
        ),
        None => "The execution order is damaged; reset the order.".into(),
    }
}

/// The ids of the order in `set`: `None` when it holds none (or an empty
/// one), the sentence when it is damaged.
pub fn tokens(set: &ParamSet) -> Result<Option<Vec<&str>>, String> {
    let s = match set.get_raw(ORDER_KEY) {
        None => return Ok(None),
        Some(ParamValue::Text(s)) => s.as_str(),
        Some(_) => return Err(damaged(None)),
    };
    if s.trim().is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for t in s.split(',').map(str::trim) {
        let ok = !t.is_empty()
            && t.len() <= 64
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !ok {
            return Err(damaged(Some(&t.chars().take(32).collect::<String>())));
        }
        out.push(t);
    }
    Ok(Some(out))
}

/// `members` (declared order) in the order `tokens` gives them: the listed
/// ones in their listed order, each missing one right after its nearest
/// declared predecessor that is placed (at the start when none is).
pub fn sequence(members: &[&'static str], tokens: &[&str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(members.len());
    for t in tokens {
        if let Some(m) = members.iter().find(|m| *m == t)
            && !out.contains(m)
        {
            out.push(m);
        }
    }
    for (k, m) in members.iter().enumerate() {
        if out.contains(m) {
            continue;
        }
        let at = members[..k]
            .iter()
            .rev()
            .find_map(|p| out.iter().position(|o| o == p))
            .map_or(0, |i| i + 1);
        out.insert(at, m);
    }
    out
}

/// The sequence of a group in `set` for a kernel (the run was validated by
/// [`ToolDefinition::plan`] before the kernel was built; a damaged order
/// gives the declared one here).
pub fn sequence_in(set: &ParamSet, members: &'static [&'static str]) -> Vec<&'static str> {
    match tokens(set) {
        Ok(Some(t)) => sequence(members, &t),
        _ => members.to_vec(),
    }
}

impl ToolDefinition {
    /// The tool's reorderable groups: its kernel's, then the platform's
    /// Output group (eligible tools), each with the members it has.
    pub fn groups(&self) -> Vec<Group> {
        kernel_groups(self.id)
            .iter()
            .chain(std::iter::once(&OUTPUT))
            .map(|g| Group {
                key: g.key,
                name: g.name,
                members: g
                    .members
                    .iter()
                    .copied()
                    .filter(|m| self.function(m).is_some())
                    .collect(),
            })
            .filter(|g| !g.members.is_empty())
            .collect()
    }

    /// The order the values `set` ask for, or the sentence refusing them.
    pub fn plan(&self, set: &ParamSet) -> Result<Plan, String> {
        let groups = self.groups();
        let mut plan = Plan::default();
        let toks = tokens(set)?;
        if let Some(toks) = &toks {
            plan.custom = true;
            let mut seen: Vec<&str> = Vec::with_capacity(toks.len());
            for t in toks {
                let name = self.function(t).map(|f| f.name);
                if seen.contains(t) {
                    return Err(format!(
                        "\"{}\" is listed twice in the execution order.",
                        name.unwrap_or(t)
                    ));
                }
                seen.push(t);
                match name {
                    None => plan.notices.push(format!(
                        "The execution order names a function this version does not have ({t}); it was left out."
                    )),
                    Some(name) => {
                        if !groups.iter().any(|g| g.members.contains(t)) {
                            let reason = if *t == "m_sweep" {
                                SWEEP_REASON
                            } else {
                                STAGE_REASON
                            };
                            return Err(format!(
                                "\"{name}\" runs at a fixed place ({reason}) and cannot be moved in the execution order."
                            ));
                        }
                    }
                }
            }
        }
        for g in &groups {
            let seq = match &toks {
                Some(t) => sequence(&g.members, t),
                None => g.members.clone(),
            };
            plan.sequences.push((g.key, seq));
        }
        Ok(plan)
    }

    /// Every function in run order as the window lists it (B2-F01): runs
    /// of fixed functions (with the reason each cannot move) and the groups
    /// with their members in the order `set` asks for (the declared order
    /// when `set` holds none or an invalid one). On and off functions alike:
    /// a group shows every member's slot.
    pub fn phases(&self, set: &ParamSet) -> Vec<Phase<'_>> {
        let plan = self.plan(set).ok();
        let groups = self.groups();
        let mut out: Vec<Phase<'_>> = Vec::new();
        let mut placed: Vec<&str> = Vec::new();
        for f in self.execution_order_all() {
            if placed.contains(&f.id) {
                continue;
            }
            if let Some(g) = groups.iter().find(|g| g.members.contains(&f.id)) {
                let seq: Vec<&'static str> = plan
                    .as_ref()
                    .and_then(|p| p.sequence(g.key))
                    .map_or_else(|| g.members.clone(), <[&str]>::to_vec);
                placed.extend(seq.iter().copied());
                out.push(Phase::Group {
                    group: g.clone(),
                    members: seq.iter().filter_map(|id| self.function(id)).collect(),
                });
                continue;
            }
            let reason = if f.id == "m_sweep" {
                SWEEP_REASON
            } else {
                STAGE_REASON
            };
            match out.last_mut() {
                Some(Phase::Fixed {
                    functions,
                    reason: r,
                }) if *r == reason => functions.push(f),
                _ => out.push(Phase::Fixed {
                    functions: vec![f],
                    reason,
                }),
            }
        }
        out
    }

    /// The order text writing every group's members in the order `set` asks
    /// for, with `id` moved by `delta` places inside its group (B2-F01:
    /// every reorderable id is written, so switching a function off and on
    /// never moves its slot). `None` when `id` is in no group or cannot move
    /// that way.
    pub fn moved_order(&self, set: &ParamSet, id: &str, delta: isize) -> Option<String> {
        let plan = self.plan(set).unwrap_or_else(|_| {
            self.plan(&ParamSet::new())
                .expect("no order is always a valid order")
        });
        let mut seqs: Vec<Vec<&'static str>> = Vec::new();
        let mut moved = false;
        for g in self.groups() {
            let mut seq = plan
                .sequence(g.key)
                .map_or(g.members.clone(), <[&str]>::to_vec);
            if let Some(i) = seq.iter().position(|m| *m == id) {
                let j = i.checked_add_signed(delta)?;
                if j >= seq.len() {
                    return None;
                }
                let m = seq.remove(i);
                seq.insert(j, m);
                moved = true;
            }
            seqs.push(seq);
        }
        moved.then(|| seqs.concat().join(","))
    }

    /// What is wrong with the groups (B2-F01): a member of two groups, a
    /// group with fewer than two members, members that are not neighbours in
    /// the declared order (a group is one chain of the pipeline). (Declared
    /// members the tool lacks are left out of its groups; the registered
    /// tools' declarations are checked against their definitions by
    /// `tests/b2_execution_order.rs`.)
    pub(crate) fn group_problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let declared = self.execution_order_all();
        let mut seen: Vec<&str> = Vec::new();
        for g in self.groups() {
            if g.members.len() < 2 {
                out.push(format!("group {} has fewer than two members", g.key));
            }
            for m in &g.members {
                if seen.contains(m) {
                    out.push(format!("{m} in two groups"));
                }
                seen.push(m);
            }
            // A generator's catalog order is its contract numbering, not its
            // pipeline: its groups list the pipeline's order themselves.
            if self.family == super::registry::Family::Generator {
                continue;
            }
            let mut at: Vec<usize> = g
                .members
                .iter()
                .filter_map(|m| declared.iter().position(|f| f.id == *m))
                .collect();
            let declared_order = at.windows(2).all(|w| w[0] < w[1]);
            at.sort_unstable();
            if !declared_order || at.windows(2).any(|w| w[1] != w[0] + 1) {
                out.push(format!(
                    "group {} is not one chain of the declared order",
                    g.key
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_members_follow_their_declared_predecessor() {
        let m = &["a", "b", "c", "d"];
        assert_eq!(sequence(m, &[]), ["a", "b", "c", "d"]);
        assert_eq!(sequence(m, &["c", "a"]), ["c", "d", "a", "b"]);
        assert_eq!(sequence(m, &["d", "c", "b", "a"]), ["d", "c", "b", "a"]);
        assert_eq!(sequence(m, &["b"]), ["a", "b", "c", "d"]);
        assert_eq!(sequence(m, &["x", "d", "y", "a"]), ["d", "a", "b", "c"]);
    }

    #[test]
    fn tokens_read_the_order_text() {
        let mut s = ParamSet::new();
        assert_eq!(tokens(&s), Ok(None));
        s.set(ORDER_KEY, ParamValue::Text("  ".into()));
        assert_eq!(tokens(&s), Ok(None));
        s.set(ORDER_KEY, ParamValue::Text("n06, n08 ,m_overlaps".into()));
        assert_eq!(tokens(&s), Ok(Some(vec!["n06", "n08", "m_overlaps"])));
        s.set(ORDER_KEY, ParamValue::Text("n06,,n08".into()));
        assert!(tokens(&s).unwrap_err().contains("damaged"));
        s.set(ORDER_KEY, ParamValue::Text("N06".into()));
        assert!(tokens(&s).unwrap_err().contains("\"N06\""));
        s.set(ORDER_KEY, ParamValue::Bool(true));
        assert!(tokens(&s).unwrap_err().contains("damaged"));
    }
}
