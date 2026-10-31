//! v1.0-Launch L-B01 (contract §004 / §027 b–c): the external event-delivery
//! microanalyzer. Deterministic, no audio device and no GUI.
//!
//! It follows a project through the real pipeline — the [`Scheduler`] in
//! 64-frame blocks → [`ExternalSink`] ring stamps → [`ExternalRoute`] queue →
//! sends to a recording port — with a **simulated device**: callbacks every
//! `period` frames, each publishing a [`ClockAnchor`] (as the real callback
//! does), and a simulated wall clock. Sends are timed under two release rules:
//!
//! * **callback** — beta2: a message is sent when a callback has taken the audio
//!   it belongs to (`dispatch(read position after the callback)`, at the
//!   callback's time, plus the dispatcher's 1 ms poll);
//! * **clock** — v1.0-Launch: a message is sent when the interpolated device
//!   clock reaches its position ([`ClockAnchor::due_at`]).
//!
//! and two stamping rules: **block** (beta2: every message of a block carries
//! the block's position) and **offset** (v1.0-Launch: the block's position plus
//! the message's frame in the block). The four combinations are the ablation.
//!
//! The report compares each Note On's send time with its intended time (tempo
//! map), after removing the pipeline's constant latency (the median offset):
//! error percentiles, inter-onset intervals of a repeated key (the "spam tone")
//! and the dominant repetition rate, plus every loss counter (scheduler
//! skipped / late-dropped / deferred, queue drops).

use super::midi_out::{ClockAnchor, ExternalRoute, ExternalSink, MidiPort, OutputTarget};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A port that records (simulated time in ns, message).
#[derive(Default)]
struct TimedPort {
    now: AtomicU64,
    sent: Mutex<Vec<(u64, u32)>>,
}

impl MidiPort for TimedPort {
    fn short(&self, msg: u32) -> bool {
        self.sent
            .lock()
            .unwrap()
            .push((self.now.load(Ordering::Relaxed), msg));
        true
    }
    fn sysex(&self, _: &[u8]) -> bool {
        true
    }
}

/// The beta2 sink: the same queue, but every message at its block's start.
struct BlockStamped(ExternalSink);

impl SynthSink for BlockStamped {
    fn note_on(&mut self, c: u8, k: u8, v: u8) {
        self.0.note_on(c, k, v);
    }
    fn note_off(&mut self, c: u8, k: u8) {
        self.0.note_off(c, k);
    }
    fn channel_message(&mut self, s: u8, a: u8, b: u8) {
        self.0.channel_message(s, a, b);
    }
    fn silence(&mut self) {
        self.0.silence();
    }
    fn reset(&mut self) {
        self.0.reset();
    }
}

/// A sink that drops everything (the internal synthesizer's route 0).
struct Nothing;

impl SynthSink for Nothing {
    fn note_on(&mut self, _: u8, _: u8, _: u8) {}
    fn note_off(&mut self, _: u8, _: u8) {}
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Callback,
    Clock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamping {
    Block,
    Offset,
}

/// Simulation parameters.
#[derive(Clone, Copy, Debug)]
pub struct Sim {
    pub sample_rate: u32,
    /// Device period (frames per callback).
    pub period: u32,
    pub release: Release,
    pub stamping: Stamping,
}

/// What the analyzer found for one configuration.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub intended_note_ons: usize,
    pub sent_note_ons: usize,
    pub scheduler_skipped: u64,
    pub scheduler_late_dropped: u64,
    pub scheduler_deferred: u64,
    pub queue_dropped: u64,
    /// Constant latency removed (ns, median of send − intended).
    pub latency_ns: i64,
    /// |error| percentiles after removing the latency (µs): p50, p95, p99, max.
    pub error_us: [f64; 4],
    /// Inter-onset intervals of the probe key (µs): intended median, sent
    /// median, and the share of sent intervals within 0.25 ms of intended.
    pub probe_ioi_intended_us: f64,
    pub probe_ioi_sent_us: f64,
    pub probe_ioi_exact_share: f64,
    /// The repetition rate the probe key's sends show (Hz), from the
    /// strongest bin of their onset spectrum between 40 and 400 Hz.
    pub probe_rate_hz: f64,
}

