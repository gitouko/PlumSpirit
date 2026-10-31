//! Dense tile build cost (v0.9 §028 / §027 benchmark): builds the tiles that
//! cover a whole song and a 4-tile window (plain and outlined) for generated
//! fixtures and prints
//! `label: <ms> ms` lines (median of 5 runs). Release build:
//! `cargo run --release -p plumspirit_engine --example tile_cost`.
//! v0.9 V09-S17: the MIDI Player's readings of the 10 M-note store (played,
//! and with each kind of track correction applied in the fill —
//! `docs/PLAYER.md` P3) against the stored reading.

use plumspirit_engine::model::correction::Correction;
use plumspirit_engine::model::note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
use plumspirit_engine::model::track::GatePercent;
use plumspirit_engine::model::{NoteStore, NoteView};
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::view::tiles::{
    self, Outline, Reading, Scratch, SourcePlace, TileColors, TileOptions,
};
use std::time::Instant;

struct Colors {
    ranked: bool,
}

impl TileColors for Colors {
    fn color(&self, source: usize, n: &NoteView) -> ([u8; 4], bool) {
        let h = (source as u32).wrapping_mul(0x9E37_79B1) ^ n.sub.0.wrapping_mul(0x85EB_CA6B);
        let c = h.to_le_bytes();
        ([c[0], c[1], c[2], 255], n.selected)
    }
    fn rank(&self, source: usize) -> u16 {
        if self.ranked { source as u16 } else { 0 }
    }
}

fn store(rng: &mut Rng, n: usize, span: u64, max_dur: i64, subs: u32) -> NoteStore {
    let mut v: Vec<NoteRecord> = (0..n)
        .map(|_| NoteRecord {
            start_tick: rng.range_i64(0, span as i64) as u64,
            duration_ticks: rng.range_i64(1, max_dur) as u64,
            attrs: NoteAttrs::new(
                rng.range_i64(0, 127) as u8,
                100,
                0,
                64,
                NoteOffKind::NoteOff,
            )
            .unwrap(),
            sub: SubchannelId(rng.range_i64(0, i64::from(subs) - 1) as u32),
            group: 0,
        })
        .collect();
    v.sort_by_key(|r| r.start_tick);
    NoteStore::from_sorted_records(&v, false).unwrap()
}

