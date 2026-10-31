//! v0.9 256-Key Mode — model layer (contract §036, §058, §059, §206;
//! slice V09-S20). Pitches 0–255 live in the same 32-bit note word; standard
//! projects are byte-identical to v0.8.

use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Division, NoteAttrs, NoteOffKind, NoteRecord, NoteStore, PlumDocument, SubchannelId,
};
use plumspirit_engine::native::LUP_VERSION;
use plumspirit_engine::native::lup::{LupControl, read_project, write_project};

fn rec(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 10,
        attrs: NoteAttrs::new_wide(pitch, 90, 3, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn ctl() -> LupControl<'static> {
    LupControl {
        cancel: None,
        progress_notes: None,
    }
}

fn lup_bytes(doc: &PlumDocument) -> Vec<u8> {
    let mut c = std::io::Cursor::new(Vec::new());
    write_project(doc, &mut c, ctl()).unwrap();
    c.into_inner()
}

#[test]
fn key_range_defaults_to_standard_and_sizes_pitch_tables() {
    assert_eq!(KeyRange::default(), KeyRange::Standard);
    assert_eq!(
        (KeyRange::Standard.keys(), KeyRange::Standard.max_pitch()),
        (128, 127)
    );
    assert_eq!(
        (
            KeyRange::Extended256.keys(),
            KeyRange::Extended256.max_pitch()
        ),
        (256, 255)
    );
    assert!(!KeyRange::Standard.contains(128) && KeyRange::Extended256.contains(255));
    let d = PlumDocument::new_empty(Division::from_raw(960));
    assert_eq!(d.project().keys, KeyRange::Standard);
}

#[test]
fn stores_hold_and_query_256_key_pitches_without_extra_memory() {
    let v: Vec<NoteRecord> = (0..=255u8).map(|p| rec(u64::from(p) * 5, p)).collect();
    let s = NoteStore::from_sorted_records(&v, false).unwrap();
    let mut seen = Vec::new();
    s.query(0, u64::MAX, |_, n| seen.push(n.attrs.pitch()));
    assert_eq!(seen, (0..=255u8).collect::<Vec<_>>());
    assert_eq!(std::mem::size_of::<NoteAttrs>(), 4);
}

#[test]
fn standard_projects_save_exactly_as_before_and_wide_pitches_need_the_new_format() {
    // A standard project: version 5, the v0.8 bytes (no note word has bit 27,
    // no key-range field), saved again byte for byte.
    let mut d = PlumDocument::new_empty(Division::from_raw(960));
    let t = d.add_track().unwrap();
    let v: Vec<NoteRecord> = (0..128u8).map(|p| rec(u64::from(p), p)).collect();
    d.track_mut(t).unwrap().notes = NoteStore::from_sorted_records(&v, false).unwrap();
    let bytes = lup_bytes(&d);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), LUP_VERSION);
    let (back, report) = read_project(&bytes[..], Some(bytes.len() as u64), ctl()).unwrap();
    assert_eq!(report.version, LUP_VERSION);
    assert_eq!(back.project().keys, KeyRange::Standard);
    assert_eq!(lup_bytes(&back), bytes);
    // A note above 127 in a standard project is damage (as in v0.8): the
    // version 5 file it would make does not open, so a save is refused by its
    // verification and never replaces the old file.
    let mut v = v;
    v.push(rec(500, 200));
    d.track_mut(t).unwrap().notes = NoteStore::from_sorted_records(&v, false).unwrap();
    let bytes = lup_bytes(&d);
    assert!(read_project(&bytes[..], Some(bytes.len() as u64), ctl()).is_err());
}

