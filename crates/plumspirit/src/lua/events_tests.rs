//! B1-0030: the Events / CC API — reads with kinds and context, opaque meta
//! and SysEx, Conductor-only tempo, channel-message writes in transactions.

use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, PlumDocument, RawEvent, RawKind};
use std::sync::Arc;

fn ch(tick: u64, status: u8, d0: u8, d1: u8) -> RawEvent {
    RawEvent {
        tick,
        anchor: 0,
        kind: RawKind::Channel {
            status,
            data: [d0, d1],
        },
    }
}

/// Track 1: a text meta event, CC 7 on channels 0 / 1, pitch bend (min,
/// center, max), a program change, a SysEx.
fn project() -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let ev = &mut d.track_mut(0).unwrap().events;
    let text = ev.store_payload(b"hello").unwrap();
    let sysex = ev.store_payload(&[0x7E, 0x7F, 0x09, 0x01, 0xF7]).unwrap();
    ev.push(RawEvent {
        tick: 0,
        anchor: 0,
        kind: RawKind::Meta {
            meta_type: 0x01,
            payload: text,
        },
    })
    .unwrap();
    ev.push(ch(0, 0xB0, 7, 100)).unwrap();
    ev.push(ch(0, 0xC0, 5, 0)).unwrap();
    ev.push(ch(480, 0xB1, 7, 90)).unwrap();
    ev.push(ch(480, 0xE0, 0, 0)).unwrap();
    ev.push(ch(960, 0xE0, 0, 64)).unwrap();
    ev.push(ch(1440, 0xE0, 127, 127)).unwrap();
    ev.push(RawEvent {
        tick: 1920,
        anchor: 0,
        kind: RawKind::SysEx { payload: sysex },
    })
    .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn host_for(doc: &Arc<PlumDocument>, h: &mut History) -> Host {
    h.observe_ids(doc);
    let host = Host::new(
        Limits::default(),
        [Capability::ReadProject, Capability::EditNotes],
    )
    .unwrap();
    host.set_world(Some(World::new(
        Arc::clone(doc),
        h.incarnations().snapshot(),
    )));
    host
}

fn run(host: &Host, src: &str) -> Plain {
    host.run("events.lua", src)
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn events_are_read_with_their_kind_channel_and_values() {
    let (doc, mut h) = project();
    let host = host_for(&doc, &mut h);
    let r = run(
        &host,
        r#"local t = plum.project.current():track(1)
           assert(t:events():count() == 8)
           assert(t:events{ kind = "cc" }:count() == 2)
           assert(t:events{ kind = "cc", channel = 1 }:count() == 1)
           assert(t:events{ controller = 7 }:count() == 2)
           assert(t:events{ kind = { "cc", "program" }, end_tick = 1 }:count() == 2)
           local pb = t:events{ kind = "pitch_bend" }:list()
           assert(#pb == 3)
           assert(pb[1].value == -8192 and pb[1].raw == 0)
           assert(pb[2].value == 0 and pb[2].raw == 8192)
           assert(pb[3].value == 8191 and pb[3].raw == 16383)
           local p = t:events{ kind = "program" }:list()[1]
           assert(p.value == 5 and p.channel == 0 and p.tick == 0)
           -- Opaque metadata comes back byte for byte.
           local m = t:events{ kind = "meta" }:list()[1]
           assert(m.meta_type == 1 and m.data == "hello" and m.channel == nil)
           local s = t:events{ kind = "sysex" }:list()[1]
           assert(#s.data == 5 and s.data:byte(1) == 0x7E and s.data:byte(5) == 0xF7)
           assert(#t:events{ kind = "cc" }:handles() == 2)
           -- Tempo is the Conductor's.
           local ok, err = pcall(function() return t:events{ kind = "tempo" } end)
           assert(not ok and tostring(err):find("belong to the Conductor", 1, true), tostring(err))
           return true"#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn channel_events_are_written_in_one_undo_step_and_metadata_is_kept() {
    let (mut doc, mut h) = project();
    let before = doc.export_fingerprint();
    let host = host_for(&doc, &mut h);
    let preview = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local tx = plum.transaction{ label = "Automation" }
           assert(tx:insert_events(t, {
               { kind = "cc", tick = 240, controller = 10, value = 64 },
               { kind = "pitch_bend", tick = 240, value = -8192 },
               { kind = "program", tick = 2000, value = 33, channel = 9 },
               { kind = "channel_pressure", tick = 10, value = 1 },
               { kind = "poly_pressure", tick = 11, key = 60, value = 2 },
           }) == 5)
           assert(t:events{ kind = "cc", controller = 10 }:list()[1].value == 64)
           -- Native change: values clamp to their range.
           assert(tx:modify_events(t:events{ kind = "cc", controller = 7 }, { value_add = 50, shift = 960 }) == 2)
           local cc7 = t:events{ controller = 7 }:list()
           assert(cc7[1].value == 127 and cc7[1].tick == 960 and cc7[2].tick == 1440)
           -- A function: keep, change or delete each event.
           assert(tx:modify_events(t:events{ kind = "pitch_bend" }, function(e)
               if e.value == 0 then return false end
               if e.value == 8191 then return { value = 4096 } end
           end) == 4)
           local pb = t:events{ kind = "pitch_bend" }:list()
           assert(#pb == 3 and pb[3].value == 4096, #pb)
           assert(tx:delete_events(t:events{ kind = "program", channel = 9 }) == 1)
           -- Metadata cannot be changed, and is untouched.
           local ok, err = pcall(function() tx:delete_events(t:events{ end_tick = 1 }) end)
           assert(not ok and tostring(err):find("meta event", 1, true), tostring(err))
           ok, err = pcall(function() tx:insert_events(t, { { kind = "meta", tick = 0, value = 1 } }) end)
           assert(not ok and tostring(err):find("cannot be added", 1, true))
           ok, err = pcall(function() tx:insert_events(t, { { kind = "pitch_bend", tick = 0, value = 8192 } }) end)
           assert(not ok and tostring(err):find("-8192 - 8191", 1, true), tostring(err))
           ok, err = pcall(function() tx:insert_events(t, { { kind = "cc", tick = 0, value = 1 } }) end)
           assert(not ok and tostring(err):find("controller number", 1, true))
           assert(t:events{ kind = "meta" }:list()[1].data == "hello")
           assert(t:events{ kind = "sysex" }:count() == 1)
           local hash = p:content_hash()
           tx:commit()
           return hash"#,
    );
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        preview,
        "Preview = Apply"
    );
    let ev = doc.track(0).unwrap().events.events().to_vec();
    assert!(ev.windows(2).all(|w| w[0].tick <= w[1].tick), "sorted");
    assert_eq!(ev.len(), 8 + 5 - 1 - 1);
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    assert_eq!(d.export_fingerprint(), before);
}
