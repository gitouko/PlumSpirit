//! v0.9 §237 (V09-S33, V09-0074): presets saved by PlumSpirit 0.7 and 0.8
//! load in 0.9 to exactly the settings they hold — also after their tool
//! moves to Legacy or their functions are demoted — and presets of removed
//! tools are refused with the tool's notice.
//!
//! The fixtures `tests/fixtures/presets/v07_presets.txt` / `v08_presets.txt`
//! were written by the 0.7.0 (`c636cc0`) and 0.8.0 (`058a094`) engines
//! themselves with their own `preset::to_text`
//! (`tests/fixtures/presets/gen_presets.rs.txt`): for every tool of that
//! version, its defaults with every advanced function on, and seeded random
//! settings within each parameter's range with every function on.

use plumspirit_engine::transform::definition::{
    Category, FunctionStatus, LegacyTable, ToolDefinition, definition, definition_in,
};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::preset;
use plumspirit_engine::transform::{REMOVED_TOOLS, removed_tool, tool};
use std::collections::BTreeSet;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/presets/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// (tool id, variant, preset text) of every preset in a fixture file.
fn presets(name: &str) -> Vec<(String, String, String)> {
    let text = fixture(name);
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(head) = line.strip_prefix("%%% ") {
            let (id, variant) = head.trim_end().split_once(' ').expect("%%% id variant");
            out.push((id.to_owned(), variant.to_owned(), String::new()));
        } else {
            out.last_mut()
                .expect("text before a header")
                .2
                .push_str(line);
        }
    }
    out
}

/// The tool's definition with the tool moved to Legacy (when it can move)
/// and, separately, with every own function demoted.
fn moved_and_demoted(def: &ToolDefinition) -> Vec<ToolDefinition> {
    let t = tool(def.id).unwrap();
    let mut out = Vec::new();
    if matches!(def.category, Category::PianoRoll | Category::Experimental) {
        let tools: &'static [&'static str] = Box::leak(vec![def.id].into_boxed_slice());
        let moved = definition_in(
            t,
            &LegacyTable {
                tools,
                functions: &[],
            },
        );
        assert!(moved.category.is_legacy(), "{}", def.id);
        out.push(moved);
    }
    let own: Vec<(&'static str, &'static str)> = def
        .functions
        .iter()
        .filter(|f| !f.mandatory)
        .map(|f| (def.id, f.id))
        .collect();
    if !own.is_empty() && def.category != Category::OneClick {
        let functions: &'static [(&'static str, &'static str)] = Box::leak(own.into_boxed_slice());
        let demoted = definition_in(
            t,
            &LegacyTable {
                tools: &[],
                functions,
            },
        );
        assert!(
            demoted
                .functions
                .iter()
                .all(|f| f.mandatory || f.status == FunctionStatus::Legacy),
            "{}",
            def.id
        );
        out.push(demoted);
    }
    out
}

