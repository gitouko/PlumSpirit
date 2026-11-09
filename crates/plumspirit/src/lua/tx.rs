//! Script transactions (v1.0-beta1 B1-0026 / B1-0027, `docs/LUA_API.md`
//! "Transactions").
//!
//! `plum.transaction{label = ...}` opens one change set. Every write is
//! applied at once to a **working copy** of the project (validated by the
//! same engine operations the app uses — `editor::folders` planners,
//! `history::plan_track_moves`, `prepare_rename`), so the script reads its
//! own changes, and the same [`Operation`] is recorded for the commit. The
//! document itself is never touched during a run: `tx:commit()` hands the
//! recorded operations to the host as one `Operation::Group` (one Undo step,
//! all or nothing), which the app applies after the run ([`Commits`]). A
//! script that fails, cancels, or ends with the transaction still open
//! changes nothing.
//!
//! Deleting notes counts them; at [`CONFIRM_REMOVED_NOTES`] or more the
//! commit needs the user's confirmation before the app applies it.

use mlua::{AnyUserData, Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::editor::controllers::prepare_rename;
use plumspirit_engine::editor::{PreparedEdit, folders};
use plumspirit_engine::history::{
    History, HistoryLimits, Incarnations, Operation, TrackPlace, plan_track_moves,
};
use plumspirit_engine::model::{
    Interp, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RankSet, RankSetBuilder, RawEvent,
    RawKind, Subchannel, SubchannelId, TempoValue, TrackId, project::Bpm,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::handles::{
    Handle, LuaHandle, Resolved, World, WorldSlot, handle_list, handle_or_nil, with_world,
};
use super::{Capability, Category, Host, stop};

#[path = "tx_note_input.rs"]
mod note_input;

#[path = "groups.rs"]
pub(super) mod groups;

#[path = "tx_writer.rs"]
mod writer;

#[path = "tx_image_mask.rs"]
mod image_mask;

#[path = "metadata.rs"]
pub(super) mod metadata;

/// Notes a transaction may remove before its commit needs the user's
/// confirmation.
pub const CONFIRM_REMOVED_NOTES: u64 = 1_000_000;

/// Distinct transaction labels kept for the Undo menu (the history stores
/// `&'static str`); beyond this, commits are labeled "Script Edit".
const MAX_LABELS: usize = 4096;

fn intern(label: &str) -> &'static str {
    static LABELS: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let label = label.trim();
    if label.is_empty() {
        return "Script Edit";
    }
    let label: String = label.chars().take(80).collect();
    let Ok(mut g) = LABELS.lock() else {
        return "Script Edit";
    };
    let set = g.get_or_insert_with(HashSet::new);
    if let Some(&s) = set.get(label.as_str()) {
        return s;
    }
    if set.len() >= MAX_LABELS {
        return "Script Edit";
    }
    let s: &'static str = Box::leak(label.into_boxed_str());
    set.insert(s);
    s
}

/// The native quota remains held while Lua retains a transaction-born handle.
pub(super) struct BirthFence {
    live: Arc<super::reactive::Fence>,
    run: u64,
    applied: std::sync::atomic::AtomicBool,
    _native: super::bridge::Reservation,
}
impl std::fmt::Debug for BirthFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BirthFence")
            .field("active", &self.active())
            .finish()
    }
}
impl BirthFence {
    pub(super) fn active(&self) -> bool {
        self.live.active()
    }
    pub(super) fn valid_in(&self, run: u64) -> bool {
        self.active()
            && (self.run == run || self.applied.load(std::sync::atomic::Ordering::Acquire))
    }
}
/// Cancellation, failed execution and refused/discarded Apply expire born handles.
struct BirthGuard {
    fence: Option<Arc<BirthFence>>,
    accepted: bool,
    run: u64,
}
impl BirthGuard {
    fn new(run: u64) -> Self {
        Self {
            fence: None,
            accepted: false,
            run,
        }
    }
    fn token(&mut self, lua: &Lua) -> mlua::Result<Arc<BirthFence>> {
        if let Some(fence) = &self.fence {
            return Ok(Arc::clone(fence));
        }
        let native = lua
            .app_data_ref::<super::bridge::NativeBudget>()
            .ok_or_else(|| runtime("Handle accounting unavailable."))?
            .reserve(128)
            .map_err(|e| e.lua())?;
        let fence = Arc::new(BirthFence {
            live: super::reactive::Fence::live(),
            run: self.run,
            applied: std::sync::atomic::AtomicBool::new(false),
            _native: native,
        });
        self.fence = Some(Arc::clone(&fence));
        Ok(fence)
    }
    fn accept(&mut self) {
        self.accepted = true;
        if let Some(f) = &self.fence {
            f.applied.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}
impl Drop for BirthGuard {
    fn drop(&mut self) {
        if !self.accepted
            && let Some(f) = &self.fence
        {
            f.live.invalidate();
        }
    }
}

/// One committed transaction: an `Operation::Group` and what it does.
pub struct CommitStep {
    birth: BirthGuard,
    birth_range: (u32, u32),
    pub label: &'static str,
    /// The transaction's edits (`None`: it changed only the selection).
    op: Option<Operation>,
    /// Note selections to set afterwards (no Undo step): the final selected
    /// ranks of every host the transaction selected in.
    selections: Vec<(NoteHost, RankSet)>,
    /// Writes in the transaction.
    pub operations: usize,
    /// Notes the transaction removes (deleted tracks' notes).
    pub removed_notes: u64,
    /// Notes the transaction creates (inserted, changed, duplicated).
    pub inserted_notes: u64,
    writer_notes: u64,
    native: Vec<super::bridge::Reservation>,
}

/// Why committed changes were not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitError {
    /// The project changed after the run read it; nothing was applied.
    ProjectChanged,
    /// A background job holds the project; nothing was applied.
    Busy,
    /// Step `step` (0-based; the earlier ones are applied) needs more undo
    /// memory than the history has. [`Commits::apply_without_undo`] applies
    /// the result without undo after the user agrees, as for any edit.
    TooLargeForUndo {
        step: usize,
        label: &'static str,
        message: String,
    },
    /// The step was refused (a sentence); the earlier ones are applied.
    Refused { step: usize, message: String },
}

impl CommitError {
    pub fn user_message(&self) -> String {
        match self {
            CommitError::ProjectChanged => {
                "The project changed while the script ran, so its changes were not applied.".into()
            }
            CommitError::Busy => "The project is busy (a background job is using it); the script's changes were not applied.".into(),
            CommitError::TooLargeForUndo { message, .. } | CommitError::Refused { message, .. } => {
                message.clone()
            }
        }
    }
}

/// What a run committed, in order, for the app to apply after the run.
pub struct Commits {
    /// The document the run read; the steps apply to exactly that state.
    base: Option<Arc<PlumDocument>>,
    /// The project after every step (the last transaction's working copy).
    result: Arc<PlumDocument>,
    pub steps: Vec<CommitStep>,
    _native: Vec<super::bridge::Reservation>,
    format_review: Option<Box<super::format_review::Report>>,
}

impl Commits {
    pub(crate) fn prepare_format_review(
        &mut self,
        budget: &super::bridge::NativeBudget,
        check: impl FnMut() -> mlua::Result<()>,
    ) -> mlua::Result<()> {
        let source = self
            .base
            .as_deref()
            .ok_or_else(|| runtime("The import review source expired."))?;
        self.format_review = Some(Box::new(super::format_review::Report::prepare(
            source,
            &self.result,
            budget,
            check,
        )?));
        Ok(())
    }
    pub(crate) fn format_review(&self) -> Option<&super::format_review::Report> {
        self.format_review.as_deref()
    }
    /// The project as it is after every step (what a preview draws).
    pub fn result(&self) -> &PlumDocument {
        &self.result
    }
    /// Native review checks freshness without consuming the staged transaction.
    pub(crate) fn source_matches(&self, doc: &Arc<PlumDocument>) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| Arc::ptr_eq(base, doc))
    }

    /// A step removes so many notes that the user must confirm first.
    pub fn needs_confirmation(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.removed_notes >= CONFIRM_REMOVED_NOTES)
    }

    /// Called only after the native response source guard validates the live
    /// document. Ordinary API01 pointer-equality semantics remain unchanged.
    pub(crate) fn bind_response_base(
        &mut self,
        doc: &Arc<PlumDocument>,
    ) -> Result<(), CommitError> {
        if self.steps.len() != 1
            || self.steps[0].op.is_none()
            || !self.steps[0].selections.is_empty()
            || self.steps[0].removed_notes > super::response::MAX_NOTES
            || self.steps[0].inserted_notes > super::response::MAX_NOTES
        {
            return Err(CommitError::Refused {step:0,message:"An automatic response needs exactly one bounded musical transaction and no explicit selection changes.".into()});
        }
        self.base = Some(Arc::clone(doc));
        Ok(())
    }

    /// Notes all steps remove.
    pub fn removed_notes(&self) -> u64 {
        self.steps.iter().map(|s| s.removed_notes).sum()
    }

    /// Applies the steps through `history` (each one Undo step) to `doc`,
    /// which must still be the document the run read. Returns how many
    /// steps were applied; a failing step changed nothing and later steps
    /// are not applied. After [`CommitError::TooLargeForUndo`] the commits
    /// can still be applied with [`Commits::apply_without_undo`].
    pub fn apply(
        &mut self,
        history: &mut History,
        doc: &mut Arc<PlumDocument>,
    ) -> Result<usize, CommitError> {
        match self.base.take() {
            Some(base) if Arc::ptr_eq(&base, doc) => {}
            Some(base) => {
                self.base = Some(base);
                return Err(CommitError::ProjectChanged);
            }
            None => return Err(CommitError::ProjectChanged),
        }
        let Some(d) = Arc::get_mut(doc) else {
            return Err(CommitError::Busy);
        };
        let steps = std::mem::take(&mut self.steps);
        let mut applied = 0;
        for mut s in steps {
            let label = s.label;
            if let Some(op) = s.op
                && let Err(e) = history.apply(d, op)
            {
                let message = format!("{label} was not applied: {}", e.user_message());
                return Err(match e {
                    plumspirit_engine::history::HistoryError::TooLarge { .. }
                    | plumspirit_engine::history::HistoryError::Edit(
                        plumspirit_engine::editor::EditError::UndoTooLarge { .. },
                    ) => CommitError::TooLargeForUndo {
                        step: applied,
                        label,
                        message,
                    },
                    _ => CommitError::Refused {
                        step: applied,
                        message,
                    },
                });
            }
            for (host, ranks) in &s.selections {
                let ok = host.target(d).ok().and_then(|t| {
                    plumspirit_engine::editor::notes_mut(d, t)
                        .ok()
                        .map(|n| n.select_ranks(ranks).is_ok())
                });
                if ok != Some(true) {
                    return Err(CommitError::Refused {
                        step: applied,
                        message: format!("{label}: the selection could not be set."),
                    });
                }
            }
            s.birth.accept();
            applied += 1;
        }
        Ok(applied)
    }

    /// Makes the project what the run committed, without undo (the history
    /// is cleared), after [`CommitError::TooLargeForUndo`] and the user's
    /// agreement — or directly. `doc` must be the document the run read, or
    /// that document with some of the steps applied.
    pub fn apply_without_undo(
        self,
        history: &mut History,
        doc: &mut Arc<PlumDocument>,
    ) -> Result<(), CommitError> {
        let Commits {
            base,
            result,
            mut steps,
            ..
        } = self;
        drop(base);
        let new = Arc::try_unwrap(result).unwrap_or_else(|shared| (*shared).clone());
        let Some(d) = Arc::get_mut(doc) else {
            return Err(CommitError::Busy);
        };
        history.replace_without_undo(d, new);
        for s in &mut steps {
            s.birth.accept();
        }
        Ok(())
    }
}

