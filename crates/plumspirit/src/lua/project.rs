//! The Project API, reads (v1.0-beta1 B1-0026, `docs/LUA_API.md` "Project").
//!
//! `plum.project.current()` hands out the project handle; the project and
//! Conductor handles read the run's [`World`]: file, type, dirty state,
//! revision, timing division, duration, counts, the Conductor's tempo map,
//! time signatures and markers. Every value is exact where PlumSpirit's is:
//! ticks are integers, BPM is the exact decimal as a string (plus the MIDI
//! tempo in microseconds); floats only for seconds.

use mlua::{Lua, Table, UserDataMethods, Value};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::{
    PlumDocument, SmfFormat, TempoMap, TempoValue, TimingKind, project::Bpm,
};

use super::handles::{
    LuaHandle, Resolved, World, current_world, handle_list, not_for, with_resolved, with_world,
};
use super::ticks;

/// Installs `plum.project`.
pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "current",
        lua.create_function(|lua, ()| {
            super::handles::require_read(lua)?;
            current_project(lua)
        })?,
    )?;
    plum.set("project", t)?;
    Ok(())
}

fn current_project(lua: &Lua) -> mlua::Result<Value> {
    match current_world(lua) {
        Some(w) => lua
            .create_userdata(LuaHandle(w.project()))
            .map(Value::UserData),
        None => Ok(Value::Nil),
    }
}

/// The document behind a project or Conductor handle.
fn doc_of<'a>(r: &Resolved<'a>) -> Option<&'a PlumDocument> {
    match r {
        Resolved::Project(d) | Resolved::Conductor(d) => Some(d),
        _ => None,
    }
}

/// Seconds from the song start to `tick` through the Conductor's tempo map.
pub fn seconds_at(doc: &PlumDocument, tick: u64) -> f64 {
    TempoMap::for_document(doc, 48_000).seconds_at_tick(tick)
}

fn tempo_table(lua: &Lua, doc: &PlumDocument) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, p) in doc.conductor().tempo_points().iter().enumerate() {
        let t = lua.create_table()?;
        t.set("tick", ticks::to_lua(p.tick)?)?;
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
        t.set("ramp", p.interp == Interp::Linear)?;
        t.set("implicit", p.implicit)?;
        out.raw_set(i + 1, t)?;
    }
    Ok(out)
}

fn signature_table(lua: &Lua, doc: &PlumDocument) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, s) in doc.conductor().time_signatures().iter().enumerate() {
        let t = lua.create_table()?;
        t.set("tick", ticks::to_lua(s.tick)?)?;
        t.set("numerator", i64::from(s.numerator))?;
        t.set("denominator", i64::from(s.denominator()))?;
        t.set("implicit", s.implicit)?;
        out.raw_set(i + 1, t)?;
    }
    Ok(out)
}

fn marker_table(lua: &Lua, doc: &PlumDocument) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, m) in doc.conductor().markers().iter().enumerate() {
        let t = lua.create_table()?;
        t.set("tick", ticks::to_lua(m.start)?)?;
        if let Some(end) = m.end {
            t.set("end_tick", ticks::to_lua(end)?)?;
        }
        t.set("title", m.title.as_str())?;
        t.set("chapter", m.is_chapter())?;
        t.set("cue", m.cue)?;
        out.raw_set(i + 1, t)?;
    }
    Ok(out)
}

fn division_table(lua: &Lua, doc: &PlumDocument) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    match doc.division().kind() {
        TimingKind::Ppqn(p) => {
            t.set("kind", "ppqn")?;
            t.set("ppqn", i64::from(p))?;
        }
        TimingKind::Smpte {
            frames_per_second,
            ticks_per_frame,
        } => {
            t.set("kind", "smpte")?;
            t.set("fps", i64::from(frames_per_second))?;
            t.set("ticks_per_frame", i64::from(ticks_per_frame))?;
        }
    }
    Ok(t)
}

