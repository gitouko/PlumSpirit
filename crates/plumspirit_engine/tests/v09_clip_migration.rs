//! V09-S06 / V09-0029 (contract §040, §241): projects saved before clips open
//! losslessly as one identity clip per track. The fixtures were written by
//! PlumSpirit v0.8 itself (058a094) and, for 256-Key Mode, by v0.9 before
//! clips (aff02e9); each `.hashes` file holds the semantic hashes that build
//! computed (`fixtures/v08/make_v08_fixtures.rs.txt`,
//! `fixtures/v08/make_v09_keys_fixture.rs.txt`). v0.9 must find the same
//! notes, channels, subchannels, groups, controller / meta events, tempo map
//! (ramps, signatures, markers, chapters, Master Volume) and track structure
//! (tree, properties, folders), and rewrite each file byte for byte.

use plumspirit_engine::midi::import_bytes;
use plumspirit_engine::model::track::TrackKind;
use plumspirit_engine::model::{ClipPlacement, PlumDocument};
use plumspirit_engine::native::{
    LUP_VERSION, LUP_VERSION_256, project_from_bytes, project_to_bytes, verify_project,
};

include!("fixtures/v08/semantic_hashes.rs");

/// (file stem, format version it was written in).
const FIXTURES: [(&str, u16); 7] = [
    ("empty_v08", LUP_VERSION),
    ("simple_midi_v08", LUP_VERSION),
    ("large_tree_v08", LUP_VERSION),
    ("multi_subchannel_v08", LUP_VERSION),
    ("cc_heavy_v08", LUP_VERSION),
    ("tempo_automation_v08", LUP_VERSION),
    ("keys256_v09", LUP_VERSION_256),
];

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/v08/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn hashes(name: &str) -> String {
    String::from_utf8(fixture(name)).unwrap()
}

fn uvarint(b: &[u8], pos: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let byte = b[*pos];
        *pos += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// The sections of a `.lup` file: (tag, whole section bytes incl. checksum,
/// payload range).
fn sections(b: &[u8]) -> Vec<([u8; 4], &[u8], std::ops::Range<usize>)> {
    let count = u32::from_le_bytes(b[16..20].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(count);
    let mut at = 32;
    for _ in 0..count {
        let tag: [u8; 4] = b[at..at + 4].try_into().unwrap();
        let len = u64::from_le_bytes(b[at + 8..at + 16].try_into().unwrap()) as usize;
        let payload = at + 16..at + 16 + len;
        let end = payload.end + 4;
        out.push((tag, &b[at..end], payload));
        at = end;
    }
    assert_eq!(at, b.len(), "nothing follows the last section");
    out
}

/// `again` is `old` byte for byte — header, every section and checksum —
/// except META's informational writer text, which must name this build.
fn assert_same_except_writer(old: &[u8], again: &[u8], name: &str) {
    assert_eq!(old[..32], again[..32], "{name}: header");
    let (a, b) = (sections(old), sections(again));
    assert_eq!(a.len(), b.len(), "{name}: section count");
    for ((ta, sa, pa), (tb, sb, pb)) in a.iter().zip(&b) {
        assert_eq!(ta, tb, "{name}: section order");
        if ta != b"META" {
            assert!(
                sa == sb,
                "{name}: section {} byte for byte",
                String::from_utf8_lossy(ta)
            );
            continue;
        }
        // name text, writer text, then the rest of META.
        let split = |bytes: &[u8], r: &std::ops::Range<usize>| {
            let p = &bytes[r.clone()];
            let mut pos = 0;
            let n = uvarint(p, &mut pos) as usize;
            let project = p[pos..pos + n].to_vec();
            pos += n;
            let w = uvarint(p, &mut pos) as usize;
            let writer = String::from_utf8(p[pos..pos + w].to_vec()).unwrap();
            (project, writer, p[pos + w..].to_vec())
        };
        let (na, wa, ra) = split(old, pa);
        let (nb, wb, rb) = split(again, pb);
        assert_eq!(na, nb, "{name}: META project name");
        assert!(ra == rb, "{name}: META after the writer stamp");
        assert!(
            wa.starts_with("PlumSpirit "),
            "{name}: fixture stamp {wa:?}"
        );
        assert_eq!(
            wb,
            format!("PlumSpirit {}", plumspirit_engine::VERSION),
            "{name}: writer stamp"
        );
    }
}

/// One identity clip per track and nothing else (§040).
fn assert_identity_clips(d: &PlumDocument, name: &str) {
    assert!(d.clips_are_identity(), "{name}");
    assert!(d.clips().is_empty(), "{name}");
    d.validate_clips().unwrap();
    for i in 0..d.track_count() {
        let id = d.track(i).unwrap().id();
        assert_eq!(
            d.placements_on(i),
            vec![ClipPlacement::primary(id)],
            "{name}: track {}",
            i + 1
        );
    }
}

#[test]
fn every_saved_project_opens_as_one_identity_clip_per_track_with_equal_hashes() {
    for (name, version) in FIXTURES {
        let bytes = fixture(&format!("{name}.lup"));
        let want = hashes(&format!("{name}.hashes"));
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), version, "{name}");
        let (d, report) = project_from_bytes(&bytes).unwrap();
        assert_eq!(report.migrated_from, None, "{name}: nothing to convert");
        assert!(report.warnings.is_empty(), "{name}: {:?}", report.warnings);
        assert_identity_clips(&d, name);
        assert_eq!(hashes_text(&d), want, "{name}: semantic hashes");
        // v0.9 writes exactly the bytes the older build wrote (no CLIP, same
        // version), so the older build keeps opening the file … — apart from
        // META's informational writer stamp, which names the build that saved
        // it (beta1 B1-S01: the stamp moved with the 0.9.0 / 1.0.0-beta.1 bumps).
        let again = project_to_bytes(&d).unwrap();
        assert_same_except_writer(&bytes, &again, name);
        verify_project(&d, &again[..], Some(again.len() as u64)).unwrap();
        // … and opening the rewritten file keeps everything.
        let (back, _) = project_from_bytes(&again).unwrap();
        assert_identity_clips(&back, name);
        assert_eq!(hashes_text(&back), want, "{name}: after saving again");
    }
}

