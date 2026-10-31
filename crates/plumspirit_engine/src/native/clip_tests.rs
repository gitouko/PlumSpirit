//! `.lup` clips (V09-S06, v0.9 contract §040, §216, §241): the `CLIP` section
//! round trip, the post-save verifier, and hostile input. In the crate because
//! the clip table is changed only through `PlumDocument::clips_mut` (clip
//! operations through the history arrive with V09-S07).

use super::lup::{
    LUP_VERSION, LUP_VERSION_256, LUP_VERSION_CLIPS, LupControl, LupError, load_project,
    project_from_bytes, project_to_bytes, save_project, verify_project,
};
use crate::codec::{Crc32, put_uvarint};
use crate::model::project::KeyRange;
use crate::model::track::TrackKind;
use crate::model::{
    Bpm, ClipId, ClipPlacement, ContentId, ContentRef, GroupInfo, NoteAttrs, NoteOffKind,
    NoteRecord, PlumDocument, RawEvent, RawKind, SubchannelId, TrackId,
};

fn note(start: u64, pitch: u8, sub: u32, group: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 120,
        attrs: NoteAttrs::new(pitch, 100, (sub % 16) as u8, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group,
    }
}

/// Three tracks with notes and a saved note group.
fn base() -> PlumDocument {
    let mut d = PlumDocument::new_project("Clips ✓".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    let g = d.project_mut().groups.add(GroupInfo {
        name: Some("Riff".into()),
        ..GroupInfo::default()
    });
    for i in 0..d.track_count() {
        let notes: Vec<NoteRecord> = (0..16u64)
            .map(|k| {
                note(
                    k * 240,
                    48 + (k as u8) + i as u8,
                    0,
                    if k == 3 { g } else { 0 },
                )
            })
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
    }
    d
}

fn tid(d: &PlumDocument, i: usize) -> TrackId {
    d.track(i).unwrap().id()
}

/// A table with every kind of record: a moved / windowed / muted / named /
/// coloured primary, a named-only primary, a removed primary, a linked
/// instance of a track's own content on another track, and an owned content
/// (notes with subchannels, a group, a muted note; CC, pitch bend, a text
/// meta event and SysEx) placed twice.
fn rich() -> PlumDocument {
    let mut d = base();
    let (t0, t1, t2) = (tid(&d, 0), tid(&d, 1), tid(&d, 2));
    let g = d.project().groups.entries()[0].id;
    let c = d.clips_mut();
    c.set_placement(ClipPlacement {
        start: 960,
        offset: 240,
        length: Some(1920),
        muted: true,
        name: Some("Intro".into()),
        color: Some(0xFF8000),
        ..ClipPlacement::primary(t0)
    });
    c.set_placement(ClipPlacement {
        name: Some("Bass ♪".into()),
        ..ClipPlacement::primary(t2)
    });
    c.remove_placement(ClipId::Primary(t1));
    let link = c.new_clip_id();
    c.set_placement(ClipPlacement {
        id: link,
        start: 3840,
        ..ClipPlacement::primary(t0)
    });
    let cid = c.new_content();
    let a = c.new_clip_id();
    c.set_placement(ClipPlacement {
        id: a,
        track: t1,
        content: ContentRef::Owned(cid),
        start: 7680,
        ..ClipPlacement::primary(t1)
    });
    let b = c.new_clip_id();
    c.set_placement(ClipPlacement {
        id: b,
        track: t2,
        content: ContentRef::Owned(cid),
        start: 15360,
        offset: 480,
        length: Some(960),
        color: Some(0x0000FF),
        ..ClipPlacement::primary(t2)
    });
    let o = c.content_mut(cid).unwrap();
    o.name = Some("Hook".into());
    o.color = Some(0x12_34_56);
    let mut notes: Vec<NoteRecord> = (0..40u64)
        .map(|k| note(k * 60, 60 + (k % 12) as u8, (k % 4) as u32 * 7, 0))
        .collect();
    notes[5].group = g;
    notes[6].attrs = notes[6].attrs.with_muted(true);
    o.notes.insert_sorted_batch(&notes, false).unwrap();
    let text = o.events.store_payload(b"verse").unwrap();
    let sysex = o
        .events
        .store_payload(&[0x7E, 0x7F, 0x09, 0x01, 0xF7])
        .unwrap();
    for (tick, kind) in [
        (
            0,
            RawKind::Channel {
                status: 0xB0,
                data: [7, 100],
            },
        ),
        (
            0,
            RawKind::Meta {
                meta_type: 0x01,
                payload: text,
            },
        ),
        (
            120,
            RawKind::Channel {
                status: 0xE3,
                data: [0, 0x50],
            },
        ),
        (480, RawKind::SysEx { payload: sysex }),
    ] {
        o.events
            .push(RawEvent {
                tick,
                anchor: 0,
                kind,
            })
            .unwrap();
    }
    o.touch_events();
    d.validate_clips().unwrap();
    d
}

fn version_of(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[8], bytes[9]])
}

