//! Stable, fresh-checked handles (v1.0-beta1 B1-0022, `docs/LUA_API.md`
//! "Handles").
//!
//! A script never holds a pointer or an index: it holds a [`Handle`] — the
//! project it came from, the object's stable id and the id's incarnation
//! from the history's ledger (`plumspirit_engine::history::identity`). Every
//! use resolves the handle against the run's [`World`] (one immutable
//! document snapshot). A handle whose object was deleted — or whose creation
//! was undone, or that belongs to another project — is **expired**: `:valid()`
//! answers `false` and every API call that needs the object fails with the
//! `expired` category instead of reaching a different object that now has
//! the same id.
//!
//! Notes and controller events have no ids in PlumSpirit (their place in the
//! copy-on-write chunks is the identity). Their handles are snapshots: valid
//! while the notes (events) of their track or clip content are exactly as
//! they were — `content_revision` is never reused within a run — and the
//! note (event) at the place still has the remembered values.

use mlua::{AnyUserData, MetaMethod, UserData, UserDataMethods, UserDataRef};
use plumspirit_engine::history::{IdKey, IdKind, IncarnationSnapshot, ProjectSerial};
use plumspirit_engine::model::{
    ClipId, ClipPlacement, ContentId, ContentRef, EventStore, FolderInfo, NoteLoc, NoteStore,
    NoteView, OwnedContent, PlumDocument, RawEvent, Subchannel, SubchannelId, Track, TrackId,
};
use std::sync::Arc;

use super::{Category, stop};

/// What one script run reads: the project's document at one moment, the id
/// ledger at the same moment and what the app knows about the project
/// beyond the document. Cheap to clone (shared pointers).
#[derive(Clone)]
pub struct World {
    pub doc: Arc<PlumDocument>,
    pub ids: IncarnationSnapshot,
    pub info: Arc<ProjectInfo>,
    /// Nonzero for private transaction group handles; never persisted.
    pub(super) group_scope: u64,
    pub(super) run: u64,
    pub(super) group_origin_floor: u32,
}

/// Where the open project came from (`project:file_type()`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileType {
    /// Created with New Project, never saved.
    #[default]
    New,
    /// A native `.lup` project.
    Lup,
    /// An imported MIDI file (Save asks for a `.lup` destination).
    Midi,
}

impl FileType {
    pub fn name(self) -> &'static str {
        match self {
            FileType::New => "new",
            FileType::Lup => "lup",
            FileType::Midi => "midi",
        }
    }
}

/// The app's knowledge of the open project that is not in the document.
#[derive(Clone, Debug, Default)]
pub struct ProjectInfo {
    /// The project's file, if it has one.
    pub file: Option<String>,
    pub file_type: FileType,
    /// Unsaved changes.
    pub dirty: bool,
    /// The history's state id: changes with every edit, Undo and Redo.
    pub revision: u64,
    /// The dark theme is on (automatic track colors depend on it).
    pub dark: bool,
    /// Immutable native note palette captured for this run, never a UI callback.
    pub note_palette: Arc<crate::ui::theme::NotePalette>,
    /// Track ids whose subtracks are collapsed in the Track List, sorted.
    pub collapsed: Vec<u32>,
    /// The active track (the Piano Roll's), the Track List's selected tracks
    /// and clips, and the ruler's time selection `[start, end)`.
    pub active_track: Option<TrackId>,
    pub selected_tracks: Vec<TrackId>,
    pub selected_clips: Vec<ClipId>,
    pub time_selection: Option<(u64, u64)>,
    /// Immutable native transport facts captured with this run, never a live pointer.
    pub playback: Option<super::playback::Snapshot>,
    /// Opaque native worker preparation resources, never converted into Lua values.
    pub playback_resources: Option<crate::audio::midi_overlay::Resources>,
    pub views: [Option<super::visual::Snapshot>; 2],
}

