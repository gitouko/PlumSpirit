//! Root-free staged native MIDI submission. Only Rust-owned data crosses threads.
use super::{Snapshot, control};
use crate::audio::midi_overlay::{PreparedVoice, SYNTH_BYTES};
use crate::lua::{
    Capability, Category, Host,
    bridge::{JobLease, NativeBudget},
    notes, stop,
};
use mlua::{Lua, Table, Value};
use plumspirit_engine::playback::midi_plan::{CompileError, Event, Prepared, compiled_bytes};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Staged {
    voice: Box<PreparedVoice>,
    guard: Snapshot,
    lease: Arc<JobLease>,
    run_epoch: Arc<AtomicU64>,
    run_version: u64,
    version: u64,
}
#[derive(Default)]
pub(crate) struct Slot {
    pub cancel_epoch: Arc<AtomicU64>,
    staged: Option<Staged>,
}
struct Data {
    voice: Mutex<Option<Box<PreparedVoice>>>,
    guard: Snapshot,
    lease: Arc<JobLease>,
    run_epoch: Arc<AtomicU64>,
    run_version: u64,
    cancel_epoch: Arc<AtomicU64>,
    version: u64,
    used: AtomicBool,
}
#[derive(Clone)]
pub struct Intent(Arc<Data>);
impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiIntent")
            .field("guard", &self.0.guard)
            .field("job", &self.0.lease.job)
            .finish_non_exhaustive()
    }
}
impl PartialEq for Intent {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Intent {
    pub fn valid(&self) -> bool {
        !self.0.used.load(Ordering::Acquire)
            && self.0.run_epoch.load(Ordering::Acquire) == self.0.run_version
            && self.0.cancel_epoch.load(Ordering::Acquire) == self.0.version
            && !self.0.lease.cancellation_requested()
            && std::time::Instant::now() < self.0.lease.deadline()
    }
    pub fn take_voice(&self) -> Option<Box<PreparedVoice>> {
        if !self.valid()
            || self
                .0
                .used
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        self.0.voice.lock().ok()?.take()
    }
    pub fn refuse(&self) {
        self.0.used.store(true, Ordering::Release);
    }
    pub fn guard(&self) -> Snapshot {
        self.0.guard
    }
    pub fn plugin(&self) -> &Arc<str> {
        &self.0.lease.plugin
    }
    pub fn cancel_epoch(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.0.cancel_epoch)
    }
    pub fn version(&self) -> u64 {
        self.0.version
    }
}
pub(crate) fn begin_run(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        slot.staged = None;
    }
}
pub(crate) fn end_run(lua: &Lua, ok: bool) -> Option<Intent> {
    let mut slot = lua.app_data_mut::<Slot>()?;
    let s = slot.staged.take()?;
    if !ok
        || s.lease.check().is_err()
        || slot.cancel_epoch.load(Ordering::Acquire) != s.version
        || s.run_epoch.load(Ordering::Acquire) != s.run_version
    {
        return None;
    }
    Some(Intent(Arc::new(Data {
        voice: Mutex::new(Some(s.voice)),
        guard: s.guard,
        lease: s.lease,
        run_epoch: s.run_epoch,
        run_version: s.run_version,
        cancel_epoch: Arc::clone(&slot.cancel_epoch),
        version: s.version,
        used: AtomicBool::new(false),
    })))
}
pub(crate) fn check_transaction(lua: &Lua) -> mlua::Result<()> {
    if lua
        .app_data_ref::<Slot>()
        .is_some_and(|s| s.staged.is_some())
    {
        return Err(stop(
            Category::Runtime,
            "A staged MIDI plan requires a standalone command without musical transactions or transport actions.",
        ));
    }
    Ok(())
}
pub(super) fn install(lua: &Lua, playback: &Table) -> mlua::Result<()> {
    playback.set(
        "tick_after",
        lua.create_function(|lua, seconds: f64| {
            crate::lua::reactive::check_read(lua)?;
            notes::check_budget(lua)?;
            let grants = lua
                .app_data_ref::<crate::lua::handles::Granted>()
                .map(|g| Arc::clone(&g.0))
                .unwrap_or_default();
            Host::require(&grants, Capability::ReadPlayback)?;
            if !seconds.is_finite() || !(0.0..=60.0).contains(&seconds) {
                return Err(stop(
                    Category::Runtime,
                    "Playback offset must be finite seconds from 0 through 60.",
                ));
            }
            let (initial, resources) = lua
                .app_data_ref::<crate::lua::handles::WorldSlot>()
                .and_then(|w| {
                    w.0.as_ref()
                        .and_then(|w| w.info.playback.zip(w.info.playback_resources.clone()))
                })
                .ok_or_else(|| {
                    stop(
                        Category::Runtime,
                        "No captured running native playback time map is available.",
                    )
                })?;
            let observed = lua
                .app_data_ref::<crate::lua::reactive::Scope>()
                .and_then(|s| {
                    s.delivery.as_ref().and_then(|d| match &d.event.data {
                        crate::lua::reactive::Data::Transport(snapshot) => Some(*snapshot),
                        _ => None,
                    })
                });
            let snapshot = observed.unwrap_or(initial);
            if snapshot.stamp != initial.stamp
                || snapshot.action_revision != initial.action_revision
                || snapshot.configured_loop != initial.configured_loop
            {
                return Err(stop(
                    Category::Expired,
                    "The captured event and native playback time map no longer match.",
                ));
            }
            if seconds == 0.0 {
                return crate::lua::ticks::to_lua(snapshot.tick);
            }
            let base = resources.tempo.frame_at_tick(snapshot.tick);
            let delta = (seconds * f64::from(resources.sample_rate)).ceil() as u64;
            let frame = base
                .checked_add(delta)
                .ok_or_else(|| stop(Category::Runtime, "Playback time offset overflowed."))?;
            crate::lua::ticks::to_lua(
                resources
                    .tempo
                    .first_tick_at_or_after_frame(frame)
                    .max(snapshot.tick),
            )
        })?,
    )?;
    playback.set(
        "cancel_midi",
        lua.create_function(|lua, ()| {
            crate::lua::reactive::check_write(lua)?;
            let grants = lua
                .app_data_ref::<crate::lua::handles::Granted>()
                .map(|g| Arc::clone(&g.0))
                .unwrap_or_default();
            Host::require(&grants, Capability::ReadPlayback)?;
            Host::require(&grants, Capability::EmitMidi)?;
            let lease = lua
                .app_data_ref::<crate::lua::bridge::Session>()
                .and_then(|s| s.lease.clone())
                .ok_or_else(|| stop(Category::Expired, "No MIDI command is active."))?;
            lease.check().map_err(|e| e.lua())?;
            if !lease.user_invoked() {
                return Err(stop(
                    Category::Capability,
                    "MIDI cancellation requires an explicitly invoked plugin command.",
                ));
            }
            let mut controls = lua
                .app_data_mut::<control::Slot>()
                .ok_or_else(|| stop(Category::Runtime, "MIDI controls are unavailable."))?;
            if controls.count >= control::MAX_REQUESTS {
                return Err(stop(
                    Category::Budget,
                    "Too many playback requests in this job.",
                ));
            }
            controls.count += 1;
            lua.app_data_ref::<Slot>()
                .ok_or_else(|| stop(Category::Runtime, "MIDI lifecycle is unavailable."))?
                .cancel_epoch
                .fetch_add(1, Ordering::AcqRel);
            let out = lua.create_table()?;
            out.set("cancellation_requested", true)?;
            Ok(out)
        })?,
    )
}
pub(super) fn stage(lua: &Lua, events: &[Event], options: Table) -> mlua::Result<Table> {
    crate::lua::reactive::check_write(lua)?;
    let grants = lua
        .app_data_ref::<crate::lua::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&grants, Capability::ReadPlayback)?;
    Host::require(&grants, Capability::EmitMidi)?;
    let lease = lua
        .app_data_ref::<crate::lua::bridge::Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| stop(Category::Expired, "No MIDI output job is active."))?;
    lease.check().map_err(|e| e.lua())?;
    if !lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "MIDI output requires an explicitly invoked native plugin command.",
        ));
    }
    check_transaction(lua)?;
    super::loop_intent::check_transaction(lua)?;
    if crate::lua::tx::has_pending_work(lua) {
        return Err(stop(
            Category::Runtime,
            "Stage MIDI in a standalone command without musical transactions.",
        ));
    }
    if options.metatable().is_some() {
        return Err(stop(
            Category::Runtime,
            "MIDI output options must be plain.",
        ));
    }
    for pair in options.pairs::<Value, Value>() {
        let (key, _) = pair?;
        if !matches!(key,Value::String(s) if s.as_bytes().as_ref()==b"output") {
            return Err(stop(
                Category::Runtime,
                "Only output='internal' is supported for MIDI plans.",
            ));
        }
    }
    if !matches!(options.raw_get::<Value>("output")?,Value::String(s) if s.as_bytes().as_ref()==b"internal")
    {
        return Err(stop(
            Category::Runtime,
            "Choose output='internal' explicitly; shared external routes are unsupported.",
        ));
    }
    let (initial,resources)=lua.app_data_ref::<crate::lua::handles::WorldSlot>()
        .and_then(|w|w.0.as_ref().and_then(|w|w.info.playback.zip(w.info.playback_resources.clone())))
        .ok_or_else(||stop(Category::Runtime,"Start native playback with a loaded SoundFont and without Pre-Render before staging MIDI."))?;
    let guard = {
        let mut control = lua
            .app_data_mut::<control::Slot>()
            .ok_or_else(|| stop(Category::Runtime, "MIDI output is unavailable."))?;
        if control.count >= control::MAX_REQUESTS {
            return Err(stop(
                Category::Budget,
                "Too many playback requests in this job.",
            ));
        }
        control.count += 1;
        control
            .expected
            .ok_or_else(|| stop(Category::Expired, "No captured playback authority."))?
    };
    if guard != initial || !guard.state.is_running() || lease.project != Some(guard.stamp) {
        return Err(stop(
            Category::Expired,
            "Playback changed during this command; use a new standalone MIDI command.",
        ));
    }
    let bytes = compiled_bytes(events.len()).ok_or_else(|| {
        stop(
            Category::Runtime,
            "A scheduled plan requires 1 through 262144 events.",
        )
    })?;
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .ok_or_else(|| stop(Category::Runtime, "Native MIDI accounting is unavailable."))?;
    let charge = budget
        .reserve(SYNTH_BYTES + bytes + 4096)
        .map_err(|e| e.lua())?;
    let check = || {
        notes::check_budget(lua)?;
        lease.check().map_err(|e| e.lua())
    };
    let prepared = Prepared::compile(events, &resources.tempo, check).map_err(|e| match e {
        CompileError::Interrupted(e) => e,
        CompileError::Invalid(s) => stop(Category::Runtime, s),
        CompileError::Allocation => stop(Category::Budget, "MIDI plan allocation failed."),
    })?;
    let voice =
        PreparedVoice::new(prepared, resources, charge).map_err(|s| stop(Category::Runtime, s))?;
    check()?;
    let run_epoch = lua
        .app_data_ref::<super::loop_intent::Slot>()
        .map(|s| Arc::clone(&s.epoch))
        .ok_or_else(|| stop(Category::Runtime, "MIDI lifecycle scope is unavailable."))?;
    let run_version = run_epoch.load(Ordering::Acquire);
    let mut slot = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| stop(Category::Runtime, "MIDI staging is unavailable."))?;
    let version = slot.cancel_epoch.load(Ordering::Acquire);
    slot.staged = Some(Staged {
        voice: Box::new(voice),
        guard,
        lease,
        run_epoch,
        run_version,
        version,
    });
    let out = lua.create_table()?;
    out.set("staged", true)?;
    out.set("output", "internal")?;
    out.set("count", events.len())?;
    Ok(out)
}
#[cfg(test)]
mod tests;
