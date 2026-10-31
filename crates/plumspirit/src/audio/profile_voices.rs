//! v1.0-beta2 (B2-F13, B2-S32): the voice ladder — what the synthesizer costs per
//! block at 256 / 512 / 1,024 / 2,048 / 3,000 / 4,096 voices. It is the baseline the
//! B2-S33 changes are kept or reverted against (same machine, same SoundFont).
//!
//! ```text
//! cargo test --release -p plumspirit --bin PlumSpirit voice_ladder -- --ignored --nocapture --test-threads=1
//! ```
//!
//! (SoundFont: `PLUMSPIRIT_TEST_SF2`, else GeneralUser GS, else TimGM6mb, as
//! [`super::profile::test_soundfont`].) Each voice count runs two deterministic fixtures
//! with Voices set to that count and the other synthesis settings at their defaults:
//!
//! * **held**: notes struck in one block on a sustaining organ (GM 19) and held, so the
//!   voice count stays the same for the whole run. RustySynth starts one voice per
//!   matching preset × instrument region, so the SoundFont's regions say how many voices
//!   each note takes; only keys that take the same number are used, and the bank's
//!   per-shard demand says whether any shard was asked for more than its voices.
//! * **burst**: [`super::profile::bursts`] (as many notes as voices, all starting on
//!   every beat, piano) through the Scheduler: event dispatch, deferral, voice turnover.
//!
//! A block's cost is the worker's audio work for it: events, `SynthBank::render`,
//! `MasterBus::process`. The table gives p50 / p95 / p99 / max, the share of the
//! 64-frame budget used (mean, p99), the blocks over budget (offline, so these are the
//! blocks a device would risk an underrun on, not underruns), process RSS and the output
//! peak / RMS after the master bus.

use super::engine::{BLOCK, BankSink, limits_for};
use super::synth::{MasterBus, SynthBank, SynthConfig};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler};
use rustysynth::SoundFont;
use std::sync::Arc;
use std::time::Instant;

/// The voice counts of the ladder (contract §012).
pub const LADDER: [usize; 6] = [256, 512, 1_024, 2_048, 3_000, 4_096];
/// GM Church Organ: its voices sustain for as long as the key is held.
pub const ORGAN: u8 = 19;
pub const VELOCITY: u8 = 100;
pub const SAMPLE_RATE: u32 = 48_000;
const DRUMS: u8 = 9;

/// The preset RustySynth plays for `program` on a melodic channel (bank 0): its
/// own lookup — the last preset with that bank and patch, else the preset with
/// the lowest bank / patch.
fn preset_for(font: &SoundFont, program: u8) -> Option<&rustysynth::Preset> {
    let presets = font.get_presets();
    presets
        .iter()
        .rev()
        .find(|p| p.get_bank_number() == 0 && p.get_patch_number() == i32::from(program))
        .or_else(|| {
            presets
                .iter()
                .min_by_key(|p| (p.get_bank_number() << 16) | p.get_patch_number())
        })
}

/// Voices RustySynth starts for one note: the preset regions holding the key and
/// velocity, times the instrument regions holding them. `None` when a region has
/// an exclusive class (such a note reuses a voice instead of starting one).
pub fn voices_per_note(font: &SoundFont, program: u8, key: u8, velocity: u8) -> Option<usize> {
    let (k, v) = (i32::from(key), i32::from(velocity));
    let mut n = 0;
    for pr in preset_for(font, program)?.get_regions() {
        if !pr.contains(k, v) {
            continue;
        }
        let instrument = font.get_instruments().get(pr.get_instrument_id())?;
        for ir in instrument.get_regions() {
            if ir.contains(k, v) {
                if ir.get_exclusive_class() != 0 {
                    return None;
                }
                n += 1;
            }
        }
    }
    Some(n)
}

/// Notes of the held fixture.
#[derive(Debug)]
pub struct HeldPlan {
    /// (channel, key), struck in this order.
    pub notes: Vec<(u8, u8)>,
    /// Voices each note starts.
    pub per_note: usize,
}

impl HeldPlan {
    pub fn voices(&self) -> usize {
        self.notes.len() * self.per_note
    }
}

