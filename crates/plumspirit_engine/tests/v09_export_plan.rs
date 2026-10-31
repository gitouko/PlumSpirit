//! V09-S16 (contract §053, §247; V09-0039): the canonical export plan
//! (`docs/EXPORT_PLAN.md`).
//!
//! Step 2a — the golden export corpus. Every case below is exported with
//! every option combination and the SHA-256 of the written bytes is compared
//! with `fixtures/v09/export_golden.txt`, recorded with the export path as it
//! was before the plan was refactored (clips through `export_image`, 80586db).
//! The refactor must not change a single byte. Regenerate the file only for a
//! deliberate change of the exported bytes:
//! `PLUMSPIRIT_WRITE_GOLDEN=1 cargo test -p plumspirit_engine --test v09_export_plan`.
//!
//! Deliberate changes so far: V09-S23 step 4 (V09-0102) writes notes of one
//! channel and key that start together shortest first (`midi::overlap`).
//! Four lines changed — `multi_subchannel_v08+clips` without layers, where a
//! linked copy puts two notes of channel 2, key 21 at tick 100,720 longer
//! first: their two Note Ons swap places, nothing else moves (same length,
//! same events per tick).

mod common;

use plumspirit_engine::editor::clips::{
    add_clip, delete_clips, duplicate_clips, move_clips, resize_clip, set_muted,
};
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::{
    ExportOptions, WideKeys, export_to_bytes_with, plan_export_with, validate_exported_bytes,
};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::{GatePercent, TrackProps};
use plumspirit_engine::model::{Bpm, ClipId, ContentRef, NoteOffKind, NoteRecord, PlumDocument};

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4), std only.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn the_test_sha256_matches_the_standard_vectors() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

// ---------------------------------------------------------------------------
// The corpus: `tests/common/corpus.rs`.

use common::corpus::*;

/// What exporting `d` with `o` gives: `<sha256> <length>` of the bytes (each
/// file also validated against the plan, and exported twice to the same
/// bytes), or the refusal.
fn outcome(d: &PlumDocument, o: ExportOptions) -> String {
    let plan = plan_export_with(d, o);
    if let Some(b) = plan.blockers.first() {
        return format!("blocked {}", &sha256(b.as_bytes())[..16]);
    }
    match export_to_bytes_with(d, &plan) {
        Err(e) => format!("error {}", &sha256(e.user_message().as_bytes())[..16]),
        Ok(bytes) => {
            validate_exported_bytes(d, &plan, &bytes)
                .unwrap_or_else(|e| panic!("the written file validates: {e}"));
            assert_eq!(
                export_to_bytes_with(d, &plan).unwrap(),
                bytes,
                "deterministic"
            );
            format!("{} {}", sha256(&bytes), bytes.len())
        }
    }
}

const GOLDEN: &str = "v09/export_golden.txt";