/// An open transaction.
struct OpenTx {
    birth: BirthGuard,
    track_origin_floor: u32,
    serial: u64,
    group_origin_floor: u32,
    label: &'static str,
    work: Arc<PlumDocument>,
    ids: Incarnations,
    history: History,
    ops: Vec<Operation>,
    removed_notes: u64,
    inserted_notes: u64,
    /// Hosts whose note selection the script set.
    selection_hosts: Vec<NoteHost>,
    writers: Vec<writer::Entry>,
    writer_notes: u64,
    native: Vec<super::bridge::Reservation>,
}

/// Transaction state of one Lua state (app data).
#[derive(Default)]
pub(super) struct TxSlot {
    open: Option<OpenTx>,
    /// The world the run started with (restored when it ends).
    run_world: Option<World>,
    commits: Vec<CommitStep>,
    next_serial: u64,
    writer_accepted: u64,
    group_edits: usize,
    metadata_edits: usize,
    run_serial: u64,
}

impl TxSlot {
    pub(super) fn track_birth(
        &mut self,
        lua: &Lua,
        id: TrackId,
    ) -> mlua::Result<Option<Arc<BirthFence>>> {
        if let Some(tx) = self.open.as_mut()
            && id.0 >= tx.track_origin_floor
            && id.0 < tx.work.next_track_id()
        {
            return tx.birth.token(lua).map(Some);
        }
        match self
            .commits
            .iter_mut()
            .rev()
            .find(|s| id.0 >= s.birth_range.0 && id.0 < s.birth_range.1)
        {
            Some(s) => s.birth.token(lua).map(Some),
            None => Ok(None),
        }
    }
    /// The world reads see while a transaction is open: its working copy.
    pub(super) fn world(&self, base: Option<&World>) -> Option<World> {
        let tx = self.open.as_ref()?;
        let info = base.map(|w| Arc::clone(&w.info)).unwrap_or_default();
        Some(World {
            doc: Arc::clone(&tx.work),
            ids: tx.ids.snapshot(),
            info,
            run: self.run_serial,
            group_scope: tx.serial,
            group_origin_floor: tx.group_origin_floor,
        })
    }
}

/// Called by [`Host::run`] before a run: forgets an earlier run's state.
pub(super) fn begin_run(lua: &Lua) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
    let serial = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let world = lua.app_data_ref::<WorldSlot>().and_then(|s| s.0.clone());
    if let Some(mut slot) = lua.app_data_mut::<TxSlot>() {
        slot.open = None;
        slot.commits.clear();
        slot.writer_accepted = 0;
        slot.group_edits = 0;
        slot.metadata_edits = 0;
        slot.run_world = world;
        slot.run_serial = serial;
    }
    if let Some(mut slot) = lua.app_data_mut::<WorldSlot>()
        && let Some(w) = slot.0.as_mut()
    {
        w.run = serial;
    }
}

/// Called by [`Host::run`] after a run: a failed run keeps nothing; an open
/// transaction is canceled; the world goes back to what the run read.
pub(super) fn end_run(lua: &Lua, ok: bool) -> Option<Commits> {
    let (world, mut commits) = {
        let mut slot = lua.app_data_mut::<TxSlot>()?;
        slot.open = None;
        let commits = std::mem::take(&mut slot.commits);
        (slot.run_world.take(), commits)
    };
    // The last commit's working copy is in the world slot: the result.
    let mut result = None;
    if let Some(mut w) = lua.app_data_mut::<WorldSlot>() {
        result = w.0.as_ref().map(|cur| Arc::clone(&cur.doc));
        w.0 = world.clone();
    }
    if !ok || commits.is_empty() {
        return None;
    }
    let result = result?;
    let native = commits
        .iter_mut()
        .flat_map(|c| std::mem::take(&mut c.native))
        .collect();
    world.map(|w| Commits {
        base: Some(w.doc),
        result,
        steps: commits,
        _native: native,
        format_review: None,
    })
}

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

fn require(lua: &Lua, cap: Capability) -> mlua::Result<()> {
    super::formats::check_write(lua)?;
    super::reactive::check_transaction_write(lua)?;
    if matches!(cap, Capability::EditTracks | Capability::EditConductor) {
        super::response::explicit_only(lua, "Structural and Conductor edits")?;
    }
    if super::reactive::responding(lua)
        && cap != Capability::ReadProject
        && lua
            .app_data_ref::<TxSlot>()
            .and_then(|s| s.open.as_ref().map(|t| t.ops.len()))
            .is_some_and(|n| n >= super::response::MAX_OPERATIONS)
    {
        return Err(stop(
            Category::Budget,
            "An automatic response supports at most eight batched native editing operations.",
        ));
    }

    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, cap)
}

/// An exact BPM from a decimal string or a number (a float is read as its
/// shortest decimal form: `143.5` is 143.5).
fn bpm_arg(v: &Value) -> mlua::Result<Bpm> {
    let text = match v {
        Value::String(s) => s.to_string_lossy(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) if n.is_finite() => format!("{n}"),
        other => {
            return Err(runtime(format!(
                "The tempo must be a BPM (a number or a decimal string such as \"143.5\"), not {}.",
                other.type_name()
            )));
        }
    };
    Bpm::parse(&text).map_err(|e| runtime(format!("The tempo {text} is not usable: {e}")))
}

fn tick_arg(v: &Value, what: &str) -> mlua::Result<u64> {
    super::ticks::from_lua(v, what)
}