/// Notes on `program` that start `voices` voices (or the most that fit under it):
/// keys whose notes start the most common number of voices, over the 15 melodic
/// channels, key by key; past 15 × keys notes the same keys sound again.
pub fn held_plan(font: &SoundFont, program: u8, voices: usize) -> HeldPlan {
    let counts: Vec<(u8, usize)> = (0..128u8)
        .filter_map(|k| Some((k, voices_per_note(font, program, k, VELOCITY)?)))
        .filter(|&(_, n)| n > 0)
        .collect();
    let mut tally = [0usize; 64];
    for &(_, n) in &counts {
        tally[n.min(63)] += 1;
    }
    // The most common count; ties: the smaller.
    let per_note = (1..64)
        .max_by_key(|&n| (tally[n], usize::MAX - n))
        .unwrap_or(1);
    let keys: Vec<u8> = counts
        .iter()
        .filter(|&&(_, n)| n == per_note)
        .map(|&(k, _)| k)
        .collect();
    let mut notes = Vec::new();
    if !keys.is_empty() {
        'fill: loop {
            for &k in &keys {
                for c in (0..16u8).filter(|&c| c != DRUMS) {
                    if (notes.len() + 1) * per_note > voices {
                        break 'fill;
                    }
                    notes.push((c, k));
                }
            }
        }
    }
    HeldPlan { notes, per_note }
}

/// One row of the ladder.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub fixture: &'static str,
    /// Voices setting (= capacity of the bank).
    pub voices: usize,
    pub shards: usize,
    pub render_threads: usize,
    pub notes: u64,
    /// Held: voices the notes start (SoundFont regions). Burst: 0 (not uniform).
    pub voices_requested: usize,
    /// Held: voices asked of shards beyond their voices (RustySynth steals its
    /// lowest-priority voice for each). Burst: blocks × shards in which a shard's
    /// held notes exceeded its voices (`SynthBank::overflow_blocks`).
    pub over_capacity: u64,
    pub blocks: usize,
    /// Nanoseconds: the strike (held: the block with every note-on).
    pub first_block_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub over_budget: usize,
    pub rss_bytes: Option<u64>,
    pub peak: f32,
    pub rms: f32,
    /// Burst: scheduler counts.
    pub note_ons_deferred: u64,
    pub note_ons_skipped: u64,
    pub max_lateness_frames: u64,
}