/// The sections of a file: (tag, flags, payload).
fn sections(bytes: &[u8]) -> Vec<([u8; 4], u32, Vec<u8>)> {
    let n = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let mut at = 32;
    let mut out = Vec::new();
    for _ in 0..n {
        let tag: [u8; 4] = bytes[at..at + 4].try_into().unwrap();
        let flags = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        let len = u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap()) as usize;
        out.push((tag, flags, bytes[at + 16..at + 16 + len].to_vec()));
        at += 16 + len + 4;
    }
    assert_eq!(at, bytes.len());
    out
}

/// Reassembles a file with valid checksums (header version `version`).
fn assemble(version: u16, secs: &[([u8; 4], u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&super::lup::LUP_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(secs.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    let crc = Crc32::of(&out[..28]);
    out.extend_from_slice(&crc.to_le_bytes());
    for (tag, flags, payload) in secs {
        let mut head = Vec::new();
        head.extend_from_slice(tag);
        head.extend_from_slice(&flags.to_le_bytes());
        head.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        let mut c = Crc32::new();
        c.update(&head);
        c.update(&Crc32::of(payload).to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(payload);
        out.extend_from_slice(&c.finish().to_le_bytes());
    }
    out
}

/// `bytes` with its `CLIP` payload replaced by `payload`.
fn with_clip_payload(bytes: &[u8], payload: Vec<u8>) -> Vec<u8> {
    let mut secs = sections(bytes);
    let i = secs
        .iter()
        .position(|s| &s.0 == b"CLIP")
        .expect("a CLIP section");
    secs[i].2 = payload;
    assemble(version_of(bytes), &secs)
}

fn uv(out: &mut Vec<u8>, v: u64) {
    put_uvarint(out, v);
}

/// Opening must fail with a sentence about damage that names `what`.
fn refused(bytes: &[u8], what: &str) {
    let e = project_from_bytes(bytes).expect_err(what);
    let m = e.user_message();
    assert!(
        m.contains(what) && m.starts_with("The project file is damaged"),
        "{what}: {m}"
    );
    assert!(m.ends_with(").") || m.ends_with('.'), "a sentence: {m}");
}

#[test]
fn a_clip_table_survives_saving_and_reopening_exactly() {
    let d = rich();
    let bytes = project_to_bytes(&d).unwrap();
    assert_eq!(version_of(&bytes), LUP_VERSION_CLIPS);
    let clip = sections(&bytes)
        .into_iter()
        .find(|s| &s.0 == b"CLIP")
        .expect("CLIP written");
    assert_eq!(clip.1, 1, "mandatory");
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert_eq!(report.version, LUP_VERSION_CLIPS);
    assert_eq!(report.migrated_from, None);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.fingerprint(), d.fingerprint());
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    back.validate_clips().unwrap();
    assert!(!back.clips_are_identity());
    let (a, b) = (back.clips(), d.clips());
    assert_eq!(a.next_ids(), b.next_ids());
    assert_eq!(a.removed_primaries(), b.removed_primaries());
    let strip = |v: &[ClipPlacement]| -> Vec<ClipPlacement> {
        v.iter()
            .map(|p| ClipPlacement {
                revision: 0,
                ..p.clone()
            })
            .collect()
    };
    assert_eq!(
        strip(a.explicit_placements()),
        strip(b.explicit_placements())
    );
    assert_eq!(a.contents().len(), 1);
    let (x, y) = (&a.contents()[0], &b.contents()[0]);
    assert_eq!((x.id, &x.name, x.color), (y.id, &y.name, y.color));
    assert!(
        x.notes
            .iter()
            .map(|n| n.record())
            .eq(y.notes.iter().map(|n| n.record()))
    );
    assert_eq!(x.events.len(), 4);
    assert_eq!(
        x.events.payload(match x.events.events()[1].kind {
            RawKind::Meta { payload, .. } => payload,
            _ => panic!("meta"),
        }),
        b"verse"
    );
    // Every placement reads the same after the round trip.
    for i in 0..d.track_count() {
        assert_eq!(strip(&back.placements_on(i)), strip(&d.placements_on(i)));
    }
    // Saving again is byte-identical, and the verifier accepts the file.
    assert_eq!(project_to_bytes(&back).unwrap(), bytes);
    verify_project(&d, &bytes[..], Some(bytes.len() as u64)).unwrap();
}

#[test]
fn the_verifier_notices_any_clip_difference() {
    let d = rich();
    let bytes = project_to_bytes(&d).unwrap();
    let ok = |doc: &PlumDocument| verify_project(doc, &bytes[..], Some(bytes.len() as u64));
    ok(&d).unwrap();
    let (t0, t1) = (tid(&d, 0), tid(&d, 1));
    let cid = d.clips().contents()[0].id;
    let mut cases: Vec<(&str, PlumDocument)> = Vec::new();
    let mut e = d.clone();
    let mut p = e.clips().placement(ClipId::Primary(t0)).unwrap();
    p.start += 1;
    e.clips_mut().set_placement(p);
    cases.push(("a moved clip", e));
    let mut e = d.clone();
    let mut p = e.clips().placement(ClipId::Primary(t0)).unwrap();
    p.name = Some("Outro".into());
    e.clips_mut().set_placement(p);
    cases.push(("a renamed clip", e));
    let mut e = d.clone();
    e.clips_mut().set_placement(ClipPlacement::primary(t1));
    cases.push(("a restored primary", e));
    let mut e = d.clone();
    e.clips_mut().new_clip_id();
    cases.push(("the next clip id", e));
    let mut e = d.clone();
    e.clips_mut().content_mut(cid).unwrap().name = None;
    cases.push(("a content name", e));
    let mut e = d.clone();
    e.clips_mut()
        .content_mut(cid)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(9, 99, 0, 0)], false)
        .unwrap();
    cases.push(("a content note", e));
    let mut e = d.clone();
    let o = e.clips_mut().content_mut(cid).unwrap();
    o.events
        .push(RawEvent {
            tick: 960,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0xB0,
                data: [10, 64],
            },
        })
        .unwrap();
    cases.push(("a content event", e));
    let mut e = d.clone();
    *e.clips_mut() = crate::model::ClipTable::default();
    cases.push(("no clips at all", e));
    for (what, e) in &cases {
        assert!(ok(e).is_err(), "{what} must not verify");
    }
    // And a clip-less file does not verify against a document with clips.
    let plain = project_to_bytes(&base()).unwrap();
    assert!(verify_project(&d, &plain[..], Some(plain.len() as u64)).is_err());
}