/// The 256-key format (contract §060, §242): a 256-Key Mode project saves as
/// version 6 with its key range; notes 0 / 127 / 128 / 200 / 255 (on every
/// channel kind and subchannel) save and reopen without truncation, the
/// post-save verification passes, saving again is byte-identical; v0.8 (which
/// reads up to version 5) refuses the file instead of misreading it; the
/// same note words in a version 5 header are refused.
#[test]
fn a_256_key_project_saves_and_reopens_every_key() {
    use plumspirit_engine::native::lup::verify_project;
    use plumspirit_engine::native::{LUP_VERSION_256, LUP_VERSION_CLIPS, LupError};
    let keys = [0u8, 127, 128, 200, 255, 200, 128];
    let d = wide_doc(KeyRange::Extended256, &keys);
    let bytes = lup_bytes(&d);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), LUP_VERSION_256);
    verify_project(&d, &bytes[..], Some(bytes.len() as u64)).unwrap();
    let (back, report) = read_project(&bytes[..], Some(bytes.len() as u64), ctl()).unwrap();
    assert_eq!(report.version, LUP_VERSION_256);
    assert_eq!(report.migrated_from, None);
    assert_eq!(back.project().keys, KeyRange::Extended256);
    assert_eq!(pitches(&back), keys.to_vec());
    assert_eq!(back.fingerprint(), d.fingerprint());
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert_eq!(lup_bytes(&back), bytes, "saving again is byte-identical");
    // The key range is part of the verified state: a standard document does
    // not match the file.
    let mut std = d.clone();
    std.project_mut().keys = KeyRange::Standard;
    assert_ne!(std.state_fingerprint(), d.state_fingerprint());
    assert!(verify_project(&std, &bytes[..], Some(bytes.len() as u64)).is_err());
    // v0.8 reads up to version 5 and refuses anything newer.
    assert!(u16::from_le_bytes([bytes[8], bytes[9]]) > LUP_VERSION);
    // The same file labelled version 5: the wide note words are damage.
    let mut v5 = bytes.clone();
    v5[8..10].copy_from_slice(&LUP_VERSION.to_le_bytes());
    let crc = plumspirit_engine::codec::Crc32::of(&v5[..28]);
    v5[28..32].copy_from_slice(&crc.to_le_bytes());
    assert!(read_project(&v5[..], Some(v5.len() as u64), ctl()).is_err());
    // Version 7 (clips) keeps the version 6 layout: the same file labelled 7
    // opens with the same keys.
    let mut v7 = bytes.clone();
    v7[8..10].copy_from_slice(&LUP_VERSION_CLIPS.to_le_bytes());
    let crc = plumspirit_engine::codec::Crc32::of(&v7[..28]);
    v7[28..32].copy_from_slice(&crc.to_le_bytes());
    let (back7, _) = read_project(&v7[..], Some(v7.len() as u64), ctl()).unwrap();
    assert_eq!(back7.state_fingerprint(), d.state_fingerprint());
    // A newer version than this release reads is refused by name.
    let mut v8 = bytes.clone();
    v8[8..10].copy_from_slice(&(LUP_VERSION_CLIPS + 1).to_le_bytes());
    let crc = plumspirit_engine::codec::Crc32::of(&v8[..28]);
    v8[28..32].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        read_project(&v8[..], Some(v8.len() as u64), ctl()).unwrap_err(),
        LupError::NewerVersion(LUP_VERSION_CLIPS + 1)
    );
}

fn wide_doc(keys: KeyRange, pitches: &[u8]) -> PlumDocument {
    let mut d = PlumDocument::new_empty(Division::from_raw(960));
    d.project_mut().keys = keys;
    let t = d.add_track().unwrap();
    let v: Vec<NoteRecord> = pitches
        .iter()
        .enumerate()
        .map(|(i, &p)| rec(i as u64 * 10, p))
        .collect();
    let mut s = NoteStore::from_sorted_records(&v, false).unwrap();
    s.select_all();
    d.track_mut(t).unwrap().notes = s;
    d
}

fn pitches(d: &PlumDocument) -> Vec<u8> {
    let mut v = Vec::new();
    d.track(1)
        .unwrap()
        .notes
        .query(0, u64::MAX, |_, n| v.push(n.attrs.pitch()));
    v
}