/// Registers the project and Conductor methods on handles.
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    // --- project-only reads (from the app's ProjectInfo or the document)
    fn project<R>(
        lua: &Lua,
        this: &LuaHandle,
        name: &str,
        f: impl FnOnce(&World, &PlumDocument) -> mlua::Result<R>,
    ) -> mlua::Result<R> {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Project(d) => f(w, d),
            _ => Err(not_for(name, &this.0)),
        })
    }
    m.add_method("file", |lua, this, ()| {
        project(lua, this, "file", |w, _| Ok(w.info.file.clone()))
    });
    m.add_method("file_type", |lua, this, ()| {
        project(lua, this, "file_type", |w, _| Ok(w.info.file_type.name()))
    });
    m.add_method("dirty", |lua, this, ()| {
        project(lua, this, "dirty", |w, _| Ok(w.info.dirty))
    });
    m.add_method("revision", |lua, this, ()| {
        project(lua, this, "revision", |w, _| {
            Ok(i64::try_from(w.info.revision).unwrap_or(i64::MAX))
        })
    });
    m.add_method("format", |lua, this, ()| {
        project(lua, this, "format", |_, d| {
            Ok(match d.export_format() {
                SmfFormat::SingleTrack => 0,
                SmfFormat::Parallel => 1,
                SmfFormat::Sequential => 2,
            })
        })
    });
    m.add_method("ppqn", |lua, this, ()| {
        project(lua, this, "ppqn", |_, d| {
            Ok(d.division()
                .musical_ppqn()
                .map(|p| i64::try_from(p).unwrap_or(i64::MAX)))
        })
    });
    m.add_method("division", |lua, this, ()| {
        project(lua, this, "division", |_, d| division_table(lua, d))
    });
    m.add_method("key_range", |lua, this, ()| {
        project(lua, this, "key_range", |_, d| {
            Ok(d.project().keys.keys() as i64)
        })
    });
    m.add_method("duration_ticks", |lua, this, ()| {
        project(lua, this, "duration_ticks", |_, d| {
            ticks::to_lua(d.duration_ticks())
        })
    });
    m.add_method("duration_seconds", |lua, this, ()| {
        project(lua, this, "duration_seconds", |_, d| {
            Ok(seconds_at(d, d.duration_ticks()))
        })
    });
    m.add_method("seconds_at", |lua, this, tick: Value| {
        let tick = ticks::from_lua(&tick, "The tick")?;
        project(lua, this, "seconds_at", |_, d| Ok(seconds_at(d, tick)))
    });
    m.add_method("content_hash", |lua, this, ()| {
        project(lua, this, "content_hash", |_, d| {
            Ok(format!("{:016x}", d.export_fingerprint()))
        })
    });
    m.add_method("track_count", |lua, this, ()| {
        project(lua, this, "track_count", |_, d| Ok(d.track_count() as i64))
    });
    m.add_method("selected_note_count", |lua, this, ()| {
        project(lua, this, "selected_note_count", |_, d| {
            let tracks: usize = d.tracks().iter().map(|t| t.notes.selected_count()).sum();
            let contents: usize = d
                .clips()
                .contents()
                .iter()
                .map(|c| c.notes.selected_count())
                .sum();
            Ok((tracks + contents) as i64)
        })
    });
    m.add_method("conductor", |lua, this, ()| {
        project(lua, this, "conductor", |w, _| Ok(LuaHandle(w.conductor())))
    });
    m.add_method("folders", |lua, this, ()| {
        project(lua, this, "folders", |w, d| {
            handle_list(
                lua,
                d.project()
                    .folders
                    .folders()
                    .iter()
                    .filter_map(|f| w.folder(f.id)),
            )
        })
    });
    // --- the Conductor (also on the project handle)
    m.add_method("tempos", |lua, this, ()| {
        with_resolved(lua, &this.0, |r| match doc_of(&r) {
            Some(d) => tempo_table(lua, d),
            None => Err(not_for("tempos", &this.0)),
        })
    });
    m.add_method("time_signatures", |lua, this, ()| {
        with_resolved(lua, &this.0, |r| match doc_of(&r) {
            Some(d) => signature_table(lua, d),
            None => Err(not_for("time_signatures", &this.0)),
        })
    });
    m.add_method("markers", |lua, this, ()| {
        with_resolved(lua, &this.0, |r| match doc_of(&r) {
            Some(d) => marker_table(lua, d),
            None => Err(not_for("markers", &this.0)),
        })
    });
}