#[test]
fn projects_without_clips_are_written_exactly_as_before() {
    let d = base();
    assert!(d.clips_are_identity());
    let bytes = project_to_bytes(&d).unwrap();
    assert_eq!(version_of(&bytes), LUP_VERSION);
    assert!(sections(&bytes).iter().all(|s| &s.0 != b"CLIP"));
    // Changing a clip and changing it back leaves no record and no CLIP.
    let mut e = d.clone();
    let t0 = tid(&e, 0);
    let mut p = e.clips().placement(ClipId::Primary(t0)).unwrap();
    p.start = 480;
    e.clips_mut().set_placement(p.clone());
    assert_eq!(
        version_of(&project_to_bytes(&e).unwrap()),
        LUP_VERSION_CLIPS
    );
    p.start = 0;
    e.clips_mut().set_placement(p);
    assert_eq!(project_to_bytes(&e).unwrap(), bytes);
    // 256-Key Mode without clips stays version 6; with clips it is 7 and
    // keeps its key range.
    let mut w = d.clone();
    w.project_mut().keys = KeyRange::Extended256;
    assert_eq!(version_of(&project_to_bytes(&w).unwrap()), LUP_VERSION_256);
    let mut p = w.clips().placement(ClipId::Primary(t0)).unwrap();
    p.muted = true;
    w.clips_mut().set_placement(p);
    let wb = project_to_bytes(&w).unwrap();
    assert_eq!(version_of(&wb), LUP_VERSION_CLIPS);
    let (back, _) = project_from_bytes(&wb).unwrap();
    assert_eq!(back.project().keys, KeyRange::Extended256);
    assert_eq!(back.state_fingerprint(), w.state_fingerprint());
}

