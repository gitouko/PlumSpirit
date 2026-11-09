//! The Tracks / Subtracks / Folders API, reads (v1.0-beta1 B1-0027,
//! `docs/LUA_API.md` "Tracks").
//!
//! Track handles read the track tree in list (depth-first) order: parent,
//! children, depth, folder, name, label, color, Mute / Solo, collapse and
//! visibility, note and clip counts, the default MIDI channel and the
//! subchannels with their MIDI channels (the logical → export mapping: a
//! note keeps its own channel; a subchannel's channel is what its new notes
//! get). Folder and subchannel handles read their own fields.

use mlua::{Lua, UserDataMethods, Value};
use plumspirit_engine::model::{PlumDocument, Track, TrackId};

use super::handles::{LuaHandle, Resolved, World, handle_list, handle_or_nil, not_for, with_world};

/// The top-level ancestor of the track at `index` (itself when top-level).
fn root_of(doc: &PlumDocument, index: usize) -> usize {
    let mut i = index;
    let mut guard = doc.track_count();
    while let Some(p) = doc.track(i).and_then(Track::parent) {
        match doc.index_of(p) {
            Some(pi) if guard > 0 => {
                i = pi;
                guard -= 1;
            }
            _ => break,
        }
    }
    i
}

/// Number of ancestors of the track at `index`.
fn depth_of(doc: &PlumDocument, index: usize) -> usize {
    let mut d = 0;
    let mut i = index;
    while let Some(p) = doc.track(i).and_then(Track::parent) {
        match doc.index_of(p) {
            Some(pi) if d < doc.track_count() => {
                i = pi;
                d += 1;
            }
            _ => break,
        }
    }
    d
}

fn collapsed(w: &World, id: TrackId) -> bool {
    w.info.collapsed.binary_search(&id.0).is_ok()
}

/// Shown in the Track List: no collapsed ancestor and not in a collapsed
/// folder.
fn visible(w: &World, doc: &PlumDocument, index: usize) -> bool {
    let mut i = index;
    let mut guard = doc.track_count();
    while let Some(p) = doc.track(i).and_then(Track::parent) {
        if collapsed(w, p) {
            return false;
        }
        match doc.index_of(p) {
            Some(pi) if guard > 0 => {
                i = pi;
                guard -= 1;
            }
            _ => break,
        }
    }
    let root = doc.tracks()[i].id();
    let folder = doc.project().folders.folder_of(root);
    folder == 0 || !doc.project().folders_collapsed.contains(&folder)
}

/// The Track List's rows top to bottom: the Conductor, then tracks with a
/// folder header before each run of a folder's top-level tracks (an empty
/// folder before its anchor track, or at the end). `only_visible` leaves out
/// what collapsed tracks and folded folders hide (the app's own row list).
fn rows(w: &World, doc: &PlumDocument, only_visible: bool) -> Vec<super::handles::Handle> {
    let mut out = vec![w.conductor()];
    if only_visible {
        let collapsed: std::collections::HashSet<TrackId> =
            w.info.collapsed.iter().map(|&id| TrackId(id)).collect();
        let ordinals = doc.track_ordinals();
        for row in crate::app::hierarchy::visible_rows(doc, &collapsed, &ordinals) {
            let h = if row.header {
                w.folder(row.folder)
            } else {
                w.track_at(row.index)
            };
            out.extend(h);
        }
        return out;
    }
    let folders = &doc.project().folders;
    let members: std::collections::HashSet<u32> = doc
        .tracks()
        .iter()
        .filter(|t| t.parent().is_none())
        .map(|t| folders.folder_of(t.id()))
        .filter(|&f| f != 0)
        .collect();
    let mut shown_empty = std::collections::HashSet::new();
    let mut current = 0u32;
    for (i, t) in doc.tracks().iter().enumerate() {
        if t.parent().is_none() {
            for f in folders
                .folders()
                .iter()
                .filter(|f| !members.contains(&f.id) && f.anchor == Some(t.id()))
            {
                out.extend(w.folder(f.id));
                shown_empty.insert(f.id);
                current = 0;
            }
            let f = folders.folder_of(t.id());
            if f != current {
                current = f;
                if f != 0 {
                    out.extend(w.folder(f));
                }
            }
        }
        out.extend(w.track_at(i));
    }
    for f in folders.folders() {
        if !members.contains(&f.id) && !shown_empty.contains(&f.id) {
            out.extend(w.folder(f.id));
        }
    }
    out
}