/// The 64-frame block at [`SAMPLE_RATE`], in nanoseconds.
pub fn budget_ns() -> f64 {
    BLOCK as f64 / f64::from(SAMPLE_RATE) * 1e9
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

struct Meter {
    peak: f32,
    sum_sq: f64,
    samples: u64,
}

impl Meter {
    fn new() -> Meter {
        Meter {
            peak: 0.0,
            sum_sq: 0.0,
            samples: 0,
        }
    }
    fn add(&mut self, block: &[f32]) {
        for &s in block {
            self.peak = self.peak.max(s.abs());
            self.sum_sq += f64::from(s) * f64::from(s);
        }
        self.samples += block.len() as u64;
    }
    fn rms(&self) -> f32 {
        (self.sum_sq / self.samples.max(1) as f64).sqrt() as f32
    }
}

fn finish(row: &mut Row, mut costs: Vec<u64>, meter: &Meter, rss: Option<u64>) {
    let budget = budget_ns();
    row.blocks = costs.len();
    row.over_budget = costs.iter().filter(|&&c| c as f64 > budget).count();
    row.mean_ns = costs.iter().sum::<u64>() as f64 / costs.len().max(1) as f64;
    costs.sort_unstable();
    row.p50_ns = percentile(&costs, 0.50);
    row.p95_ns = percentile(&costs, 0.95);
    row.p99_ns = percentile(&costs, 0.99);
    row.max_ns = costs.last().copied().unwrap_or(0);
    row.peak = meter.peak;
    row.rms = meter.rms();
    row.rss_bytes = rss;
}

fn max_rss(a: Option<u64>) -> Option<u64> {
    match (a, crate::diagnostics::process_memory_bytes()) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}

/// The held fixture: strike, then `blocks` blocks of the held voices.
pub fn run_held(font: &Arc<SoundFont>, voices: usize, blocks: usize) -> Row {
    run_held_on(font, voices, blocks, |cfg| {
        SynthBank::new(font, SAMPLE_RATE, cfg).expect("bank")
    })
}

/// [`run_held`] on a bank `build` makes from the configuration.
pub fn run_held_on(
    font: &Arc<SoundFont>,
    voices: usize,
    blocks: usize,
    build: impl FnOnce(&SynthConfig) -> SynthBank,
) -> Row {
    let cfg = SynthConfig {
        max_voices: voices,
        ..SynthConfig::default()
    };
    let mut bank = build(&cfg);
    let mut master = MasterBus::new(SAMPLE_RATE, &cfg);
    let plan = held_plan(font, ORGAN, voices);
    for c in (0..16u8).filter(|&c| c != DRUMS) {
        bank.channel_message(0xC0 | c, ORGAN, 0);
    }
    let mut out = [0f32; BLOCK * 2];
    let mut meter = Meter::new();
    let started = Instant::now();
    for &(c, k) in &plan.notes {
        bank.note_on(c, k, VELOCITY);
    }
    bank.render(&mut out);
    master.process(&mut out);
    let first_block_ns = started.elapsed().as_nanos() as u64;
    meter.add(&out);
    let shards = bank.shard_demand().len();
    let per_shard = bank.capacity() / shards;
    let over_capacity = bank
        .shard_demand()
        .iter()
        .map(|&n| (n as usize * plan.per_note).saturating_sub(per_shard) as u64)
        .sum();
    let mut rss = max_rss(None);
    let mut costs = Vec::with_capacity(blocks);
    for b in 0..blocks {
        let t = Instant::now();
        bank.render(&mut out);
        master.process(&mut out);
        costs.push(t.elapsed().as_nanos() as u64);
        meter.add(&out);
        if b % 256 == 0 {
            rss = max_rss(rss);
        }
    }
    let mut row = Row {
        fixture: "held",
        voices: bank.capacity(),
        shards,
        render_threads: bank.render_threads(),
        notes: plan.notes.len() as u64,
        voices_requested: plan.voices(),
        over_capacity,
        first_block_ns,
        ..Row::default()
    };
    finish(&mut row, costs, &meter, max_rss(rss));
    row
}

/// The burst fixture: `voices` notes starting on every beat for `beats` beats
/// (120 BPM), each an eighth note long, through the Scheduler.
pub fn run_burst(font: &Arc<SoundFont>, voices: usize, beats: u64) -> Row {
    run_burst_on(voices, beats, |cfg| {
        SynthBank::new(font, SAMPLE_RATE, cfg).expect("bank")
    })
}

/// [`run_burst`] on a bank `build` makes from the configuration.
pub fn run_burst_on(
    voices: usize,
    beats: u64,
    build: impl FnOnce(&SynthConfig) -> SynthBank,
) -> Row {
    let cfg = SynthConfig {
        max_voices: voices,
        ..SynthConfig::default()
    };
    let mut bank = build(&cfg);
    let mut master = MasterBus::new(SAMPLE_RATE, &cfg);
    let doc = super::profile::bursts(voices, beats, 240);
    let index = Arc::new(PlaybackIndex::build(&doc, SAMPLE_RATE));
    let mut sched = Scheduler::new(index, limits_for(&cfg, SAMPLE_RATE, false));
    sched.seek(0, &mut BankSink(&mut bank));
    // The song, plus a beat of tails.
    let blocks = ((beats + 1) as f64 * 0.5 * f64::from(SAMPLE_RATE) / BLOCK as f64) as usize;
    let mut out = [0f32; BLOCK * 2];
    let mut meter = Meter::new();
    let mut rss = max_rss(None);
    let mut costs = Vec::with_capacity(blocks);
    for b in 0..blocks {
        let t = Instant::now();
        sched.advance(BLOCK as u32, &mut BankSink(&mut bank));
        bank.render(&mut out);
        master.process(&mut out);
        costs.push(t.elapsed().as_nanos() as u64);
        meter.add(&out);
        if b % 256 == 0 {
            rss = max_rss(rss);
        }
    }
    let st = sched.stats();
    let mut row = Row {
        fixture: "burst",
        voices: bank.capacity(),
        shards: bank.shard_demand().len(),
        render_threads: bank.render_threads(),
        notes: st.note_ons,
        over_capacity: bank.overflow_blocks,
        first_block_ns: costs.first().copied().unwrap_or(0),
        note_ons_deferred: st.note_ons_deferred,
        note_ons_skipped: st.note_ons_skipped,
        max_lateness_frames: st.max_lateness,
        ..Row::default()
    };
    finish(&mut row, costs, &meter, max_rss(rss));
    row
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|m| m.trim().to_string())
        })
        .unwrap_or_else(|| "unknown CPU".into())
}

fn db(x: f32) -> f64 {
    20.0 * f64::from(x.max(1e-9)).log10()
}

