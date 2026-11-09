//! v1.0-Launch L-C03: a project made in PlumSpirit (not imported) exports
//! every note exactly — velocity, start, length, MIDI channel, release
//! velocity, track and subchannel — and its controllers, program, pitch bend
//! and tempo; after a `.lup` save / reopen too, with running status and with
//! an explicit status byte on every message, and as Kiva reads it.
//! And the Kiva condition of L-C02 (`docs/references/v1_0_launch/KIVA_NOTES.md`):
//! a file whose notes are all below Kiva's Audio Note Threshold and that has
//! no control event gives Kiva's audio side nothing at all.

mod common;

use common::kiva::kiva_load;
use plumspirit_engine::codec::{Crc32, put_uvarint};
use plumspirit_engine::editor::clips::duplicate_clips;
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{
    ExportOptions, export_to_bytes_with, import_bytes, plan_export_with, validate_exported_bytes,
};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::{
    Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RawEvent, RawKind, Subchannel,
    SubchannelId, TempoMap, TempoValue,
};
use plumspirit_engine::native::lup::{project_from_bytes, project_to_bytes};

/// The velocities the owner's art uses and the usual ones.
const VELS: [u8; 6] = [1, 2, 25, 64, 100, 127];

fn rec(start: u64, dur: u64, key: u8, vel: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(
            key,
            vel,
            ch,
            if start.is_multiple_of(2) { 0 } else { 64 },
            NoteOffKind::NoteOff,
        )
        .unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

/// A brand-new project: Track 1 with two subchannels (MIDI channels 1 and
/// 4) and dense same-tick chords; Track 2 with a program, volume, pan and
/// pitch bends; Track 3 whose primary clip is placed a second time; a tempo
/// ramp and a step; every velocity of `VELS`.
fn project() -> PlumDocument {
    let q = 960u64;
    let mut d = PlumDocument::new_project("launch export".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![
                Subchannel::new(SubchannelId(1), 0),
                Subchannel::new(SubchannelId(2), 3),
            ],
        },
    )
    .unwrap();
    let ch_of = |sub: u32| if sub == 2 { 3 } else { 0 };
    let mut t0: Vec<NoteRecord> = (0..240u64)
        .map(|k| {
            let sub = (k % 3) as u32;
            rec(
                k * 120,
                90 + k % 5,
                36 + (k % 48) as u8,
                VELS[(k % 6) as usize],
                ch_of(sub),
                sub,
            )
        })
        .collect();
    // Dense chords: 64 notes at one tick, four times.
    for j in 0..4u64 {
        for i in 0..64u64 {
            let sub = (i % 3) as u32;
            t0.push(rec(
                32 * q + j * 480,
                1 + i,
                30 + i as u8,
                VELS[((i + j) % 6) as usize],
                ch_of(sub),
                sub,
            ));
        }
    }
    t0.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&t0, false)
        .unwrap();
    let t1: Vec<NoteRecord> = (0..60u64)
        .map(|k| {
            rec(
                k * 240 + 60,
                200,
                60 + (k % 12) as u8,
                VELS[((k + 3) % 6) as usize],
                2,
                0,
            )
        })
        .collect();
    let track = d.track_mut(1).unwrap();
    track.notes.insert_sorted_batch(&t1, false).unwrap();
    for (tick, status, a, b) in [
        (0u64, 0xC2u8, 33u8, 0u8),
        (0, 0xB2, 7, 100),
        (q, 0xB2, 10, 20),
        (2 * q, 0xE2, 0, 80),
        (3 * q, 0xE2, 0, 64),
    ] {
        track
            .events
            .push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Channel {
                    status,
                    data: [a, b],
                },
            })
            .unwrap();
    }
    let t2: Vec<NoteRecord> = (0..48u64)
        .map(|k| {
            rec(
                k * 120,
                100,
                48 + (k % 24) as u8,
                VELS[(k % 6) as usize],
                5,
                0,
            )
        })
        .collect();
    d.track_mut(2)
        .unwrap()
        .notes
        .insert_sorted_batch(&t2, false)
        .unwrap();
    let t2id = d.track(2).unwrap().id();
    let op = duplicate_clips(&d, &[(ClipId::Primary(t2id), 2, 20 * q)], false).unwrap();
    h.apply(&mut d, op).unwrap();
    for (tick, value, interp) in [
        (
            4 * q,
            TempoValue::Bpm(Bpm::from_integer(90).unwrap()),
            Interp::Linear,
        ),
        (8 * q + 1, TempoValue::Micros(400_000), Interp::Step),
    ] {
        let e = d
            .conductor()
            .prepare_set_tempo(None, tick, value, interp)
            .unwrap();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit: e,
                label: "t",
            },
        )
        .unwrap();
    }
    d
}