#[test]
fn every_case_exports_exactly_the_recorded_bytes() {
    let mut lines = Vec::new();
    for (name, d) in corpus() {
        for (opts, o) in combos(&d) {
            lines.push(format!("{name}\t{opts}\t{}", outcome(&d, o)));
        }
    }
    let got = lines.join("\n") + "\n";
    let path = format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("PLUMSPIRIT_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = String::from_utf8(fixture(GOLDEN)).unwrap();
    let want: Vec<&str> = want.lines().collect();
    let differs: Vec<String> = lines
        .iter()
        .zip(&want)
        .filter(|(a, b)| a.as_str() != **b)
        .map(|(a, b)| format!("  got  {a}\n  want {b}"))
        .collect();
    assert!(
        differs.is_empty() && lines.len() == want.len(),
        "{} of {} exports differ from {GOLDEN} ({} lines recorded):\n{}",
        differs.len(),
        lines.len(),
        want.len(),
        differs.join("\n")
    );
}

#[test]
fn the_corpus_covers_clips_corrections_rescale_fillers_layers_and_wide_keys() {
    let cases = corpus();
    let get = |n: &str| &cases.iter().find(|(name, _)| name == n).unwrap().1;
    for (name, d) in &cases {
        if name.contains("clips") {
            assert!(!d.clips_are_identity(), "{name}");
        }
    }
    let d = get("clips_corrections");
    assert!(
        plan_export_with(d, ExportOptions::default())
            .corrections
            .iter()
            .flatten()
            .count()
            >= 2
    );
    let d = get("clips_ppqn_96000");
    assert!(
        plan_export_with(d, ExportOptions::default())
            .rescale
            .is_some()
    );
    let bytes = |d: &PlumDocument| {
        export_to_bytes_with(d, &plan_export_with(d, ExportOptions::default())).unwrap()
    };
    // Gap fillers: a Sequencer-Specific "PlumSpirit gap" meta in the file.
    let gap = bytes(get("clips_gap_fillers"));
    assert!(gap.windows(2).any(|w| w == [0xFF, 0x7F]));
    // Layers: more chunks than tracks with split subchannels.
    let d = get("multi_subchannel_v08+clips");
    assert!(plan_export_with(d, ExportOptions::default()).chunk_count(d) > d.track_count());
    // Notes above key 127, written by each policy (corrections off: the
    // fixture's key correction is refused on purpose).
    for name in ["keys256_v09", "keys256_v09+clips"] {
        let d = get(name);
        assert_eq!(combos(d).len(), 16, "{name}");
        for w in [
            WideKeys::Discard,
            WideKeys::Fold,
            WideKeys::Clamp,
            WideKeys::Experimental,
        ] {
            let o = ExportOptions {
                wide_keys: w,
                apply_corrections: false,
                ..ExportOptions::default()
            };
            assert!(plan_export_with(d, o).blockers.is_empty(), "{name} {w:?}");
        }
    }
    // Owned content and a deleted primary.
    let d = get("clips_owned_deleted_primary");
    assert!(!d.clips().contents().is_empty());
    assert!(!d.clips().removed_primaries().is_empty());
}

// ---------------------------------------------------------------------------
// Step 2b: clip tracks are read in place (`ExportPlan::placed`); the copied
// reference `PlumDocument::materialize_placements` must export the same bytes.

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Every placed clip id and primary of `d`.
fn clip_ids(d: &PlumDocument) -> Vec<ClipId> {
    let mut v: Vec<ClipId> = (0..d.track_count())
        .flat_map(|i| d.placements_on(i))
        .map(|p| p.id)
        .collect();
    v.dedup();
    v
}

/// A random clip layout: `steps` random clip edits on [`song`].
/// Gives every track of `d` text events at the ticks of some of its own
/// controller events (just before them), so the order of a track's own
/// events and its placements' channel events at one tick is exercised.
fn with_text(d: &mut PlumDocument, ticks: &[u64]) {
    for i in 0..d.track_count() {
        let t = d.track_mut(i).unwrap();
        let mut out = plumspirit_engine::model::EventStore::new();
        let payload = out.store_payload(format!("text {i}").as_bytes()).unwrap();
        let text = RawEvent {
            tick: 0,
            anchor: 0,
            kind: RawKind::Meta {
                meta_type: plumspirit_engine::model::events::meta::TEXT,
                payload,
            },
        };
        let mut pending: Vec<u64> = ticks.to_vec();
        for e in t.events.events() {
            while pending.first().is_some_and(|&p| p <= e.tick) {
                out.push(RawEvent {
                    tick: pending.remove(0),
                    ..text
                })
                .unwrap();
            }
            assert!(
                matches!(e.kind, RawKind::Channel { .. }),
                "song() has only controllers"
            );
            out.push(*e).unwrap();
        }
        for p in pending {
            out.push(RawEvent { tick: p, ..text }).unwrap();
        }
        t.events = out;
    }
}

fn random_layout(seed: u64, steps: usize) -> PlumDocument {
    let mut r = Rng(seed);
    let mut d = song(480);
    let q = 480u64;
    with_text(&mut d, &[0, q, 3 * q, 40 * q]);
    d.add_track().unwrap();
    for _ in 0..steps {
        let ids = clip_ids(&d);
        if ids.is_empty() {
            let op = add_clip(&d, 0, r.below(20) * q, 4 * q).unwrap();
            apply(&mut d, op);
            continue;
        }
        let id = ids[r.below(ids.len() as u64) as usize];
        let track = r.below(d.track_count() as u64) as usize;
        let at = r.below(64) * q / 2 + r.below(3) * 7;
        let op = match r.below(7) {
            0 => move_clips(&d, &[(id, track, at)]),
            1 => resize_clip(
                &d,
                id,
                at,
                r.below(16) * q / 4,
                Some(1 + r.below(12) * q / 2),
            ),
            2 => duplicate_clips(&d, &[(id, track, at)], true),
            3 => duplicate_clips(&d, &[(id, track, at)], false),
            4 => set_muted(&d, &[id], r.below(3) == 0),
            5 if ids.len() > 2 => delete_clips(&d, &[id]),
            _ => add_clip(&d, track, at, 2 * q + r.below(8) * q),
        };
        if let Ok(op) = op {
            apply(&mut d, op);
        }
        // Give owned contents notes now and then (directly: the export reads
        // whatever the content holds).
        if r.below(3) == 0
            && let Some(c) = d.clips().contents().last().map(|c| c.id)
        {
            let base = r.below(8) * q / 4;
            let recs: Vec<NoteRecord> = (0..6u64)
                .map(|k| note(base + k * q / 3, q / 2, 50 + k as u8 * 3, (k % 2) as u8))
                .collect();
            let store = d.notes_of_mut(ContentRef::Owned(c)).unwrap();
            if store.is_empty() {
                store.insert_sorted_batch(&recs, false).unwrap();
            }
        }
    }
    d
}

#[test]
fn random_clip_layouts_export_the_same_bytes_as_their_materialized_copy() {
    let mut compared = 0;
    for seed in 0..48u64 {
        let mut d = random_layout(seed, 4 + (seed % 9) as usize);
        if seed % 4 == 1 {
            props(
                &mut d,
                (seed % 3) as usize,
                TrackProps {
                    tick_correction: [-37i64, 41, 0][(seed % 3) as usize],
                    correction_from_measure: (seed % 2) as u32,
                    key_correction: [0i16, 5, -3][(seed % 3) as usize],
                    gate: GatePercent::from_hundredths(
                        [10_000, 15_000, 4_000][(seed % 3) as usize],
                    )
                    .unwrap(),
                    key_range: (20, 110),
                    ..TrackProps::default()
                },
            );
        }
        let reference = d.materialize_placements();
        assert!(reference.clips_are_identity());
        for (opts, o) in combos(&d) {
            let got = outcome(&d, o);
            let want = outcome(&reference, o);
            assert_eq!(got, want, "seed {seed} {opts}");
            let a = plan_export_with(&d, o);
            let b = plan_export_with(&reference, o);
            assert_eq!(a.disclosures(), b.disclosures(), "seed {seed} {opts}");
            assert_eq!(a.structure, b.structure, "seed {seed} {opts}");
            if !d.clips_are_identity() {
                compared += 1;
            }
        }
    }
    assert!(compared > 200, "{compared} clip exports compared");
}

#[test]
fn a_plan_made_before_the_clips_changed_is_refused() {
    let d = song(960);
    let plan = plan_export_with(&d, ExportOptions::default());
    let moved = clip_song(960);
    let err = export_to_bytes_with(&moved, &plan).unwrap_err();
    assert!(
        err.user_message().contains("different state"),
        "{}",
        err.user_message()
    );
}

#[test]
fn the_plan_reads_clip_notes_and_events_in_place() {
    let d = clip_song(960);
    let plan = plan_export_with(&d, ExportOptions::default());
    let by_track = d.placed_contents_by_track();
    let mut lanes = 0;
    for (i, placed) in by_track.iter().enumerate() {
        let Some(p) = &plan.placed[i] else {
            continue;
        };
        let unmuted: Vec<_> = placed.iter().filter(|pc| !pc.placement.muted).collect();
        assert_eq!(p.lanes.len(), unmuted.len(), "track {i}");
        for (lane, pc) in p.lanes.iter().zip(unmuted) {
            // The same chunks and the same event list: nothing copied.
            assert!(lane.snap.same_as(&pc.notes.snapshot()), "track {i}");
            assert!(lane.events.same_as(pc.events), "track {i}");
            lanes += 1;
        }
    }
    assert!(lanes >= 4, "{lanes} lanes");
}

#[test]
fn a_document_copy_shares_its_events_until_one_side_is_edited() {
    let d = song(960);
    let mut copy = d.clone();
    assert!(
        copy.track(0)
            .unwrap()
            .events
            .same_as(&d.track(0).unwrap().events)
    );
    let before = d.track(0).unwrap().events.clone();
    copy.track_mut(0)
        .unwrap()
        .events
        .push(RawEvent {
            tick: 1_000_000,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0xB0,
                data: [7, 1],
            },
        })
        .unwrap();
    assert!(
        !copy
            .track(0)
            .unwrap()
            .events
            .same_as(&d.track(0).unwrap().events)
    );
    assert_eq!(
        d.track(0).unwrap().events,
        before,
        "the original is untouched"
    );
    assert_eq!(copy.track(0).unwrap().events.len(), before.len() + 1);
    // Other tracks still share.
    assert!(
        copy.track(1)
            .unwrap()
            .events
            .same_as(&d.track(1).unwrap().events)
    );
}

