//! Original-byte logical script runs; source graphemes are indivisible.
use super::*;
use unicode_script::{Script, ScriptExtension, UnicodeScript};

struct Run {
    first: usize,
    last: usize,
    scripts: ScriptExtension,
    preferred: Option<Script>,
}
impl Run {
    fn script(&self) -> (Script, bool) {
        if let Some(s) = self.preferred.filter(|s| self.scripts.contains_script(*s)) {
            return (s, true);
        }
        match Script::try_from(self.scripts) {
            Ok(s) => (
                s,
                !matches!(s, Script::Common | Script::Inherited | Script::Unknown),
            ),
            Err(_) => (Script::Unknown, false),
        }
    }
}
fn runs_controlled(
    lua: &Lua,
    value: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let cap = if let Some(t) = opts {
        keys(&t, &["max_runs"])?;
        limit(t.raw_get("max_runs")?, MAX_RUNS, MAX_RUNS)?
    } else {
        MAX_RUNS
    };
    let source = input(value, &mut check)?;
    let _workspace = workspace(lua, source.len())?;
    let mut runs: Vec<Run> = Vec::new();
    runs.try_reserve_exact(cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded script runs."))?;
    let mut current: Option<Run> = None;
    let mut scalars = 0usize;
    let push = |runs: &mut Vec<Run>, run| -> mlua::Result<()> {
        if runs.len() == cap {
            return Err(stop(
                Category::Budget,
                "Script runs exceed max_runs; no partial analysis returned.",
            ));
        }
        runs.push(run);
        Ok(())
    };
    for (offset, grapheme) in source.grapheme_indices(true) {
        let mut scripts = ScriptExtension::from(Script::Common);
        let mut preferred = None;
        let mut has_common = false;
        for c in grapheme.chars() {
            if scalars.is_multiple_of(64) {
                check()?;
            }
            scalars += 1;
            scripts.intersect_with(c.script_extension());
            let script = c.script();
            has_common |= script == Script::Common;
            if preferred.is_none()
                && !matches!(script, Script::Common | Script::Inherited | Script::Unknown)
            {
                preferred = Some(script);
            }
        }
        // The extension intersection treats Inherited as universal. Preserve a
        // neutral Common base (emoji/punctuation) instead of relabeling its marks.
        if has_common && scripts.is_inherited() {
            scripts = ScriptExtension::from(Script::Common);
        }
        preferred = preferred.filter(|s| scripts.contains_script(*s));
        let next = Run {
            first: offset,
            last: offset + grapheme.len(),
            scripts,
            preferred,
        };
        if let Some(mut previous) = current.take() {
            let mut intersection = previous.scripts.intersection(scripts);
            if intersection.is_inherited() && (previous.scripts.is_common() || scripts.is_common())
            {
                intersection = ScriptExtension::from(Script::Common);
            }
            if !intersection.is_empty() || (previous.scripts.is_empty() && scripts.is_empty()) {
                previous.last = next.last;
                previous.scripts = intersection;
                previous.preferred = previous
                    .preferred
                    .filter(|s| intersection.contains_script(*s))
                    .or(preferred.filter(|s| intersection.contains_script(*s)));
                current = Some(previous);
            } else {
                push(&mut runs, previous)?;
                current = Some(next);
            }
        } else {
            current = Some(next);
        }
    }
    if let Some(run) = current {
        push(&mut runs, run)?;
    }
    check()?;
    let result = lua.create_table()?;
    let first = lua.create_table()?;
    let last = lua.create_table()?;
    let script = lua.create_table()?;
    let resolved = lua.create_table()?;
    for (i, run) in runs.iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let (tag, certain) = run.script();
        first.raw_set(i + 1, run.first + 1)?;
        last.raw_set(i + 1, run.last + 1)?;
        script.raw_set(i + 1, tag.short_name())?;
        resolved.raw_set(i + 1, certain)?;
    }
    check()?;
    result.raw_set("text", source.as_ref())?;
    result.raw_set("count", runs.len())?;
    result.raw_set("first", first)?;
    result.raw_set("last", last)?;
    result.raw_set("script", script)?;
    result.raw_set("resolved", resolved)?;
    Ok(result)
}
pub(super) fn install(lua: &Lua, text: &Table) -> mlua::Result<()> {
    let (a, b, c) = unicode_script::UNICODE_VERSION;
    text.set("script_unicode_version", format!("{a}.{b}.{c}"))?;
    text.set(
        "script_runs",
        lua.create_function(|lua, (value, opts): (Value, Option<Table>)| {
            runs_controlled(lua, value, opts, || notes::check_budget(lua))
        })?,
    )
}

#[cfg(test)]
#[path = "text_script_tests.rs"]
mod tests;
