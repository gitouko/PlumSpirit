//! Immutable native transport facts. No Lua is evaluated by audio or rendering.
use super::{Capability, Host, bridge::ProjectStamp};
use crate::audio::{Transport, TransportState};
use mlua::{Lua, Table};
use plumspirit_engine::model::timing::TimingKind;
use plumspirit_engine::model::{Division, PlumDocument};
use std::sync::Arc;

pub mod control;
pub mod loop_intent;
pub mod midi;
pub mod plan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub stamp: ProjectStamp,
    pub action_revision: u64,
    pub midi_status: crate::audio::midi_overlay::Status,
    pub midi_pending: u32,
    pub state: TransportState,
    pub tick: u64,
    pub anchor: Option<u64>,
    pub looping: Option<(u64, u64)>,
    pub configured_loop: plumspirit_engine::model::project::LoopConfig,
    pub speed_bits: u64,
    pub bpm_bits: u64,
    pub division: Division,
    pub numerator: u8,
    pub denom_pow: u8,
    pub clocks: u8,
    pub thirty_seconds: u8,
}
impl Snapshot {
    /// Binary-search Conductor facts only; never builds an index or walks notes.
    pub fn capture(transport: &Transport, doc: &PlumDocument, stamp: ProjectStamp) -> Self {
        let tick = transport.position();
        let meter = doc.conductor().signature_at(tick);
        Self {
            stamp,
            action_revision: transport.action_revision(),
            midi_status: transport.midi_status(),
            midi_pending: transport.midi_pending(),
            state: transport.state(),
            tick,
            anchor: transport.anchor(),
            looping: transport.loop_range(),
            configured_loop: doc.project().looping,
            speed_bits: transport.speed().to_f64().to_bits(),
            bpm_bits: doc.conductor().bpm_at(tick).to_bits(),
            division: doc.division(),
            numerator: meter.numerator,
            denom_pow: meter.denom_pow,
            clocks: meter.clocks,
            thirty_seconds: meter.thirty_seconds,
        }
    }
    pub fn valid(&self) -> bool {
        let speed = f64::from_bits(self.speed_bits);
        let bpm = f64::from_bits(self.bpm_bits);
        speed.is_finite()
            && speed > 0.0
            && bpm.is_finite()
            && bpm > 0.0
            && self.numerator > 0
            && self.denom_pow <= 7
            && self.looping.is_none_or(|(a, b)| a < b)
            && self.configured_loop.range.is_none_or(|(a, b)| a < b)
    }
    pub(super) fn table(&self, lua: &Lua) -> mlua::Result<Table> {
        if !self.valid() {
            return Err(super::stop(
                super::Category::Runtime,
                "Invalid native transport snapshot.",
            ));
        }
        let t = lua.create_table()?;
        t.set("schema", 1)?;
        t.set("midi_output", self.midi_status.name())?;
        t.set("midi_pending", self.midi_pending)?;
        t.set(
            "action_revision",
            super::ticks::to_lua(self.action_revision)?,
        )?;
        t.set("project", super::ticks::to_lua(self.stamp.serial)?)?;
        t.set("revision", super::ticks::to_lua(self.stamp.revision)?)?;
        t.set(
            "state",
            match self.state {
                TransportState::Stopped => "stopped",
                TransportState::Playing => "playing",
                TransportState::Paused => "paused",
                TransportState::Seeking => "seeking",
                TransportState::AudioUnavailable => "audio_unavailable",
            },
        )?;
        t.set("running", self.state.is_running())?;
        t.set("tick", super::ticks::to_lua(self.tick)?)?;
        t.set("anchor", self.anchor.map(super::ticks::to_lua).transpose()?)?;
        if let Some((a, b)) = self.looping {
            let range = lua.create_table()?;
            range.set("start", super::ticks::to_lua(a)?)?;
            range.set("finish", super::ticks::to_lua(b)?)?;
            t.set("loop", range)?;
        }
        let configured = lua.create_table()?;
        configured.set("enabled", self.configured_loop.enabled)?;
        if let Some((a, b)) = self.configured_loop.range {
            configured.set("start", super::ticks::to_lua(a)?)?;
            configured.set("finish", super::ticks::to_lua(b)?)?;
        }
        t.set("configured_loop", configured)?;
        t.set("speed", f64::from_bits(self.speed_bits))?;
        t.set("bpm", f64::from_bits(self.bpm_bits))?;
        let meter = lua.create_table()?;
        meter.set("numerator", self.numerator)?;
        meter.set("denominator", 1u32 << self.denom_pow)?;
        meter.set("clocks", self.clocks)?;
        meter.set("thirty_seconds", self.thirty_seconds)?;
        t.set("meter", meter)?;
        let division = lua.create_table()?;
        match self.division.kind() {
            TimingKind::Ppqn(ppqn) => {
                division.set("kind", "ppqn")?;
                division.set("ppqn", ppqn)?;
            }
            TimingKind::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => {
                division.set("kind", "smpte")?;
                division.set("frames_per_second", frames_per_second)?;
                division.set("ticks_per_frame", ticks_per_frame)?;
            }
        }
        t.set("division", division)?;
        Ok(t)
    }
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "snapshot",
        lua.create_function(|lua, ()| {
            super::reactive::check_read(lua)?;
            let granted = lua
                .app_data_ref::<super::handles::Granted>()
                .map(|g| Arc::clone(&g.0))
                .unwrap_or_default();
            Host::require(&granted, Capability::ReadPlayback)?;
            // An observer reads exactly the captured event; ordinary jobs read fixed run facts.
            let observed = lua.app_data_ref::<super::reactive::Scope>().and_then(|s| {
                s.delivery.as_ref().and_then(|d| match &d.event.data {
                    super::reactive::Data::Transport(s) => Some(*s),
                    _ => d.playback,
                })
            });
            let snapshot = observed.or_else(|| {
                lua.app_data_ref::<super::handles::WorldSlot>()
                    .and_then(|w| w.0.as_ref().and_then(|w| w.info.playback))
            });
            snapshot.map(|s| s.table(lua)).transpose()
        })?,
    )?;
    control::install(lua, &t)?;
    loop_intent::install(lua, &t)?;
    plan::install(lua, &t)?;
    midi::install(lua, &t)?;
    plum.set("playback", t)
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