// ---------------------------------------------------------------------------
// Step 2c: the export snapshot (EXPORT_PLAN.md D3, D5; contract §247).

use plumspirit_engine::midi::ExportSnapshot;
use plumspirit_engine::midi::semantic::semantic_from_bytes_with;

/// (tick, channel, key, velocity) Note Ons and (tick, channel, key,
/// velocity, Note On with velocity 0) terminations of one chunk, sorted.
type NoteSets = (Vec<(u64, u8, u8, u8)>, Vec<(u64, u8, u8, u8, bool)>);

fn note_sets(s: &ExportSnapshot, chunks: usize) -> Vec<NoteSets> {
    let mut v: Vec<NoteSets> = vec![(Vec::new(), Vec::new()); chunks];
    s.for_each_note(|n| {
        let (ons, offs) = &mut v[n.chunk as usize];
        ons.push((n.start, n.channel, n.key, n.velocity));
        offs.push(match n.off {
            NoteOffKind::NoteOff => (n.end, n.channel, n.key, n.off_velocity, false),
            NoteOffKind::NoteOnZero => (n.end, n.channel, n.key, 0, true),
        });
    });
    for (ons, offs) in &mut v {
        ons.sort_unstable();
        offs.sort_unstable();
    }
    v
}

#[test]
fn the_snapshot_is_exactly_what_the_exporter_writes() {
    let mut checked = 0;
    for (name, d) in corpus() {
        for (opts, o) in combos(&d) {
            let s = ExportSnapshot::take(&d, o);
            assert!(s.is_current(&d, o));
            let plan = plan_export_with(&d, o);
            if !plan.blockers.is_empty() {
                assert_eq!(s.blocker(), plan.blockers.first().map(String::as_str));
                assert!(s.to_bytes().is_err(), "{name} {opts}");
                continue;
            }
            let bytes = s.to_bytes().unwrap();
            assert_eq!(
                bytes,
                export_to_bytes_with(&d, &plan).unwrap(),
                "{name} {opts}"
            );
            let file = semantic_from_bytes_with(&bytes, o.wide_keys == WideKeys::Experimental)
                .unwrap_or_else(|e| panic!("{name} {opts}: {e}"));
            // The streams, without bytes, are the file (§247).
            assert_eq!(s.semantic(), file, "{name} {opts}");
            // Every note as the Player and the Inspector read it is a Note On
            // and a termination of the file, chunk by chunk.
            let sets = note_sets(&s, file.tracks.len());
            for (ci, t) in file.tracks.iter().enumerate() {
                assert_eq!(sets[ci].0, t.note_ons, "{name} {opts}: chunk {ci} Note Ons");
                assert_eq!(sets[ci].1, t.note_offs, "{name} {opts}: chunk {ci} ends");
            }
            assert_eq!(s.note_count(), file.total_notes() as u64, "{name} {opts}");
            // Kiva's loader (emulated, V09-S23) reads the whole file and the
            // same number of notes (how it pairs same-key overlaps is
            // V09-0102's subject).
            let k = common::kiva::kiva_load(&bytes);
            assert!(k.clean(), "{name} {opts}: {:?} {:?}", k.error, k.corrupt());
            assert_eq!(k.notes().len() as u64, s.note_count(), "{name} {opts}");
            // An independent reader (V09-0099, §166): the plan's semantic
            // digest is the file's, notes exact wherever Standard MIDI is
            // unambiguous; one Note On per planned note.
            use common::smf_digest::{compare, digest, snapshot_digest};
            let fd = digest(&bytes, o.wide_keys == WideKeys::Experimental)
                .unwrap_or_else(|e| panic!("{name} {opts}: {e}"));
            let sd = snapshot_digest(&s);
            compare(&sd, &fd).unwrap_or_else(|e| panic!("{name} {opts}: {e}"));
            assert_eq!(fd.note_ons() as u64, s.note_count(), "{name} {opts}");
            // V09-0102: the writer's overlap counts are the snapshot's; the
            // tracks PlumSpirit's import pairs differently (the validation)
            // are exactly those with nested notes; a chunk with nested notes
            // is what the digest leaves unpaired.
            let mut cur = std::io::Cursor::new(Vec::new());
            let ws = s.write(&mut cur, Default::default()).unwrap();
            let overlaps = s.same_key_overlaps();
            assert_eq!(ws.same_key, overlaps, "{name} {opts}");
            let nested: Vec<usize> = overlaps
                .iter()
                .filter(|(_, o)| o.nested > 0)
                .map(|&(t, _)| t)
                .collect();
            let v = validate_exported_bytes(&d, &plan, &bytes).unwrap();
            // (A lossy change of resolution can move notes together; the
            // counts are the project's, the validation the file's.)
            if plan.rescale.as_ref().is_none_or(|r| r.exact) {
                assert_eq!(v.tracks_with_different_pairing, nested, "{name} {opts}");
                assert_eq!(
                    sd.tracks.iter().any(|t| !t.nested_keys.is_empty()),
                    !nested.is_empty(),
                    "{name} {opts}"
                );
            }
            // V09-0050: Maximum Compatibility (every status byte) holds the
            // same events, with no running status, and loads in Kiva too.
            let x = ExportSnapshot::take(
                &d,
                ExportOptions {
                    explicit_status: true,
                    ..o
                },
            );
            let xb = x.to_bytes().unwrap();
            let xd = digest(&xb, o.wide_keys == WideKeys::Experimental)
                .unwrap_or_else(|e| panic!("{name} {opts} explicit: {e}"));
            compare(&fd, &xd).unwrap_or_else(|e| panic!("{name} {opts} explicit: {e}"));
            compare(&snapshot_digest(&x), &xd)
                .unwrap_or_else(|e| panic!("{name} {opts} explicit: {e}"));
            assert!(
                xd.tracks.iter().all(|t| t.running_status == 0),
                "{name} {opts}: explicit status"
            );
            assert!(xb.len() >= bytes.len(), "{name} {opts}");
            assert!(
                common::kiva::kiva_load(&xb).clean(),
                "{name} {opts} explicit"
            );
            // The tempo map a player of the file follows.
            let mut tempos: Vec<(u64, u32)> = file
                .tracks
                .iter()
                .flat_map(|t| t.other.iter())
                .filter(|(_, b)| b.len() == 9 && b[..6] == [0xFF, 0x51, 0, 0, 0, 3])
                .map(|(tick, b)| (*tick, u32::from_be_bytes([0, b[6], b[7], b[8]])))
                .collect();
            tempos.sort_by_key(|&(t, _)| t);
            assert_eq!(s.tempo_changes(), tempos, "{name} {opts}");
            checked += 1;
        }
    }
    assert!(checked >= 190, "{checked} snapshots compared");
}