/// Move / Transpose, Flip Y and Scale Y honour the key range: 0–255 in
/// 256-Key Mode, 0–127 otherwise; history (undo / redo of the pitch table
/// and of the record shift) keeps pitches above 127.
#[test]
fn pitch_edits_follow_the_key_range_and_undo_keeps_wide_pitches() {
    use plumspirit_engine::editor::{self, EditControl, ExactFactor};
    use plumspirit_engine::history::{History, HistoryLimits, Operation};
    let mut d = wide_doc(KeyRange::Extended256, &[200, 230, 250]);
    let mut h = History::new(HistoryLimits::default());
    let apply = |d: &mut PlumDocument, h: &mut History, e| {
        h.apply(
            d,
            Operation::Prepared {
                edit: e,
                label: "edit",
            },
        )
        .unwrap();
    };
    // Transpose (pitch table) up to 255 exactly; one more is refused.
    let e = editor::prepare_move(&d, 1, 0, 5, EditControl::default()).unwrap();
    apply(&mut d, &mut h, e);
    assert_eq!(pitches(&d), [205, 235, 255]);
    assert!(editor::prepare_move(&d, 1, 0, 1, EditControl::default()).is_err());
    // Move in time and pitch (record shift).
    let e = editor::prepare_move(&d, 1, 7, -100, EditControl::default()).unwrap();
    apply(&mut d, &mut h, e);
    assert_eq!(pitches(&d), [105, 135, 155]);
    // Flip Y and Scale Y keep the range.
    let e = editor::prepare_flip_y(&d, 1).unwrap();
    apply(&mut d, &mut h, e);
    assert_eq!(pitches(&d), [155, 125, 105]);
    let e = editor::prepare_scale_y(&d, 1, &ExactFactor::parse("2").unwrap()).unwrap();
    apply(&mut d, &mut h, e);
    assert_eq!(pitches(&d), [180, 120, 80]);
    assert!(editor::prepare_scale_y(&d, 1, &ExactFactor::parse("3").unwrap()).is_err());
    // Undo all the way back, then redo all the way forward.
    for _ in 0..4 {
        h.undo(&mut d).unwrap();
    }
    assert_eq!(pitches(&d), [200, 230, 250]);
    for _ in 0..4 {
        h.redo(&mut d).unwrap();
    }
    assert_eq!(pitches(&d), [180, 120, 80]);
    // The same edits in a standard project stop at 127.
    let d = wide_doc(KeyRange::Standard, &[100, 120, 125]);
    assert!(editor::prepare_move(&d, 1, 0, 3, EditControl::default()).is_err());
    assert!(editor::prepare_move(&d, 1, 0, 2, EditControl::default()).is_ok());
    let b = d.track(1).unwrap().notes.selection_bounds().unwrap();
    assert_eq!(editor::clamp_move_delta(&b, 0, 50, KeyRange::Standard).1, 2);
    assert_eq!(
        editor::clamp_move_delta(&b, 0, 500, KeyRange::Extended256).1,
        130
    );
}

/// The transform platform runs in the project's key range: in 256-Key Mode a
/// tool may write pitches up to 255 and its boundary policy acts at 255; in
/// a standard project the same run is limited to 127 as before.
#[test]
fn tools_run_in_the_project_key_range() {
    use plumspirit_engine::transform::param::{ParamSet, ParamValue};
    use plumspirit_engine::transform::registry::{BuildCtx, tool};
    use plumspirit_engine::transform::run::{collect_for, run_kernel};
    use plumspirit_engine::transform::{Ctx, Limits, TrackScope};
    let run = |d: &PlumDocument, semis: i64, octaves: i64, policy: u32| {
        let t = tool("transpose").unwrap();
        let mut ps = ParamSet::defaults(&(t.spec)().params);
        ps.set("semis", ParamValue::Int(semis));
        ps.set("octaves", ParamValue::Int(octaves));
        ps.set("b.pitch", ParamValue::Choice(policy));
        let sc = [TrackScope {
            index: 1,
            all: false,
            content: None,
        }];
        let src = collect_for(t, d, &sc, 1 << 24).unwrap();
        let mut ctx = Ctx::simple(960);
        ctx.keys = d.project().keys;
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let k = t.kernel(&ps, &b).unwrap();
        run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
            .map(|r| r.notes.iter().map(|n| n.pitch).collect::<Vec<_>>())
    };
    let wide = wide_doc(KeyRange::Extended256, &[100, 120, 126]);
    assert_eq!(run(&wide, 120, 0, 5).unwrap(), [220, 240, 246]);
    assert!(
        run(&wide, 127, 1, 5).is_err(),
        "refused: 265 would leave 0–255"
    );
    assert_eq!(
        run(&wide, 127, 1, 1).unwrap(),
        [239, 255, 255],
        "clamped at 255"
    );
    let std = wide_doc(KeyRange::Standard, &[100, 120, 126]);
    assert!(
        run(&std, 2, 0, 5).is_err(),
        "refused: 128 would leave 0–127"
    );
    assert_eq!(
        run(&std, 10, 0, 1).unwrap(),
        [110, 127, 127],
        "clamped at 127"
    );
}