fn print_row(r: &Row) {
    let b = budget_ns();
    let rss = r
        .rss_bytes
        .map_or("n/a".into(), |x| format!("{:.0}", x as f64 / 1_048_576.0));
    println!(
        "| {} | {} | {} / {} | {} | {} | {} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {:.1} % | {:.1} % | {} / {} | {} | {:+.1} / {:+.1} | {} |",
        r.fixture,
        r.voices,
        r.shards,
        r.render_threads,
        r.notes,
        if r.fixture == "held" {
            r.voices_requested.to_string()
        } else {
            "—".into()
        },
        r.over_capacity,
        r.first_block_ns as f64 / 1e3,
        r.p50_ns as f64 / 1e3,
        r.p95_ns as f64 / 1e3,
        r.p99_ns as f64 / 1e3,
        r.max_ns as f64 / 1e3,
        r.mean_ns / b * 100.0,
        r.p99_ns as f64 / b * 100.0,
        r.over_budget,
        r.blocks,
        rss,
        db(r.peak),
        db(r.rms),
        if r.fixture == "burst" {
            format!(
                "{} deferred (max {:.1} ms late), {} skipped",
                r.note_ons_deferred,
                r.max_lateness_frames as f64 / f64::from(SAMPLE_RATE) * 1e3,
                r.note_ons_skipped
            )
        } else {
            "—".into()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare admission choices on the same overloaded note stream. These
    /// deliberately change sound; this is not a claim of equal voice output.
    #[test]
    #[ignore = "measurement: release, explicit SoundFont, one test thread"]
    fn voice_policy_ab() {
        use crate::audio::synth::VoicePolicy;
        let (path, font) =
            super::super::profile::test_soundfont_with_path().expect("test SoundFont");
        println!(
            "Voice policies: {} / {} / {} CPUs / font {} / {} Hz / {} frames",
            std::env::consts::OS,
            cpu_model(),
            std::thread::available_parallelism().map_or(1, usize::from),
            path.display(),
            SAMPLE_RATE,
            BLOCK
        );
        println!(
            "round,voices,policy,note_requests,at_bound,not_started,released_oldest,onset_us,p50_us,p99_us,peak_dbfs,rms_dbfs"
        );
        for round in 0..3 {
            for voices in LADDER {
                let plan = held_plan(&font, ORGAN, voices);
                for offset in 0..3 {
                    let policy = VoicePolicy::ALL[(offset + round) % 3];
                    let cfg = SynthConfig {
                        max_voices: voices,
                        voice_policy: policy,
                        effects: false,
                        ..SynthConfig::default()
                    };
                    let mut bank = SynthBank::new(&font, SAMPLE_RATE, &cfg).unwrap();
                    for c in (0..16u8).filter(|&c| c != DRUMS) {
                        bank.channel_message(0xC0 | c, ORGAN, 0);
                    }
                    let mut out = [0.0; BLOCK * 2];
                    let mut master = MasterBus::new(SAMPLE_RATE, &cfg);
                    let mut meter = Meter::new();
                    let start = Instant::now();
                    for &(c, k) in plan.notes.iter().cycle().take(voices * 2) {
                        bank.note_on(c, k, VELOCITY);
                    }
                    bank.render(&mut out);
                    master.process(&mut out);
                    let onset = start.elapsed().as_secs_f64() * 1e6;
                    let mut costs = Vec::with_capacity(500);
                    for _ in 0..500 {
                        let start = Instant::now();
                        bank.render(&mut out);
                        master.process(&mut out);
                        costs.push(start.elapsed().as_nanos() as u64);
                        meter.add(&out);
                    }
                    let mut row = Row::default();
                    finish(&mut row, costs, &meter, None);
                    println!(
                        "{round},{voices},{},{},{},{},{},{onset:.1},{:.1},{:.1},{:.2},{:.2}",
                        policy.key(),
                        voices * 2,
                        bank.at_capacity,
                        bank.not_started,
                        bank.released_oldest,
                        row.p50_ns as f64 / 1000.0,
                        row.p99_ns as f64 / 1000.0,
                        db(row.peak),
                        db(row.rms)
                    );
                }
            }
        }
    }

    fn tiny() -> Arc<SoundFont> {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        Arc::new(SoundFont::new(&mut &bytes[..]).unwrap())
    }

    /// The held fixture asks for exactly the voices of each rung, spread so no
    /// shard is asked for more than its voices, and both fixtures render.
    #[test]
    fn the_voice_ladder_fixtures_ask_for_each_rungs_voices_and_render() {
        let font = tiny();
        for voices in LADDER {
            let plan = held_plan(&font, ORGAN, voices);
            assert!(plan.per_note >= 1, "{voices}");
            assert!(plan.voices() <= voices && plan.voices() + plan.per_note > voices);
        }
        for voices in [256, 3_000] {
            let held = run_held(&font, voices, 16);
            assert_eq!(held.voices, voices);
            assert_eq!(
                held.voices_requested, voices,
                "the tiny font: one voice a note"
            );
            assert_eq!(held.over_capacity, 0, "{voices}: no shard over its voices");
            assert_eq!(held.blocks, 16);
            assert!(held.p50_ns <= held.p99_ns && held.p99_ns <= held.max_ns);
            assert!(held.peak > 0.0 && held.rms > 0.0, "{voices}: sounds");
            let burst = run_burst(&font, voices, 2);
            assert_eq!(burst.notes as usize, voices * 2, "{voices}: every note");
            assert_eq!(burst.note_ons_skipped, 0);
        }
    }

    /// B2-S33 part 2: render lanes compared in one process (interleaved
    /// rounds, medians), so the VM's run-to-run drift cannot decide it.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture --test-threads=1"]
    fn render_lanes_ab() {
        let Some(font) = super::super::profile::test_soundfont() else {
            println!("skipped: no SoundFont");
            return;
        };
        let cpus = std::thread::available_parallelism().map_or(1, usize::from);
        const ROUNDS: usize = 5;
        println!(
            "Render lanes A/B (B2-S33): {cpus} CPUs, {}; held organ, 1,500 blocks a run, {ROUNDS} interleaved rounds, medians",
            cpu_model()
        );
        println!(
            "| voices | shards | lanes | p50 µs | p95 µs | p99 µs | mean use | blocks over budget |"
        );
        println!("|---|---|---|---|---|---|---|---|");
        let med = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        for voices in [512usize, 1_024, 2_048, 4_096] {
            let shards = SynthConfig {
                max_voices: voices,
                ..SynthConfig::default()
            }
            .shards()
            .0;
            let lanes: Vec<usize> = (1..=cpus.min(4).min(shards)).collect();
            let mut rows: Vec<Vec<Row>> = vec![Vec::new(); lanes.len()];
            for round in 0..ROUNDS {
                let mut order: Vec<usize> = (0..lanes.len()).collect();
                if round % 2 == 1 {
                    order.reverse();
                }
                for i in order {
                    let l = lanes[i];
                    rows[i].push(run_held_on(&font, voices, 1_500, |cfg| {
                        SynthBank::with_lanes(&font, SAMPLE_RATE, cfg, l).expect("bank")
                    }));
                }
            }
            for (i, &l) in lanes.iter().enumerate() {
                let r = &rows[i];
                let f = |g: fn(&Row) -> f64| med(r.iter().map(g).collect());
                println!(
                    "| {voices} | {shards} | {l}{} | {:.0} | {:.0} | {:.0} | {:.1} % | {:.0} |",
                    if l == crate::audio::synth::render_lanes(shards, cpus) {
                        " (today)"
                    } else {
                        ""
                    },
                    f(|r| r.p50_ns as f64) / 1e3,
                    f(|r| r.p95_ns as f64) / 1e3,
                    f(|r| r.p99_ns as f64) / 1e3,
                    f(|r| r.mean_ns) / budget_ns() * 100.0,
                    f(|r| r.over_budget as f64),
                );
            }
        }
        // The handoff on a silent bank (effects still run on every shard).
        println!("\n| silent bank, voices | shards | lanes | p50 µs | p99 µs | mean use |");
        println!("|---|---|---|---|---|---|");
        for voices in [512usize, 1_024] {
            let cfg = SynthConfig {
                max_voices: voices,
                ..SynthConfig::default()
            };
            let shards = cfg.shards().0;
            for l in 1..=cpus.min(4).min(shards) {
                let mut runs = Vec::new();
                for _ in 0..ROUNDS {
                    let mut bank =
                        SynthBank::with_lanes(&font, SAMPLE_RATE, &cfg, l).expect("bank");
                    let mut master = MasterBus::new(SAMPLE_RATE, &cfg);
                    let mut out = [0f32; BLOCK * 2];
                    let mut costs = Vec::with_capacity(3_000);
                    for _ in 0..3_000 {
                        let t = Instant::now();
                        bank.render(&mut out);
                        master.process(&mut out);
                        costs.push(t.elapsed().as_nanos() as u64);
                    }
                    let mut row = Row::default();
                    finish(&mut row, costs, &Meter::new(), None);
                    runs.push(row);
                }
                let f = |g: fn(&Row) -> f64| med(runs.iter().map(g).collect());
                println!(
                    "| {voices} | {shards} | {l} | {:.1} | {:.1} | {:.2} % |",
                    f(|r| r.p50_ns as f64) / 1e3,
                    f(|r| r.p99_ns as f64) / 1e3,
                    f(|r| r.mean_ns) / budget_ns() * 100.0,
                );
            }
        }
    }

    /// B2-S33 part 3: events applied at once on the worker (before) vs queued
    /// and applied in each group's lane (after), in one process, interleaved.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture --test-threads=1"]
    fn dispatch_ab() {
        let Some(font) = super::super::profile::test_soundfont() else {
            println!("skipped: no SoundFont");
            return;
        };
        const ROUNDS: usize = 8;
        println!(
            "Dispatch A/B (B2-S33): {} CPUs, {}; {ROUNDS} interleaved rounds (order alternating), medians",
            std::thread::available_parallelism().map_or(1, usize::from),
            cpu_model()
        );
        println!(
            "| fixture | voices | dispatch | first block µs | p50 µs | p95 µs | p99 µs | mean use | blocks over budget |"
        );
        println!("|---|---|---|---|---|---|---|---|---|");
        let med = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        for voices in [1_024usize, 2_048, 4_096] {
            for fixture in ["held", "burst"] {
                let mut rows: [Vec<Row>; 2] = [Vec::new(), Vec::new()];
                for round in 0..ROUNDS {
                    // Alternate which variant runs first (the second run of a
                    // round can differ systematically on this VM).
                    let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
                    for i in order {
                        let now = i == 0;
                        let build = |cfg: &SynthConfig| {
                            let mut b = SynthBank::new(&font, SAMPLE_RATE, cfg).expect("bank");
                            b.dispatch_now = now;
                            b
                        };
                        rows[i].push(if fixture == "held" {
                            run_held_on(&font, voices, 750, build)
                        } else {
                            run_burst_on(voices, 8, build)
                        });
                    }
                }
                for (i, name) in ["at once (before)", "in the lanes"].into_iter().enumerate() {
                    let r = &rows[i];
                    let f = |g: fn(&Row) -> f64| med(r.iter().map(g).collect());
                    println!(
                        "| {fixture} | {voices} | {name} | {:.0} | {:.0} | {:.0} | {:.0} | {:.1} % | {:.0} |",
                        f(|r| r.first_block_ns as f64) / 1e3,
                        f(|r| r.p50_ns as f64) / 1e3,
                        f(|r| r.p95_ns as f64) / 1e3,
                        f(|r| r.p99_ns as f64) / 1e3,
                        f(|r| r.mean_ns) / budget_ns() * 100.0,
                        f(|r| r.over_budget as f64),
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture --test-threads=1"]
    fn voice_ladder() {
        let Some((path, font)) = super::super::profile::test_soundfont_with_path() else {
            println!("skipped: no SoundFont");
            return;
        };
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        println!(
            "Voice ladder (B2-S32): {} ({} CPUs available, {}); SoundFont {} ({bytes} bytes, {} presets); {} Hz, {}-frame blocks (budget {:.0} µs); defaults otherwise (effects on, gain −3 dB, limiter on)",
            std::env::consts::OS,
            std::thread::available_parallelism().map_or(1, usize::from),
            cpu_model(),
            path.display(),
            font.get_presets().len(),
            SAMPLE_RATE,
            BLOCK,
            budget_ns() / 1e3,
        );
        let plan = held_plan(&font, ORGAN, 4_096);
        println!(
            "Held fixture: GM {ORGAN} starts {} voice(s) a note on the keys used; burst fixture: piano (GM 0), drums on channel 10",
            plan.per_note
        );
        println!(
            "| fixture | voices | shards / threads | notes | voices requested | over capacity | first block µs | p50 µs | p95 µs | p99 µs | max µs | mean use | p99 use | blocks over budget | RSS MiB | peak / RMS dBFS | events |"
        );
        println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        for voices in LADDER {
            print_row(&run_held(&font, voices, 2_250));
            print_row(&run_burst(&font, voices, 8));
        }
    }
}