/// Every note a track plays, in song ticks: (start, length, key, velocity,
/// channel, release velocity, subchannel), sorted.
type Played = (u64, u64, u8, u8, u8, u8, u32);

fn played(d: &PlumDocument) -> Vec<Vec<Played>> {
    (0..d.track_count())
        .map(|i| {
            let mut v: Vec<Played> = Vec::new();
            for pc in d.placed_contents(i) {
                let (lo, hi) = pc.window();
                let shift = pc.shift();
                for n in pc.notes.iter() {
                    if n.start_tick >= lo && n.start_tick < hi {
                        let a = n.attrs;
                        v.push((
                            (i128::from(n.start_tick) + shift) as u64,
                            n.duration_ticks,
                            a.pitch(),
                            a.velocity(),
                            a.channel(),
                            a.off_velocity(),
                            n.sub.0,
                        ));
                    }
                }
            }
            v.sort_unstable();
            v
        })
        .collect()
}

fn channel_events(d: &PlumDocument, track: usize) -> Vec<(u64, u8, u8, u8)> {
    d.track(track)
        .unwrap()
        .events
        .events()
        .iter()
        .filter_map(|e| match e.kind {
            RawKind::Channel { status, data } => Some((e.tick, status, data[0], data[1])),
            _ => None,
        })
        .collect()
}

fn export(d: &PlumDocument, explicit_status: bool) -> Vec<u8> {
    let plan = plan_export_with(
        d,
        ExportOptions {
            explicit_status,
            ..ExportOptions::default()
        },
    );
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let bytes = export_to_bytes_with(d, &plan).unwrap();
    validate_exported_bytes(d, &plan, &bytes).unwrap();
    bytes
}

#[test]
fn a_project_made_in_plumspirit_exports_every_note_and_event_exactly() {
    let d = project();
    let want = played(&d);
    assert_eq!(
        want.iter().map(Vec::len).sum::<usize>(),
        240 + 256 + 60 + 96
    );
    for (i, notes) in want.iter().enumerate() {
        let vels: std::collections::BTreeSet<u8> = notes.iter().map(|n| n.3).collect();
        assert_eq!(
            vels.into_iter().collect::<Vec<_>>(),
            VELS,
            "track {i} uses every velocity"
        );
    }
    let lup = project_to_bytes(&d).unwrap();
    let (reopened, report) = project_from_bytes(&lup).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(played(&reopened), want, ".lup keeps every note");
    for (source, label) in [(&d, "project"), (&reopened, "reopened .lup")] {
        for explicit in [false, true] {
            let bytes = export(source, explicit);
            let case = format!("{label}, explicit status {explicit}");
            let (back, _) = import_bytes(&bytes).unwrap();
            assert_eq!(back.track_count(), 3, "{case}");
            assert_eq!(
                played(&back),
                want,
                "{case}: notes, lengths, velocities, channels, subchannels"
            );
            assert_eq!(
                channel_events(&back, 1),
                channel_events(&d, 1),
                "{case}: program / CC / bend"
            );
            // Tempo: a linear ramp is written as steps (the export plan), so
            // every note must start at the same time (within 1 ms), and the
            // step tempo after the ramp is exact.
            let (ta, tb) = (
                TempoMap::for_document(&d, 48_000),
                TempoMap::for_document(&back, 48_000),
            );
            for n in want.iter().flatten() {
                let (fa, fb) = (ta.frame_at_tick(n.0), tb.frame_at_tick(n.0));
                assert!(
                    fa.abs_diff(fb) <= 48,
                    "{case}: tick {} plays at frame {fa} / {fb}",
                    n.0
                );
            }
            assert_eq!(
                back.conductor()
                    .tempo_points()
                    .last()
                    .map(|p| (p.tick, p.value)),
                d.conductor()
                    .tempo_points()
                    .last()
                    .map(|p| (p.tick, p.value)),
                "{case}: the step tempo"
            );
            // Kiva reads the same notes with the same velocities.
            let k = kiva_load(&bytes);
            assert!(k.clean(), "{case}: {:?}", k.error);
            let mut kv: Vec<(u64, u8, u8)> = k.notes().iter().map(|n| (n.0, n.2, n.4)).collect();
            kv.sort_unstable();
            let mut pv: Vec<(u64, u8, u8)> =
                want.iter().flatten().map(|n| (n.0, n.2, n.3)).collect();
            pv.sort_unstable();
            assert_eq!(kv, pv, "{case}: Kiva's notes");
        }
    }
}