/// The object a handle names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The open project.
    Project,
    /// The project's single Conductor (tempo, meter, markers).
    Conductor,
    /// A track; a subtrack is a track with a parent.
    Track(TrackId),
    /// A Track Folder (ids from 1).
    Folder(u32),
    /// A project-wide native note group.
    Group { id: u32, scope: u64 },
    /// A clip placement (a primary clip is its track's own content).
    Clip(ClipId),
    /// A clip content the project owns (linked clips share one).
    Content(ContentId),
    /// A subchannel of a track.
    Subchannel(TrackId, SubchannelId),
    /// One note of a track's or a content's notes, at the note state
    /// `revision` (a `content_revision`).
    Note {
        host: ContentRef,
        revision: u64,
        loc: NoteLoc,
        note: NoteView,
    },
    /// One preserved event (controller, pitch bend, program change, …) of a
    /// track or content, at the event state `revision`.
    Event {
        host: ContentRef,
        revision: u64,
        index: u32,
        event: RawEvent,
    },
}

/// A stable reference to one object of one open project.
#[derive(Clone, Debug)]
pub struct Handle {
    project: ProjectSerial,
    target: Target,
    /// The incarnation of the id the handle depends on (see `ledger_key`).
    incarnation: u32,
    birth: Option<Arc<super::tx::BirthFence>>,
    // Only native custom-tool content targets bind an editing context. Ordinary
    // API0.1 content handles keep their existing first-placement interpretation.
    content_owner: Option<(TrackId, u32)>,
}

impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        self.project == other.project
            && self.target == other.target
            && self.incarnation == other.incarnation
    }
}
impl Eq for Handle {}
fn attach_birth(lua: &mlua::Lua, mut h: Handle) -> mlua::Result<Handle> {
    let track = match h.target {
        Target::Track(id) | Target::Clip(ClipId::Primary(id)) | Target::Subchannel(id, _) => {
            Some(id)
        }
        Target::Note {
            host: ContentRef::Track(id),
            ..
        }
        | Target::Event {
            host: ContentRef::Track(id),
            ..
        } => Some(id),
        _ => None,
    };
    if let Some(id) = track
        && let Some(mut slot) = lua.app_data_mut::<super::tx::TxSlot>()
    {
        h.birth = slot.track_birth(lua, id)?;
    }
    Ok(h)
}

