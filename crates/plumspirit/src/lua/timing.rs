//! The Timing / Math / Analysis API (v1.0-beta1 B1-0032, `docs/LUA_API.md`
//! "Timing and analysis").
//!
//! Project reads for the tempo map and meter (tempo and time signature at a
//! tick, bar / beat positions, ticks ↔ seconds, note lengths), a quantize
//! helper, and analysis of note queries and the selection through
//! PlumSpirit's own engines: `SelectionAnalysis` (the Selection Analytics
//! readout) and `AnalysisIndex` (the Progressor) give **exact** peak NPS /
//! poly / PPS; `q:estimate()` is a fast upper bound and says so
//! (`exact = false`).

use mlua::{Lua, Table, UserDataMethods, Value};
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing, document_corrections};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::project::Bpm;
use plumspirit_engine::model::{NoteStore, PlumDocument, TempoValue};
use plumspirit_engine::view::grid::MeterMap;
use std::sync::Arc;

use super::handles::{LuaHandle, Resolved, World, not_for, with_world};
use super::notes::{LuaNoteQuery, check_budget};
use super::{Category, stop, ticks};

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

fn meter(doc: &PlumDocument) -> MeterMap {
    MeterMap::new(doc.division(), &doc.conductor().meter_events())
}

fn window_ns(opts: Option<&Table>) -> mlua::Result<u64> {
    let secs = match opts {
        Some(t) => t.get::<Option<f64>>("window_seconds")?.unwrap_or(1.0),
        None => 1.0,
    };
    if !(secs.is_finite() && secs > 0.0 && secs <= 3600.0) {
        return Err(runtime(format!(
            "window_seconds must be more than 0 and at most 3600; {secs} is not."
        )));
    }
    Ok((secs * 1e9).round() as u64)
}

/// The cached project analysis index for one document state (app data).
#[derive(Default)]
pub(super) struct IndexCache(Option<(usize, u64, Arc<AnalysisIndex>)>);

fn project_index(lua: &Lua, doc: &Arc<PlumDocument>) -> mlua::Result<Arc<AnalysisIndex>> {
    let key = (Arc::as_ptr(doc) as usize, doc.analysis_revision());
    if let Some(c) = lua.app_data_ref::<IndexCache>()
        && let Some((p, r, idx)) = &c.0
        && (*p, *r) == key
    {
        return Ok(Arc::clone(idx));
    }
    check_budget(lua)?;
    let idx = Arc::new(AnalysisIndex::for_document(doc));
    if let Some(mut c) = lua.app_data_mut::<IndexCache>() {
        c.0 = Some((key.0, key.1, Arc::clone(&idx)));
    }
    Ok(idx)
}

/// A selection analysis as a script table.
fn analysis_table(lua: &Lua, a: &SelectionAnalysis) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("notes", a.selected as i64)?;
    t.set("counted", a.counted as i64)?;
    if a.counted > 0 {
        t.set("first_tick", ticks::to_lua(a.first_tick)?)?;
        t.set("last_tick", ticks::to_lua(a.last_tick)?)?;
    }
    t.set("span_seconds", a.span_ns as f64 / 1e9)?;
    t.set("peak_poly", a.peak_poly as i64)?;
    t.set("peak_poly_tick", ticks::to_lua(a.peak_poly_tick)?)?;
    t.set("peak_unique_keys", i64::from(a.peak_unique))?;
    t.set("peak_nps", a.peak_nps as i64)?;
    t.set("peak_nps_tick", ticks::to_lua(a.peak_nps_tick)?)?;
    t.set("peak_pps", a.peak_pps)?;
    t.set("avg_nps", a.avg_nps())?;
    t.set("avg_poly", a.avg_poly())?;
    t.set("window_seconds", a.window_ns as f64 / 1e9)?;
    t.set("exact", true)?;
    Ok(t)
}

fn run_analysis(
    lua: &Lua,
    doc: &PlumDocument,
    inputs: &[SelectionInput],
    window: u64,
) -> mlua::Result<Table> {
    let timing = Timing::for_document(doc);
    let cancel = lua
        .app_data_ref::<super::notes::Budget>()
        .map(|b| Arc::clone(&b.cancel));
    let a = SelectionAnalysis::build_for(inputs, &timing, window, cancel.as_deref())
        .ok_or_else(|| stop(Category::Canceled, "the script was canceled"))?;
    check_budget(lua)?;
    analysis_table(lua, &a)
}