/// Tool parameters follow the key range (V09-S20 step 6): keyed bounds and
/// defaults of ±127 are ±255 in a 256-key project, octave registers reach
/// ten octaves further, and a standard project reads exactly as before.
#[test]
fn keyed_tool_parameters_follow_the_key_range() {
    use plumspirit_engine::transform::param::{Keyed, ParamSet, ParamValue, Params, int, pitch};
    let (std, wide) = (KeyRange::Standard, KeyRange::Extended256);
    let specs = vec![
        pitch("to", "To", 127),
        int("semis", "Semitones", -127, 127, 0).keyed(),
        int("from", "From", -127, 1_000_000, 40).keyed(),
        int("oct", "Octave", 0, 9, 4).keyed_octaves(),
        int("octs", "Octaves", -10, 10, 0).keyed_octaves(),
        int("vel", "Velocity", 1, 127, 127),
    ];
    assert_eq!(specs[0].keyed, Keyed::Keys, "pitch parameters are keyed");
    let ranges = |k| {
        specs
            .iter()
            .map(|s| s.range_in(k).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ranges(std),
        [
            (0, 127),
            (-127, 127),
            (-127, 1_000_000),
            (0, 9),
            (-10, 10),
            (1, 127)
        ]
    );
    assert_eq!(
        ranges(wide),
        [
            (0, 255),
            (-255, 255),
            (-255, 1_000_000),
            (0, 19),
            (-20, 20),
            (1, 127)
        ],
        "velocity is not keyed"
    );
    assert_eq!(
        ParamSet::defaults_in(&specs, std),
        ParamSet::defaults(&specs)
    );
    let d = ParamSet::defaults_in(&specs, wide);
    assert_eq!(d.get_raw("to"), Some(&ParamValue::Int(255)), "the top key");
    assert_eq!(d.get_raw("vel"), Some(&ParamValue::Int(127)));
    let mut set = ParamSet::new();
    set.set("to", ParamValue::Int(200));
    set.set("semis", ParamValue::Int(-200));
    set.set("oct", ParamValue::Int(17));
    let p = Params::new(&specs, &set);
    assert_eq!((p.int("to"), p.int("semis"), p.int("oct")), (127, -127, 9));
    let p = Params::new(&specs, &set).with_keys(wide);
    assert_eq!((p.int("to"), p.int("semis"), p.int("oct")), (200, -200, 17));
    // Missing values take the key range's defaults.
    let empty = ParamSet::new();
    assert_eq!(Params::new(&specs, &empty).with_keys(wide).int("to"), 255);
    assert_eq!(Params::new(&specs, &empty).int("to"), 127);
}

/// Row masks and occupancy grids cover the project's keyboard: 128 rows in
/// a standard project (as v0.8), 256 in 256-Key Mode — pitch 200 is its own
/// row, never folded onto 127.
#[test]
fn row_masks_and_grids_cover_the_keyboard() {
    use plumspirit_engine::transform::TNote;
    use plumspirit_engine::transform::spatial::{Grid, RowMask};
    let n = |s: u64, p: u8| TNote {
        start: s,
        dur: 100,
        pitch: p,
        vel: 90,
        ..TNote::default()
    };
    let notes = [n(0, 127), n(200, 200), n(400, 255)];
    let m = RowMask::from_notes(KeyRange::Extended256, &notes);
    assert_eq!(m.row(127), &[(0, 100)]);
    assert_eq!(m.row(200), &[(200, 300)]);
    assert_eq!(m.row(255), &[(400, 500)]);
    assert_eq!(m.expand(0, 2).row(254), &[(400, 500)]);
    // Contracting by a row: the keyboard's top row (255) touches the outside.
    assert!(m.contract(0, 1, 0, 1000).row(255).is_empty());
    let s = RowMask::from_notes(KeyRange::Standard, &notes);
    assert_eq!(s.row(127), &[(0, 100)], "standard: row 127 only");
    assert!(s.row(200).is_empty(), "outside the standard keyboard");
    let idx = [0, 1, 2];
    let g = Grid::build(KeyRange::Extended256, &notes, &idx, 0, 500, 10, 1, 0);
    assert_eq!((g.h, g.row_of(200), g.pitch_of(g.h - 1)), (256, 200, 255));
    let g = Grid::build(KeyRange::Standard, &notes[..1], &[0], 0, 500, 10, 1, 0);
    assert_eq!((g.h, g.pitch_of(g.h - 1)), (128, 127));
}

/// HZ-Chop and SAFOR in 256-Key Mode: key frequencies continue the map's
/// octave doubling above 127, and SAFOR keeps key 200 apart from key 72
/// (v0.8 masked the pitch to 7 bits: 200 & 127 = 72).
#[test]
fn hz_chop_and_safor_handle_keys_above_127() {
    use plumspirit_engine::transform::TNote;
    use plumspirit_engine::transform::tools::plugins::{hz_of, safor_sustains};
    for k in 128..=255u8 {
        assert_eq!(hz_of(k), 2.0 * hz_of(k - 12), "key {k}");
    }
    assert!(hz_of(255) > hz_of(254));
    let n = |s: u64, d: u64, p: u8| TNote {
        start: s,
        dur: d,
        pitch: p,
        vel: 90,
        ..TNote::default()
    };
    // Key 72 sustains over 0–1000; key 200 starts inside it.
    let (pieces, changed, _) = safor_sustains(&[n(0, 1000, 72), n(500, 100, 200)]);
    assert_eq!(changed, 0, "different keys never cut each other");
    let mut got: Vec<(u64, u64, u8)> = pieces.iter().map(|p| (p.start, p.dur, p.pitch)).collect();
    got.sort();
    assert_eq!(got, [(0, 1000, 72), (500, 100, 200)]);
}

/// Pasted artwork is checked against the project's keys: a standard project
/// refuses notes above 127 (v0.8 folded them onto 127), a 256-key project
/// writes them as they are.
#[test]
fn pasted_artwork_is_checked_against_the_key_range() {
    use plumspirit_engine::history::History;
    use plumspirit_engine::transform::paste::{self, ColorInfo, ColorNote, Generated, PasteSpec};
    let art = Generated {
        notes: vec![
            ColorNote {
                start: 0,
                dur: 10,
                pitch: 100,
                vel: 90,
                color: 0,
            },
            ColorNote {
                start: 0,
                dur: 10,
                pitch: 200,
                vel: 90,
                color: 0,
            },
        ],
        colors: vec![ColorInfo::new([255, 0, 0])],
    };
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_empty(Division::from_raw(960));
        d.project_mut().keys = keys;
        d.add_track().unwrap();
        let spec = PasteSpec::new(d.track(0).unwrap().id(), 0);
        let plan = paste::plan(&d, &art, &spec);
        if keys == KeyRange::Standard {
            let e = plan.error.expect("refused");
            assert!(e.contains("200") && e.contains("127"), "{e}");
            continue;
        }
        assert!(plan.error.is_none(), "{:?}", plan.error);
        let op = paste::build(&d, &art, &spec, &plan, "Paste").unwrap();
        History::default().apply(&mut d, op).unwrap();
        let mut got: Vec<u8> = (0..d.track_count())
            .flat_map(|t| {
                d.track(t)
                    .unwrap()
                    .notes
                    .iter()
                    .map(|v| v.attrs.pitch())
                    .collect::<Vec<_>>()
            })
            .collect();
        got.sort();
        assert_eq!(got, [100, 200]);
    }
}