#[test]
fn a_snapshot_is_unchanged_by_later_edits_of_the_document() {
    let mut d = clip_song(960);
    let s = ExportSnapshot::take(&d, ExportOptions::default());
    let before = s.semantic();
    let bytes = s.to_bytes().unwrap();
    // Edit notes, clips and events after the snapshot was taken.
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(5, 100, 99, 3)], false)
        .unwrap();
    let p1 = primary(&d, 1);
    let op = move_clips(&d, &[(p1, 2, 70 * 960)]).unwrap();
    apply(&mut d, op);
    let t2 = d.track(2).unwrap();
    let edit = plumspirit_engine::editor::controllers::prepare_rename(t2, "Renamed").unwrap();
    apply(
        &mut d,
        Operation::Events {
            edit,
            label: "Rename",
        },
    );
    assert!(!s.is_current(&d, ExportOptions::default()));
    assert_eq!(
        s.semantic(),
        before,
        "the snapshot keeps the state it was taken at"
    );
    assert_eq!(s.to_bytes().unwrap(), bytes);
    let now = ExportSnapshot::take(&d, ExportOptions::default());
    assert_ne!(now.semantic(), before);
    // Track 1's content plays twice (its moved primary and the linked copy
    // on track 3), so the new note is written twice.
    assert_eq!(now.note_count(), s.note_count() + 2);
}