/// The analysis inputs of a query: its notes as a selection.
fn query_input(
    lua: &Lua,
    w: &World,
    q: &LuaNoteQuery,
) -> mlua::Result<(SelectionInput, Vec<super::notes::Hit>)> {
    let (host, hits) = super::notes::evaluate(lua, q)?;
    let store = host
        .notes(&w.doc)
        .ok_or_else(|| runtime("Those notes no longer exist."))?;
    let mut notes: NoteStore = store.clone();
    let mut b = plumspirit_engine::model::RankSetBuilder::default();
    for h in &hits {
        b.push(h.rank);
    }
    notes
        .select_ranks(&b.finish())
        .map_err(|_| stop(Category::Internal, "the notes changed during the analysis"))?;
    let correction = match host.content {
        None => w
            .doc
            .index_of(host.track)
            .and_then(|i| document_corrections(&w.doc).get(i).copied().flatten()),
        Some(_) => None,
    };
    Ok((
        SelectionInput {
            id: host.track,
            notes,
            correction,
            shift: 0,
        },
        hits,
    ))
}

/// Installs `plum.timing`.
pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "quantize",
        lua.create_function(|_, (tick, grid, mode): (Value, Value, Option<String>)| {
            let tick = ticks::from_lua(&tick, "The tick")?;
            let grid = ticks::from_lua(&grid, "The grid")?;
            if grid == 0 {
                return Err(runtime("The grid must be at least 1 tick."));
            }
            let below = tick - tick % grid;
            let q = match mode.as_deref().unwrap_or("nearest") {
                "floor" => below,
                "ceil" => {
                    if tick % grid == 0 {
                        tick
                    } else {
                        below.checked_add(grid).ok_or_else(|| runtime("The tick is too large."))?
                    }
                }
                // Halves go up.
                "nearest" => {
                    if tick % grid >= grid - grid / 2 {
                        below.checked_add(grid).ok_or_else(|| runtime("The tick is too large."))?
                    } else {
                        below
                    }
                }
                other => {
                    return Err(runtime(format!(
                        "The mode \u{201C}{other}\u{201D} is not \"nearest\", \"floor\" or \"ceil\"."
                    )));
                }
            };
            ticks::to_lua(q)
        })?,
    )?;
    plum.set("timing", t)?;
    Ok(())
}

/// Ticks of a note value such as "1/4", "1/8t" (triplet), "1/16." (dotted),
/// "1" or "2" (whole notes), exactly, at `ppqn`.
fn note_ticks(ppqn: u64, value: &str) -> Result<u64, String> {
    let v = value.trim();
    let (body, num, den) = if let Some(b) = v.strip_suffix('t') {
        (b, 2u64, 3u64)
    } else if let Some(b) = v.strip_suffix('.') {
        (b, 3, 2)
    } else {
        (v, 1, 1)
    };
    let (n, d) = match body.split_once('/') {
        Some((a, b)) => (a.trim().parse::<u64>(), b.trim().parse::<u64>()),
        None => (body.parse::<u64>(), Ok(1)),
    };
    let (Ok(n), Ok(d)) = (n, d) else {
        return Err(format!(
            "\u{201C}{value}\u{201D} is not a note value (\"1/4\", \"1/8t\", \"1/16.\", \"1\")."
        ));
    };
    if n == 0 || d == 0 {
        return Err(format!("\u{201C}{value}\u{201D} is not a note value."));
    }
    // ticks = ppqn * 4 * n / d * num / den
    let top = u128::from(ppqn) * 4 * u128::from(n) * u128::from(num);
    let bottom = u128::from(d) * u128::from(den);
    if top % bottom != 0 {
        return Err(format!(
            "{value} is not a whole number of ticks at {ppqn} PPQN."
        ));
    }
    u64::try_from(top / bottom).map_err(|_| format!("{value} is too long."))
}