fn run(label: &str, stores: &[NoteStore], ranked: bool, span: u64) {
    let refs: Vec<&NoteStore> = stores.iter().collect();
    let colors = Colors { ranked };
    // Whole song in ~8 tiles (2,048 px), and a 4-tile window at 16× zoom.
    let whole = tiles::level_for(2048.0 / span as f64);
    let zoomed = whole + 16;
    let outlined = TileOptions {
        outline: Some(Outline::new(255, 75)),
        ..TileOptions::default()
    };
    for (what, level, count, opt) in [
        ("whole song", whole, 9i64, TileOptions::default()),
        ("4-tile window", zoomed, 4, TileOptions::default()),
        ("whole song, outlined", whole, 9, outlined),
        ("4-tile window, outlined", zoomed, 4, outlined),
    ] {
        let mut times = Vec::new();
        let mut sc = Scratch::default();
        let mut hash = 0u64;
        for _ in 0..5 {
            let t = Instant::now();
            for i in 0..count {
                let out = tiles::build_tile(
                    &refs,
                    &colors,
                    level,
                    i + if count == 4 { 20 } else { 0 },
                    opt,
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                hash ^= out.hash();
            }
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{label}, {what} ({count} tiles): {:.3} ms   [{hash:016x}]",
            times[2]
        );
    }
}

/// The Player's tiles (outlined, darkness 90) of `stores` read as `read`.
fn run_read(label: &str, stores: &[&NoteStore], read: Reading, span: u64) {
    let colors = Colors { ranked: false };
    let whole = tiles::level_for(2048.0 / span as f64);
    let opt = TileOptions {
        outline: Some(Outline::new(255, 90)),
        ..TileOptions::default()
    };
    for (what, level, count) in [
        ("whole song", whole, 9i64),
        ("4-tile window", whole + 16, 4),
    ] {
        let mut times = Vec::new();
        let mut sc = Scratch::default();
        let mut hash = 0u64;
        for _ in 0..5 {
            let t = Instant::now();
            for i in 0..count {
                let out = tiles::build_tile_read(
                    stores,
                    &vec![SourcePlace::IDENTITY; stores.len()],
                    &vec![read; stores.len()],
                    &colors,
                    level,
                    i + if count == 4 { 20 } else { 0 },
                    opt,
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                hash ^= out.hash();
            }
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "Player {label}, {what} ({count} tiles): {:.3} ms   [{hash:016x}]",
            times[2]
        );
    }
}

fn main() {
    let mut rng = Rng::new(0x711E);
    let span = 2_000_000;
    let big = store(&mut rng, 10_000_000, span, 50, 16);
    run(
        "10M notes, 1 store, 16 subchannels",
        std::slice::from_ref(&big),
        false,
        span,
    );
    let c = |from_tick: u64, ticks: i64, keys: i16, gate: u32| Correction {
        from_tick,
        ticks,
        keys,
        gate: GatePercent::from_hundredths(gate).unwrap(),
        range: (0, 127),
        top: 127,
    };
    for (label, read) in [
        ("stored", Reading::Stored),
        ("played", Reading::Played(None)),
        (
            "tick +120 from mid-song",
            Reading::Played(Some(c(span / 2, 120, 0, 10_000))),
        ),
        ("keys +12", Reading::Played(Some(c(0, 0, 12, 10_000)))),
        ("gate 250 %", Reading::Played(Some(c(0, 0, 0, 25_000)))),
        (
            "tick -50,000 (clamps)",
            Reading::Played(Some(c(0, -50_000, 0, 10_000))),
        ),
        (
            "tick -700 from mid-song, keys -5, gate 50 %",
            Reading::Played(Some(c(span / 2, -700, -5, 5_000))),
        ),
    ] {
        run_read(&format!("10M notes, {label}"), &[&big], read, span);
    }
    drop(big);
    // v1.0-beta1 B1-0141: the Player across many tracks (PFA order across
    // the sources): 128 tracks x 78,125 notes = 10M.
    let tracks: Vec<NoteStore> = (0..128)
        .map(|_| store(&mut rng, 78_125, span, 50, 1))
        .collect();
    let refs: Vec<&NoteStore> = tracks.iter().collect();
    run_read(
        "10M notes in 128 tracks, played",
        &refs,
        Reading::Played(None),
        span,
    );
    // v1.0-beta1 B1-0111: the keyboard's lit keys, once per frame.
    let reads = vec![Reading::Played(None); refs.len()];
    let places = vec![SourcePlace::IDENTITY; refs.len()];
    let masks: Vec<[u64; 4]> = refs
        .iter()
        .zip(&reads)
        .map(|(s, r)| tiles::lit_mask(s, *r))
        .collect();
    let mut out = Vec::new();
    let t = Instant::now();
    let mut lit = 0usize;
    for k in 0..1000u64 {
        tiles::lit_keys(
            &refs,
            &places,
            &reads,
            &masks,
            k * (span / 1000),
            128,
            &mut out,
        );
        lit += out.iter().filter(|o| o.is_some()).count();
    }
    println!(
        "Player 10M notes in 128 tracks, lit keys per frame: {:.3} ms ({lit} lit over 1000 frames)",
        t.elapsed().as_secs_f64()
    );
    drop(tracks);
    let onion: Vec<NoteStore> = (0..1000)
        .map(|_| store(&mut rng, 1000, span, 200, 16))
        .collect();
    run("Onion 1000 stores x 1000 notes, ranked", &onion, true, span);
    let many: Vec<NoteStore> = (0..16)
        .map(|_| store(&mut rng, 100_000, span, 400, 64))
        .collect();
    run(
        "16 stores x 100k notes x 64 subchannels, ranked",
        &many,
        true,
        span,
    );
    // Outline stress (v0.9 §265): 64 layers × 16 subchannels on 4 pitches.
    let stress: Vec<NoteStore> = (0..64)
        .map(|_| {
            let mut v: Vec<NoteRecord> = (0..50_000u64)
                .map(|k| NoteRecord {
                    start_tick: k * (span / 50_000) + rng.range_i64(0, 3) as u64,
                    duration_ticks: rng.range_i64(1, 60) as u64,
                    attrs: NoteAttrs::new(
                        60 + rng.range_i64(0, 3) as u8,
                        100,
                        0,
                        64,
                        NoteOffKind::NoteOff,
                    )
                    .unwrap(),
                    sub: SubchannelId(rng.range_i64(0, 15) as u32),
                    group: 0,
                })
                .collect();
            v.sort_by_key(|r| r.start_tick);
            NoteStore::from_sorted_records(&v, false).unwrap()
        })
        .collect();
    run(
        "Outline stress 64 layers x 50k notes on 4 pitches, ranked",
        &stress,
        true,
        span,
    );
    run(
        "200k long notes (up to the whole song)",
        &[store(&mut rng, 200_000, span, span as i64, 16)],
        false,
        span,
    );
}