/// What a valid handle resolves to.
pub enum Resolved<'a> {
    Project(&'a PlumDocument),
    Conductor(&'a PlumDocument),
    Track {
        index: usize,
        track: &'a Track,
    },
    Folder(&'a FolderInfo),
    Group(&'a plumspirit_engine::model::GroupInfo),
    Clip(ClipPlacement),
    Content(&'a OwnedContent),
    Subchannel {
        index: usize,
        track: &'a Track,
        sub: &'a Subchannel,
    },
    Note {
        notes: &'a NoteStore,
        loc: NoteLoc,
        note: NoteView,
    },
    Event {
        events: &'a EventStore,
        index: usize,
        event: RawEvent,
    },
}

/// The ledger key whose incarnation a target depends on (`None`: the
/// project-wide singletons).
fn ledger_key(t: &Target) -> Option<IdKey> {
    let host_key = |h: &ContentRef| match h {
        ContentRef::Track(id) => (IdKind::Track, 0, id.0),
        ContentRef::Owned(c) => (IdKind::Content, 0, c.0),
    };
    match t {
        Target::Project | Target::Conductor => None,
        Target::Track(id) | Target::Clip(ClipId::Primary(id)) => Some((IdKind::Track, 0, id.0)),
        Target::Folder(f) => Some((IdKind::Folder, 0, *f)),
        Target::Group { id, .. } => Some((IdKind::Group, 0, *id)),
        Target::Clip(ClipId::Placed(n)) => Some((IdKind::Clip, 0, *n)),
        Target::Content(c) => Some((IdKind::Content, 0, c.0)),
        Target::Subchannel(t, s) => Some((IdKind::Subchannel, t.0, s.0)),
        Target::Note { host, .. } | Target::Event { host, .. } => Some(host_key(host)),
    }
}

/// A note's identity without its selection flag.
fn same_note(a: &NoteView, b: &NoteView) -> bool {
    NoteView {
        selected: false,
        ..*a
    } == NoteView {
        selected: false,
        ..*b
    }
}

impl World {
    pub fn new(doc: Arc<PlumDocument>, ids: IncarnationSnapshot) -> World {
        World {
            doc,
            ids,
            group_scope: 0,
            run: 0,
            group_origin_floor: u32::MAX,
            info: Arc::new(ProjectInfo {
                dark: true,
                ..ProjectInfo::default()
            }),
        }
    }

    /// The same world with the app's project information.
    pub fn with_info(mut self, info: ProjectInfo) -> World {
        self.info = Arc::new(info);
        self
    }

    pub fn project_serial(&self) -> ProjectSerial {
        self.ids.project()
    }

    fn make(&self, target: Target) -> Handle {
        Handle {
            project: self.ids.project(),
            target,
            incarnation: ledger_key(&target).map_or(0, |k| self.ids.of(k)),
            birth: None,
            content_owner: None,
        }
    }

    /// The project handle.
    pub fn project(&self) -> Handle {
        self.make(Target::Project)
    }

    /// The Conductor handle.
    pub fn conductor(&self) -> Handle {
        self.make(Target::Conductor)
    }

    /// The track at list position `index` (tree order).
    pub fn track_at(&self, index: usize) -> Option<Handle> {
        let t = self.doc.track(index)?;
        Some(self.make(Target::Track(t.id())))
    }

    pub fn track(&self, id: TrackId) -> Option<Handle> {
        self.doc.index_of(id)?;
        Some(self.make(Target::Track(id)))
    }

    pub fn group(&self, id: u32) -> Option<Handle> {
        self.doc.project().groups.get(id)?;
        let scope = if id >= self.group_origin_floor {
            self.group_scope
        } else {
            0
        };
        Some(self.make(Target::Group { id, scope }))
    }

    pub fn folder(&self, id: u32) -> Option<Handle> {
        self.doc.project().folders.get(id)?;
        Some(self.make(Target::Folder(id)))
    }

    pub fn clip(&self, id: ClipId) -> Option<Handle> {
        self.doc.clips().placement(id)?;
        Some(self.make(Target::Clip(id)))
    }

    pub fn content(&self, id: ContentId) -> Option<Handle> {
        self.doc.clips().content(id)?;
        Some(self.make(Target::Content(id)))
    }

    pub(super) fn content_on_track(&self, id: ContentId, track: TrackId) -> Option<Handle> {
        self.doc.track_by_id(track)?;
        if !self
            .doc
            .clips()
            .explicit_placements()
            .iter()
            .any(|p| p.track == track && p.content == ContentRef::Owned(id))
        {
            return None;
        }
        let mut handle = self.content(id)?;
        handle.content_owner = Some((track, self.ids.of((IdKind::Track, 0, track.0))));
        Some(handle)
    }

    pub fn subchannel(&self, track: TrackId, id: SubchannelId) -> Option<Handle> {
        if !self.doc.track_by_id(track)?.subchannels().contains(id) {
            return None;
        }
        Some(self.make(Target::Subchannel(track, id)))
    }

    /// The note at `loc` of `host`'s notes.
    pub fn note(&self, host: ContentRef, loc: NoteLoc) -> Option<Handle> {
        let notes = self.doc.notes_of(host)?;
        let note = notes.get(loc)?;
        Some(self.make(Target::Note {
            host,
            revision: notes.content_revision(),
            loc,
            note,
        }))
    }

    /// The preserved event at `index` of `host`'s events.
    pub fn event(&self, host: ContentRef, index: usize) -> Option<Handle> {
        let (_, revision) = self.doc.content_revision(host)?;
        let event = *self.doc.events_of(host)?.events().get(index)?;
        Some(self.make(Target::Event {
            host,
            revision,
            index: u32::try_from(index).ok()?,
            event,
        }))
    }
}

impl Handle {
    pub fn target(&self) -> Target {
        self.target
    }

    pub fn project_serial(&self) -> ProjectSerial {
        self.project
    }

    /// The handle's family name (stable API: `"project"`, `"conductor"`,
    /// `"track"`, `"folder"`, `"clip"`, `"clip_content"`, `"subchannel"`,
    /// `"note"`, `"event"`).
    pub fn family(&self) -> &'static str {
        match self.target {
            Target::Project => "project",
            Target::Conductor => "conductor",
            Target::Track(_) => "track",
            Target::Folder(_) => "folder",
            Target::Group { .. } => "group",
            Target::Clip(_) => "clip",
            Target::Content(_) => "clip_content",
            Target::Subchannel(..) => "subchannel",
            Target::Note { .. } => "note",
            Target::Event { .. } => "event",
        }
    }

    /// A string naming the object, equal for every handle to the same object
    /// incarnation and different otherwise (usable as a table key; stable
    /// while the project stays open).
    pub fn key(&self) -> String {
        let p = self.project;
        let i = self.incarnation;
        match self.target {
            Target::Project => format!("project:{p}"),
            Target::Conductor => format!("conductor:{p}"),
            Target::Track(t) => format!("track:{p}:{}.{i}", t.0),
            Target::Folder(f) => format!("folder:{p}:{f}.{i}"),
            Target::Group { id, scope } => format!("group:{p}:{id}.{i}@{scope}"),
            Target::Clip(ClipId::Primary(t)) => format!("clip:{p}:t{}.{i}", t.0),
            Target::Clip(ClipId::Placed(n)) => format!("clip:{p}:{n}.{i}"),
            Target::Content(c) => format!("clip_content:{p}:{}.{i}", c.0),
            Target::Subchannel(t, s) => format!("subchannel:{p}:{}/{}.{i}", t.0, s.0),
            Target::Note {
                host,
                revision,
                loc,
                ..
            } => format!(
                "note:{p}:{}@{revision}:{}.{}",
                host_name(host),
                loc.chunk,
                loc.index
            ),
            Target::Event {
                host,
                revision,
                index,
                ..
            } => format!("event:{p}:{}@{revision}:{index}", host_name(host)),
        }
    }

    pub(super) fn content_host(&self) -> Option<TrackId> {
        self.content_owner.map(|(track, _)| track)
    }

    /// Resolves the handle in `w`, or says (as a sentence) why it is expired.
    pub fn resolve<'a>(&self, w: &'a World) -> Result<Resolved<'a>, String> {
        if self.birth.as_ref().is_some_and(|f| !f.valid_in(w.run)) {
            return Err("This handle belongs to an abandoned transaction; reacquire it from the current project.".into());
        }
        if self.project != w.ids.project() {
            return Err(format!(
                "This {} handle belongs to a project that is no longer open; get a new handle from the current project.",
                self.family_words()
            ));
        }
        if let Target::Group { scope, .. } = self.target
            && scope != 0
            && scope != w.group_scope
        {
            return Err("This group handle belongs to a closed transaction; reacquire it from the current project.".into());
        }
        if let Some(k) = ledger_key(&self.target)
            && w.ids.of(k) != self.incarnation
        {
            return Err(self.gone());
        }
        let doc = &*w.doc;
        if let Some((track, incarnation)) = self.content_owner {
            let Target::Content(content) = self.target else {
                return Err(self.gone());
            };
            if w.ids.of((IdKind::Track, 0, track.0)) != incarnation
                || doc.track_by_id(track).is_none()
                || !doc
                    .clips()
                    .explicit_placements()
                    .iter()
                    .any(|p| p.track == track && p.content == ContentRef::Owned(content))
            {
                return Err("This tool's clip editing track is expired; reopen the tool on the current clip.".into());
            }
        }
        let r = match self.target {
            Target::Project => Some(Resolved::Project(doc)),
            Target::Conductor => Some(Resolved::Conductor(doc)),
            Target::Track(id) => doc.index_of(id).map(|index| Resolved::Track {
                index,
                track: &doc.tracks()[index],
            }),
            Target::Folder(f) => doc.project().folders.get(f).map(Resolved::Folder),
            Target::Group { id, .. } => doc.project().groups.get(id).map(Resolved::Group),
            Target::Clip(id) => doc.clips().placement(id).map(Resolved::Clip),
            Target::Content(c) => doc.clips().content(c).map(Resolved::Content),
            Target::Subchannel(t, s) => doc.index_of(t).and_then(|index| {
                let track = &doc.tracks()[index];
                track
                    .subchannels()
                    .get(s)
                    .map(|sub| Resolved::Subchannel { index, track, sub })
            }),
            Target::Note {
                host,
                revision,
                loc,
                note,
            } => doc.notes_of(host).and_then(|notes| {
                if notes.content_revision() != revision {
                    return None;
                }
                let now = notes.get(loc)?;
                same_note(&now, &note).then_some(Resolved::Note {
                    notes,
                    loc,
                    note: now,
                })
            }),
            Target::Event {
                host,
                revision,
                index,
                event,
            } => doc.content_revision(host).and_then(|(_, ev_rev)| {
                if ev_rev != revision {
                    return None;
                }
                let events = doc.events_of(host)?;
                let i = index as usize;
                let now = *events.events().get(i)?;
                (now == event).then_some(Resolved::Event {
                    events,
                    index: i,
                    event: now,
                })
            }),
        };
        r.ok_or_else(|| self.gone())
    }

    /// True when the handle resolves in `w`.
    pub fn is_valid(&self, w: &World) -> bool {
        self.resolve(w).is_ok()
    }

    pub fn family_words(&self) -> &'static str {
        match self.target {
            Target::Content(_) => "clip content",
            _ => self.family(),
        }
    }

    fn gone(&self) -> String {
        match self.target {
            Target::Note { .. } => "This note handle is expired: the notes of its track or clip changed since it was taken (or the note was deleted); read the notes again.".into(),
            Target::Event { .. } => "This event handle is expired: the events of its track or clip changed since it was taken (or the event was deleted); read the events again.".into(),
            _ => format!(
                "This {} handle is expired: the {} it referred to was deleted (or its creation was undone); get a new handle.",
                self.family_words(),
                self.family_words()
            ),
        }
    }
}