#[test]
fn a_transactional_save_writes_and_verifies_clips() {
    let dir = std::env::temp_dir().join(format!("plumspirit-clips-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clips.lup");
    let d = rich();
    let report = save_project(&d, &path, LupControl::default()).unwrap();
    assert!(report.write.notes >= 40 + 48, "content notes are counted");
    let (back, _) = load_project(&path, LupControl::default()).unwrap();
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    std::fs::remove_dir_all(&dir).ok();
}

/// A minimal CLIP payload: `placements` already encoded, no removed
/// primaries, `contents` already encoded.
fn payload(next_clip: u64, next_content: u64, placements: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    uv(&mut p, 1);
    uv(&mut p, next_clip);
    uv(&mut p, next_content);
    uv(&mut p, placements.len() as u64);
    for x in placements {
        p.extend_from_slice(x);
    }
    p.extend_from_slice(tail);
    p
}

/// One placement record.
fn placement(placed: Option<u32>, primary_of: u32, host: u32, owned: Option<u32>) -> Vec<u8> {
    let mut p = Vec::new();
    match placed {
        Some(n) => {
            p.push(1);
            uv(&mut p, u64::from(n));
        }
        None => {
            p.push(0);
            uv(&mut p, u64::from(primary_of));
        }
    }
    uv(&mut p, u64::from(host));
    match owned {
        Some(c) => {
            p.push(1);
            uv(&mut p, u64::from(c));
        }
        None => {
            p.push(0);
            uv(&mut p, u64::from(primary_of));
        }
    }
    uv(&mut p, 960); // start
    uv(&mut p, 0); // offset
    p.push(0); // flags
    p
}

#[test]
fn damaged_clip_sections_are_refused_with_sentences() {
    let good = project_to_bytes(&rich()).unwrap();
    let d = base();
    let (t0, t1) = (tid(&d, 0).0, tid(&d, 1).0);
    let no_rest = [0u8, 0u8]; // no removed primaries, no contents
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("unknown layout", {
            let mut p = payload(0, 0, &[], &no_rest);
            p[0] = 2;
            p
        }),
        (
            "shows a missing clip content 5",
            payload(1, 6, &[placement(Some(0), t0, t0, Some(5))], &no_rest),
        ),
        (
            "sits on a missing track (id 77)",
            payload(1, 0, &[placement(Some(0), t0, 77, None)], &no_rest),
        ),
        (
            "shows a missing track (id 78)",
            payload(1, 0, &[placement(Some(0), 78, t0, None)], &no_rest),
        ),
        (
            "clip 4 is not below the next id 4",
            payload(4, 0, &[placement(Some(4), t0, t0, None)], &no_rest),
        ),
        ("is stored although it is unchanged", {
            // Primary of t0 on t0 showing t0, start 0, offset 0, no flags.
            let x = vec![0, t0 as u8, t0 as u8, 0, t0 as u8, 0, 0, 0];
            payload(0, 0, &[x], &no_rest)
        }),
        (
            "not in id order",
            payload(
                0,
                0,
                &[placement(None, t1, t1, None), placement(None, t0, t0, None)],
                &no_rest,
            ),
        ),
        ("unknown clip flags", {
            let mut x = placement(None, t0, t0, None);
            *x.last_mut().unwrap() = 0x10;
            payload(0, 0, &[x], &no_rest)
        }),
        ("unknown clip kind", {
            let mut x = placement(None, t0, t0, None);
            x[0] = 2;
            payload(0, 0, &[x], &no_rest)
        }),
        ("unknown content kind", {
            let mut x = placement(None, t0, t0, None);
            x[3] = 9;
            payload(0, 0, &[x], &no_rest)
        }),
        ("invalid length (0 ticks", {
            let mut x = placement(None, t0, t0, None);
            *x.last_mut().unwrap() = 8;
            x.push(0);
            payload(0, 0, &[x], &no_rest)
        }),
        ("1 to 200 characters", {
            let mut x = placement(None, t0, t0, None);
            *x.last_mut().unwrap() = 2;
            uv(&mut x, 2);
            x.extend_from_slice(b"  ");
            payload(0, 0, &[x], &no_rest)
        }),
        ("a clip name is not text", {
            let mut x = placement(None, t0, t0, None);
            *x.last_mut().unwrap() = 2;
            uv(&mut x, 2);
            x.extend_from_slice(&[0xC3, 0x28]);
            payload(0, 0, &[x], &no_rest)
        }),
        (
            "a removed clip belongs to a missing track (id 50)",
            payload(0, 0, &[], &[1, 50, 0]),
        ),
        (
            "is both removed and placed",
            payload(0, 0, &[placement(None, t0, t0, None)], &[1, t0 as u8, 0]),
        ),
        ("clip content 0 is not placed anywhere", {
            let mut tail = vec![0u8, 1]; // no removed, one content
            tail.extend_from_slice(&[0, 0, 0, 0]); // id 0, flags 0, 0 notes, 0 events
            payload(0, 1, &[], &tail)
        }),
        ("unknown content flags", {
            let mut tail = vec![0u8, 1];
            tail.extend_from_slice(&[0, 1, 0, 0]);
            payload(1, 1, &[placement(Some(0), t0, t0, Some(0))], &tail)
        }),
        ("invalid note subchannel", {
            let mut tail = vec![0u8, 1, 0, 0, 1]; // content 0, flags 0, 1 note
            tail.extend_from_slice(&[0, 120]); // start delta 0, duration 120
            let word = note(0, 60, 0, 0).attrs.raw() | (1 << 31);
            tail.extend_from_slice(&word.to_le_bytes());
            tail.push(0); // subchannel 0 is never stored
            tail.push(0); // no events
            payload(1, 1, &[placement(Some(0), t0, t0, Some(0))], &tail)
        }),
        ("refers to group 9", {
            let mut tail = vec![0u8, 1, 0, 0, 1];
            tail.extend_from_slice(&[0, 120]);
            let word = note(0, 60, 0, 0).attrs.raw() | (1 << 30);
            tail.extend_from_slice(&word.to_le_bytes());
            tail.push(9);
            tail.push(0);
            payload(1, 1, &[placement(Some(0), t0, t0, Some(0))], &tail)
        }),
        ("invalid note (pitch/velocity/channel)", {
            // A 256-key pitch in a standard project.
            let mut tail = vec![0u8, 1, 0, 0, 1];
            tail.extend_from_slice(&[0, 120]);
            let word = note(0, 60, 0, 0).attrs.raw() | (1 << 27);
            tail.extend_from_slice(&word.to_le_bytes());
            tail.push(0);
            payload(1, 1, &[placement(Some(0), t0, t0, Some(0))], &tail)
        }),
        (
            "clip count exceeds the section",
            payload(0, 0, &[], &[])
                .into_iter()
                .take(3)
                .chain([0xFF, 0xFF, 0x03])
                .collect(),
        ),
        ("a section ends too early", payload(0, 0, &[], &[0])),
        (
            "unexpected extra data in section CLIP",
            payload(0, 0, &[], &[0, 0, 0]),
        ),
    ];
    for (what, p) in cases {
        let bytes = with_clip_payload(&good, p);
        let e = project_from_bytes(&bytes).expect_err(what);
        let m = e.user_message();
        assert!(
            m.starts_with("The project file is damaged") && m.contains(what) && m.ends_with('.'),
            "{what}: {m}"
        );
    }
}