/// Image to MIDI and Spiderweb take their keys from the keyboard they are
/// given: the last key may reach 255 in 256-Key Mode (127 otherwise), and
/// Spiderweb's notes outside its board rows make nothing.
#[test]
fn image_to_midi_and_spiderweb_use_the_given_keyboard() {
    use plumspirit_engine::spiderweb::{edit, output};
    use plumspirit_engine::transform::image_midi::{Rgba, Settings, prepare};
    let img = Rgba::new(2, 2, vec![[255, 0, 0, 255]; 4]).unwrap();
    let s = Settings {
        first_key: 100,
        last_key: 200,
        ..Default::default()
    };
    let e = prepare(&img, &s).unwrap_err();
    assert!(e.contains("0 to 127"), "{e}");
    let s = Settings {
        keys: KeyRange::Extended256,
        ..s
    };
    let p = prepare(&img, &s).unwrap();
    assert_eq!(p.h, 101, "one row per key 100–200");
    let rendered: Vec<[i64; 6]> = vec![[0, 10, 60, 90, 0, 0], [0, 10, 200, 90, 0, 0]];
    let (a, _) = output::to_generated(&rendered, 1, true, 128);
    assert_eq!(a.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), [60]);
    let (a, _) = output::to_generated(&rendered, 1, true, 256);
    assert_eq!(
        a.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        [60, 200]
    );
    // Notes taken from a 256-key project keep their keys in the shape.
    assert!(edit::notes_shape(&[[0, 100, 200, 90, 0]], 960, "Notes").is_some());
}