/// The L-C02 condition, stated on bytes: with Kiva's Audio Note Threshold at
/// 15, the owner's `Test.mid` (every note velocity 1, no control event) has
/// no audio event and no control event; any one loud note or one control
/// event (the `one-loud` / `program0` variants) gives Kiva's audio side
/// something to merge.
#[test]
fn the_kiva_empty_audio_condition_holds_for_test_mid_and_not_for_its_variants() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/references/v1_0_launch/fixtures");
    let audio_and_control = |bytes: &[u8]| {
        let k = kiva_load(bytes);
        assert!(k.clean());
        let audio = k.notes().iter().filter(|n| n.4 >= 15).count();
        let (_, _) = import_bytes(bytes).unwrap();
        let control = control_events(bytes);
        (audio, control)
    };
    let test = std::fs::read(fixtures.join("Test.mid")).unwrap();
    assert_eq!(
        audio_and_control(&test),
        (0, 0),
        "Test.mid: nothing for Kiva's audio side"
    );
    let v = |n: &str| std::fs::read(fixtures.join("kiva-variants").join(n)).unwrap();
    assert_eq!(audio_and_control(&v("Test.one-loud.mid")), (1, 0));
    assert_eq!(audio_and_control(&v("Test.program0.mid")), (0, 1));
    assert_eq!(audio_and_control(&v("Test.vel15.mid")).0, 5_020);
}

/// Channel messages that are not notes (Kiva's control events).
fn control_events(bytes: &[u8]) -> usize {
    let (d, _) = import_bytes(bytes).unwrap();
    (0..d.track_count())
        .map(|i| channel_events(&d, i).len())
        .sum()
}