#[test]
fn a_clip_section_where_it_does_not_belong_is_refused() {
    let good = project_to_bytes(&rich()).unwrap();
    let secs = sections(&good);
    // Version 5 / 6 files do not know CLIP: it is an unknown mandatory section.
    for v in [LUP_VERSION, LUP_VERSION_256] {
        let mut s = secs.clone();
        if v == LUP_VERSION {
            // META without the key range.
            let meta = &mut s[0].2;
            meta.pop();
        }
        let e = project_from_bytes(&assemble(v, &s)).unwrap_err();
        assert_eq!(
            e,
            LupError::UnsupportedSection("CLIP".into()),
            "version {v}"
        );
    }
    // Twice.
    let i = secs.iter().position(|s| &s.0 == b"CLIP").unwrap();
    let mut twice = secs.clone();
    twice.insert(i, secs[i].clone());
    refused(
        &assemble(LUP_VERSION_CLIPS, &twice),
        "the clips appear twice",
    );
    // Before the tracks.
    let mut early = secs.clone();
    let clip = early.remove(i);
    early.insert(2, clip);
    refused(
        &assemble(LUP_VERSION_CLIPS, &early),
        "the clips must follow the tracks",
    );
    // A damaged checksum names the clip data.
    let mut bad = good.clone();
    let at =
        32 + secs[..i].iter().map(|s| 16 + s.2.len() + 4).sum::<usize>() + 16 + secs[i].2.len();
    bad[at] ^= 0x40;
    let m = project_from_bytes(&bad).unwrap_err().user_message();
    assert!(
        m.contains("the clip data at byte") && m.contains("failed its checksum"),
        "{m}"
    );
    // A version 7 file without CLIP is a project without clips.
    let mut none = secs.clone();
    none.remove(i);
    let (back, _) = project_from_bytes(&assemble(LUP_VERSION_CLIPS, &none)).unwrap();
    assert!(back.clips_are_identity());
}

