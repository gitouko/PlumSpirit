use super::*;
use crate::model::{
    Bpm, Division, PlumDocument, TempoValue,
    conductor::{Interp, TempoPoint},
    tempo::Speed,
};

fn event(tick: u64, status: u8, data1: u8, data2: u8) -> Event {
    Event {
        tick,
        status,
        data1,
        data2,
    }
}
fn cc(tick: u64) -> Event {
    event(tick, 0xB0, 1, 64)
}
fn tempo() -> TempoMap {
    TempoMap::from_tempos(48_000, 24_000, &[])
}
fn compile(events: &[Event]) -> Result<Prepared, CompileError<()>> {
    Prepared::compile(events, &tempo(), || Ok::<(), ()>(()))
}
#[derive(Default, Debug, PartialEq, Eq)]
struct Recorder {
    offset: u32,
    messages: Vec<(u32, u8, u8, u8)>,
    silences: usize,
}
impl SynthSink for Recorder {
    fn note_on(&mut self, ch: u8, k: u8, v: u8) {
        self.messages.push((self.offset, 0x90 | ch, k, v));
    }
    fn note_off(&mut self, ch: u8, k: u8) {
        self.messages.push((self.offset, 0x80 | ch, k, 0));
    }
    fn channel_message(&mut self, s: u8, a: u8, b: u8) {
        self.messages.push((self.offset, s, a, b));
    }
    fn silence(&mut self) {
        self.silences += 1;
    }
    fn reset(&mut self) {
        panic!("A plugin cursor must not reset song state");
    }
    fn timed(&self) -> bool {
        true
    }
    fn at_offset(&mut self, offset: u32) {
        self.offset = offset;
    }
}
#[test]
fn native_validation_cannot_be_bypassed_with_invalid_bytes_order_or_tick_overflow() {
    assert_eq!(compiled_bytes(0), None);
    assert_eq!(compiled_bytes(MAX_EVENTS + 1), None);
    assert_eq!(
        compiled_bytes(MAX_EVENTS),
        Some(MAX_EVENTS * size_of::<FrameEvent>())
    );
    assert!(compile(&[]).is_err());
    for bad in [
        event(0, 0x70, 1, 1),
        event(0, 0xF0, 1, 1),
        event(0, 0xFF, 1, 1),
        event(0, 0x90, 128, 1),
        event(0, 0x90, 1, 128),
        event(0, 0xC0, 1, 1),
        event(0, 0xD0, 1, 1),
        event(0, 0xB0, 120, 0),
        event(0, 0xBF, 127, 0),
        event(u64::MAX, 0xB0, 1, 1),
    ] {
        assert!(!bad.valid());
        assert!(compile(&[bad]).is_err(), "accepted {bad:?}");
    }
    assert!(compile(&[cc(2), cc(1)]).is_err());
    assert!(
        Prepared::compile(
            &[cc(0)],
            &TempoMap::from_tempos(1, 1, &[]),
            || Ok::<(), ()>(())
        )
        .is_err()
    );
    let slow = TempoMap::from_tempos(
        384_000,
        1,
        &[(0, TempoValue::Bpm(Bpm::parse("1").unwrap()))],
    );
    assert!(Prepared::compile(&[cc(i64::MAX as u64)], &slow, || Ok::<(), ()>(())).is_err());
}
#[test]
fn density_limit_uses_sliding_frames_and_span_limit_accepts_exact_boundary() {
    let mut burst = vec![cc(63); 64];
    assert!(compile(&burst).is_ok());
    burst.push(cc(64));
    assert!(compile(&burst).is_err()); // straddles fixed block buckets
    let spread: Vec<_> = (0..MAX_EVENTS).map(|i| cc(i as u64)).collect();
    let prepared = compile(&spread).unwrap();
    assert_eq!(prepared.events().len(), MAX_EVENTS);
    assert_eq!(prepared.sample_rate(), 48000);
    assert!(compile(&[cc(0), cc(48000 * 60)]).is_ok());
    assert!(compile(&[cc(0), cc(48000 * 60 + 1)]).is_err());
    let oversized = vec![cc(0); MAX_EVENTS + 1];
    assert!(compile(&oversized).is_err());
}
#[test]
fn held_key_budget_pairing_and_zero_velocity_note_off_are_checked_before_publication() {
    assert!(compile(&[event(0, 0x90, 60, 100)]).is_err());
    assert!(compile(&[event(0, 0x80, 60, 0)]).is_err());
    assert!(
        compile(&[
            event(0, 0x90, 60, 100),
            event(1, 0x90, 60, 100),
            event(2, 0x80, 60, 0)
        ])
        .is_err()
    );
    assert!(compile(&[event(0, 0x91, 60, 100), event(1, 0x90, 60, 0)]).is_err());
    assert!(compile(&[event(0, 0x9F, 127, 127), event(1, 0x9F, 127, 0)]).is_ok());
    let mut events: Vec<_> = (0..128).map(|i| event(i, 0x90, i as u8, 100)).collect();
    events.extend((0..128).map(|i| event(128 + i, 0x80, i as u8, 64)));
    assert!(compile(&events).is_ok());
    events.insert(128, event(128, 0x91, 0, 100));
    assert!(compile(&events).is_err());
}
#[test]
fn exact_native_tempo_ramp_speed_and_smpte_maps_are_used_without_float_tick_rounding() {
    let points = [
        TempoPoint {
            id: 1,
            tick: 0,
            value: TempoValue::Bpm(Bpm::parse("60").unwrap()),
            interp: Interp::Linear,
            implicit: false,
            origin: None,
        },
        TempoPoint {
            id: 2,
            tick: 480,
            value: TempoValue::Bpm(Bpm::parse("120").unwrap()),
            interp: Interp::Step,
            implicit: false,
            origin: None,
        },
    ];
    let ramp = TempoMap::from_points(48000, 480, &points);
    let fast = TempoMap::from_points_at(48000, 480, &points, Speed::parse("2").unwrap());
    for map in [&ramp, &fast] {
        let p = Prepared::compile(
            &[event(0, 0x90, 60, 100), event(480, 0x80, 60, 0)],
            map,
            || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(p.events()[1].frame, map.frame_at_tick(480));
    }
    assert!((ramp.frame_at_tick(480) as f64 - 48000.0 * 2f64.ln()).abs() < 1.0);
    assert!((fast.frame_at_tick(480) as f64 - 24000.0 * 2f64.ln()).abs() < 1.0);
    let smpte = PlumDocument::new_empty(Division::from_raw(0xE728));
    let map = TempoMap::for_document(&smpte, 48000);
    let p = Prepared::compile(&[cc(0), cc(1000)], &map, || Ok::<(), ()>(())).unwrap();
    assert_eq!(p.events()[1].frame, 48000);
    let big = 9_007_199_254_740_993;
    let p = compile(&[cc(big), cc(big + 1)]).unwrap();
    assert_eq!(p.events()[0].event.tick, big);
    assert_eq!(p.events()[1].event.tick, big + 1);
}
#[test]
fn cancellation_during_validation_and_result_copy_never_returns_partial_prepared_data() {
    let input: Vec<_> = (0..8192).map(cc).collect();
    for stop_at in [1, 17, 34, 50, 66] {
        let mut checks = 0;
        let result = Prepared::compile(&input, &tempo(), || {
            checks += 1;
            if checks == stop_at {
                Err("canceled")
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(CompileError::Interrupted("canceled"))),
            "{stop_at}"
        );
        assert_eq!(checks, stop_at);
    }
}
#[test]
fn bounded_cursor_preserves_authored_order_offsets_and_native_song_sink_is_untouched() {
    let events = [
        event(0, 0xC0, 3, 0),
        event(0, 0x90, 60, 100),
        event(63, 0xE0, 1, 64),
        event(64, 0x90, 60, 0),
        event(64, 0x90, 60, 90),
        event(127, 0x80, 60, 70),
    ];
    let p = compile(&events).unwrap();
    let release_end = p.release_end();
    let mut cursor = Cursor::new(p, 0).unwrap();
    let mut song = Recorder::default();
    song.note_on(0, 60, 120);
    let before = song.messages.clone();
    let mut plugin = Recorder::default();
    assert_eq!(cursor.advance(0, 64, &mut plugin), Status::Active);
    assert_eq!(
        plugin.messages,
        vec![(0, 0xC0, 3, 0), (0, 0x90, 60, 100), (63, 0xE0, 1, 64)]
    );
    assert_eq!(cursor.pending(), 3);
    cursor.advance(64, 64, &mut plugin);
    assert_eq!(
        &plugin.messages[3..],
        &[(0, 0x80, 60, 0), (0, 0x90, 60, 90), (63, 0x80, 60, 0)]
    );
    assert_eq!(cursor.pending(), 0);
    for frame in (128..release_end).step_by(64) {
        cursor.advance(frame, 64, &mut plugin);
    }
    assert_eq!(cursor.status(), Status::Finished);
    assert_eq!(plugin.silences, 1);
    cursor.cancel(&mut plugin);
    cursor.advance(0, 64, &mut plugin);
    assert_eq!(plugin.silences, 1);
    assert_eq!(song.messages, before);
    assert_eq!(song.silences, 0);
}
#[test]
fn canceled_and_discontinuous_clocks_release_owned_voices_once_and_never_restart() {
    for (frame, frames) in [(0, 0), (0, 65), (1, 64), (u64::MAX, 64)] {
        let mut cursor = Cursor::new(
            compile(&[event(0, 0x90, 60, 1), event(100, 0x80, 60, 0)]).unwrap(),
            0,
        )
        .unwrap();
        let mut sink = Recorder::default();
        assert_eq!(
            cursor.advance(frame, frames, &mut sink),
            Status::ClockChanged
        );
        cursor.cancel(&mut sink);
        assert_eq!(sink.silences, 1);
        assert_eq!(cursor.advance(0, 64, &mut sink), Status::ClockChanged);
        assert!(sink.messages.is_empty());
    }
    let mut cursor = Cursor::new(
        compile(&[event(0, 0x90, 60, 1), event(100, 0x80, 60, 0)]).unwrap(),
        0,
    )
    .unwrap();
    let mut sink = Recorder::default();
    cursor.advance(0, 64, &mut sink);
    cursor.cancel(&mut sink);
    cursor.cancel(&mut sink);
    assert_eq!(sink.silences, 1);
    assert_eq!(cursor.status(), Status::Canceled);
    assert_eq!(sink.messages.len(), 1);
    assert_eq!(cursor.pending(), 1);
    assert!(Cursor::new(compile(&[cc(10)]).unwrap(), 11).is_err());
}
#[test]
fn any_valid_block_dispatch_is_bounded_and_distinct_ticks_that_share_a_frame_keep_order() {
    let many: Vec<_> = (0..4096).map(cc).collect();
    let mut cursor = Cursor::new(compile(&many).unwrap(), 0).unwrap();
    let mut sink = Recorder::default();
    for frame in (0..4096).step_by(64) {
        let before = sink.messages.len();
        cursor.advance(frame, 64, &mut sink);
        assert!(sink.messages.len() - before <= MAX_BLOCK_EVENTS);
    }
    let compressed = TempoMap::from_tempos(48000, 48000, &[]); // 2 ticks/frame
    let p = Prepared::compile(
        &[event(0, 0xC0, 1, 0), event(1, 0xC0, 2, 0)],
        &compressed,
        || Ok::<(), ()>(()),
    )
    .unwrap();
    assert_eq!(p.events()[0].frame, p.events()[1].frame);
    let mut cursor = Cursor::new(p, 0).unwrap();
    let mut sink = Recorder::default();
    cursor.advance(0, 1, &mut sink);
    assert_eq!(sink.messages, vec![(0, 0xC0, 1, 0), (0, 0xC0, 2, 0)]);
}