/// A clip's colour (its own, else its track's) as `{ r, g, b, custom }`;
/// `None` for other handles.
fn clip_color(lua: &Lua, this: &LuaHandle) -> mlua::Result<Option<mlua::Table>> {
    if !matches!(this.0.target(), super::handles::Target::Clip(_)) {
        return Ok(None);
    }
    with_world(lua, &this.0, |w, r| {
        let Resolved::Clip(p) = r else {
            return Err(not_for("color", &this.0));
        };
        let [r, g, b] = match p.color {
            Some(c) => [(c >> 16) as u8, (c >> 8) as u8, c as u8],
            None => {
                let t = w.doc.track_by_id(p.track);
                let custom = t.and_then(|t| t.props().onion_color);
                custom.unwrap_or_else(|| crate::ui::theme::auto_track_rgb(p.track.0, w.info.dark))
            }
        };
        let c = lua.create_table()?;
        c.set("r", i64::from(r))?;
        c.set("g", i64::from(g))?;
        c.set("b", i64::from(b))?;
        c.set("custom", p.color.is_some())?;
        Ok(Some(c))
    })
}

/// Registers the track, folder and subchannel methods (and `name` /
/// `id` for every family that has them).
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    fn track<R>(
        lua: &Lua,
        this: &LuaHandle,
        name: &str,
        f: impl FnOnce(&World, &PlumDocument, usize, &Track) -> mlua::Result<R>,
    ) -> mlua::Result<R> {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Track { index, track } => f(w, &w.doc, index, track),
            _ => Err(not_for(name, &this.0)),
        })
    }

    m.add_method("name", |lua, this, ()| {
        with_world(lua, &this.0, |_, r| {
            Ok(match r {
                Resolved::Project(d) => d.project().name.clone(),
                Resolved::Track { track, .. } => track.name().unwrap_or("").to_owned(),
                Resolved::Folder(f) => f.name.clone(),
                Resolved::Group(g) => g.name.clone().unwrap_or_default(),
                Resolved::Subchannel { sub, .. } => sub
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Subchannel {}", sub.id.number())),
                Resolved::Clip(p) => p.name.clone().unwrap_or_default(),
                Resolved::Content(c) => c.name.clone().unwrap_or_default(),
                Resolved::Conductor(_) => "Conductor".to_owned(),
                _ => return Err(not_for("name", &this.0)),
            })
        })
    });
    m.add_method("id", |lua, this, ()| {
        with_world(lua, &this.0, |_, r| {
            Ok(match r {
                Resolved::Track { track, .. } => i64::from(track.id().0),
                Resolved::Folder(f) => i64::from(f.id),
                Resolved::Group(g) => i64::from(g.id),
                Resolved::Subchannel { sub, .. } => i64::from(sub.id.0),
                Resolved::Content(c) => i64::from(c.id.0),
                _ => return Err(not_for("id", &this.0)),
            })
        })
    });

    // --- tracks
    m.add_method("label", |lua, this, ()| {
        track(lua, this, "label", |_, d, i, _| Ok(d.track_label(i)))
    });
    m.add_method("index", |lua, this, ()| {
        track(lua, this, "index", |_, _, i, _| Ok(i as i64 + 1))
    });
    m.add_method("depth", |lua, this, ()| {
        track(lua, this, "depth", |_, d, i, _| Ok(depth_of(d, i) as i64))
    });
    m.add_method("parent", |lua, this, ()| {
        track(lua, this, "parent", |w, _, _, t| {
            handle_or_nil(lua, t.parent().and_then(|p| w.track(p)))
        })
    });
    m.add_method("children", |lua, this, ()| {
        track(lua, this, "children", |w, d, i, _| {
            handle_list(
                lua,
                d.children_of(i).into_iter().filter_map(|c| w.track_at(c)),
            )
        })
    });
    m.add_method("folder", |lua, this, ()| {
        track(lua, this, "folder", |w, d, i, _| {
            let root = d.tracks()[root_of(d, i)].id();
            let f = d.project().folders.folder_of(root);
            handle_or_nil(lua, (f != 0).then(|| w.folder(f)).flatten())
        })
    });
    m.add_method("color", |lua, this, ()| {
        if let Some(c) = clip_color(lua, this)? {
            return Ok(c);
        }
        track(lua, this, "color", |w, _, _, t| {
            let custom = t.props().onion_color;
            let [r, g, b] =
                custom.unwrap_or_else(|| crate::ui::theme::auto_track_rgb(t.id().0, w.info.dark));
            let c = lua.create_table()?;
            c.set("r", i64::from(r))?;
            c.set("g", i64::from(g))?;
            c.set("b", i64::from(b))?;
            c.set("custom", custom.is_some())?;
            Ok(c)
        })
    });
    m.add_method("mute", |lua, this, ()| {
        with_world(lua, &this.0, |_, r| match r {
            Resolved::Track { track, .. } => Ok(track.mute),
            // A muted clip is shown but does not play, export or count.
            Resolved::Clip(p) => Ok(p.muted),
            _ => Err(not_for("mute", &this.0)),
        })
    });
    m.add_method("solo", |lua, this, ()| {
        track(lua, this, "solo", |_, _, _, t| Ok(t.solo))
    });
    m.add_method("collapsed", |lua, this, ()| {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Track { track, .. } => Ok(collapsed(w, track.id())),
            Resolved::Folder(f) => Ok(w.doc.project().folders_collapsed.contains(&f.id)),
            _ => Err(not_for("collapsed", &this.0)),
        })
    });
    m.add_method("visible", |lua, this, ()| {
        track(lua, this, "visible", |w, d, i, _| Ok(visible(w, d, i)))
    });
    m.add_method("note_count", |lua, this, ()| {
        with_world(lua, &this.0, |w, r| match r {
            // What the track plays: its clips' windows (B1-S06).
            Resolved::Track { index, .. } => Ok(w.doc.placed_note_count(index) as i64),
            Resolved::Content(c) => Ok(c.notes.len() as i64),
            Resolved::Project(d) => Ok(d.note_count() as i64),
            _ => Err(not_for("note_count", &this.0)),
        })
    });
    m.add_method("stored_note_count", |lua, this, ()| {
        track(lua, this, "stored_note_count", |_, _, _, t| {
            Ok(t.notes.len() as i64)
        })
    });
    m.add_method("clip_count", |lua, this, ()| {
        track(lua, this, "clip_count", |_, d, i, _| {
            Ok(d.placements_on(i).len() as i64)
        })
    });
    m.add_method("default_channel", |lua, this, ()| {
        track(lua, this, "default_channel", |_, _, _, t| {
            Ok(i64::from(t.props().default_channel))
        })
    });
    m.add_method("subchannels", |lua, this, ()| {
        track(lua, this, "subchannels", |w, _, _, t| {
            handle_list(
                lua,
                t.subchannels()
                    .entries()
                    .iter()
                    .filter_map(|s| w.subchannel(t.id(), s.id)),
            )
        })
    });

    // --- folders
    m.add_method("tracks", |lua, this, ()| {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Folder(f) => {
                let d = &w.doc;
                let folders = &d.project().folders;
                handle_list(
                    lua,
                    d.tracks()
                        .iter()
                        .filter(|t| t.parent().is_none() && folders.folder_of(t.id()) == f.id)
                        .filter_map(|t| w.track(t.id())),
                )
            }
            Resolved::Project(d) => {
                handle_list(lua, (0..d.track_count()).filter_map(|i| w.track_at(i)))
            }
            _ => Err(not_for("tracks", &this.0)),
        })
    });

    // --- the Track List's rows
    m.add_method("rows", |lua, this, opts: Option<mlua::Table>| {
        let only_visible = match &opts {
            Some(t) => t.get::<Option<bool>>("visible")?.unwrap_or(false),
            None => false,
        };
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Project(d) => handle_list(lua, rows(w, d, only_visible)),
            _ => Err(not_for("rows", &this.0)),
        })
    });

    // --- subchannels
    m.add_method("note_color", |lua, this, velocity: Value| {
        with_world(lua, &this.0, |w, r| {
            let Resolved::Subchannel { sub, .. } = r else {
                return Err(not_for("note_color", &this.0));
            };
            let velocity = match velocity {
                Value::Nil => 100,
                Value::Integer(v @ 1..=127) => v as u8,
                _ => {
                    return Err(super::stop(
                        super::Category::Runtime,
                        "note_color([velocity]) needs an integer velocity from1through127.",
                    ));
                }
            };
            let color = w
                .info
                .note_palette
                .entry(sub.id.0, velocity, w.info.dark)
                .fill;
            let out = lua.create_table_with_capacity(0, 3)?;
            out.set("r", color.r())?;
            out.set("g", color.g())?;
            out.set("b", color.b())?;
            Ok(out)
        })
    });
    m.add_method("number", |lua, this, ()| {
        with_world(lua, &this.0, |_, r| match r {
            Resolved::Subchannel { sub, .. } => Ok(sub.id.number() as i64),
            _ => Err(not_for("number", &this.0)),
        })
    });
    m.add_method("channel", |lua, this, ()| {
        with_world(lua, &this.0, |_, r| match r {
            Resolved::Subchannel { sub, .. } => Ok(i64::from(sub.midi_channel)),
            _ => Err(not_for("channel", &this.0)),
        })
    });
    m.add_method("track", |lua, this, arg: Value| {
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Subchannel { track, .. } => handle_or_nil(lua, w.track(track.id())),
            // The track a clip sits on (B1-0028).
            Resolved::Clip(p) => handle_or_nil(lua, w.track(p.track)),
            Resolved::Project(d) => {
                // project:track(n): the track at list position n (1-based).
                let n = match arg {
                    Value::Integer(n) => n,
                    _ => {
                        return Err(super::stop(
                            super::Category::Runtime,
                            "project:track(n) needs the track's list position (1, 2, ...).",
                        ));
                    }
                };
                let i = usize::try_from(n - 1).ok().filter(|&i| i < d.track_count());
                handle_or_nil(lua, i.and_then(|i| w.track_at(i)))
            }
            _ => Err(not_for("track", &this.0)),
        })
    });
}