/// Track corrections follow the key range (V09-S20): in a 256-key project
/// the default key range (0–127, "the whole keyboard") reaches 255 and the
/// key correction may move notes up to 255; a standard project still
/// filters and refuses at 127.
#[test]
fn track_corrections_follow_the_key_range() {
    use plumspirit_engine::model::correction::{Corrected, Correction, check};
    use plumspirit_engine::model::track::TrackProps;
    use plumspirit_engine::view::grid::MeterMap;
    let meter = MeterMap::new(Division::from_raw(960), &[]);
    let props = TrackProps {
        key_correction: 12,
        ..TrackProps::default()
    };
    let pitch_of = |c: &Correction, p: u8| match c.note(rec(0, p)) {
        Corrected::Note { note, .. } => Some(note.attrs.pitch()),
        _ => None,
    };
    let wide = Correction::resolve(&props, &meter, KeyRange::Extended256).unwrap();
    assert_eq!(wide.range, (0, 255));
    assert_eq!(pitch_of(&wide, 200), Some(212));
    assert_eq!(pitch_of(&wide, 120), Some(132));
    assert_eq!(pitch_of(&wide, 250), None, "262 is above 255");
    let std = Correction::resolve(&props, &meter, KeyRange::Standard).unwrap();
    assert_eq!(std.range, (0, 127));
    assert_eq!(pitch_of(&std, 115), Some(127));
    assert_eq!(pitch_of(&std, 120), None, "132 is above 127");
    // A narrower key range stays as it is.
    let narrow = TrackProps {
        key_range: (60, 100),
        ..TrackProps::default()
    };
    let c = Correction::resolve(&narrow, &meter, KeyRange::Extended256).unwrap();
    assert_eq!(c.range, (60, 100));
    // The report names the range that was left.
    let d = wide_doc(KeyRange::Extended256, &[250]);
    let report = check(d.track(1).unwrap(), 1, &wide);
    assert_eq!(report.pitch_overflow, 1);
    assert!(report.notes("Track 1")[0].contains("(0–255)"));
}

/// Track corrections persist in the key range (V09-S21 step 1): a 256-key
/// project keeps a key correction of +200 and a key range up to 255 through
/// the undo history, the persistent journal (restart, undo, redo) and the
/// `.lup`; a standard project refuses them.
#[test]
fn wide_track_corrections_persist_in_the_256_key_format() {
    use plumspirit_engine::history::journal::{Journal, identity, path_key};
    use plumspirit_engine::history::{History, HistoryLimits, Operation};
    use plumspirit_engine::model::track::TrackProps;
    use plumspirit_engine::native::{load_project, save_project};
    let props = TrackProps {
        key_correction: 200,
        key_range: (20, 255),
        ..TrackProps::default()
    };
    let op = |d: &PlumDocument| Operation::TrackProps {
        track: d.track(1).unwrap().id(),
        props: props.clone(),
        end: None,
        label: "Track Properties",
    };
    // Standard: refused.
    let mut std = wide_doc(KeyRange::Standard, &[10, 20]);
    let mut h = History::default();
    let o = op(&std);
    assert!(h.apply(&mut std, o).is_err());
    // 256 keys: history + journal + .lup.
    let dir = std::env::temp_dir().join(format!("plumspirit-v09keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("wide.lup");
    let jpath = dir.join("wide.plh");
    let mut d = wide_doc(KeyRange::Extended256, &[10, 200]);
    save_project(&d, &file, ctl()).unwrap();
    let before = identity(&d);
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(before)).unwrap();
    let o = op(&d);
    h.apply(&mut d, o).unwrap();
    assert_eq!(d.track(1).unwrap().props(), &props);
    let after = identity(&d);
    save_project(&d, &file, ctl()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, after);
    drop(h);
    // Restart: the file and the journal give back the corrections.
    let (mut d, _) = load_project(&file, ctl()).unwrap();
    assert_eq!(d.project().keys, KeyRange::Extended256);
    assert_eq!(d.track(1).unwrap().props(), &props);
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&d)).unwrap();
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(identity(&d), before);
    h.redo(&mut d).unwrap().unwrap();
    assert_eq!(identity(&d), after);
    assert_eq!(d.track(1).unwrap().props(), &props);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Copy / paste, prepared edits and presets keep 0–255 (V09-S21 step 2):