#[test]
fn a_midi_file_imports_as_identity_clips_exactly_as_v08_imported_it() {
    let (d, _) = import_bytes(&fixture("simple_midi_v08.mid")).unwrap();
    assert_identity_clips(&d, "simple_midi_v08.mid");
    assert_eq!(
        hashes_text(&d),
        hashes("simple_midi_v08.mid.hashes"),
        "v0.9 imports the MIDI file as v0.8 did"
    );
}

/// The fixtures cover what §241 names: Track / Subtrack trees, several
/// subchannels, controller-heavy tracks and tempo automation (guards against
/// regenerating them with less in them).
#[test]
fn the_fixtures_contain_what_they_are_named_for() {
    let open = |n: &str| project_from_bytes(&fixture(&format!("{n}.lup"))).unwrap().0;
    let empty = open("empty_v08");
    assert_eq!(empty.note_count(), 0);
    let tree = open("large_tree_v08");
    assert!(tree.track_count() >= 20 && tree.note_count() >= 90_000);
    assert!(tree.tracks().iter().any(|t| t.parent().is_some()));
    let depth2 = tree.tracks().iter().any(|t| {
        t.parent()
            .and_then(|p| tree.track_by_id(p))
            .is_some_and(|p| p.parent().is_some())
    });
    assert!(depth2, "subtracks of subtracks");
    assert!(!tree.project().folders.folders().is_empty());
    assert!(tree.tracks().iter().any(|t| t.mute) && tree.tracks().iter().any(|t| t.solo));
    let subs = open("multi_subchannel_v08");
    assert!(subs.tracks().iter().all(|t| t.subchannels().len() == 6));
    assert!(subs.project().groups.len() >= 3);
    assert!(
        subs.tracks()
            .iter()
            .any(|t| t.notes.iter().any(|n| n.attrs.muted()))
    );
    let cc = open("cc_heavy_v08");
    let events: usize = cc.tracks().iter().map(|t| t.events.len()).sum();
    assert!(events >= 60_000, "{events} events");
    let tempo = open("tempo_automation_v08");
    let c = tempo.conductor();
    assert!(c.tempo_points().len() >= 500);
    assert!(
        c.tempo_points()
            .iter()
            .any(|p| format!("{:?}", p.interp) == "Linear")
    );
    assert!(c.markers().iter().any(|m| m.end.is_some()));
    assert!(c.time_signatures().len() >= 5 && !c.master_volume().is_empty());
    let wide = open("keys256_v09");
    assert!(
        wide.tracks()
            .iter()
            .any(|t| t.notes.iter().any(|n| n.attrs.pitch() > 127))
    );
}