/// Registers the timing and analysis methods (project and query handles).
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    fn project<R>(
        lua: &Lua,
        this: &LuaHandle,
        name: &str,
        f: impl FnOnce(&World) -> mlua::Result<R>,
    ) -> mlua::Result<R> {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Project(_) | Resolved::Conductor(_) => f(w),
            _ => Err(not_for(name, &this.0)),
        })
    }
    m.add_method("tempo_at", |lua, this, tick: Value| {
        let tick = ticks::from_lua(&tick, "The tick")?;
        project(lua, this, "tempo_at", |w| {
            let c = w.doc.conductor();
            let points = c.tempo_points();
            let i = c.tempo_index_at(tick);
            let p = points
                .get(i)
                .ok_or_else(|| runtime("The project has no tempo."))?;
            let t = lua.create_table()?;
            let ramping =
                p.interp == Interp::Linear && points.get(i + 1).is_some() && p.tick < tick;
            if ramping {
                let ppqn = w.doc.division().musical_ppqn().unwrap_or(960);
                let map = plumspirit_engine::model::TempoMap::for_document(&w.doc, 48_000);
                let bpm = map.bpm_at_tick(tick, ppqn);
                t.set("bpm_float", bpm)?;
                t.set("bpm", format!("{bpm:.6}"))?;
                t.set("micros", (60_000_000.0 / bpm).round() as i64)?;
                t.set("exact", false)?;
            } else {
                let (bpm, micros) = match p.value {
                    TempoValue::Bpm(b) => (b.to_string(), b.to_micros_per_quarter().micros),
                    TempoValue::Micros(us) => (
                        Bpm::from_micros_per_quarter(us)
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| format!("{:.6}", p.bpm_f64())),
                        us,
                    ),
                };
                t.set("bpm", bpm)?;
                t.set("bpm_float", p.bpm_f64())?;
                t.set("micros", i64::from(micros))?;
                t.set("exact", true)?;
            }
            t.set("ramp", p.interp == Interp::Linear)?;
            Ok(t)
        })
    });
    m.add_method("time_signature_at", |lua, this, tick: Value| {
        let tick = ticks::from_lua(&tick, "The tick")?;
        project(lua, this, "time_signature_at", |w| {
            let sigs = w.doc.conductor().time_signatures();
            let i = sigs.partition_point(|s| s.tick <= tick).saturating_sub(1);
            let s = sigs
                .get(i)
                .ok_or_else(|| runtime("The project has no time signature."))?;
            let t = lua.create_table()?;
            t.set("numerator", i64::from(s.numerator))?;
            t.set("denominator", i64::from(s.denominator()))?;
            t.set("tick", ticks::to_lua(s.tick)?)?;
            Ok(t)
        })
    });
    m.add_method("bar_beat", |lua, this, tick: Value| {
        let tick = ticks::from_lua(&tick, "The tick")?;
        project(lua, this, "bar_beat", |w| {
            let (bar, beat, rest) = meter(&w.doc).bar_beat(tick).ok_or_else(|| {
                runtime("This project has SMPTE timing: there are no bars or beats.")
            })?;
            let t = lua.create_table()?;
            t.set("bar", bar as i64)?;
            t.set("beat", beat as i64)?;
            t.set("tick", rest as i64)?;
            Ok(t)
        })
    });
    m.add_method(
        "tick_of",
        |lua, this, (bar, beat, tick): (i64, Option<i64>, Option<i64>)| {
            project(lua, this, "tick_of", |w| {
                let mm = meter(&w.doc);
                let bar_u = u64::try_from(bar)
                    .ok()
                    .filter(|&b| b >= 1)
                    .ok_or_else(|| runtime(format!("Bars count from 1; {bar} is not a bar.")))?;
                let start = mm
                    .measure_start(bar_u)
                    .ok_or_else(|| runtime("This project has SMPTE timing: there are no bars."))?;
                let beat = beat.unwrap_or(1);
                let beats = u64::try_from(beat - 1)
                    .map_err(|_| runtime(format!("Beats count from 1; {beat} is not a beat.")))?;
                let at = mm
                    .advance_beats(start, beats)
                    .ok_or_else(|| runtime("That position is past the last tick."))?;
                let extra = u64::try_from(tick.unwrap_or(0))
                    .map_err(|_| runtime("The tick within the beat must not be negative."))?;
                ticks::to_lua(
                    at.checked_add(extra)
                        .ok_or_else(|| runtime("That position is past the last tick."))?,
                )
            })
        },
    );
    m.add_method("tick_at_seconds", |lua, this, secs: f64| {
        project(lua, this, "tick_at_seconds", |w| {
            if !(secs.is_finite() && secs >= 0.0) {
                return Err(runtime(format!(
                    "The time must be 0 seconds or more; {secs} is not."
                )));
            }
            // The analysis timebase: one frame per nanosecond.
            let map = plumspirit_engine::model::TempoMap::for_document_at(
                &w.doc,
                1_000_000_000,
                plumspirit_engine::model::Speed::NORMAL,
            );
            let ns = (secs * 1e9).round();
            if ns > u64::MAX as f64 {
                return Err(runtime("That time is too far away."));
            }
            ticks::to_lua(map.first_tick_at_or_after_frame(ns as u64))
        })
    });
    m.add_method("note_length", |lua, this, value: String| {
        project(lua, this, "note_length", |w| {
            let ppqn = w.doc.division().musical_ppqn().ok_or_else(|| {
                runtime("This project has SMPTE timing: note values have no length in ticks.")
            })?;
            ticks::to_lua(note_ticks(ppqn, &value).map_err(runtime)?)
        })
    });
    m.add_method(
        "metrics_at",
        |lua, this, (tick, opts): (Value, Option<Table>)| {
            let tick = ticks::from_lua(&tick, "The tick")?;
            let window = window_ns(opts.as_ref())?;
            project(lua, this, "metrics_at", |w| {
                let idx = project_index(lua, &w.doc)?;
                let timing = Timing::for_document(&w.doc);
                let mx = idx.metrics(&Scope::Project, &timing, tick, window);
                let t = lua.create_table()?;
                t.set("notes_passed", mx.notes_passed as i64)?;
                t.set("total_notes", mx.total_notes as i64)?;
                t.set("poly", mx.poly as i64)?;
                t.set("nps", mx.nps as i64)?;
                t.set("pps", mx.pps)?;
                t.set("seconds", mx.nanos as f64 / 1e9)?;
                t.set("exact", true)?;
                Ok(t)
            })
        },
    );
}