/// Any byte of a CLIP payload changed (checksums made valid again) opens or is
/// refused — never a panic, never an invalid table.
#[test]
fn random_damage_in_clip_data_never_panics_or_yields_an_invalid_table() {
    let good = project_to_bytes(&rich()).unwrap();
    let secs = sections(&good);
    let i = secs.iter().position(|s| &s.0 == b"CLIP").unwrap();
    let clean = secs[i].2.clone();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut opened, mut refused_n) = (0, 0);
    for _ in 0..3000 {
        let mut p = clean.clone();
        for _ in 0..1 + next() % 3 {
            let at = (next() % p.len() as u64) as usize;
            p[at] = match next() % 3 {
                0 => p[at] ^ (1 << (next() % 8)),
                1 => next() as u8,
                _ => p[at].wrapping_add(1),
            };
        }
        if next() % 8 == 0 {
            let cut = (next() % p.len() as u64) as usize;
            p.truncate(cut);
        }
        let mut s = secs.clone();
        s[i].2 = p;
        match project_from_bytes(&assemble(LUP_VERSION_CLIPS, &s)) {
            Ok((doc, _)) => {
                doc.validate_clips().unwrap();
                // What opened saves and reopens unchanged.
                let again = project_to_bytes(&doc).unwrap();
                let (back, _) = project_from_bytes(&again).unwrap();
                assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
                opened += 1;
            }
            Err(e) => {
                assert!(e.user_message().ends_with('.'));
                refused_n += 1;
            }
        }
    }
    assert!(
        opened > 0 && refused_n > 0,
        "{opened} opened, {refused_n} refused"
    );
}

/// Owned contents stream through the same batches as tracks: a large one
/// round trips exactly.
#[test]
fn a_large_owned_content_round_trips() {
    let mut d = base();
    let t1 = tid(&d, 1);
    let c = d.clips_mut();
    let cid = c.new_content();
    let id = c.new_clip_id();
    c.set_placement(ClipPlacement {
        id,
        track: t1,
        content: ContentRef::Owned(cid),
        ..ClipPlacement::primary(t1)
    });
    let notes: Vec<NoteRecord> = (0..200_000u64)
        .map(|k| note(k / 3 * 10, (k % 128) as u8, (k % 5) as u32, 0))
        .collect();
    c.content_mut(cid)
        .unwrap()
        .notes
        .insert_sorted_batch(&notes, false)
        .unwrap();
    let bytes = project_to_bytes(&d).unwrap();
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert!(report.notes >= 200_000);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert_eq!(
        back.clips().content(ContentId(0)).unwrap().notes.len(),
        200_000
    );
}

include!("../../tests/fixtures/v08/semantic_hashes.rs");

/// A project v0.8 saved, given clips in v0.9 (§040, §216): its tracks keep
/// every content hash v0.8 computed (notes, channels, subchannels, groups,
/// events, tempo, structure incl. folders); only where things play changes;
/// the clips and the folders survive saving and reopening together.
#[test]
fn a_v08_project_given_clips_keeps_its_content_and_folders() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v08/");
    let bytes = std::fs::read(format!("{dir}large_tree_v08.lup")).unwrap();
    let want = std::fs::read_to_string(format!("{dir}large_tree_v08.hashes")).unwrap();
    let (mut d, _) = project_from_bytes(&bytes).unwrap();
    assert!(d.clips_are_identity());
    let (t1, t2) = (tid(&d, 1), tid(&d, 2));
    let c = d.clips_mut();
    c.set_placement(ClipPlacement {
        start: 7680,
        name: Some("Moved".into()),
        ..ClipPlacement::primary(t1)
    });
    let link = c.new_clip_id();
    c.set_placement(ClipPlacement {
        id: link,
        track: t2,
        start: 96_000,
        ..ClipPlacement::primary(t1)
    });
    d.validate_clips().unwrap();
    let saved = project_to_bytes(&d).unwrap();
    assert_eq!(version_of(&saved), LUP_VERSION_CLIPS);
    let (back, _) = project_from_bytes(&saved).unwrap();
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert!(!back.project().folders.folders().is_empty());
    let got = hashes_text(&back);
    let content = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| !l.contains("fingerprint"))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(content(&got), content(&want), "track contents unchanged");
    assert_ne!(
        got, want,
        "where clips play is music: the fingerprints move"
    );
}