/// L-C04 (owner run 2026-10-30: `Test.mid` freezes Kiva v1.1.18-1m at Audio
/// Note Threshold 15, `testing_purposes_only.mid` — which has velocity-100
/// notes — loads): with `player_safe_start` the export of `Test.lup` gets one
/// Program Change 0 (the General MIDI default) at tick 0 before the first
/// note, so Kiva's Pre-render engine has a control event to merge; every
/// note, velocity, length and channel stays as it was, and nothing else is
/// added. A project that already has a control event is written byte for
/// byte as without the option; off, nothing is added.
#[test]
fn player_safe_start_adds_one_neutral_program_change_only_when_the_file_has_no_control_event() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/references/v1_0_launch/fixtures");
    let (test, _) = project_from_bytes(&std::fs::read(fixtures.join("Test.lup")).unwrap()).unwrap();
    let write = |d: &PlumDocument, safe: bool, explicit_status: bool| {
        let plan = plan_export_with(
            d,
            ExportOptions {
                explicit_status,
                player_safe_start: safe,
                ..ExportOptions::default()
            },
        );
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let bytes = export_to_bytes_with(d, &plan).unwrap();
        validate_exported_bytes(d, &plan, &bytes).unwrap();
        (plan, bytes)
    };
    // Off: the file the owner exported (the pinned fixture), unchanged.
    let (plan_off, off) = write(&test, false, false);
    assert_eq!(plan_off.player_safe_start, None);
    assert_eq!(off, std::fs::read(fixtures.join("Test.mid")).unwrap());
    for explicit in [false, true] {
        let (plan, on) = write(&test, true, explicit);
        assert_eq!(plan.player_safe_start, Some(0), "channel 1");
        // Exactly the bytes of the off export plus `00 C0 00` after the
        // tick-0 Conductor events, before the first note.
        let (_, off) = write(&test, false, explicit);
        assert_eq!(on.len(), off.len() + 3);
        // The first chunk's length field (bytes 18..22) grows by 3.
        let len = |b: &[u8]| u32::from_be_bytes(b[18..22].try_into().unwrap());
        assert_eq!(len(&on), len(&off) + 3);
        assert_eq!(&on[..18], &off[..18], "header and MTrk tag unchanged");
        let first = (22..off.len())
            .find(|&i| off[i] != on[i])
            .expect("the program change is inserted");
        // Delta 0 + Program Change 0, inserted where the bytes part (its
        // delta may share a byte with the next event's delta 0).
        let at = (first.saturating_sub(1)..=first)
            .find(|&p| on[p..p + 3] == [0x00, 0xC0, 0x00])
            .expect("delta 0, Program Change 0 on channel 1");
        assert_eq!(&on[22..at], &off[22..at], "everything before it unchanged");
        assert_eq!(&on[at + 3..], &off[at..], "everything after it unchanged");
        // It follows the tick-0 tempo / time signature and precedes the first
        // Note On.
        assert_eq!(on[at + 3], 0x00);
        assert_eq!(on[at + 4] & 0xF0, 0x90, "the first note follows");
        // Kiva: the same notes and velocities, and now one control event.
        let (k_on, k_off) = (kiva_load(&on), kiva_load(&off));
        assert!(k_on.clean(), "{:?}", k_on.error);
        assert_eq!(k_on.notes(), k_off.notes());
        assert_eq!(k_on.notes().iter().filter(|n| n.4 >= 15).count(), 0);
        assert_eq!(control_events(&on), 1);
        let (back, _) = import_bytes(&on).unwrap();
        assert_eq!(played(&back), played(&import_bytes(&off).unwrap().0));
    }
    // The default export (running status) is byte for byte the `program0`
    // variant the owner's Kiva check runs (OWNER_CHECKS C3).
    assert_eq!(
        write(&test, true, false).1,
        std::fs::read(fixtures.join("kiva-variants/Test.program0.mid")).unwrap()
    );
    // A project with a controller / program / bend already: nothing added.
    let d = project();
    let (plan, with) = write(&d, true, false);
    assert_eq!(plan.player_safe_start, None);
    assert_eq!(with, write(&d, false, false).1);
    // The lowest channel the notes use.
    let mut d = PlumDocument::new_project("ch".into(), Bpm::DEFAULT, 960);
    let notes = vec![rec(0, 10, 60, 1, 5, 0), rec(10, 10, 61, 1, 2, 0)];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&notes, false)
        .unwrap();
    assert_eq!(write(&d, true, false).0.player_safe_start, Some(2));
}