/// One kind of document edit.
type Edit = Box<dyn Fn(&mut PlumDocument)>;

#[test]
fn the_export_key_moves_with_every_export_relevant_edit_and_not_with_view_state() {
    let base = || clip_song(960);
    let key = |d: &PlumDocument| d.export_key();
    let mut edits: Vec<(&str, Edit)> = Vec::new();
    edits.push((
        "notes",
        Box::new(|d| {
            d.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&[note(7, 10, 60, 0)], false)
                .unwrap();
        }),
    ));
    edits.push((
        "track events (rename)",
        Box::new(|d| {
            let edit =
                plumspirit_engine::editor::controllers::prepare_rename(d.track(1).unwrap(), "X")
                    .unwrap();
            apply(
                d,
                Operation::Events {
                    edit,
                    label: "Rename",
                },
            );
        }),
    ));
    edits.push((
        "tempo",
        Box::new(|d| {
            let edit = d
                .conductor()
                .prepare_set_start_tempo(plumspirit_engine::model::TempoValue::Bpm(
                    Bpm::parse("133.5").unwrap(),
                ))
                .unwrap();
            apply(
                d,
                Operation::Conductor {
                    edit,
                    label: "Tempo",
                },
            );
        }),
    ));
    edits.push((
        "track properties",
        Box::new(|d| {
            props(
                d,
                1,
                TrackProps {
                    key_correction: 2,
                    ..TrackProps::default()
                },
            )
        }),
    ));
    edits.push((
        "explicit end",
        Box::new(|d| {
            let track = d.track(2).unwrap().id();
            apply(
                d,
                Operation::TrackProps {
                    track,
                    props: d.track(2).unwrap().props().clone(),
                    end: Some(900_000),
                    label: "End",
                },
            );
        }),
    ));
    edits.push((
        "clip move",
        Box::new(|d| {
            let p = primary(d, 2);
            let op = move_clips(d, &[(p, 2, 5 * 960)]).unwrap();
            apply(d, op);
        }),
    ));
    edits.push((
        "clip mute",
        Box::new(|d| {
            let p = primary(d, 2);
            let op = set_muted(d, &[p], true).unwrap();
            apply(d, op);
        }),
    ));
    edits.push((
        "subchannels",
        Box::new(|d| {
            apply(
                d,
                Operation::AddSubchannels {
                    track: 0,
                    subs: vec![plumspirit_engine::model::track::Subchannel::new(
                        plumspirit_engine::model::SubchannelId(5),
                        4,
                    )],
                },
            );
        }),
    ));
    edits.push((
        "add track",
        Box::new(|d| {
            d.add_track().unwrap();
        }),
    ));
    edits.push((
        "key range",
        Box::new(|d| {
            apply(
                d,
                Operation::SetKeyRange {
                    keys: KeyRange::Extended256,
                },
            );
        }),
    ));
    for (what, edit) in &edits {
        let mut d = base();
        let k0 = key(&d);
        edit(&mut d);
        assert_ne!(key(&d), k0, "{what} must change the export key");
    }
    // View state: selection, Mute / Solo.
    let mut d = base();
    let k0 = key(&d);
    d.track_mut(0).unwrap().notes.select_all();
    d.track_mut(1).unwrap().mute = true;
    d.track_mut(2).unwrap().solo = true;
    assert_eq!(key(&d), k0, "view state leaves the export key alone");
}