/// §237: every preset written by 0.7 / 0.8 loads in 0.9 with no key
/// skipped and every value kept — saving it again gives the same bytes —
/// through today's definition, the definition with the tool moved to Legacy
/// and the one with all its functions demoted; the functions it switched on
/// are the same stable functions (by id) in all three, in the declared
/// order. A removed tool's preset is refused with exactly its notice.
#[test]
fn presets_saved_by_v07_and_v08_load_to_the_same_settings() {
    let mut problems = Vec::new();
    for (file, tools, removed_want) in [
        (
            "v07_presets.txt",
            62,
            &[
                "interpolate",
                "level_scale",
                "lfo",
                "mute",
                "magnetic_field",
                "fractal_echo",
                "contour_trace",
                "skeletonize",
                "voronoi_scatter",
                "morph_bridge",
                "density_sculptor",
                "ribbon",
                "harmonic_prism",
                "time_fold",
                "pitch_terrain",
                "cellular_growth",
                "recursive_tile",
            ][..],
        ),
        (
            "v08_presets.txt",
            65,
            &[
                "interpolate",
                "level_scale",
                "lfo",
                "mute",
                "magnetic_field",
                "fractal_echo",
                "contour_trace",
                "skeletonize",
                "voronoi_scatter",
                "morph_bridge",
                "density_sculptor",
                "ribbon",
                "harmonic_prism",
                "time_fold",
                "pitch_terrain",
                "cellular_growth",
                "recursive_tile",
            ][..],
        ),
    ] {
        let all = presets(file);
        assert_eq!(all.len(), 2 * tools, "{file}");
        let mut removed = BTreeSet::new();
        for (id, variant, text) in &all {
            let at = format!("{file} {id} {variant}");
            let Some(t) = tool(id) else {
                let notice = removed_tool(id)
                    .unwrap_or_else(|| panic!("{at}: neither registered nor removed"));
                assert_eq!(preset::parse(text, id, &[]).unwrap_err(), notice, "{at}");
                removed.insert(id.as_str());
                continue;
            };
            let def = definition(t);
            let mut seen = None;
            for d in std::iter::once(def.clone()).chain(moved_and_demoted(&def)) {
                let (p, skipped) = match preset::parse(text, id, &d.specs()) {
                    Ok(r) => r,
                    Err(e) => {
                        problems.push(format!("{at}: refused: {e}"));
                        break;
                    }
                };
                if !skipped.is_empty() {
                    problems.push(format!("{at}: keys skipped {skipped:?}"));
                }
                if preset::to_text(&p) != *text {
                    problems.push(format!("{at}: settings changed when saved again"));
                }
                // The switches it turned on name the same functions.
                let on: Vec<&str> = d.execution_order(&p.values).iter().map(|f| f.id).collect();
                let mut ids = on.clone();
                ids.sort_unstable();
                let want: Vec<&str> = {
                    let mut w: Vec<&str> = d
                        .functions
                        .iter()
                        .filter(|f| {
                            matches!(p.values.get_raw(f.switch), Some(ParamValue::Bool(true)))
                        })
                        .map(|f| f.id)
                        .collect();
                    w.sort_unstable();
                    w
                };
                assert_eq!(ids, want, "{at}");
                match &seen {
                    None => seen = Some((p, on.len())),
                    Some((first, n)) => {
                        assert_eq!(&p, first, "{at}: {:?}", d.category);
                        assert_eq!(on.len(), *n, "{at}");
                    }
                }
            }
            // The old version's functions were all switched on: every own
            // function of today's tool that existed then is on.
            if let Some((p, _)) = &seen {
                for f in def.functions.iter().filter(|f| !f.mandatory) {
                    if text.contains(&format!("\np.{}=1\n", f.switch)) {
                        assert!(
                            matches!(p.values.get_raw(f.switch), Some(ParamValue::Bool(true))),
                            "{at}: {}",
                            f.id
                        );
                    }
                }
            }
        }
        let want: BTreeSet<&str> = removed_want.iter().copied().collect();
        assert_eq!(removed, want, "{file}: removed tools");
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert_eq!(REMOVED_TOOLS.len(), 17);
}

/// §237 and the preset rules: a key from a newer version is reported and
/// never applied (the rest loads); a damaged value refuses the whole file;
/// a preset is never applied to another tool.
#[test]
fn unknown_keys_are_reported_and_damaged_presets_refused() {
    let all = presets("v08_presets.txt");
    let (id, _, text) = all
        .iter()
        .find(|(id, v, _)| id == "transpose" && v == "random")
        .unwrap();
    let def = definition(tool(id).unwrap());
    let (base, _) = preset::parse(text, id, &def.specs()).unwrap();
    let newer = format!("{text}p.n42=1\np.n42.width=3\n");
    let (p, skipped) = preset::parse(&newer, id, &def.specs()).unwrap();
    assert_eq!(skipped, vec!["n42".to_owned(), "n42.width".to_owned()]);
    assert_eq!(p, base);
    let damaged = text.replace("\np.a3=1\n", "\np.a3=perhaps\n");
    assert_ne!(&damaged, text);
    assert!(preset::parse(&damaged, id, &def.specs()).is_err());
    let err = preset::parse(text, "shear", &def.specs()).unwrap_err();
    assert!(err.contains("transpose"), "{err}");
}