/// notes copied in a 256-key project paste with their keys into a 256-key
/// project and are refused by a standard one; any prepared edit that would
/// put a pitch above 127 into a standard project is refused at commit (no
/// change, no undo step); a preset keeps a pitch of 200 as text and the tool
/// reads it in the project's key range.
#[test]
fn clipboard_edits_and_presets_keep_wide_pitches() {
    use plumspirit_engine::editor::{EditError, PreparedEdit, clipboard::Clipboard};
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::transform::param::{ParamSet, ParamValue, Params, pitch};
    use plumspirit_engine::transform::preset::{Preset, parse, to_text};
    let src = wide_doc(KeyRange::Extended256, &[60, 200, 255]);
    let clip = Clipboard::copy_selected(src.track(1).unwrap())
        .unwrap()
        .unwrap();
    let mut wide = wide_doc(KeyRange::Extended256, &[10]);
    let edit = clip.prepare_paste(&wide, 1, 1000).unwrap();
    let mut h = History::default();
    h.apply(
        &mut wide,
        Operation::Prepared {
            edit,
            label: "Paste",
        },
    )
    .unwrap();
    let mut got = pitches(&wide);
    got.sort();
    assert_eq!(got, [10, 60, 200, 255]);
    let std = wide_doc(KeyRange::Standard, &[10]);
    assert!(matches!(
        clip.prepare_paste(&std, 1, 1000),
        Err(EditError::PitchOutOfRange { top: 127, .. })
    ));
    // A prepared insert carrying pitch 200 into a standard project.
    let mut std = std;
    let before = std.state_fingerprint();
    let t = std.track(1).unwrap();
    let edit = PreparedEdit::InsertSelected {
        host: plumspirit_engine::model::ContentRef::Track(t.id()),
        base_revision: t.notes.revision(),
        records: vec![rec(5000, 200)],
        subs_track: t.id(),
        new_subs: Vec::new(),
    };
    let mut h = History::default();
    assert!(
        h.apply(
            &mut std,
            Operation::Prepared {
                edit,
                label: "Insert"
            }
        )
        .is_err()
    );
    assert_eq!(std.state_fingerprint(), before);
    assert_eq!(h.undo_len(), 0);
    // Presets: values are text; the tool reads them in the project's range.
    let specs = vec![pitch("to", "To", 127)];
    let mut values = ParamSet::new();
    values.set("to", ParamValue::Int(200));
    let text = to_text(&Preset {
        tool: "x".into(),
        name: "Wide".into(),
        values,
    });
    let (back, skipped) = parse(&text, "x", &specs).unwrap();
    assert!(skipped.is_empty());
    assert_eq!(back.values.get_raw("to"), Some(&ParamValue::Int(200)));
    assert_eq!(
        Params::new(&specs, &back.values)
            .with_keys(KeyRange::Extended256)
            .int("to"),
        200
    );
    assert_eq!(Params::new(&specs, &back.values).int("to"), 127);
}