impl Report {
    pub fn json(&self, name: &str) -> String {
        format!(
            "{{\"config\":\"{name}\",\"intended_note_ons\":{},\"sent_note_ons\":{},\"scheduler_skipped\":{},\"scheduler_late_dropped\":{},\"scheduler_deferred\":{},\"queue_dropped\":{},\"latency_ms\":{:.3},\"error_us\":{{\"p50\":{:.1},\"p95\":{:.1},\"p99\":{:.1},\"max\":{:.1}}},\"probe_ioi_intended_us\":{:.1},\"probe_ioi_sent_us\":{:.1},\"probe_ioi_exact_share\":{:.3},\"probe_rate_hz\":{:.1}}}",
            self.intended_note_ons,
            self.sent_note_ons,
            self.scheduler_skipped,
            self.scheduler_late_dropped,
            self.scheduler_deferred,
            self.queue_dropped,
            self.latency_ns as f64 / 1e6,
            self.error_us[0],
            self.error_us[1],
            self.error_us[2],
            self.error_us[3],
            self.probe_ioi_intended_us,
            self.probe_ioi_sent_us,
            self.probe_ioi_exact_share,
            self.probe_rate_hz
        )
    }
}

/// The probe: a key repeated every `period_ticks` (the video's line: 145 BPM,
/// 1,920 PPQN, every 40 ticks = 8.62 ms = 116 Hz) for `seconds`, over a dense
/// background of chords on other keys (`chord` notes every `chord_ticks`).
pub fn spam_document(
    bpm: u64,
    ppqn: u32,
    period_ticks: u64,
    seconds: u64,
    chord: u8,
    chord_ticks: u64,
) -> (PlumDocument, u8) {
    const PROBE: u8 = 46; // A#2
    let mut d = PlumDocument::new_project("spam".into(), Bpm::from_integer(bpm).unwrap(), ppqn);
    let ticks = seconds * bpm * u64::from(ppqn) / 60;
    let note = |s: u64, dur: u64, p: u8, ch: u8| NoteRecord {
        start_tick: s,
        duration_ticks: dur,
        attrs: NoteAttrs::new(p, 100, ch, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    };
    let mut v: Vec<NoteRecord> = (0..ticks / period_ticks)
        .map(|i| note(i * period_ticks, period_ticks / 2, PROBE, 0))
        .collect();
    let mut t = 0;
    let mut k = 0u64;
    while t < ticks {
        for c in 0..u64::from(chord) {
            let key = 60 + ((k * 7 + c * 5) % 48) as u8;
            v.push(note(t, chord_ticks.max(2) - 1, key, 1 + (c % 15) as u8));
        }
        t += chord_ticks;
        k += 1;
    }
    v.sort_by_key(|n| (n.start_tick, n.attrs.pitch()));
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, false)
        .unwrap();
    (d, PROBE)
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// The strongest repetition rate (Hz) in 40–400 Hz of onset times (ns):
/// the magnitude of Σ exp(−2πi f t) over the onsets, 0.5 Hz steps.
fn dominant_rate(onsets: &[u64]) -> f64 {
    let ts: Vec<f64> = onsets.iter().map(|&t| t as f64 / 1e9).collect();
    let mut best = (0.0, 0.0);
    let mut f = 40.0;
    while f <= 400.0 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for t in &ts {
            let a = 2.0 * std::f64::consts::PI * f * t;
            re += a.cos();
            im += a.sin();
        }
        let m = re.hypot(im);
        if m > best.1 {
            best = (f, m);
        }
        f += 0.5;
    }
    best.0
}

