//! Deterministic cmap coverage selection, separate from shaping and presentation.
use super::super::layout;
use super::*;
use unicode_segmentation::UnicodeSegmentation;
const MAX_FONTS: usize = 8;
const MAX_IGNORED: usize = 32;
const MAX_RUNS: usize = 4096;
#[derive(Clone, Copy)]
struct Cluster {
    first: usize,
    last: usize,
    font: usize,
    required: bool,
    ignored: usize,
}
struct Run {
    cluster: Cluster,
    graphemes: usize,
}
fn dense(t: &Table, max: usize) -> mlua::Result<usize> {
    if t.metatable().is_some() {
        return Err(bad("Font coverage lists must be plain dense arrays."));
    }
    let n = t.raw_len();
    if n > max {
        return Err(stop(
            Category::Budget,
            "Font coverage list exceeds its limit.",
        ));
    }
    let mut count = 0;
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        count += 1;
        if count > n || !matches!(k,Value::Integer(i) if i>=1 && i as usize<=n) {
            return Err(bad("Font coverage lists must be plain dense arrays."));
        }
    }
    if count != n {
        return Err(bad("Font coverage list contains holes."));
    }
    Ok(n)
}
fn controlled(
    lua: &Lua,
    value: Value,
    fonts: Table,
    options: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let source = layout::input(value, &mut check)?;
    let _workspace = layout::workspace(lua, source.len())?;
    let font_count = dense(&fonts, MAX_FONTS)?;
    if font_count == 0 {
        return Err(bad("Font coverage requires1–8authorized Font handles."));
    }
    let mut ignored = [None; MAX_IGNORED];
    let mut ignored_len = 0;
    let cap = if let Some(options) = options {
        layout::keys(&options, &["max_runs", "ignored_codepoints"])?;
        let cap = layout::limit(options.raw_get("max_runs")?, MAX_RUNS, MAX_RUNS)?;
        match options.raw_get::<Value>("ignored_codepoints")? {
            Value::Nil => {}
            Value::Table(t) => {
                ignored_len = dense(&t, MAX_IGNORED)?;
                for (i, slot) in ignored.iter_mut().enumerate().take(ignored_len) {
                    let Value::Integer(n) = t.raw_get::<Value>(i + 1)? else {
                        return Err(bad("Ignored codepoints must be Unicode scalar integers."));
                    };
                    *slot = Some(u32::try_from(n).ok().and_then(char::from_u32).ok_or_else(
                        || bad("Ignored codepoints must be Unicode scalar integers."),
                    )?);
                }
                for i in 0..ignored_len {
                    if ignored[..i].contains(&ignored[i]) {
                        return Err(bad("Ignored codepoints must not repeat."));
                    }
                }
            }
            _ => {
                return Err(bad(
                    "ignored_codepoints must be a plain dense scalar array.",
                ));
            }
        }
        cap
    } else {
        MAX_RUNS
    };
    let ignored = &ignored[..ignored_len];
    let mut clusters = Vec::new();
    clusters
        .try_reserve_exact(source.len().min(MAX_RUNS))
        .map_err(|_| stop(Category::Memory, "Cannot allocate font coverage clusters."))?;
    let mut scalar_count = 0usize;
    for (first, text) in source.grapheme_indices(true) {
        let (mut required, mut skipped) = (false, 0);
        for c in text.chars() {
            if scalar_count.is_multiple_of(64) {
                check()?;
            }
            scalar_count += 1;
            if ignored.contains(&Some(c)) {
                skipped += 1;
            } else {
                required = true;
            }
        }
        clusters.push(Cluster {
            first,
            last: first + text.len(),
            font: 0,
            required,
            ignored: skipped,
        });
    }
    // Validate every supplied handle, including an otherwise unused fallback.
    // Each font's already-bounded native parser is entered once, never per scalar.
    for f in 1..=font_count {
        check()?;
        let handle = fonts.raw_get::<AnyUserData>(f)?;
        let font = handle.borrow::<Font>()?;
        font.with(lua, |_, face| {
            let map = face.charmap();
            let count = u32::from(
                face.metrics(Size::unscaled(), LocationRef::default())
                    .glyph_count,
            );
            for (i, cluster) in clusters.iter_mut().enumerate() {
                if i.is_multiple_of(64) {
                    check()?;
                }
                if cluster.font != 0 || !cluster.required {
                    continue;
                }
                let mut covered = true;
                for c in source[cluster.first..cluster.last].chars() {
                    check()?;
                    if ignored.contains(&Some(c)) {
                        continue;
                    }
                    match map.map(c).map(|id| id.to_u32()).filter(|id| *id != 0) {
                        Some(id) if id < count => {}
                        Some(_) => {
                            return Err(bad("Font maps a scalar to an invalid glyph index."));
                        }
                        None => {
                            covered = false;
                            break;
                        }
                    }
                }
                if covered {
                    cluster.font = f;
                }
            }
            Ok(())
        })?;
    }
    let mut runs: Vec<Run> = Vec::new();
    runs.try_reserve_exact(cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate font coverage runs."))?;
    let (mut covered, mut missing, mut ignored_clusters, mut ignored_scalars) = (0, 0, 0, 0);
    for (i, cluster) in clusters.iter().copied().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        ignored_scalars += cluster.ignored;
        if !cluster.required {
            ignored_clusters += 1;
        } else if cluster.font == 0 {
            missing += 1;
        } else {
            covered += 1;
        }
        if let Some(previous) = runs.last_mut()
            && previous.cluster.font == cluster.font
            && previous.cluster.required == cluster.required
        {
            previous.cluster.last = cluster.last;
            previous.cluster.ignored += cluster.ignored;
            previous.graphemes += 1;
        } else {
            if runs.len() == cap {
                return Err(stop(
                    Category::Budget,
                    "Font coverage exceeds max_runs; no partial result returned.",
                ));
            }
            runs.push(Run {
                cluster,
                graphemes: 1,
            });
        }
    }
    let out = lua.create_table()?;
    let names = ["first", "last", "font", "status", "graphemes", "ignored"];
    let columns = names
        .iter()
        .map(|_| lua.create_table_with_capacity(runs.len(), 0))
        .collect::<mlua::Result<Vec<_>>>()?;
    for (i, run) in runs.iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let c = run.cluster;
        columns[0].raw_set(i + 1, c.first + 1)?;
        columns[1].raw_set(i + 1, c.last + 1)?;
        columns[2].raw_set(i + 1, c.font)?;
        columns[3].raw_set(
            i + 1,
            if !c.required {
                "ignored"
            } else if c.font == 0 {
                "missing"
            } else {
                "covered"
            },
        )?;
        columns[4].raw_set(i + 1, run.graphemes)?;
        columns[5].raw_set(i + 1, c.ignored)?;
    }
    for (name, column) in names.iter().zip(columns) {
        out.raw_set(*name, column)?;
    }
    out.raw_set("count", runs.len())?;
    out.raw_set("text", source.as_ref())?;
    out.raw_set("font_count", font_count)?;
    out.raw_set("grapheme_count", clusters.len())?;
    out.raw_set("covered_graphemes", covered)?;
    out.raw_set("missing_graphemes", missing)?;
    out.raw_set("ignored_graphemes", ignored_clusters)?;
    out.raw_set("ignored_scalar_count", ignored_scalars)?;
    check()?;
    Ok(out)
}
pub(super) fn install(lua: &Lua, text: &Table) -> mlua::Result<()> {
    text.set("max_coverage_fonts", MAX_FONTS)?;
    text.set("max_coverage_ignored_codepoints", MAX_IGNORED)?;
    text.set("max_coverage_runs", MAX_RUNS)?;
    text.set(
        "font_coverage_runs",
        lua.create_function(
            |lua, (value, fonts, options): (Value, Table, Option<Table>)| {
                controlled(lua, value, fonts, options, || notes::check_budget(lua))
            },
        )?,
    )
}
#[cfg(test)]
#[path = "text_font_coverage_tests.rs"]
mod tests;
