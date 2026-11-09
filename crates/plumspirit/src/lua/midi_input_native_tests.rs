//! Manufactured packets/threads; these tests do not open a MIDI device.
use super::*;
use std::sync::Arc;
fn packet(status: u8, a: u8, b: u8) -> u32 {
    u32::from(status) | (u32::from(a) << 8) | (u32::from(b) << 16) | 0xab00_0000
}
#[test]
fn known_short_packets_and_all_channels_follow_native_data_lengths_without_running_status() {
    for ch in 0..16 {
        for (base, a, b, kind) in [
            (0x80, 60, 64, Kind::NoteOff),
            (0x90, 60, 100, Kind::NoteOn),
            (0x90, 60, 0, Kind::NoteOff),
            (0xa0, 60, 80, Kind::PolyPressure),
            (0xb0, 74, 127, Kind::ControlChange),
            (0xb0, 123, 0, Kind::ControlChange),
            (0xc0, 127, 255, Kind::ProgramChange),
            (0xd0, 99, 255, Kind::ChannelPressure),
            (0xe0, 0, 64, Kind::PitchBend),
        ] {
            let m = Message::decode(packet(base | ch, a, b)).unwrap();
            assert_eq!(m.status, base | ch);
            assert_eq!(m.channel(), Some(ch));
            assert_eq!(m.data1, a);
            assert_eq!(m.data2, if base == 0xc0 || base == 0xd0 { 0 } else { b });
            assert_eq!(m.kind, kind);
            assert!(!m.kind.name().is_empty());
            assert_eq!(m.note_on_zero(), base == 0x90 && b == 0);
        }
    }
    for status in 0..=255u8 {
        let m = Message::decode(packet(status, 0, 0));
        assert_eq!(
            m.is_some(),
            (0x80..=0xef).contains(&status)
                || [0xf8, 0xfa, 0xfb, 0xfc, 0xfe, 0xff].contains(&status),
            "{status:x}"
        );
    }
    for (status, kind) in [
        (0xf8, Kind::Clock),
        (0xfa, Kind::Start),
        (0xfb, Kind::Continue),
        (0xfc, Kind::Stop),
        (0xfe, Kind::ActiveSense),
        (0xff, Kind::Reset),
    ] {
        let m = Message::decode(packet(status, 255, 255)).unwrap();
        assert_eq!(m.kind, kind);
        assert_eq!(m.channel(), None);
        assert_eq!((m.data1, m.data2), (0, 0));
    }
    for status in 0x80..=0xefu8 {
        assert!(Message::decode(packet(status, 128, 0)).is_none());
        if status & 0xf0 != 0xc0 && status & 0xf0 != 0xd0 {
            assert!(Message::decode(packet(status, 0, 128)).is_none());
        }
    }
    assert_eq!(
        Message::decode(packet(0xe0, 0, 0)).unwrap().bend(),
        Some(-8192)
    );
    assert_eq!(
        Message::decode(packet(0xe0, 0, 64)).unwrap().bend(),
        Some(0)
    );
    assert_eq!(
        Message::decode(packet(0xe0, 127, 127)).unwrap().bend(),
        Some(8191)
    );
    assert_eq!(Message::decode(packet(0x90, 60, 1)).unwrap().bend(), None);
}
#[test]
fn clock_wrap_regressions_and_late_previous_epoch_keep_raw_time_and_arrival_order() {
    let c = Capture::new(16).unwrap();
    let stamps = [u32::MAX - 2, u32::MAX, 1, u32::MAX - 1, 0, 2];
    let expected = [WRAP - 3, WRAP - 1, WRAP + 1, WRAP - 2, WRAP, WRAP + 2];
    for (i, raw) in stamps.into_iter().enumerate() {
        assert_eq!(
            c.ingest(packet(0x90, 60, 1), raw, i as u64, false),
            Admission::Accepted
        );
    }
    let batch = c.drain(16).unwrap();
    assert_eq!(batch.events.len(), 6);
    for (i, e) in batch.events.iter().enumerate() {
        assert_eq!(e.sequence, i as u64 + 1);
        assert_eq!(e.driver_ms, stamps[i]);
        assert_eq!(e.timestamp_ms, expected[i]);
        assert_eq!(e.received_ns, i as u64);
        assert_eq!(e.out_of_order, i == 3 || i == 4);
    }
    assert_eq!(batch.stats.clock_regressions, 2);
    let mut clock = Clock::default();
    assert_eq!(clock.extend(100), Some((100, false)));
    assert_eq!(clock.extend(90), Some((90, true)));
    assert_eq!(clock.extend(101), Some((101, false)));
    clock.high = Some(u64::MAX - 1);
    assert!(clock.extend(0).is_none());
}
#[test]
fn integer_token_bucket_has_exact_fractional_refill_burst_cap_and_no_backwards_credit() {
    let mut r = Rate::default();
    for _ in 0..BURST {
        assert!(r.admit(0));
    }
    assert!(!r.admit(0));
    assert!(!r.admit(30517));
    assert!(r.admit(30518));
    assert!(!r.admit(30517));
    let mut r = Rate::default();
    for _ in 0..BURST {
        assert!(r.admit(0));
    }
    // Independent exact count over one second in nanosecond quanta.
    let mut accepted = 0;
    for now in (1000..=1_000_000_000).step_by(1000) {
        accepted += u64::from(r.admit(now));
    }
    assert_eq!(accepted, RATE_PER_SECOND);
    for _ in 0..BURST {
        assert!(r.admit(u64::MAX));
    }
    assert!(!r.admit(u64::MAX));
}
#[test]
fn queue_drop_newest_sequence_gaps_loss_counts_and_bounded_drain_are_explicit() {
    let c = Capture::new(2).unwrap();
    let word = packet(0x90, 60, 1);
    assert_eq!(c.ingest(word, 0, 0, false), Admission::Accepted);
    assert_eq!(c.ingest(word, 1, 1, true), Admission::Accepted);
    assert_eq!(c.ingest(word, 2, 2, false), Admission::QueueFull);
    assert_eq!(c.ingest(packet(0xf0, 0, 0), 3, 3, true), Admission::Invalid);
    let a = c.drain(1).unwrap();
    assert_eq!(a.events[0].sequence, 1);
    assert_eq!(a.stats.queued, 1);
    assert_eq!(a.stats.queue_dropped, 1);
    assert_eq!(a.stats.driver_overruns, 2);
    assert_eq!(a.stats.invalid, 1);
    let b = c.drain(1).unwrap();
    assert_eq!(b.events[0].sequence, 2);
    assert!(b.events[0].driver_overrun);
    assert_eq!(c.ingest(word, 4, 4, false), Admission::Accepted);
    assert_eq!(c.drain(256).unwrap().events[0].sequence, 4);
    assert_eq!(c.drain(0).err(), Some(Error::BatchSize));
    assert_eq!(c.drain(257).err(), Some(Error::BatchSize));
    let capacity = c.state.lock().unwrap().queue.capacity();
    for i in 0..100 {
        c.ingest(word, 10 + i, 100_000_000 + i as u64, false);
        c.drain(1).unwrap();
    }
    assert_eq!(c.state.lock().unwrap().queue.capacity(), capacity);
}
#[test]
fn rate_rejection_close_counter_exhaustion_and_native_capacity_never_wrap_or_leak_events() {
    assert!(Capture::new(0).is_err());
    assert!(Capture::new(MAX_QUEUE + 1).is_err());
    assert!(Capture::reservation_bytes(0).is_none());
    assert!(
        Capture::reservation_bytes(MAX_QUEUE).unwrap() >= MAX_QUEUE * std::mem::size_of::<Event>()
    );
    let c = Capture::new(MAX_QUEUE).unwrap();
    let word = packet(0x90, 60, 1);
    for _ in 0..BURST {
        assert_eq!(c.ingest(word, 0, 0, false), Admission::Accepted);
    }
    assert_eq!(c.ingest(word, 0, 0, false), Admission::RateLimited);
    let batch = c.drain(256).unwrap();
    assert_eq!(batch.events.len(), 256);
    assert_eq!(batch.stats.rate_dropped, 1);
    c.close(Status::Disconnected);
    c.close(Status::Closed);
    assert_eq!(c.status(), Status::Disconnected);
    assert_eq!(c.ingest(word, 0, 0, false), Admission::Closed);
    let end = c.drain(256).unwrap();
    assert!(end.events.is_empty());
    assert_eq!(end.stats.queued, 0);
    let c = Capture::new(4).unwrap();
    c.state.lock().unwrap().sequence = u64::MAX;
    assert_eq!(c.ingest(word, 0, 0, false), Admission::Closed);
    assert_eq!(c.status(), Status::Exhausted);
    assert!(c.drain(4).unwrap().events.is_empty());
    let c = Capture::new(4).unwrap();
    c.invalid.store(u64::MAX, Ordering::Relaxed);
    c.ingest(0, 0, 0, false);
    assert_eq!(c.status(), Status::Exhausted);
    assert_eq!(c.invalid.load(Ordering::Relaxed), u64::MAX);
}
#[test]
fn producer_try_lock_reports_contention_instead_of_waiting_for_worker() {
    let c = Arc::new(Capture::new(4).unwrap());
    let held = c.state.lock().unwrap();
    let other = Arc::clone(&c);
    let thread = std::thread::spawn(move || other.ingest(packet(0x90, 60, 1), 0, 0, false));
    assert_eq!(thread.join().unwrap(), Admission::Contended);
    drop(held);
    let b = c.drain(4).unwrap();
    assert!(b.events.is_empty());
    assert_eq!(b.stats.contended, 1);
    assert_eq!(b.stats.seen, 0);
}
#[test]
fn deterministic_manufactured_replay_keeps_message_times_sequences_and_drop_policy() {
    let replay = || {
        let c = Capture::new(4).unwrap();
        let mut events = Vec::new();
        for i in 0..100u64 {
            c.ingest(
                packet(0x90 + (i % 16) as u8, 48 + (i % 12) as u8, (i % 127) as u8),
                i as u32,
                i * 10_000_000,
                i == 20,
            );
            if i % 3 == 2 {
                events.extend(c.drain(2).unwrap().events);
            }
        }
        loop {
            let b = c.drain(4).unwrap();
            if b.events.is_empty() {
                return (events, b.stats);
            }
            events.extend(b.events);
        }
    };
    let a = replay();
    let b = replay();
    assert_eq!(a, b);
    assert!(a.1.queue_dropped > 0);
    assert_eq!(
        a.1.accepted + a.1.queue_dropped + a.1.rate_dropped,
        a.1.seen
    );
}
#[test]
fn concurrent_manufactured_producers_and_worker_preserve_fifo_capacity_and_close_fence() {
    let c = Arc::new(Capture::new(128).unwrap());
    let mut threads = Vec::new();
    for ch in 0..4 {
        let c = Arc::clone(&c);
        threads.push(std::thread::spawn(move || {
            for i in 0..10_000 {
                c.ingest(packet(0x90 | ch, 60, 1), i, i as u64 * 1_000_000, false);
            }
        }));
    }
    let mut last = 0;
    let mut delivered = 0;
    loop {
        let b = c.drain(256).unwrap();
        assert!(b.stats.queued <= 128);
        for e in b.events {
            assert!(e.sequence > last);
            last = e.sequence;
            delivered += 1;
        }
        if threads.iter().all(|t| t.is_finished()) {
            break;
        }
        std::thread::yield_now();
    }
    for t in threads {
        t.join().unwrap();
    }
    loop {
        let b = c.drain(256).unwrap();
        if b.events.is_empty() {
            assert_eq!(b.stats.accepted, delivered);
            assert!(b.stats.contended + b.stats.queue_dropped + b.stats.rate_dropped > 0);
            break;
        }
        for e in b.events {
            assert!(e.sequence > last);
            last = e.sequence;
            delivered += 1;
        }
    }
    c.close(Status::Closed);
    assert!(c.drain(256).unwrap().events.is_empty());
}