/// A subchannel handle's track id and subchannel.
fn subchannel_arg(lua: &Lua, v: &Value) -> mlua::Result<(TrackId, Subchannel)> {
    let h = handle_arg(v, "The subchannel")?;
    with_world(lua, &h, |_, r| match r {
        Resolved::Subchannel { track, sub, .. } => Ok((track.id(), sub.clone())),
        _ => Err(runtime(format!(
            "A subchannel handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

fn channel_arg(v: Option<i64>) -> mlua::Result<Option<u8>> {
    match v {
        None => Ok(None),
        Some(c) => u8::try_from(c)
            .ok()
            .filter(|&c| c <= 15)
            .map(Some)
            .ok_or_else(|| {
                runtime(format!(
                    "A MIDI channel is 0 - 15 (shown as 1 - 16); {c} is not."
                ))
            }),
    }
}

pub(crate) fn has_pending_work(lua: &Lua) -> bool {
    lua.app_data_ref::<TxSlot>()
        .is_some_and(|s| s.open.is_some() || !s.commits.is_empty())
}

/// Installs `plum.transaction`.
pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    writer::install_limits(lua, plum)?;
    metadata::install(lua, plum)?;
    let open = lua.create_function(|lua, opts: Option<Table>| {
        super::formats::check_open(lua)?;
        super::reactive::check_transaction_write(lua)?;
        super::playback::loop_intent::check_transaction(lua)?;
        super::playback::midi::check_transaction(lua)?;
        let label = match &opts {
            Some(t) => t.get::<Option<String>>("label")?,
            None => None,
        };
        let base = super::handles::current_world(lua)
            .ok_or_else(|| runtime("No project is open, so there is nothing to change."))?;
        let metadata_native = if !base.doc.project().metadata.is_empty() {
            Some(
                lua.app_data_ref::<super::bridge::NativeBudget>()
                    .ok_or_else(|| runtime("Metadata accounting unavailable."))?
                    .reserve(
                        base.doc
                            .project()
                            .metadata
                            .heap_bytes()
                            .saturating_mul(2)
                            .saturating_add(4096),
                    )
                    .map_err(|e| e.lua())?,
            )
        } else {
            None
        };
        let mut slot = lua
            .app_data_mut::<TxSlot>()
            .ok_or_else(|| runtime("Transactions are not available here."))?;
        if super::reactive::responding(lua) && !slot.commits.is_empty() {
            return Err(stop(
                Category::Budget,
                "An automatic response may commit only one transaction.",
            ));
        }
        if slot.open.is_some() {
            return Err(runtime(
                "A transaction is already open; commit or cancel it first.",
            ));
        }
        slot.next_serial += 1;
        let serial = slot.next_serial;
        let work = Arc::new((*base.doc).clone());
        let ids = Incarnations::resume(&base.ids, &work);
        slot.open = Some(OpenTx {
            birth: BirthGuard::new(slot.run_serial),
            track_origin_floor: base.doc.next_track_id(),
            serial,
            group_origin_floor: if base.group_scope != 0 {
                base.group_origin_floor
            } else {
                base.doc.project().groups.next_id()
            },
            label: intern(label.as_deref().unwrap_or("")),
            work,
            ids,
            // The working copy keeps no undo (cleared after every write),
            // so no edit is refused for the undo memory here; the
            // project's own history decides at the commit.
            history: History::new(HistoryLimits {
                max_bytes: usize::MAX / 4,
                max_entries: 2_000,
            }),
            ops: Vec::new(),
            removed_notes: 0,
            inserted_notes: 0,
            selection_hosts: Vec::new(),
            writers: Vec::new(),
            writer_notes: 0,
            native: metadata_native.into_iter().collect(),
        });
        Ok(LuaTx(serial))
    })?;
    lua.set_named_registry_value("plumspirit.native.transaction", open.clone())?;
    plum.set("transaction", open)
}

pub(super) fn open_owned(lua: &Lua, label: &str) -> mlua::Result<AnyUserData> {
    let open: mlua::Function = lua.named_registry_value("plumspirit.native.transaction")?;
    let opts = lua.create_table()?;
    opts.raw_set("label", label)?;
    open.call(opts)
}

/// What a write asks for, with the objects already resolved to ids.
pub(super) enum Step {
    SetMetadata {
        table: Box<plumspirit_engine::model::metadata::Metadata>,
    },
    SetGroups {
        table: Box<plumspirit_engine::model::GroupTable>,
    },
    AddTrack {
        after: Option<TrackId>,
    },
    AddTrackToFolder {
        folder: u32,
    },
    AddSubtracks {
        parent: TrackId,
        count: usize,
    },
    DeleteTrack {
        id: TrackId,
    },
    Move {
        roots: Vec<TrackId>,
        target: TrackId,
        place: TrackPlace,
    },
    RenameTrack {
        id: TrackId,
        name: String,
    },
    CreateFolder {
        name: String,
        tracks: Vec<TrackId>,
    },
    RenameFolder {
        id: u32,
        name: String,
    },
    DeleteFolder {
        id: u32,
    },
    IntoFolder {
        tracks: Vec<TrackId>,
        id: u32,
    },
    OutOfFolders {
        tracks: Vec<TrackId>,
    },
    AddSubchannel {
        track: TrackId,
        sub: Subchannel,
    },
    EditSubchannel {
        track: TrackId,
        sub: Subchannel,
        move_notes: bool,
    },
    DeleteSubchannel {
        track: TrackId,
        id: SubchannelId,
    },
    SetTempo {
        tick: u64,
        bpm: Bpm,
        interp: Interp,
    },
    RemoveTempo {
        tick: u64,
    },
    SetSignature {
        tick: u64,
        numerator: u8,
        denom_pow: u8,
    },
    RemoveSignature {
        tick: u64,
    },
    AddMarker {
        start: u64,
        end: Option<u64>,
        title: String,
    },
    RemoveMarkers {
        tick: u64,
    },
    EraseNotes {
        host: NoteHost,
        ranks: RankSet,
    },
    InsertNotes {
        host: NoteHost,
        records: Vec<NoteRecord>,
    },
    /// A Track List clip edit (B1-0028, `lua/clips.rs`).
    Clips(super::clips::ClipStep),
    ReplaceEvents {
        host: NoteHost,
        remove: Vec<usize>,
        add: Vec<RawEvent>,
    },
}

use super::notes::{Host as NoteHost, LuaNoteQuery, check_budget, evaluate};

impl Step {
    /// The capability the step needs.
    fn capability(&self) -> Capability {
        match self {
            Step::SetMetadata { .. } => Capability::EditMetadata,
            Step::SetTempo { .. }
            | Step::RemoveTempo { .. }
            | Step::SetSignature { .. }
            | Step::RemoveSignature { .. }
            | Step::AddMarker { .. }
            | Step::RemoveMarkers { .. } => Capability::EditConductor,
            Step::SetGroups { .. }
            | Step::EraseNotes { .. }
            | Step::InsertNotes { .. }
            | Step::ReplaceEvents { .. } => Capability::EditNotes,
            _ => Capability::EditTracks,
        }
    }
}

fn index(doc: &PlumDocument, id: TrackId) -> Result<usize, String> {
    doc.index_of(id)
        .ok_or_else(|| "That track no longer exists.".to_owned())
}

fn indices(doc: &PlumDocument, ids: &[TrackId]) -> Result<Vec<usize>, String> {
    ids.iter().map(|&id| index(doc, id)).collect()
}

/// The engine operation for `step` on `doc` (deterministic: the same
/// document gives the same operation).
fn build(step: &Step, doc: &PlumDocument) -> Result<Operation, String> {
    Ok(match step {
        Step::SetMetadata { table } => Operation::SetMetadata {
            table: table.clone(),
            label: "Edit Plugin Metadata",
        },
        Step::SetGroups { table } => Operation::SetGroups {
            table: table.clone(),
            label: "Edit Note Groups",
        },
        Step::Clips(c) => super::clips::build(c, doc)?,
        Step::AddTrack { after: None } => Operation::AddTrack,
        Step::AddTrack { after: Some(t) } => folders::plan_add_track_below(doc, index(doc, *t)?)?,
        Step::AddTrackToFolder { folder } => folders::plan_add_track_to_folder(doc, *folder)?,
        Step::AddSubtracks { parent, count } => Operation::AddSubtracks {
            parent: index(doc, *parent)?,
            count: *count,
        },
        Step::DeleteTrack { id } => Operation::DeleteTrack {
            index: index(doc, *id)?,
        },
        Step::Move {
            roots,
            target,
            place,
        } => plan_track_moves(doc, &indices(doc, roots)?, index(doc, *target)?, *place)?,
        Step::RenameTrack { id, name } => {
            let t = &doc.tracks()[index(doc, *id)?];
            Operation::Events {
                edit: prepare_rename(t, name).map_err(|e| e.user_message())?,
                label: "Rename Track",
            }
        }
        Step::CreateFolder { name, tracks } => {
            folders::plan_create(doc, &indices(doc, tracks)?, name, None)?.0
        }
        Step::RenameFolder { id, name } => folders::plan_rename(doc, *id, name)?,
        Step::DeleteFolder { id } => folders::plan_delete(doc, *id)?,
        Step::IntoFolder { tracks, id } => folders::plan_into(doc, &indices(doc, tracks)?, *id)?,
        Step::OutOfFolders { tracks } => folders::plan_out(doc, &indices(doc, tracks)?)?,
        Step::AddSubchannel { track, sub } => Operation::AddSubchannels {
            track: index(doc, *track)?,
            subs: vec![sub.clone()],
        },
        Step::EditSubchannel {
            track,
            sub,
            move_notes,
        } => Operation::EditSubchannel {
            track: index(doc, *track)?,
            sub: sub.clone(),
            move_notes: *move_notes,
        },
        Step::DeleteSubchannel { track, id } => Operation::DeleteSubchannel {
            track: index(doc, *track)?,
            id: *id,
        },
        Step::SetTempo { tick, bpm, interp } => Operation::Conductor {
            edit: doc
                .conductor()
                .prepare_set_tempo(None, *tick, TempoValue::Bpm(*bpm), *interp)
                .map_err(|e| e.to_string())?,
            label: "Set Tempo",
        },
        Step::RemoveTempo { tick } => {
            let c = doc.conductor();
            let ids: Vec<u32> = c
                .tempo_points()
                .iter()
                .filter(|p| p.tick == *tick)
                .map(|p| p.id)
                .collect();
            if ids.is_empty() {
                return Err(format!("There is no tempo change at tick {tick}."));
            }
            Operation::Conductor {
                edit: c.prepare_remove_tempo(&ids).map_err(|e| e.to_string())?,
                label: "Remove Tempo",
            }
        }
        Step::SetSignature {
            tick,
            numerator,
            denom_pow,
        } => Operation::Conductor {
            edit: doc
                .conductor()
                .prepare_set_signature(None, *tick, *numerator, *denom_pow)
                .map_err(|e| e.to_string())?,
            label: "Set Time Signature",
        },
        Step::RemoveSignature { tick } => {
            let c = doc.conductor();
            let ids: Vec<u32> = c
                .time_signatures()
                .iter()
                .filter(|s| s.tick == *tick)
                .map(|s| s.id)
                .collect();
            if ids.is_empty() {
                return Err(format!("There is no time signature at tick {tick}."));
            }
            Operation::Conductor {
                edit: c
                    .prepare_remove_signatures(&ids)
                    .map_err(|e| e.to_string())?,
                label: "Remove Time Signature",
            }
        }
        Step::AddMarker { start, end, title } => Operation::Conductor {
            edit: doc
                .conductor()
                .prepare_set_marker(None, *start, *end, title)
                .map_err(|e| e.to_string())?,
            label: "Add Marker",
        },
        Step::RemoveMarkers { tick } => {
            let c = doc.conductor();
            let ids: Vec<u32> = c
                .markers()
                .iter()
                .filter(|m| m.start == *tick)
                .map(|m| m.id)
                .collect();
            if ids.is_empty() {
                return Err(format!("There is no marker at tick {tick}."));
            }
            Operation::Conductor {
                edit: c.prepare_remove_markers(&ids).map_err(|e| e.to_string())?,
                label: "Remove Markers",
            }
        }
        Step::EraseNotes { host, ranks } => Operation::EraseNotes {
            track: host.target(doc)?,
            ranks: ranks.clone(),
        },
        Step::InsertNotes { host, records } => {
            host.target(doc)?;
            let notes = host
                .notes(doc)
                .ok_or_else(|| "Those notes no longer exist.".to_owned())?;
            Operation::Prepared {
                edit: PreparedEdit::InsertSelected {
                    host: host.content_ref(),
                    base_revision: notes.revision(),
                    records: records.clone(),
                    subs_track: host.track,
                    new_subs: Vec::new(),
                },
                label: "Insert Notes",
            }
        }
        Step::ReplaceEvents { host, remove, add } => {
            let view = plumspirit_engine::editor::note_view(doc, host.target(doc)?)
                .map_err(|e| e.user_message())?;
            Operation::Events {
                edit: plumspirit_engine::editor::controllers::prepare_replace(view, remove, add)
                    .map_err(|e| e.user_message())?,
                label: "Edit Events",
            }
        }
    })
}

/// Applies `step` to transaction `serial`'s working copy and records it.
pub(super) fn apply(lua: &Lua, serial: u64, step: Step, removed_notes: u64) -> mlua::Result<()> {
    require(lua, step.capability())?;
    if matches!(&step, Step::ReplaceEvents { .. }) {
        super::response::explicit_only(lua, "Controller event edits")?;
    }
    let inserted = match &step {
        Step::InsertNotes { records, .. } => records.len() as u64,
        _ => 0,
    };
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions are not available here."))?;
    let tx = match slot.open.as_mut() {
        Some(tx) if tx.serial == serial => tx,
        _ => {
            return Err(runtime(
                "This transaction is closed (committed or canceled); open a new one.",
            ));
        }
    };
    super::response::check_removed(lua, tx.removed_notes, removed_notes)?;
    groups::check_step(lua, &tx.work, &step)?;
    let _metadata_temporary = if matches!(&step, Step::DeleteTrack { .. } | Step::SetGroups { .. })
        && !tx.work.project().metadata.is_empty()
    {
        Some(
            lua.app_data_ref::<super::bridge::NativeBudget>()
                .ok_or_else(|| runtime("Metadata accounting unavailable."))?
                .reserve(
                    tx.work
                        .project()
                        .metadata
                        .heap_bytes()
                        .saturating_mul(4)
                        .saturating_add(65536),
                )
                .map_err(|e| e.lua())?,
        )
    } else {
        None
    };
    let (for_work, for_commit) = {
        let doc = &*tx.work;
        (build(&step, doc), build(&step, doc))
    };
    let (op, recorded) = match (for_work, for_commit) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Err(runtime(e)),
    };
    let work = Arc::get_mut(&mut tx.work)
        .ok_or_else(|| stop(Category::Internal, "the working copy is shared"))?;
    tx.history
        .apply(work, op)
        .map_err(|e| runtime(format!("That change was refused: {}", e.user_message())))?;
    // The working copy needs no undo: free what the step recorded.
    tx.history.clear();
    tx.ids.observe(work);
    tx.ops.push(recorded);
    tx.removed_notes = tx.removed_notes.saturating_add(removed_notes);
    tx.inserted_notes = tx.inserted_notes.saturating_add(inserted);
    Ok(())
}

/// Reads the working copy (the world reads see) with `f`.
pub(super) fn read<R>(lua: &Lua, f: impl FnOnce(&World) -> mlua::Result<R>) -> mlua::Result<R> {
    let w = super::handles::current_world(lua)
        .ok_or_else(|| runtime("No project is open, so there is nothing to change."))?;
    f(&w)
}

/// The track id behind a track handle (expired / wrong family: an error).
pub(super) fn track_id(lua: &Lua, h: &Handle) -> mlua::Result<TrackId> {
    with_world(lua, h, |_, r| match r {
        Resolved::Track { track, .. } => Ok(track.id()),
        _ => Err(runtime(format!(
            "A track handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

fn folder_id(lua: &Lua, h: &Handle) -> mlua::Result<u32> {
    with_world(lua, h, |_, r| match r {
        Resolved::Folder(f) => Ok(f.id),
        _ => Err(runtime(format!(
            "A folder handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

/// A handle argument.
pub(super) fn handle_arg(v: &Value, what: &str) -> mlua::Result<Handle> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<LuaHandle>()
            .map(|h| h.0.clone())
            .map_err(|_| runtime(format!("{what} must be a handle."))),
        other => Err(runtime(format!(
            "{what} must be a handle, not {}.",
            other.type_name()
        ))),
    }
}

/// A handle or a list of handles, as track ids.
fn track_ids(lua: &Lua, v: &Value, what: &str) -> mlua::Result<Vec<TrackId>> {
    match v {
        Value::Table(t) => {
            let mut out = Vec::new();
            for item in t.clone().sequence_values::<Value>() {
                out.push(track_id(lua, &handle_arg(&item?, what)?)?);
            }
            Ok(out)
        }
        other => Ok(vec![track_id(lua, &handle_arg(other, what)?)?]),
    }
}

/// What `modify_notes` / `duplicate_notes` change (native, one Rust loop).
#[derive(Clone, Debug, Default)]
struct Changes {
    shift: i64,
    transpose: i32,
    velocity: Option<u8>,
    velocity_add: i32,
    velocity_scale: Option<f64>,
    gate: Option<u64>,
    gate_add: i64,
    gate_scale: Option<f64>,
    channel: Option<u8>,
    subchannel: Option<SubchannelId>,
    /// What happens to a note pushed outside the keys or before tick 0.
    outside: Outside,
    metadata: note_input::Metadata,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Outside {
    #[default]
    Error,
    Clamp,
    Drop,
}

impl Changes {
    fn from_lua(t: &Table) -> mlua::Result<Changes> {
        let int = |k: &str| t.get::<Option<i64>>(k);
        let num = |k: &str| -> mlua::Result<Option<f64>> {
            match t.get::<Value>(k)? {
                Value::Nil => Ok(None),
                Value::Integer(i) => Ok(Some(i as f64)),
                Value::Number(n) if n.is_finite() && n >= 0.0 => Ok(Some(n)),
                other => Err(runtime(format!(
                    "{k} must be a number of at least 0, not {}.",
                    other
                        .to_string()
                        .unwrap_or_else(|_| other.type_name().into())
                ))),
            }
        };
        let velocity = match int("velocity")? {
            None => None,
            Some(v) => Some(
                u8::try_from(v)
                    .ok()
                    .filter(|v| (1..=127).contains(v))
                    .ok_or_else(|| runtime(format!("A velocity is 1 - 127; {v} is not.")))?,
            ),
        };
        let gate = match t.get::<Value>("gate")? {
            Value::Nil => None,
            v => Some(super::ticks::from_lua(&v, "gate")?),
        };
        let subchannel = match int("subchannel")? {
            None => None,
            Some(v) => Some(super::notes::subchannel_id(v)?),
        };
        let outside = match t.get::<Option<String>>("outside")?.as_deref() {
            None | Some("error") => Outside::Error,
            Some("clamp") => Outside::Clamp,
            Some("drop") => Outside::Drop,
            Some(other) => {
                return Err(runtime(format!(
                    "outside is \"error\", \"clamp\" or \"drop\", not \"{other}\"."
                )));
            }
        };
        Ok(Changes {
            shift: int("shift")?.unwrap_or(0),
            transpose: i32::try_from(int("transpose")?.unwrap_or(0))
                .map_err(|_| runtime("transpose is too large."))?,
            velocity,
            velocity_add: i32::try_from(int("velocity_add")?.unwrap_or(0))
                .map_err(|_| runtime("velocity_add is too large."))?,
            velocity_scale: num("velocity_scale")?,
            gate,
            gate_add: int("gate_add")?.unwrap_or(0),
            gate_scale: num("gate_scale")?,
            channel: channel_arg(int("channel")?)?,
            subchannel,
            outside,
            metadata: note_input::Metadata::parse(&note_input::Input::Row(t))?,
        })
    }

    /// `r` changed, or `None` when it is dropped (outside = "drop").
    fn apply(&self, r: &NoteRecord, max_key: u8) -> Result<Option<NoteRecord>, String> {
        let mut out = *r;
        // Time.
        let start = i128::from(r.start_tick) + i128::from(self.shift);
        out.start_tick = if start < 0 {
            match self.outside {
                Outside::Error => {
                    let at = r.start_tick;
                    return Err(format!("a note at tick {at} would move before tick 0"));
                }
                Outside::Clamp => 0,
                Outside::Drop => return Ok(None),
            }
        } else {
            u64::try_from(start).map_err(|_| "a note would move past the last tick".to_owned())?
        };
        // Gate: set, scale, add (in that order), never negative.
        let exact_gate = self.gate.unwrap_or(r.duration_ticks);
        let gate = match self.gate_scale {
            Some(f) => (exact_gate as f64 * f).round() as i128,
            None => i128::from(exact_gate),
        };
        let gate = gate
            .checked_add(i128::from(self.gate_add))
            .ok_or_else(|| "a gate is too long".to_owned())?
            .max(0);
        out.duration_ticks = u64::try_from(gate).map_err(|_| "a gate is too long".to_owned())?;
        if out.start_tick.checked_add(out.duration_ticks).is_none() {
            return Err("a note would end past the last tick".into());
        }
        // Key.
        let key = i64::from(r.attrs.pitch()) + i64::from(self.transpose);
        let key = if key < 0 || key > i64::from(max_key) {
            match self.outside {
                Outside::Error => {
                    return Err(format!(
                        "key {} transposed by {} is outside the project's keys (0 - {max_key})",
                        r.attrs.pitch(),
                        self.transpose
                    ));
                }
                Outside::Clamp => key.clamp(0, i64::from(max_key)),
                Outside::Drop => return Ok(None),
            }
        } else {
            key
        };
        let mut attrs = r
            .attrs
            .with_pitch_wide(key as u8)
            .map_err(|e| e.to_string())?;
        // Velocity: set, scale, add; always 1 - 127.
        let mut v = f64::from(self.velocity.unwrap_or(r.attrs.velocity()));
        if let Some(f) = self.velocity_scale {
            v = (v * f).round();
        }
        let v = (i128::from(v as i64) + i128::from(self.velocity_add)).clamp(1, 127) as u8;
        attrs = attrs.with_velocity(v).map_err(|e| e.to_string())?;
        if let Some(c) = self.channel {
            attrs = attrs.with_channel(c).map_err(|e| e.to_string())?;
        }
        out.attrs = attrs;
        if let Some(s) = self.subchannel {
            out.sub = s;
        }
        self.metadata
            .apply(out)
            .map(Some)
            .map_err(|e| super::error_text(&e))
    }
}

/// A note from a script table (absolute fields; missing ones from `base`).
fn record_from_table(
    t: &Table,
    base: Option<&NoteRecord>,
    default_channel: u8,
) -> mlua::Result<NoteRecord> {
    record_from_input(note_input::Input::Row(t), base, default_channel)
}

fn record_from_input(
    input: note_input::Input<'_>,
    base: Option<&NoteRecord>,
    default_channel: u8,
) -> mlua::Result<NoteRecord> {
    let tick = |k: &str| input.tick(k);
    let int = |k: &str| input.integer(k);
    let start = tick("start")?
        .or(base.map(|b| b.start_tick))
        .ok_or_else(|| runtime("A note needs a start tick."))?;
    let gate = tick("gate")?
        .or(base.map(|b| b.duration_ticks))
        .ok_or_else(|| runtime("A note needs a gate (length in ticks)."))?;
    let key = int("key")?
        .or(base.map(|b| i64::from(b.attrs.pitch())))
        .ok_or_else(|| runtime("A note needs a key."))?;
    let key = u8::try_from(key).map_err(|_| runtime(format!("A key is 0 - 255; {key} is not.")))?;
    let velocity = int("velocity")?
        .or(base.map(|b| i64::from(b.attrs.velocity())))
        .unwrap_or(100);
    let velocity = u8::try_from(velocity)
        .ok()
        .filter(|v| (1..=127).contains(v))
        .ok_or_else(|| runtime(format!("A velocity is 1 - 127; {velocity} is not.")))?;
    let channel = match int("channel")? {
        Some(c) => channel_arg(Some(c))?.unwrap_or(0),
        None => base.map(|b| b.attrs.channel()).unwrap_or(default_channel),
    };
    let sub = match int("subchannel")? {
        Some(s) => super::notes::subchannel_id(s)?,
        None => base.map(|b| b.sub).unwrap_or(SubchannelId::MAIN),
    };
    let attrs = match base {
        Some(b) => b
            .attrs
            .with_pitch_wide(key)
            .and_then(|a| a.with_velocity(velocity))
            .and_then(|a| a.with_channel(channel)),
        None => NoteAttrs::new_wide(key, velocity, channel, 0, NoteOffKind::NoteOff),
    }
    .map_err(|e| runtime(format!("That note is not valid: {e}.")))?;
    if start.checked_add(gate).is_none() {
        return Err(runtime("That note would end past the last tick."));
    }
    note_input::Metadata::parse(&input)?.apply(NoteRecord {
        start_tick: start,
        duration_ticks: gate,
        attrs,
        sub,
        group: base.map(|b| b.group).unwrap_or(0),
    })
}

fn validate_group(doc: &PlumDocument, id: u32) -> mlua::Result<()> {
    let groups = &doc.project().groups;
    if id != 0 && groups.get(id).is_none() {
        return Err(runtime("The note group does not exist."));
    }
    if groups.locked(id) {
        return Err(runtime(
            "The note group or an ancestor is locked; unlock it explicitly first.",
        ));
    }
    Ok(())
}

/// Checks keys and subchannels of new notes against the project and the
/// host track, then sorts them by start (stable).
fn finish_records(
    lua: &Lua,
    host: NoteHost,
    mut records: Vec<NoteRecord>,
) -> mlua::Result<Vec<NoteRecord>> {
    read(lua, |w| {
        let max_key = w.doc.project().keys.max_pitch();
        let track = w
            .doc
            .track_by_id(host.track)
            .ok_or_else(|| runtime("That track no longer exists."))?;
        for (i, r) in records.iter().enumerate() {
            if i.is_multiple_of(4096) {
                check_budget(lua)?;
            }
            validate_group(&w.doc, r.group)?;
            if r.attrs.pitch() > max_key {
                return Err(runtime(format!(
                    "Note {}: key {} is above this project's highest key ({max_key}); turn on 256-Key Mode first.",
                    i + 1,
                    r.attrs.pitch()
                )));
            }
            if !track.subchannels().contains(r.sub) {
                return Err(runtime(format!(
                    "Note {}: the track has no subchannel {}; add it first with tx:add_subchannel.",
                    i + 1,
                    r.sub.number()
                )));
            }
        }
        Ok(())
    })?;
    records.sort_by_key(|r| r.start_tick);
    Ok(records)
}

/// Notes the run's transactions created so far (open + committed).
fn output_so_far(lua: &Lua) -> u64 {
    lua.app_data_ref::<TxSlot>().map_or(0, |s| {
        s.open
            .as_ref()
            .map_or(0, |t| t.inserted_notes.saturating_sub(t.writer_notes))
            .saturating_add(
                s.commits
                    .iter()
                    .map(|c| c.inserted_notes.saturating_sub(c.writer_notes))
                    .sum::<u64>(),
            )
            .saturating_add(s.writer_accepted)
    })
}

/// The query argument of a note write.
fn query_arg(v: &Value) -> mlua::Result<mlua::UserDataRef<LuaNoteQuery>> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<LuaNoteQuery>()
            .map_err(|_| runtime("A note query is needed here (track:notes{...}).")),
        other => Err(runtime(format!(
            "A note query is needed here (track:notes{{...}}), not {}.",
            other.type_name()
        ))),
    }
}

/// Erases `hits` (ranks) of `host`.
fn erase(
    lua: &Lua,
    serial: u64,
    host: NoteHost,
    hits: &[super::notes::Hit],
    counted: bool,
) -> mlua::Result<()> {
    if hits.is_empty() {
        return Ok(());
    }
    read(lua, |w| {
        let store = host
            .notes(&w.doc)
            .ok_or_else(|| runtime("The note target no longer exists."))?;
        let mut expected = hits.iter();
        store.try_visit_ranks(
            hits.iter().map(|h| h.rank),
            || check_budget(lua),
            |_, note| {
                let hit = expected.next().expect("one visit per requested rank");
                let note = note.ok_or_else(|| runtime("The notes changed during the callback."))?;
                if note.record() != hit.rec {
                    return Err(runtime(
                        "The notes changed during the callback; read the query again.",
                    ));
                }
                validate_group(&w.doc, note.group)
            },
        )?;
        Ok(())
    })?;
    let mut b = RankSetBuilder::default();
    for h in hits {
        b.push(h.rank);
    }
    let removed = if counted { hits.len() as u64 } else { 0 };
    apply(
        lua,
        serial,
        Step::EraseNotes {
            host,
            ranks: b.finish(),
        },
        removed,
    )
}

/// The query argument of an event write.
fn event_query_arg(v: &Value) -> mlua::Result<mlua::UserDataRef<super::events::LuaEventQuery>> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<super::events::LuaEventQuery>()
            .map_err(|_| runtime("An event query is needed here (track:events{...}).")),
        other => Err(runtime(format!(
            "An event query is needed here (track:events{{...}}), not {}.",
            other.type_name()
        ))),
    }
}

/// Refuses a write that would touch meta, SysEx or system events.
fn channel_only(lua: &Lua, host: NoteHost, idx: &[usize]) -> mlua::Result<()> {
    read(lua, |w| {
        let store = w
            .doc
            .events_of(host.content_ref())
            .ok_or_else(|| runtime("Those events no longer exist."))?;
        for &i in idx {
            let e = store.events()[i];
            if !super::events::Kind::of(&e).writable() {
                return Err(runtime(format!(
                    "Event {} is a {} event; scripts change only cc, pitch_bend, program, channel_pressure and poly_pressure events (add a kind filter).",
                    i + 1,
                    super::events::Kind::of(&e).name()
                )));
            }
        }
        Ok(())
    })
}

/// Makes exactly `ranks` the selected notes of `host` in transaction
/// `serial`'s working copy (no Undo step; the commit sets it on the project).
fn set_selection(lua: &Lua, serial: u64, host: NoteHost, ranks: &[usize]) -> mlua::Result<()> {
    super::handles::require_read(lua)?;
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions are not available here."))?;
    let tx = match slot.open.as_mut() {
        Some(tx) if tx.serial == serial => tx,
        _ => {
            return Err(runtime(
                "This transaction is closed (committed or canceled); open a new one.",
            ));
        }
    };
    let mut b = RankSetBuilder::default();
    for &r in ranks {
        b.push(r);
    }
    let set = b.finish();
    let work = Arc::get_mut(&mut tx.work)
        .ok_or_else(|| stop(Category::Internal, "the working copy is shared"))?;
    let target = host.target(work).map_err(runtime)?;
    plumspirit_engine::editor::notes_mut(work, target)
        .map_err(|e| runtime(e.user_message()))?
        .select_ranks(&set)
        .map_err(|_| runtime("Those notes changed; read them again."))?;
    if !tx.selection_hosts.contains(&host) {
        tx.selection_hosts.push(host);
    }
    Ok(())
}

/// A transaction as Lua userdata.
pub struct LuaTx(pub(super) u64);

impl UserData for LuaTx {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        writer::add_methods(m);
        image_mask::add_methods(m);
        groups::add_tx_methods(m);
        metadata::add_tx_methods(m);
        super::clips::add_tx_methods(m);
        m.add_method("add_track", |lua, this, opts: Option<Table>| {
            let (after, folder, name) = match &opts {
                Some(t) => (
                    t.get::<Value>("after")?,
                    t.get::<Value>("folder")?,
                    t.get::<Option<String>>("name")?,
                ),
                None => (Value::Nil, Value::Nil, None),
            };
            let step = match (&after, &folder) {
                (Value::Nil, Value::Nil) => Step::AddTrack { after: None },
                (a, Value::Nil) => Step::AddTrack {
                    after: Some(track_id(lua, &handle_arg(a, "after")?)?),
                },
                (Value::Nil, f) => Step::AddTrackToFolder {
                    folder: folder_id(lua, &handle_arg(f, "folder")?)?,
                },
                _ => return Err(runtime("Give add_track either after or folder, not both.")),
            };
            let id = read(lua, |w| Ok(TrackId(w.doc.next_track_id())))?;
            apply(lua, this.0, step, 0)?;
            if let Some(name) = name {
                apply(lua, this.0, Step::RenameTrack { id, name }, 0)?;
            }
            read(lua, |w| handle_or_nil(lua, w.track(id)))
        });
        m.add_method(
            "add_subtracks",
            |lua, this, (parent, count): (Value, Option<i64>)| {
                let parent = track_id(lua, &handle_arg(&parent, "The parent")?)?;
                let count = count.unwrap_or(1);
                let count = usize::try_from(count)
                    .ok()
                    .filter(|&c| c >= 1)
                    .ok_or_else(|| {
                        runtime(format!("The count must be at least 1 (it is {count})."))
                    })?;
                let first = read(lua, |w| Ok(w.doc.next_track_id()))?;
                apply(lua, this.0, Step::AddSubtracks { parent, count }, 0)?;
                read(lua, |w| {
                    handle_list(
                        lua,
                        (0..count as u32).filter_map(|k| w.track(TrackId(first.wrapping_add(k)))),
                    )
                })
            },
        );
        m.add_method("delete_tracks", |lua, this, tracks: Value| {
            let ids = track_ids(lua, &tracks, "Each track")?;
            // A mixed request must not delete an earlier unlocked subtree before
            // discovering a locked later one that the author catches in pcall.
            read(lua, |w| {
                for &id in &ids {
                    groups::check_step(lua, &w.doc, &Step::DeleteTrack { id })?;
                }
                Ok(())
            })?;
            for id in ids {
                // A track inside an already deleted subtree is gone: skip it.
                let notes = read(lua, |w| {
                    let d = &w.doc;
                    Ok(d.index_of(id).map(|i| {
                        (i..d.subtree_end(i))
                            .map(|k| d.tracks()[k].notes.len() as u64)
                            .sum::<u64>()
                    }))
                })?;
                if let Some(n) = notes {
                    apply(lua, this.0, Step::DeleteTrack { id }, n)?;
                }
            }
            Ok(())
        });
        m.add_method(
            "move_tracks",
            |lua, this, (tracks, target, place): (Value, Value, Option<String>)| {
                let roots = track_ids(lua, &tracks, "Each track")?;
                let target = track_id(lua, &handle_arg(&target, "The target")?)?;
                let place = match place.as_deref().unwrap_or("after") {
                    "before" => TrackPlace::Before,
                    "after" => TrackPlace::After,
                    "into" => TrackPlace::Into,
                    other => {
                        return Err(runtime(format!(
                            "The place \u{201C}{other}\u{201D} is not \"before\", \"after\" or \"into\"."
                        )));
                    }
                };
                apply(
                    lua,
                    this.0,
                    Step::Move {
                        roots,
                        target,
                        place,
                    },
                    0,
                )
            },
        );
        m.add_method("rename", |lua, this, (obj, name): (Value, String)| {
            let h = handle_arg(&obj, "The object")?;
            let step = with_world(lua, &h, |_, r| match r {
                Resolved::Track { track, .. } => Ok(Step::RenameTrack {
                    id: track.id(),
                    name: name.clone(),
                }),
                Resolved::Folder(f) => Ok(Step::RenameFolder {
                    id: f.id,
                    name: name.clone(),
                }),
                Resolved::Clip(p) => Ok(Step::Clips(super::clips::ClipStep::Rename {
                    id: p.id,
                    name: Some(name.clone()),
                })),
                _ => Err(runtime(format!(
                    "rename() takes a track, a folder or a clip, not a {} handle.",
                    h.family_words()
                ))),
            })?;
            apply(lua, this.0, step, 0)
        });
        m.add_method(
            "create_folder",
            |lua, this, (name, tracks): (String, Value)| {
                let tracks = match tracks {
                    Value::Nil => Vec::new(),
                    v => track_ids(lua, &v, "Each track")?,
                };
                let id = read(lua, |w| Ok(w.doc.project().folders.next_id()))?;
                apply(lua, this.0, Step::CreateFolder { name, tracks }, 0)?;
                read(lua, |w| handle_or_nil(lua, w.folder(id)))
            },
        );
        m.add_method("delete_folder", |lua, this, f: Value| {
            let id = folder_id(lua, &handle_arg(&f, "The folder")?)?;
            apply(lua, this.0, Step::DeleteFolder { id }, 0)
        });
        m.add_method(
            "move_to_folder",
            |lua, this, (tracks, f): (Value, Value)| {
                let tracks = track_ids(lua, &tracks, "Each track")?;
                let id = folder_id(lua, &handle_arg(&f, "The folder")?)?;
                apply(lua, this.0, Step::IntoFolder { tracks, id }, 0)
            },
        );
        m.add_method("remove_from_folder", |lua, this, tracks: Value| {
            let tracks = track_ids(lua, &tracks, "Each track")?;
            apply(lua, this.0, Step::OutOfFolders { tracks }, 0)
        });
        m.add_method(
            "add_subchannel",
            |lua, this, (track, opts): (Value, Option<Table>)| {
                let track = track_id(lua, &handle_arg(&track, "The track")?)?;
                let (channel, name) = match &opts {
                    Some(t) => (
                        t.get::<Option<i64>>("channel")?,
                        t.get::<Option<String>>("name")?,
                    ),
                    None => (None, None),
                };
                let channel = channel_arg(channel)?;
                let (id, sub) = read(lua, |w| {
                    let t = w
                        .doc
                        .track_by_id(track)
                        .ok_or_else(|| runtime("That track no longer exists."))?;
                    let id = t
                        .subchannels()
                        .free_ids(1)
                        .and_then(|v| v.first().copied())
                        .ok_or_else(|| runtime("This track has no free subchannel id left."))?;
                    let mut sub = Subchannel::new(id, channel.unwrap_or(t.props().default_channel));
                    sub.name = name.filter(|n| !n.trim().is_empty());
                    Ok((id, sub))
                })?;
                apply(lua, this.0, Step::AddSubchannel { track, sub }, 0)?;
                read(lua, |w| handle_or_nil(lua, w.subchannel(track, id)))
            },
        );
        m.add_method("edit_subchannel", |lua, this, (s, opts): (Value, Table)| {
            let (track, mut sub) = subchannel_arg(lua, &s)?;
            if let Some(c) = channel_arg(opts.get::<Option<i64>>("channel")?)? {
                sub.midi_channel = c;
            }
            if let Some(n) = opts.get::<Option<String>>("name")? {
                sub.name = (!n.trim().is_empty()).then_some(n);
            }
            let move_notes = opts.get::<Option<bool>>("move_notes")?.unwrap_or(false);
            apply(
                lua,
                this.0,
                Step::EditSubchannel {
                    track,
                    sub,
                    move_notes,
                },
                0,
            )
        });
        m.add_method("delete_subchannel", |lua, this, s: Value| {
            let (track, sub) = subchannel_arg(lua, &s)?;
            let notes = read(lua, |w| {
                Ok(w.doc
                    .track_by_id(track)
                    .map(|t| t.notes.iter().filter(|n| n.sub == sub.id).count() as u64)
                    .unwrap_or(0))
            })?;
            apply(
                lua,
                this.0,
                Step::DeleteSubchannel { track, id: sub.id },
                notes,
            )
        });
        m.add_method(
            "set_tempo",
            |lua, this, (tick, bpm, opts): (Value, Value, Option<Table>)| {
                let tick = tick_arg(&tick, "The tick")?;
                let bpm = bpm_arg(&bpm)?;
                let ramp = match &opts {
                    Some(t) => t.get::<Option<bool>>("ramp")?.unwrap_or(false),
                    None => false,
                };
                let interp = if ramp { Interp::Linear } else { Interp::Step };
                apply(lua, this.0, Step::SetTempo { tick, bpm, interp }, 0)
            },
        );
        m.add_method("remove_tempo", |lua, this, tick: Value| {
            let tick = tick_arg(&tick, "The tick")?;
            apply(lua, this.0, Step::RemoveTempo { tick }, 0)
        });
        m.add_method(
            "set_time_signature",
            |lua, this, (tick, numerator, denominator): (Value, i64, i64)| {
                let tick = tick_arg(&tick, "The tick")?;
                let numerator = u8::try_from(numerator)
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| {
                        runtime(format!(
                            "A time signature has 1 - 255 beats per bar; {numerator} is not."
                        ))
                    })?;
                let denom_pow = u64::try_from(denominator)
                    .ok()
                    .filter(|d| d.is_power_of_two() && *d <= 128)
                    .map(|d| d.trailing_zeros() as u8)
                    .ok_or_else(|| {
                        runtime(format!(
                            "The beat unit must be 1, 2, 4, 8, 16, 32, 64 or 128; {denominator} is not."
                        ))
                    })?;
                apply(
                    lua,
                    this.0,
                    Step::SetSignature {
                        tick,
                        numerator,
                        denom_pow,
                    },
                    0,
                )
            },
        );
        m.add_method("remove_time_signature", |lua, this, tick: Value| {
            let tick = tick_arg(&tick, "The tick")?;
            apply(lua, this.0, Step::RemoveSignature { tick }, 0)
        });
        m.add_method(
            "add_marker",
            |lua, this, (tick, title, end): (Value, String, Value)| {
                let start = tick_arg(&tick, "The tick")?;
                let end = match end {
                    Value::Nil => None,
                    v => Some(tick_arg(&v, "The end tick")?),
                };
                apply(lua, this.0, Step::AddMarker { start, end, title }, 0)
            },
        );
        m.add_method("remove_markers", |lua, this, tick: Value| {
            let tick = tick_arg(&tick, "The tick")?;
            apply(lua, this.0, Step::RemoveMarkers { tick }, 0)
        });
        m.add_method("delete_notes", |lua, this, q: Value| {
            let q = query_arg(&q)?;
            let (host, hits) = evaluate(lua, &q)?;
            erase(lua, this.0, host, &hits, true)?;
            Ok(hits.len() as i64)
        });
        m.add_method("modify_notes", |lua, this, (q, how): (Value, Value)| {
            let q = query_arg(&q)?;
            let (host, hits) = evaluate(lua, &q)?;
            if hits.is_empty() {
                return Ok(0i64);
            }
            super::notes::check_output(lua, output_so_far(lua), hits.len() as u64)?;
            let max_key = read(lua, |w| Ok(w.doc.project().keys.max_pitch()))?;
            let mut out: Vec<NoteRecord> = Vec::with_capacity(hits.len());
            let mut changed = 0i64;
            let mut deleted = 0u64;
            match how {
                Value::Table(t) => {
                    let c = Changes::from_lua(&t)?;
                    for (i, h) in hits.iter().enumerate() {
                        if i % super::notes::CHECK_NOTES == 0 {
                            check_budget(lua)?;
                        }
                        match c.apply(&h.rec, max_key) {
                            Ok(Some(r)) => {
                                out.push(r);
                                changed += 1;
                            }
                            Ok(None) => deleted += 1,
                            Err(e) => {
                                return Err(runtime(format!(
                                    "Note at index {}: {e}; nothing was changed.",
                                    h.rank + 1
                                )));
                            }
                        }
                    }
                }
                Value::Function(f) => {
                    for h in &hits {
                        let note = super::notes::note_table(lua, h)?;
                        match f.call::<Value>(note)? {
                            Value::Nil => out.push(h.rec),
                            Value::Boolean(false) => deleted += 1,
                            Value::Table(t) => {
                                let default_channel = h.rec.attrs.channel();
                                out.push(record_from_table(&t, Some(&h.rec), default_channel)?);
                                changed += 1;
                            }
                            other => {
                                return Err(runtime(format!(
                                    "The function must return a table of changes, nil (keep) or false (delete), not {}.",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                }
                other => {
                    return Err(runtime(format!(
                        "modify_notes takes a table of changes or a function, not {}.",
                        other.type_name()
                    )));
                }
            }
            // Every note kept as it is (a function that returned nil each
            // time): nothing to record, no Undo step, the selection stays.
            if changed == 0 && deleted == 0 {
                return Ok(0);
            }
            let already=lua.app_data_ref::<TxSlot>().and_then(|s|s.open.as_ref().map(|t|t.removed_notes)).unwrap_or(0);
            super::response::check_removed(lua,already,deleted)?;
            let out = finish_records(lua, host, out)?;
            erase(lua, this.0, host, &hits, false)?;
            if !out.is_empty() {
                apply(lua, this.0, Step::InsertNotes { host, records: out }, 0)?;
            }
            if deleted > 0
                && let Some(mut slot) = lua.app_data_mut::<TxSlot>()
                && let Some(tx) = slot.open.as_mut()
            {
                tx.removed_notes = tx.removed_notes.saturating_add(deleted);
            }
            Ok(changed)
        });
        m.add_method(
            "duplicate_notes",
            |lua, this, (q, how): (Value, Option<Table>)| {
                let q = query_arg(&q)?;
                let (host, hits) = evaluate(lua, &q)?;
                read(lua, |w| {
                    for (i, hit) in hits.iter().enumerate() {
                        if i.is_multiple_of(4096) {
                            check_budget(lua)?;
                        }
                        validate_group(&w.doc, hit.rec.group)?;
                    }
                    Ok(())
                })?;
                let c = match &how {
                    Some(t) => Changes::from_lua(t)?,
                    None => Changes::default(),
                };
                super::notes::check_output(lua, output_so_far(lua), hits.len() as u64)?;
                let max_key = read(lua, |w| Ok(w.doc.project().keys.max_pitch()))?;
                let mut out = Vec::with_capacity(hits.len());
                for (i, h) in hits.iter().enumerate() {
                    if i % super::notes::CHECK_NOTES == 0 {
                        check_budget(lua)?;
                    }
                    match c.apply(&h.rec, max_key) {
                        Ok(Some(r)) => out.push(r),
                        Ok(None) => {}
                        Err(e) => {
                            return Err(runtime(format!(
                                "Note at index {}: {e}; nothing was changed.",
                                h.rank + 1
                            )));
                        }
                    }
                }
                let n = out.len() as i64;
                let out = finish_records(lua, host, out)?;
                if !out.is_empty() {
                    apply(lua, this.0, Step::InsertNotes { host, records: out }, 0)?;
                }
                Ok(n)
            },
        );
        m.add_method(
            "insert_notes",
            |lua, this, (target, notes): (Value, Table)| {
                let h = handle_arg(&target, "The track")?;
                let host = super::notes::host_of(lua, &h)?;
                let default_channel = read(lua, |w| {
                    Ok(w.doc
                        .track_by_id(host.track)
                        .map(|t| t.props().default_channel)
                        .unwrap_or(0))
                })?;
                let mut out = Vec::new();
                let count = match notes.get::<Value>("start")? {
                    Value::Table(starts) => starts.raw_len(),
                    _ => notes.raw_len(),
                };
                super::notes::check_output(lua, output_so_far(lua), count as u64)?;
                if let Value::Table(starts) = notes.get::<Value>("start")? {
                    // Struct of arrays: start / gate / key (/ velocity / channel / subchannel).
                    let n = starts.raw_len();
                    let columns = note_input::Columns::new(&notes, starts)?;
                    for i in 1..=n {
                        if i % super::notes::CHECK_NOTES == 0 {
                            check_budget(lua)?;
                        }
                        out.push(
                            record_from_input(
                                note_input::Input::Column(&columns, i),
                                None,
                                default_channel,
                            )
                            .map_err(|e| runtime(format!("Note {i}: {}", super::error_text(&e))))?,
                        );
                    }
                } else {
                    for (i, v) in notes.sequence_values::<Table>().enumerate() {
                        if i % super::notes::CHECK_NOTES == 0 {
                            check_budget(lua)?;
                        }
                        let t = v?;
                        out.push(record_from_table(&t, None, default_channel).map_err(|e| {
                            runtime(format!("Note {}: {}", i + 1, super::error_text(&e)))
                        })?);
                    }
                }
                let n = out.len() as i64;
                let out = finish_records(lua, host, out)?;
                if !out.is_empty() {
                    apply(lua, this.0, Step::InsertNotes { host, records: out }, 0)?;
                }
                Ok(n)
            },
        );
        m.add_method(
            "insert_events",
            |lua, this, (target, list): (Value, Table)| {
                super::response::explicit_only(lua, "Controller/selection changes")?;
                let h = handle_arg(&target, "The track")?;
                let host = super::notes::host_of(lua, &h)?;
                let default_channel = read(lua, |w| {
                    Ok(w.doc
                        .track_by_id(host.track)
                        .map(|t| t.props().default_channel)
                        .unwrap_or(0))
                })?;
                let mut add = Vec::new();
                for (i, t) in list.sequence_values::<Table>().enumerate() {
                    let t = t?;
                    add.push(
                        super::events::event_from_table(&t, None, default_channel).map_err(
                            |e| runtime(format!("Event {}: {}", i + 1, super::error_text(&e))),
                        )?,
                    );
                }
                let n = add.len() as i64;
                if n > 0 {
                    apply(
                        lua,
                        this.0,
                        Step::ReplaceEvents {
                            host,
                            remove: Vec::new(),
                            add,
                        },
                        0,
                    )?;
                }
                Ok(n)
            },
        );
        m.add_method("delete_events", |lua, this, q: Value| {
            super::response::explicit_only(lua, "Controller/selection changes")?;
            let q = event_query_arg(&q)?;
            let (host, idx) = super::events::evaluate(lua, &q)?;
            channel_only(lua, host, &idx)?;
            let n = idx.len() as i64;
            if n > 0 {
                apply(
                    lua,
                    this.0,
                    Step::ReplaceEvents {
                        host,
                        remove: idx,
                        add: Vec::new(),
                    },
                    0,
                )?;
            }
            Ok(n)
        });
        m.add_method("modify_events", |lua, this, (q, how): (Value, Value)| {
            super::response::explicit_only(lua,"Controller/selection changes")?;
            let q = event_query_arg(&q)?;
            let (host, idx) = super::events::evaluate(lua, &q)?;
            channel_only(lua, host, &idx)?;
            let old: Vec<RawEvent> = read(lua, |w| {
                let store = w
                    .doc
                    .events_of(host.content_ref())
                    .ok_or_else(|| runtime("Those events no longer exist."))?;
                Ok(idx.iter().map(|&i| store.events()[i]).collect())
            })?;
            let mut add = Vec::with_capacity(old.len());
            match how {
                Value::Table(c) => {
                    let shift = c.get::<Option<i64>>("shift")?.unwrap_or(0);
                    let set = c.get::<Option<i64>>("value")?;
                    let delta = c.get::<Option<i64>>("value_add")?.unwrap_or(0);
                    let channel = channel_arg(c.get::<Option<i64>>("channel")?)?;
                    for e in &old {
                        let kind = super::events::Kind::of(e);
                        let (lo, hi) = kind.value_range();
                        let value = set
                            .unwrap_or_else(|| super::events::value_of(e).unwrap_or(0))
                            .saturating_add(delta)
                            .clamp(lo, hi);
                        let tick = i128::from(e.tick) + i128::from(shift);
                        let tick = u64::try_from(tick).map_err(|_| {
                            let at = e.tick;
                            runtime(format!(
                                "An event at tick {at} would move before tick 0; nothing was changed."
                            ))
                        })?;
                        let RawKind::Channel { status, data } = e.kind else {
                            continue;
                        };
                        add.push(super::events::make(
                            kind,
                            tick,
                            channel.unwrap_or(status & 0x0F),
                            data[0],
                            value,
                        ));
                    }
                }
                Value::Function(f) => {
                    let w = super::handles::current_world(lua)
                        .ok_or_else(|| runtime("No project is open."))?;
                    let store = w
                        .doc
                        .events_of(host.content_ref())
                        .ok_or_else(|| runtime("Those events no longer exist."))?;
                    for (k, &i) in idx.iter().enumerate() {
                        let t = super::events::event_table(lua, store, i)?;
                        match f.call::<Value>(t)? {
                            Value::Nil => add.push(old[k]),
                            Value::Boolean(false) => {}
                            Value::Table(t) => add.push(super::events::event_from_table(
                                &t,
                                Some(&old[k]),
                                0,
                            )?),
                            other => {
                                return Err(runtime(format!(
                                    "The function must return a table of changes, nil (keep) or false (delete), not {}.",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                }
                other => {
                    return Err(runtime(format!(
                        "modify_events takes a table of changes or a function, not {}.",
                        other.type_name()
                    )));
                }
            }
            let n = idx.len() as i64;
            if n > 0 {
                apply(
                    lua,
                    this.0,
                    Step::ReplaceEvents {
                        host,
                        remove: idx,
                        add,
                    },
                    0,
                )?;
            }
            Ok(n)
        });
        m.add_method(
            "select_notes",
            |lua, this, (q, mode): (Value, Option<String>)| {
                super::response::explicit_only(lua,"Controller/selection changes")?;
                let q = query_arg(&q)?;
                let (host, hits) = evaluate(lua, &q)?;
                let mode = mode.unwrap_or_else(|| "replace".into());
                let current: Vec<usize> = read(lua, |w| {
                    Ok(host
                        .notes(&w.doc)
                        .map(|n| n.selected_ranks().iter().collect())
                        .unwrap_or_default())
                })?;
                let picked: Vec<usize> = hits.iter().map(|h| h.rank).collect();
                let mut fin: Vec<usize> = match mode.as_str() {
                    "replace" => picked,
                    "add" => {
                        let mut v = current;
                        v.extend(picked);
                        v
                    }
                    "remove" => {
                        let drop: std::collections::HashSet<usize> = picked.into_iter().collect();
                        current.into_iter().filter(|r| !drop.contains(r)).collect()
                    }
                    "toggle" => {
                        let cur: std::collections::HashSet<usize> = current.iter().copied().collect();
                        let pick: std::collections::HashSet<usize> = picked.iter().copied().collect();
                        cur.symmetric_difference(&pick).copied().collect()
                    }
                    other => {
                        return Err(runtime(format!(
                            "The mode \u{201C}{other}\u{201D} is not \"replace\", \"add\", \"remove\" or \"toggle\"."
                        )));
                    }
                };
                fin.sort_unstable();
                fin.dedup();
                set_selection(lua, this.0, host, &fin)?;
                Ok(fin.len() as i64)
            },
        );
        m.add_method("clear_selection", |lua, this, target: Value| {
            super::response::explicit_only(lua, "Controller/selection changes")?;
            let h = handle_arg(&target, "The track")?;
            let host = super::notes::host_of(lua, &h)?;
            set_selection(lua, this.0, host, &[])
        });
        m.add_method("estimate", |lua, this, ()| {
            let slot = lua
                .app_data_ref::<TxSlot>()
                .ok_or_else(|| runtime("Transactions are not available here."))?;
            let tx = slot
                .open
                .as_ref()
                .filter(|t| t.serial == this.0)
                .ok_or_else(|| runtime("This transaction is closed."))?;
            let t = lua.create_table()?;
            t.set("operations", tx.ops.len() as i64)?;
            t.set("staged_notes", writer::staged_notes(tx))?;
            t.set(
                "removed_notes",
                i64::try_from(tx.removed_notes).unwrap_or(i64::MAX),
            )?;
            t.set(
                "inserted_notes",
                i64::try_from(tx.inserted_notes).unwrap_or(i64::MAX),
            )?;
            t.set(
                "needs_confirmation",
                tx.removed_notes >= CONFIRM_REMOVED_NOTES,
            )?;
            Ok(t)
        });
        m.add_method("is_open", |lua, this, ()| {
            Ok(lua
                .app_data_ref::<TxSlot>()
                .and_then(|s| s.open.as_ref().map(|t| t.serial == this.0))
                .unwrap_or(false))
        });
        m.add_method("cancel", |lua, this, ()| {
            if let Some(mut slot) = lua.app_data_mut::<TxSlot>()
                && slot.open.as_ref().is_some_and(|t| t.serial == this.0)
            {
                slot.open = None;
            }
            Ok(())
        });
        m.add_function("commit", |lua, ud: AnyUserData| {
            let serial = ud.borrow::<LuaTx>()?.0;
            let (world_doc, ids, group_origin_floor) = {
                let mut slot = lua
                    .app_data_mut::<TxSlot>()
                    .ok_or_else(|| runtime("Transactions are not available here."))?;
                if let Some(tx) = slot.open.as_ref().filter(|tx| tx.serial == serial) {
                    writer::check_commit(tx)?;
                    super::response::check_removed(lua, 0, tx.removed_notes)?;
                    if super::reactive::responding(lua) && !tx.selection_hosts.is_empty() {
                        return Err(stop(
                            Category::Capability,
                            "Automatic responses cannot commit explicit selection changes.",
                        ));
                    }
                }
                let tx = match slot.open.take() {
                    Some(tx) if tx.serial == serial => tx,
                    other => {
                        slot.open = other;
                        return Err(runtime(
                            "This transaction is closed (committed or canceled); open a new one.",
                        ));
                    }
                };
                let OpenTx {
                    birth,
                    track_origin_floor,
                    label,
                    group_origin_floor,
                    work,
                    ids,
                    ops,
                    removed_notes,
                    inserted_notes,
                    selection_hosts,
                    writer_notes,
                    native,
                    ..
                } = tx;
                let selections: Vec<(NoteHost, RankSet)> = selection_hosts
                    .into_iter()
                    .filter_map(|h| h.notes(&work).map(|n| (h, n.selected_ranks())))
                    .collect();
                if !ops.is_empty() || !selections.is_empty() {
                    slot.commits.push(CommitStep {
                        birth,
                        birth_range: (track_origin_floor, work.next_track_id()),
                        label,
                        operations: ops.len(),
                        op: (!ops.is_empty()).then_some(Operation::Group { ops, label }),
                        selections,
                        removed_notes,
                        inserted_notes,
                        writer_notes,
                        native,
                    });
                }
                (work, ids, group_origin_floor)
            };
            // Later reads in this run see the committed state.
            if let Some(mut w) = lua.app_data_mut::<WorldSlot>()
                && let Some(base) = w.0.as_mut()
            {
                base.doc = world_doc;
                base.ids = ids.snapshot();
                base.group_scope = serial;
                base.group_origin_floor = group_origin_floor;
            }
            Ok(true)
        });
    }
}