/// 256-Key Mode is a Project setting changed through the undo history
/// (V09-S21 step 3a): turning it on or off is one undo step with its own
/// label, kept by the persistent journal across a restart; turning it off is
/// refused with a sentence (no change, no undo step) while a note is above
/// key 127 or a track's key range / key correction needs the wide range.
/// Drawing a note above the project's top key is refused too.
#[test]
fn turning_256_key_mode_on_and_off_is_one_undo_step_kept_by_the_journal() {
    use plumspirit_engine::history::journal::{Journal, identity, path_key};
    use plumspirit_engine::history::{History, HistoryLimits, Operation, Outcome};
    use plumspirit_engine::model::track::TrackProps;
    use plumspirit_engine::native::{load_project, save_project};
    let on = || Operation::SetKeyRange {
        keys: KeyRange::Extended256,
    };
    let off = || Operation::SetKeyRange {
        keys: KeyRange::Standard,
    };
    let dir = std::env::temp_dir().join(format!("plumspirit-v09mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("mode.lup");
    let jpath = dir.join("mode.plh");
    let mut d = wide_doc(KeyRange::Standard, &[10, 100, 127]);
    save_project(&d, &file, ctl()).unwrap();
    let before = identity(&d);
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(before)).unwrap();

    // A standard project refuses to draw above 127 (no undo step).
    let high = || Operation::InsertNote {
        track: 1.into(),
        record: rec(500, 200),
    };
    let e = h.apply(&mut d, high()).unwrap_err().user_message();
    assert!(e.contains("above the highest MIDI note (127)"), "{e}");
    assert_eq!((h.undo_len(), identity(&d)), (0, before));
    // Turning it off when it is off changes nothing.
    assert_eq!(h.apply(&mut d, off()).unwrap(), Outcome::Unchanged);
    assert_eq!(h.undo_len(), 0);

    // On: one step; the content is unchanged, the fingerprint is not.
    assert_eq!(h.apply(&mut d, on()).unwrap(), Outcome::Done);
    assert_eq!(d.project().keys, KeyRange::Extended256);
    assert_eq!(h.undo_label(), Some("Turn On 256-Key Mode"));
    assert_eq!(pitches(&d), vec![10, 100, 127]);
    let wide = identity(&d);
    assert_ne!(wide, before);
    let a = h.undo(&mut d).unwrap().unwrap();
    assert_eq!(
        (a.label, a.track, a.structure_changed),
        ("Turn On 256-Key Mode", None, false)
    );
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Standard, before)
    );
    h.redo(&mut d).unwrap().unwrap();
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Extended256, wide)
    );

    // A note at 200 now draws; turning the mode off is refused by name.
    h.apply(&mut d, high()).unwrap();
    assert_eq!(pitches(&d), vec![10, 100, 127, 200]);
    let drawn = identity(&d);
    let e = h.apply(&mut d, off()).unwrap_err().user_message();
    assert!(
        e.contains("1 note is above key 127 (the highest is 200")
            && e.contains("Nothing was changed."),
        "{e}"
    );
    assert_eq!((h.undo_len(), identity(&d)), (2, drawn));
    assert!(
        h.apply_without_undo(&mut d, off()).is_err() && identity(&d) == drawn,
        "the no-undo path refuses too"
    );
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(identity(&d), wide);

    // A track key range up to 200 also keeps the mode on.
    let tid = d.track(1).unwrap().id();
    let props = TrackProps {
        key_range: (20, 200),
        ..TrackProps::default()
    };
    h.apply(
        &mut d,
        Operation::TrackProps {
            track: tid,
            props,
            end: None,
            label: "Track Properties",
        },
    )
    .unwrap();
    let e = h.apply(&mut d, off()).unwrap_err().user_message();
    assert!(
        e.contains("key range 20–200") && e.contains("Track Properties"),
        "{e}"
    );
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(identity(&d), wide);

    // Off again: a second step; save and keep the journal.
    h.apply(&mut d, off()).unwrap();
    assert_eq!(h.undo_label(), Some("Turn Off 256-Key Mode"));
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Standard, before)
    );
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(identity(&d), wide);
    save_project(&d, &file, ctl()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, wide);
    drop(h);

    // Restart: the file is a 256-key project; the journal undoes to the
    // standard project and redoes to 256 keys again, exactly.
    let (mut d, _) = load_project(&file, ctl()).unwrap();
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Extended256, wide)
    );
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&d)).unwrap();
    assert_eq!(h.undo_label(), Some("Turn On 256-Key Mode"));
    assert_eq!(h.redo_label(), Some("Turn Off 256-Key Mode"));
    h.redo(&mut d).unwrap().unwrap();
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Standard, before)
    );
    h.undo(&mut d).unwrap().unwrap();
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Standard, before)
    );
    h.redo(&mut d).unwrap().unwrap();
    assert_eq!(
        (d.project().keys, identity(&d)),
        (KeyRange::Extended256, wide)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Piano Roll's pitch axis follows the key range: 128 rows with 127 on
/// top in a standard project (the v0.8 numbers), 256 rows with 255 on top in
/// 256-Key Mode; every key maps to its row and back.
#[test]
fn the_pitch_axis_has_one_row_per_key_of_the_project() {
    use plumspirit_engine::view::PitchAxis;
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let a = PitchAxis {
            scroll_rows: 0.0,
            row_height: 4.0,
            top: keys.max_pitch(),
        };
        assert_eq!(a.rows(), keys.keys() as f64);
        assert_eq!(a.pitch_top_y(keys.max_pitch()), 0.0);
        assert_eq!(a.max_scroll(64.0), keys.keys() as f64 - 16.0);
        for p in 0..=keys.max_pitch() {
            assert_eq!(a.y_to_pitch(a.pitch_top_y(p) + 2.0), Some(p));
            assert_eq!(a.y_to_pitch_clamped(a.pitch_top_y(p) + 2.0), p);
        }
        assert_eq!(a.y_to_pitch(-0.5), None);
        assert_eq!(a.y_to_pitch(keys.keys() as f64 * 4.0), None);
    }
}

/// Hz Bass tones keep keys 128–255 (V09-S21 step 4b): the board's keyboard
/// (128 or 256 rows) decides what sounds, so a tone drawn in a 256-key
/// project survives a standard project unchanged.
#[test]
fn hz_bass_tones_keep_keys_above_127() {
    use plumspirit_engine::json;
    use plumspirit_engine::spiderweb::hzbass::Hz;
    let v = json::parse(
        r#"{"key":60,"bpm":120,"tones":[{"t":0,"len":1,"key":200},{"t":1,"len":1,"key":60},{"t":2,"len":1,"key":256}]}"#,
    )
    .unwrap();
    let hz = Hz::clean(Some(&v)).unwrap();
    assert_eq!(
        hz.tones.iter().map(|t| t.key).collect::<Vec<_>>(),
        [200, 60],
        "256 is no key; 200 is kept"
    );
}