/// Registers the analysis methods on note queries.
pub fn add_query_methods<M: UserDataMethods<LuaNoteQuery>>(m: &mut M) {
    m.add_method("analysis", |lua, this, opts: Option<Table>| {
        let window = window_ns(opts.as_ref())?;
        let w = super::handles::current_world(lua).ok_or_else(|| runtime("No project is open."))?;
        let (input, _) = query_input(lua, &w, this)?;
        run_analysis(lua, &w.doc, &[input], window)
    });
    m.add_method("estimate", |lua, this, ()| {
        let w = super::handles::current_world(lua).ok_or_else(|| runtime("No project is open."))?;
        let host = super::notes::host_of(lua, &this.host)?;
        let store = host
            .notes(&w.doc)
            .ok_or_else(|| runtime("Those notes no longer exist."))?;
        let f = &this.filter;
        let t1 = f.t1.map_or(u64::MAX, |t| t.saturating_sub(1));
        let t = lua.create_table()?;
        t.set("notes", store.estimate_in_range(f.t0, t1) as i64)?;
        t.set("exact", false)?;
        Ok(t)
    });
    m.add_method("overlaps", |lua, this, ()| {
        let (_, hits) = super::notes::evaluate(lua, this)?;
        // Notes overlapping an earlier note of the same key and channel.
        let mut v: Vec<(u8, u8, u64, u64)> = hits
            .iter()
            .map(|h| {
                (
                    h.rec.attrs.pitch(),
                    h.rec.attrs.channel(),
                    h.rec.start_tick,
                    h.rec.start_tick.saturating_add(h.rec.duration_ticks),
                )
            })
            .collect();
        v.sort_unstable();
        let mut notes = 0i64;
        let mut i = 0;
        while i < v.len() {
            let (k, c, _, _) = v[i];
            let mut end = 0u64;
            let mut first = true;
            while i < v.len() && v[i].0 == k && v[i].1 == c {
                if !first && v[i].2 < end {
                    notes += 1;
                }
                end = end.max(v[i].3);
                first = false;
                i += 1;
            }
        }
        let t = lua.create_table()?;
        t.set("notes", notes)?;
        t.set("exact", true)?;
        Ok(t)
    });
}

/// Analysis of every selected note of the project's tracks
/// (`plum.selection.analytics`).
pub fn selection_analytics(lua: &Lua, opts: Option<&Table>) -> mlua::Result<Table> {
    let window = window_ns(opts)?;
    let w = super::handles::current_world(lua).ok_or_else(|| runtime("No project is open."))?;
    let corrections = document_corrections(&w.doc);
    let inputs: Vec<SelectionInput> = w
        .doc
        .tracks()
        .iter()
        .enumerate()
        .filter(|(_, t)| t.notes.selected_count() > 0)
        .map(|(i, t)| SelectionInput {
            id: t.id(),
            notes: t.notes.clone(),
            correction: corrections.get(i).copied().flatten(),
            shift: 0,
        })
        .collect();
    run_analysis(lua, &w.doc, &inputs, window)
}