/// Runs `doc` through the pipeline under `sim` and reports on Note Ons
/// (intended start times from the tempo map).
pub fn analyze(doc: &PlumDocument, probe: u8, sim: Sim, limits: SchedulerLimits) -> Report {
    let sr = sim.sample_rate;
    let idx = Arc::new(PlaybackIndex::new(
        doc,
        {
            let mut setup = plumspirit_engine::playback::PlaybackSetup::for_document(doc);
            setup.routes = vec![1; doc.track_count()];
            setup.route_count = 2;
            setup.control_indexes(doc)
        },
        Arc::new(plumspirit_engine::model::TempoMap::for_document(doc, sr)),
        &{
            let mut setup = plumspirit_engine::playback::PlaybackSetup::for_document(doc);
            setup.routes = vec![1; doc.track_count()];
            setup.route_count = 2;
            setup
        },
    ));
    let tempo = Arc::clone(&idx.tempo);
    let port = Arc::new(TimedPort::default());
    let route = ExternalRoute::for_target(OutputTarget::Kdmapi, port.clone());
    let mut ext = ExternalSink::new(Arc::clone(&route));
    let mut sched = Scheduler::new(idx, limits);
    let rate = 2 * u64::from(sr);
    let ns_of_frames = |f: u64| (u128::from(f) * 1_000_000_000 / u128::from(sr)) as u64;
    // Intended Note-On times (ns) per key, in order.
    let mut intended: Vec<(u64, u8, u8)> = Vec::new(); // (ns, channel, key)
    for t in doc.tracks() {
        for v in t.notes.iter() {
            let r = v.record();
            intended.push((
                ns_of_frames(tempo.frame_at_tick(r.start_tick)),
                r.attrs.channel(),
                r.attrs.pitch(),
            ));
        }
    }
    intended.sort_unstable();
    let end_frame = tempo.frame_at_tick(sched.index().end_tick) + u64::from(sr) / 10;
    // The worker renders ahead of the device by two periods (as in real time).
    let period = u64::from(sim.period);
    let ahead = 2 * period;
    let mut written = 0u64; // frames rendered (ring position / 2)
    let mut device = 0u64; // frames taken by the device
    {
        let mut nothing = Nothing;
        ext.set_block(0);
        sched.seek_multi(0, &mut [&mut nothing, &mut ext]);
    }
    let mut block_sink = BlockStamped(ExternalSink::new(Arc::clone(&route)));
    let mut cb = 0u64;
    while device < end_frame {
        // Render until two periods ahead of the device.
        while written < device + ahead {
            let stamp = 2 * written;
            let mut nothing = Nothing;
            match sim.stamping {
                Stamping::Offset => {
                    ext.set_block(stamp);
                    sched.advance_multi(64, &mut [&mut nothing, &mut ext]);
                }
                Stamping::Block => {
                    block_sink.0.set_block(stamp);
                    sched.advance_multi(64, &mut [&mut nothing, &mut block_sink]);
                }
            }
            written += 64;
        }
        // The device callback at its time takes one period.
        let t_cb = ns_of_frames(cb * period);
        let anchor = ClockAnchor {
            read: 2 * device,
            took: 2 * period,
            nanos: t_cb,
        };
        device += period;
        cb += 1;
        let t_next = ns_of_frames(cb * period);
        match sim.release {
            Release::Callback => {
                // beta2: everything the callback took, at the dispatcher's
                // next 1 ms poll after the callback.
                let poll = t_cb.div_ceil(1_000_000) * 1_000_000;
                port.now.store(poll, Ordering::Relaxed);
                route.dispatch(2 * device);
            }
            Release::Clock => {
                // The dispatcher sends each message at its due time (what is
                // already due goes at the callback's time).
                port.now.store(t_cb, Ordering::Relaxed);
                let mut released = anchor.read;
                loop {
                    let next = route.dispatch(released);
                    let Some(at) = next else { break };
                    let Some(due) = anchor.due_at(at, rate) else {
                        break;
                    };
                    if due >= t_next {
                        break;
                    }
                    port.now.store(due, Ordering::Relaxed);
                    released = released.max(anchor.position_at(due, rate));
                }
            }
        }
    }
    // What was sent.
    let sent = port.sent.lock().unwrap().clone();
    let sent_ons: Vec<(u64, u8, u8)> = sent
        .iter()
        .filter(|(_, m)| m & 0xF0 == 0x90 && (m >> 16) & 0x7F > 0)
        .map(|&(t, m)| (t, (m & 0x0F) as u8, ((m >> 8) & 0x7F) as u8))
        .collect();
    // Pair intended and sent Note Ons per (channel, key) in order.
    let mut by_key: std::collections::HashMap<(u8, u8), Vec<u64>> = Default::default();
    for &(t, c, k) in &sent_ons {
        by_key.entry((c, k)).or_default().push(t);
    }
    let mut want: std::collections::HashMap<(u8, u8), Vec<u64>> = Default::default();
    for &(t, c, k) in &intended {
        want.entry((c, k)).or_default().push(t);
    }
    let mut diffs: Vec<i64> = Vec::new();
    for (key, w) in &want {
        if let Some(s) = by_key.get(key) {
            for (a, b) in w.iter().zip(s) {
                diffs.push(*b as i64 - *a as i64);
            }
        }
    }
    if std::env::var_os("PLUMSPIRIT_DELIVERY_DEBUG").is_some() && sim.release == Release::Clock {
        for (key, w) in &want {
            if let Some(s) = by_key.get(key) {
                for (a, b) in w.iter().zip(s) {
                    if b != a {
                        eprintln!(
                            "{key:?} intended {a} sent {b} diff {}",
                            *b as i64 - *a as i64
                        );
                    }
                }
            }
        }
    }
    let mut sorted = diffs.clone();
    sorted.sort_unstable();
    let latency = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    let mut errs: Vec<f64> = diffs
        .iter()
        .map(|d| (d - latency).unsigned_abs() as f64 / 1e3)
        .collect();
    errs.sort_by(f64::total_cmp);
    let probe_sent = by_key.get(&(0, probe)).cloned().unwrap_or_default();
    let probe_want = want.get(&(0, probe)).cloned().unwrap_or_default();
    let ioi = |v: &[u64]| -> Vec<f64> {
        v.windows(2)
            .map(|w| (w[1] as f64 - w[0] as f64) / 1e3)
            .collect()
    };
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        percentile(&v, 0.5)
    };
    let (iw, is) = (ioi(&probe_want), ioi(&probe_sent));
    let exact = iw
        .iter()
        .zip(&is)
        .filter(|(a, b)| (*a - *b).abs() <= 250.0)
        .count();
    let st = sched.stats();
    Report {
        intended_note_ons: intended.len(),
        sent_note_ons: sent_ons.len(),
        scheduler_skipped: st.note_ons_skipped,
        scheduler_late_dropped: st.late_dropped,
        scheduler_deferred: st.note_ons_deferred,
        queue_dropped: route.dropped.load(Ordering::Relaxed),
        latency_ns: latency,
        error_us: [
            percentile(&errs, 0.5),
            percentile(&errs, 0.95),
            percentile(&errs, 0.99),
            errs.last().copied().unwrap_or(0.0),
        ],
        probe_ioi_intended_us: median(iw.clone()),
        probe_ioi_sent_us: median(is.clone()),
        probe_ioi_exact_share: if iw.is_empty() {
            0.0
        } else {
            exact as f64 / iw.len() as f64
        },
        probe_rate_hz: dominant_rate(&probe_sent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(release: Release, stamping: Stamping) -> Sim {
        Sim {
            sample_rate: 48_000,
            period: 480, // WASAPI shared mode: 10 ms
            release,
            stamping,
        }
    }

    /// L-B01 / L-B02: the video's line — a key every 40 ticks at 145 BPM and
    /// 1,920 PPQN (116 Hz) over chords — through the four combinations.
    /// beta2 (callback release, block stamps) turns it into 10 ms bursts (the
    /// 100 Hz the video's audio shows) and loses the 116 Hz repetition; the
    /// launch rule (clock release, offsets) keeps every interval and the
    /// 116 Hz rate. Nothing is dropped in any configuration (same scheduler
    /// limits), so the difference is timing alone.
    #[test]
    fn the_spam_tone_survives_clock_release_and_is_lost_in_device_period_bursts() {
        let (doc, probe) = spam_document(145, 1920, 40, 4, 8, 40);
        let limits = SchedulerLimits::default();
        let beta2 = analyze(&doc, probe, sim(Release::Callback, Stamping::Block), limits);
        let launch = analyze(&doc, probe, sim(Release::Clock, Stamping::Offset), limits);
        for r in [&beta2, &launch] {
            assert_eq!(r.sent_note_ons, r.intended_note_ons, "{r:?}");
            assert_eq!((r.scheduler_skipped, r.queue_dropped), (0, 0), "{r:?}");
        }
        // Intended: 40 ticks at 145 BPM, 1,920 PPQN = 8,620.7 µs (116.0 Hz),
        // on whole frames of 20.8 µs at 48 kHz.
        assert!((launch.probe_ioi_intended_us - 8_620.7).abs() < 21.0);
        assert_eq!(launch.probe_ioi_sent_us, launch.probe_ioi_intended_us);
        assert!(launch.probe_ioi_exact_share > 0.99, "{launch:?}");
        assert!((launch.probe_rate_hz - 116.0).abs() <= 1.0, "{launch:?}");
        assert!(launch.error_us[3] < 1.0, "max error {launch:?}");
        // beta2: sends land on the 10 ms grid — intervals of 0 or 10 ms.
        assert!(beta2.probe_ioi_exact_share < 0.2, "{beta2:?}");
        assert!(
            (beta2.probe_ioi_sent_us - 10_000.0).abs() < 50.0,
            "{beta2:?}"
        );
        assert!(beta2.error_us[1] > 3_000.0, "{beta2:?}");
    }

    /// The four combinations, printed as JSON (the L-B01 report; evidence).
    #[test]
    #[ignore]
    fn delivery_report() {
        let (doc, probe) = spam_document(145, 1920, 40, 6, 8, 40);
        let limits = SchedulerLimits::default();
        for (name, r, s) in [
            (
                "beta2: callback release, block stamps",
                Release::Callback,
                Stamping::Block,
            ),
            (
                "ablation: callback release, offsets",
                Release::Callback,
                Stamping::Offset,
            ),
            (
                "ablation: clock release, block stamps",
                Release::Clock,
                Stamping::Block,
            ),
            (
                "launch: clock release, offsets",
                Release::Clock,
                Stamping::Offset,
            ),
        ] {
            let rep = analyze(&doc, probe, sim(r, s), limits);
            println!("{}", rep.json(name));
        }
        // A denser burst: 2,048-note chords every 4 ticks (budget pressure).
        let (doc, probe) = spam_document(145, 1920, 40, 2, 255, 4);
        for (name, lim) in [
            (
                "launch, realtime limits, 255-note chords every 4 ticks",
                limits,
            ),
            (
                "launch, unlimited (Pre-Render limits), 255-note chords every 4 ticks",
                SchedulerLimits {
                    max_note_ons_per_block: usize::MAX,
                    max_lateness_frames: u64::MAX,
                    ..limits
                },
            ),
        ] {
            let rep = analyze(&doc, probe, sim(Release::Clock, Stamping::Offset), lim);
            println!("{}", rep.json(name));
        }
    }
}