/// L-S51 (owner request 2026-10-30): with `loudness_reference` the export of
/// Test.lup (every note velocity 1) gets one silent velocity-127 note — zero
/// length, on an unused channel muted by Channel Volume 0 at the start, on
/// the song's lowest key, at the first track's end — so Piano From Above's
/// volume correction (velocity x min(127 x notes / sum, 127 / loudest))
/// becomes 1 and velocity 1 stays 1. The song's own notes and velocities are
/// unchanged; validation sees no pairing change; Kiva loads it (the reference
/// is an audio event, its Channel Volume a control event) without a separate
/// Program Change. A song that already has a velocity-127 note gets nothing.
#[test]
fn loudness_reference_keeps_piano_from_above_at_the_written_velocities() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/references/v1_0_launch/fixtures");
    let (test, _) = project_from_bytes(&std::fs::read(fixtures.join("Test.lup")).unwrap()).unwrap();
    let write = |d: &PlumDocument, reference: bool, safe: bool| {
        let plan = plan_export_with(
            d,
            ExportOptions {
                loudness_reference: reference,
                player_safe_start: safe,
                ..ExportOptions::default()
            },
        );
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let bytes = export_to_bytes_with(d, &plan).unwrap();
        let report = validate_exported_bytes(d, &plan, &bytes).unwrap();
        assert!(
            report.tracks_with_different_pairing.is_empty(),
            "{:?}",
            report.tracks_with_different_pairing
        );
        (plan, bytes)
    };
    let (plan, on) = write(&test, true, true);
    // Byte for byte the owner's check file.
    assert_eq!(
        on,
        std::fs::read(fixtures.join("kiva-variants/Test.loudness-reference.mid")).unwrap()
    );
    let r = plan.loudness_reference.expect("a reference");
    assert_eq!(
        (r.channel, r.key, r.loudest),
        (15, 0, 1),
        "channel 16, key 0"
    );
    assert_eq!(
        plan.player_safe_start, None,
        "the reference's CC 7 suffices"
    );
    let (_, off) = write(&test, false, false);
    assert_eq!(on.len(), off.len() + 4 + 4 + 4, "CC 7 + Note On + Note Off");
    // PFA's factor from the bytes: notes and velocity sum of every Note On.
    let k = kiva_load(&on);
    assert!(k.clean(), "{:?}", k.error);
    let ons: Vec<u8> = k.notes().iter().map(|n| n.4).collect();
    assert_eq!(ons.len(), 5_021);
    let (n, sum, max) = (
        ons.len() as f64,
        ons.iter().map(|&v| f64::from(v)).sum::<f64>(),
        f64::from(*ons.iter().max().unwrap()),
    );
    let factor = (127.0 * n / sum).min(127.0 / max);
    assert_eq!(factor, 1.0, "PFA plays velocity 1 as 1");
    // Kiva: the song's notes unchanged, plus the reference.
    let song = kiva_load(&off).notes();
    let mut with = k.notes();
    let extra: Vec<_> = with.iter().filter(|n| n.4 == 127).collect();
    assert_eq!(extra.len(), 1);
    with.retain(|n| n.4 != 127);
    assert_eq!(with, song);
    // PlumSpirit reads it back: the song plus one zero-length note on channel
    // 16, and Channel Volume 0 on channel 16 at tick 0.
    let (back, _) = import_bytes(&on).unwrap();
    let mut refs = Vec::new();
    for t in back.tracks() {
        for n in t.notes.iter() {
            if n.attrs.channel() == 15 {
                refs.push((
                    n.start_tick,
                    n.duration_ticks,
                    n.attrs.pitch(),
                    n.attrs.velocity(),
                ));
            }
        }
    }
    assert_eq!(refs, vec![(r.tick, 0, 0, 127)]);
    assert_eq!(channel_events(&back, 0), vec![(0, 0xBF, 7, 0)]);
    // Already a velocity-127 note: nothing added.
    let d = project();
    let (plan, with) = write(&d, true, false);
    assert_eq!(plan.loudness_reference, None);
    assert_eq!(with, write(&d, false, false).1);
    // Every channel but the drum channel in use: skipped, with the reason;
    // the player-safe start still applies.
    let mut d = PlumDocument::new_project("busy".into(), Bpm::DEFAULT, 960);
    let notes: Vec<NoteRecord> = (0..16u8)
        .filter(|&c| c != 9)
        .map(|c| rec(u64::from(c) * 10, 10, 60, 1, c, 0))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&notes, false)
        .unwrap();
    let (plan, busy) = write(&d, true, true);
    assert_eq!(plan.loudness_reference, None);
    assert!(
        plan.loudness_reference_skipped
            .as_deref()
            .is_some_and(|w| w.contains("every channel")),
        "{:?}",
        plan.loudness_reference_skipped
    );
    assert_eq!(plan.player_safe_start, Some(0));
    assert_eq!(busy, write(&d, false, true).1);
    // The drum channel free but no other: still skipped (never channel 10).
    assert_eq!(kiva_load(&busy).notes().len(), 15);
}