/// V09-S23 step 5 (V09-0099, "no single oracle"): every golden-corpus export,
/// compact and explicit status, read by a second standard parser — Python's
/// mido via `tools/mido_check.py` (a test tool, not a dependency) — equals
/// the independent Rust reader's digest event for event. Files mido refuses
/// must be exactly the experimental 256-key ones (8-bit key bytes are not
/// Standard MIDI). Skipped with a message when mido is not installed
/// (`PLUMSPIRIT_REQUIRE_MIDO=1` makes it mandatory). Heavy (≈ 400 files
/// through Python, about 90 s): run with `--ignored` when export changes.
#[test]
#[ignore = "heavy: about 400 files through Python's mido (TEST_IMPACT engine::midi heavy)"]
fn every_corpus_export_reads_the_same_in_a_second_standard_parser() {
    use common::mido::{digest_lines, mido_lines, scratch_dir};
    use common::smf_digest::digest;
    let dir = scratch_dir("corpus-mido");
    let mut files = Vec::new();
    for (name, d) in corpus() {
        for (opts, o) in combos(&d) {
            for explicit_status in [false, true] {
                let o = ExportOptions {
                    explicit_status,
                    ..o
                };
                let plan = plan_export_with(&d, o);
                let Ok(bytes) = export_to_bytes_with(&d, &plan) else {
                    continue;
                };
                let wide = o.wide_keys == WideKeys::Experimental;
                let path = dir.join(format!("{}.mid", files.len()));
                std::fs::write(&path, &bytes).unwrap();
                let lines = digest_lines(&digest(&bytes, wide).unwrap());
                files.push((
                    format!("{name} {opts} x{explicit_status}"),
                    path,
                    wide,
                    lines,
                ));
            }
        }
    }
    assert!(files.len() >= 380, "{} files", files.len());
    let paths: Vec<_> = files.iter().map(|f| f.1.clone()).collect();
    let Some(got) = mido_lines(&paths) else {
        return;
    };
    let mut refused = 0;
    for ((what, _, wide, want), got) in files.iter().zip(&got) {
        if *wide {
            assert!(
                got.starts_with("error ") && got.contains("data byte must be in range 0..127"),
                "{what}: mido must refuse the non-standard file, got {got}"
            );
            refused += 1;
            continue;
        }
        if got != want {
            let first = got
                .lines()
                .zip(want.lines())
                .find(|(a, b)| a != b)
                .map(|(a, b)| format!("mido {a:?} vs digest {b:?}"))
                .unwrap_or_else(|| "length".into());
            panic!("{what}: {first}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "mido read {} corpus exports like the digest; refused {refused} experimental 256-key files",
        files.len() - refused
    );
}