fn host_name(h: ContentRef) -> String {
    match h {
        ContentRef::Track(t) => format!("t{}", t.0),
        ContentRef::Owned(c) => format!("c{}", c.0),
    }
}

/// The world a run reads, kept in the Lua state's app data.
pub(super) struct WorldSlot(pub Option<World>);

/// The capabilities the user granted the running plugin (app data).
pub(super) struct Granted(pub Arc<std::collections::BTreeSet<super::Capability>>);

/// Fails with the `capability` category unless the plugin may read the
/// project (every read of the project through a handle).
pub fn require_read(lua: &mlua::Lua) -> mlua::Result<()> {
    super::reactive::check_read(lua)?;
    let granted = lua
        .app_data_ref::<Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    super::Host::require(&granted, super::Capability::ReadProject)
}

/// The current run's world (a clone: a few pointer copies), or `None`.
/// While a transaction is open, reads see its working copy.
pub fn current_world(lua: &mlua::Lua) -> Option<World> {
    let base = lua.app_data_ref::<WorldSlot>().and_then(|s| s.0.clone());
    if let Some(slot) = lua.app_data_ref::<super::tx::TxSlot>()
        && let Some(w) = slot.world(base.as_ref())
    {
        return Some(w);
    }
    base
}

/// Resolves `h` against the current run's world and calls `f` with both, or
/// fails with the `expired` category (every API function that takes a handle
/// uses it).
pub fn with_world<R>(
    lua: &mlua::Lua,
    h: &Handle,
    f: impl FnOnce(&World, Resolved<'_>) -> mlua::Result<R>,
) -> mlua::Result<R> {
    require_read(lua)?;
    let Some(w) = current_world(lua) else {
        return Err(stop(
            Category::Expired,
            "No project is open, so this handle cannot be used.",
        ));
    };
    match h.resolve(&w) {
        Ok(r) => f(&w, r),
        Err(why) => Err(stop(Category::Expired, why)),
    }
}

/// [`with_world`] for functions that only read the object.
pub fn with_resolved<R>(
    lua: &mlua::Lua,
    h: &Handle,
    f: impl FnOnce(Resolved<'_>) -> mlua::Result<R>,
) -> mlua::Result<R> {
    with_world(lua, h, |_, r| f(r))
}

/// A method called on a handle of the wrong family.
pub fn not_for(method: &str, h: &Handle) -> mlua::Error {
    stop(
        Category::Runtime,
        format!(
            "{method}() is not available on a {} handle.",
            h.family_words()
        ),
    )
}

/// `hs` as a Lua array of handles.
pub fn handle_list(
    lua: &mlua::Lua,
    hs: impl IntoIterator<Item = Handle>,
) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    for (i, h) in hs.into_iter().enumerate() {
        t.raw_set(i + 1, LuaHandle(attach_birth(lua, h)?))?;
    }
    Ok(t)
}

/// `h` as a Lua value (nil for `None`).
pub fn handle_or_nil(lua: &mlua::Lua, h: Option<Handle>) -> mlua::Result<mlua::Value> {
    match h {
        Some(h) => lua
            .create_userdata(LuaHandle(attach_birth(lua, h)?))
            .map(mlua::Value::UserData),
        None => Ok(mlua::Value::Nil),
    }
}

/// A [`Handle`] as Lua userdata. Methods: `kind()`, `key()`, `valid()`,
/// `check()`; `==` compares objects; `tostring` names the object.
pub struct LuaHandle(pub Handle);

impl UserData for LuaHandle {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("kind", |lua, this, ()| {
            // A track with a parent is a subtrack (when it still exists).
            let kind = match this.0.target {
                Target::Track(_) => with_resolved(lua, &this.0, |r| {
                    Ok(match r {
                        Resolved::Track { track, .. } if track.parent().is_some() => "subtrack",
                        _ => "track",
                    })
                })
                .unwrap_or("track"),
                _ => this.0.family(),
            };
            Ok(kind)
        });
        m.add_method("key", |_, this, ()| Ok(this.0.key()));
        m.add_method("valid", |lua, this, ()| {
            Ok(with_resolved(lua, &this.0, |_| Ok(())).is_ok())
        });
        m.add_function("check", |lua, ud: AnyUserData| {
            {
                let this = ud.borrow::<LuaHandle>()?;
                with_resolved(lua, &this.0, |_| Ok(()))?;
            }
            Ok(ud)
        });
        m.add_meta_method(MetaMethod::Eq, |_, this, other: UserDataRef<LuaHandle>| {
            Ok(this.0.project == other.0.project
                && this.0.incarnation == other.0.incarnation
                && same_target(&this.0.target, &other.0.target))
        });
        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("PlumSpirit {}", this.0.key()))
        });
        super::project::add_methods(m);
        super::tracks::add_methods(m);
        super::clips::add_methods(m);
        super::notes::add_methods(m);
        super::tx::groups::add_read_methods(m);
        super::events::add_methods(m);
        super::timing::add_methods(m);
    }
}

/// Targets name the same object (notes ignore their selection flag).
fn same_target(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (
            Target::Note {
                host: h1,
                revision: r1,
                loc: l1,
                note: n1,
            },
            Target::Note {
                host: h2,
                revision: r2,
                loc: l2,
                note: n2,
            },
        ) => h1 == h2 && r1 == r2 && l1 == l2 && same_note(n1, n2),
        _ => a == b,
    }
}