/// L-S43 (2026-10-30): the owner's own `Test.lup` as it is in the Refs
/// folder now (re-saved by the owner at 13:07 local, before the video run)
/// plays exactly the notes of the pinned fixture — only the order of notes
/// that start together on different keys differs, so its export bytes
/// differ. The owner's own `Test.mid` of that run is PlumSpirit's export of
/// it with Running Status Compression off; with the player-safe start it
/// becomes the owner's file plus only the Program Change that Kiva was
/// shown to load (E0105). A smoke export of this file on the PC must
/// therefore match these, not `kiva-variants/Test.program0.mid`.
#[test]
fn the_owners_resaved_test_lup_plays_the_pinned_notes_and_exports_the_owners_test_mid() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/references/v1_0_launch/fixtures");
    let read = |p: &str| std::fs::read(fixtures.join(p)).unwrap();
    let (pinned, _) = project_from_bytes(&read("Test.lup")).unwrap();
    let (owner, _) = project_from_bytes(&read("owner-2026-10-30/Test.lup")).unwrap();
    assert_eq!(played(&owner), played(&pinned));
    assert_eq!(owner.track_count(), pinned.track_count());
    let write = |explicit_status: bool, safe: bool| {
        let plan = plan_export_with(
            &owner,
            ExportOptions {
                explicit_status,
                player_safe_start: safe,
                ..ExportOptions::default()
            },
        );
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let bytes = export_to_bytes_with(&owner, &plan).unwrap();
        validate_exported_bytes(&owner, &plan, &bytes).unwrap();
        bytes
    };
    // The owner's export of the run (explicit status, no Program Change).
    assert_eq!(write(true, false), read("owner-2026-10-30/Test.mid"));
    // The same with the player-safe start: the single-factor file Kiva loaded.
    assert_eq!(
        write(true, true),
        read("owner-2026-10-30/Test.program0.mid")
    );
    // The default (running status) export carries the same notes as the
    // pinned project's default export: only same-tick order differs.
    let (a, _) = import_bytes(&write(false, true)).unwrap();
    let (b, _) = import_bytes(&read("kiva-variants/Test.program0.mid")).unwrap();
    assert_eq!(played(&a), played(&b));
    // Saving updates the writer stamp in META and its checksum. Every other
    // byte must match the owner's historical file, including same-tick order.
    let mut expected = read("owner-2026-10-30/Test.lup");
    let meta_header = 32..48;
    let meta_start = meta_header.end;
    assert_eq!(&expected[32..36], b"META");
    assert_eq!(&expected[meta_start..meta_start + 5], b"\x04Test");
    let old_stamp = b"\x10PlumSpirit 1.0.0";
    let stamp_start = meta_start + 5;
    let stamp_range = stamp_start..stamp_start + old_stamp.len();
    assert_eq!(&expected[stamp_range.clone()], old_stamp);
    let writer = format!("PlumSpirit {}", plumspirit_engine::VERSION);
    let mut stamp = Vec::new();
    put_uvarint(&mut stamp, writer.len() as u64);
    stamp.extend_from_slice(writer.as_bytes());
    let old_size = u64::from_le_bytes(expected[40..48].try_into().unwrap());
    let size = old_size - old_stamp.len() as u64 + stamp.len() as u64;
    expected.splice(stamp_range, stamp);
    expected[40..48].copy_from_slice(&size.to_le_bytes());
    let meta_end = meta_start + size as usize;
    let payload_crc = Crc32::of(&expected[meta_start..meta_end]);
    let mut checksum = Crc32::new();
    checksum.update(&expected[meta_header]);
    checksum.update(&payload_crc.to_le_bytes());
    expected[meta_end..meta_end + 4].copy_from_slice(&checksum.finish().to_le_bytes());

    let saved = project_to_bytes(&owner).unwrap();
    assert_eq!(saved.len(), expected.len());
    assert_eq!(
        saved.iter().zip(&expected).position(|(a, b)| a != b),
        None,
        "unexpected difference from the historical file with the new writer stamp"
    );
    let (reopened, _) = project_from_bytes(&saved).unwrap();
    assert_eq!(played(&reopened), played(&owner));
}
