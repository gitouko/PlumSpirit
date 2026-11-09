//! B1-0026 / B1-0027: the Project API and the Tracks / Subtracks / Folders
//! API (reads).

use super::handles::{FileType, ProjectInfo};
use super::*;
use plumspirit_engine::editor::clips::add_clip;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use std::sync::Arc;

fn note(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 240,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

/// Track 1 "Piano" (8 notes, a clip, subchannel 2 on MIDI channel 5) with a
/// subtrack; track 2 in folder "Drums"; markers; 120 BPM 4/4 at 960 PPQN.
fn song() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let recs: Vec<NoteRecord> = (0..8u64).map(|k| note(k * 480, 60)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    plumspirit_engine::fixtures::set_many_markers(&mut d, 2, 1920);
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    let edit = plumspirit_engine::editor::controllers::prepare_rename(d.track(0).unwrap(), "Piano")
        .unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit,
            label: "Rename",
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(1), 4)],
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    let op = add_clip(&d, 0, 9600, 960).unwrap();
    h.apply(&mut d, op).unwrap();
    let last = d.track_count() - 1;
    let id = d.track(last).unwrap().id();
    let mut f = d.project().folders.clone();
    let fid = f.create("Drums", None);
    f.set_member(id, fid);
    h.apply(
        &mut d,
        Operation::SetFolders {
            folders: Box::new(f),
            label: "Folder",
        },
    )
    .unwrap();
    (d, h)
}

fn run_with(d: &PlumDocument, h: &mut History, info: ProjectInfo, src: &str) -> Plain {
    h.observe_ids(d);
    let w = World::new(Arc::new(d.clone()), h.incarnations().snapshot()).with_info(info);
    let host = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    host.set_world(Some(w));
    host.run("api.lua", src)
        .unwrap_or_else(|e| panic!("{e}\n{}", e.traceback.clone().unwrap_or_default()))
}

#[test]
fn the_project_api_reads_file_timing_counts_and_the_conductor() {
    let (d, mut h) = song();
    let info = ProjectInfo {
        file: Some("C:\\Songs\\song.lup".into()),
        file_type: FileType::Lup,
        dirty: true,
        revision: 42,
        dark: true,
        collapsed: vec![],
        ..ProjectInfo::default()
    };
    let r = run_with(
        &d,
        &mut h,
        info,
        r#"
        local p = plum.project.current()
        assert(p:kind() == "project")
        assert(p:name() == "Song", p:name())
        assert(p:file() == "C:\\Songs\\song.lup")
        assert(p:file_type() == "lup")
        assert(p:dirty() == true and p:revision() == 42)
        assert(p:ppqn() == 960 and math.type(p:ppqn()) == "integer")
        local div = p:division()
        assert(div.kind == "ppqn" and div.ppqn == 960)
        assert(p:format() == 1)
        assert(p:key_range() == 128)
        assert(p:track_count() == 3, p:track_count())
        assert(p:note_count() == 8)
        assert(p:selected_note_count() == 0)
        assert(math.type(p:duration_ticks()) == "integer")
        -- 120 BPM: one quarter (960 ticks) is half a second.
        assert(math.abs(p:seconds_at(960) - 0.5) < 1e-9, p:seconds_at(960))
        assert(math.abs(p:seconds_at("1920") - 1.0) < 1e-9)
        local t = p:tempos()
        assert(#t == 1 and t[1].tick == 0 and t[1].bpm == "120" and t[1].micros == 500000)
        assert(t[1].ramp == false)
        local s = p:time_signatures()
        assert(#s == 1 and s[1].numerator == 4 and s[1].denominator == 4)
        local c = p:conductor()
        assert(c:kind() == "conductor" and c:name() == "Conductor")
        local m = c:markers()
        assert(#m == 2 and m[1].title == "Marker 0" and m[1].tick == 0)
        assert(m[2].chapter == true and m[2].end_tick == 1920 + 3 * 1920)
        assert(#c:tempos() == 1)
        return true
    "#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn a_new_unsaved_project_has_no_file() {
    let (d, mut h) = song();
    let r = run_with(
        &d,
        &mut h,
        ProjectInfo::default(),
        r#"local p = plum.project.current()
           return p:file() == nil and p:file_type() == "new" and p:dirty() == false"#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn the_tracks_api_reads_the_tree_folders_and_subchannels() {
    let (d, mut h) = song();
    let parent = d.track(0).unwrap().id().0;
    let info = ProjectInfo {
        dark: true,
        collapsed: vec![parent],
        ..ProjectInfo::default()
    };
    let r = run_with(
        &d,
        &mut h,
        info,
        r#"
        local p = plum.project.current()
        local ts = p:tracks()
        assert(#ts == 3)
        local piano, sub, drums = ts[1], ts[2], ts[3]
        assert(piano:name() == "Piano" and piano:label() == "Piano")
        assert(piano:kind() == "track" and piano:index() == 1 and piano:depth() == 0)
        assert(piano:parent() == nil)
        local kids = piano:children()
        assert(#kids == 1 and kids[1] == sub)
        assert(sub:kind() == "subtrack" and sub:depth() == 1 and sub:parent() == piano)
        assert(sub:index() == 2)
        -- Collapse is view state: the subtrack is hidden, not gone.
        assert(piano:collapsed() == true and sub:visible() == false and piano:visible())
        assert(piano:note_count() == 8 and piano:stored_note_count() == 8)
        assert(piano:clip_count() == 2, piano:clip_count())
        assert(piano:mute() == false and piano:solo() == false)
        local c = piano:color()
        assert(math.type(c.r) == "integer" and c.custom == false)
        assert(piano:default_channel() == 0)
        local subs = piano:subchannels()
        assert(#subs == 2)
        assert(subs[1]:number() == 1 and subs[1]:id() == 0 and subs[1]:channel() == 0)
        assert(subs[2]:number() == 2 and subs[2]:channel() == 4)
        assert(subs[2]:name() == "Subchannel 2" and subs[2]:track() == piano)
        assert(piano:folder() == nil)
        local f = drums:folder()
        assert(f ~= nil and f:kind() == "folder" and f:name() == "Drums")
        assert(#f:tracks() == 1 and f:tracks()[1] == drums)
        assert(f:collapsed() == false)
        local fs = p:folders()
        assert(#fs == 1 and fs[1] == f)
        assert(p:track(3) == drums and p:track(4) == nil)
        assert(piano:id() ~= drums:id())
        -- A method of another family says what it is.
        local ok, err = pcall(function() return f:mute() end)
        assert(not ok and tostring(err):find("mute() is not available on a folder handle", 1, true), tostring(err))
        return true
    "#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn no_project_means_no_project_handle() {
    let host = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    assert_eq!(
        host.run("api.lua", "return plum.project.current() == nil")
            .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn reading_the_project_needs_the_project_read_capability() {
    let (d, mut h) = song();
    h.observe_ids(&d);
    let w = World::new(Arc::new(d.clone()), h.incarnations().snapshot());
    let host = Host::new(Limits::default(), [Capability::EditNotes]).unwrap();
    host.set_world(Some(w.clone()));
    let e = host
        .run("api.lua", "return plum.project.current()")
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
    assert!(e.message.contains("project.read"), "{e}");
    // A handle handed over by the host is no way around it.
    host.set_global_handle("t", w.track_at(0).unwrap()).unwrap();
    let e = host.run("api.lua", "return t:name()").unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
}

#[test]
fn rows_list_the_conductor_folders_and_tracks_like_the_track_list() {
    let (d, mut h) = song();
    let parent = d.track(0).unwrap().id().0;
    let info = ProjectInfo {
        collapsed: vec![parent],
        ..ProjectInfo::default()
    };
    let r = run_with(
        &d,
        &mut h,
        info,
        r#"
        local p = plum.project.current()
        local kinds = {}
        for _, row in ipairs(p:rows()) do kinds[#kinds + 1] = row:kind() end
        local all = table.concat(kinds, ",")
        assert(all == "conductor,track,subtrack,folder,track", all)
        kinds = {}
        for _, row in ipairs(p:rows{ visible = true }) do kinds[#kinds + 1] = row:kind() end
        local shown = table.concat(kinds, ",")
        assert(shown == "conductor,track,folder,track", shown)
        return true
    "#,
    );
    assert_eq!(r, Plain::Boolean(true));
}

#[test]
fn subchannel_note_color_matches_native_palettes_all_velocities_and_keeps_snapshot() {
    use crate::ui::theme::{NoteColorMode, NoteColorPrefs, NotePalette};
    let (d, mut history) = song();
    for mode in NoteColorMode::ALL {
        for dark in [false, true] {
            for seed in [0, 9_123_456] {
                let palette = Arc::new(NotePalette::new(NoteColorPrefs {
                    mode,
                    seed,
                    manual: vec![[255, 0, 0], [7, 40, 190]],
                }));
                let mut src = String::from(
                    "local subs=plum.project.current():track(1):subchannels();local s=subs[2];local a=s:note_color();local b=s:note_color(100);assert(a.r==b.r and a.g==b.g and a.b==b.b);a.r=-1;assert(s:note_color().r>=0); ",
                );
                for (i, id) in [0, 1].into_iter().enumerate() {
                    for v in 1..=127 {
                        let c = palette.entry(id, v, dark).fill;
                        src.push_str(&format!("do local c=subs[{}]:note_color({v});assert(c.r=={} and c.g=={} and c.b=={})end;",i+1,c.r(),c.g(),c.b()));
                    }
                }
                src.push_str("return true");
                assert_eq!(
                    run_with(
                        &d,
                        &mut history,
                        ProjectInfo {
                            dark,
                            note_palette: palette.clone(),
                            ..Default::default()
                        },
                        &src
                    ),
                    Plain::Boolean(true)
                );
            }
        }
    }
}
#[test]
fn subchannel_note_color_rejects_invalid_arguments_wrong_family_expiry_and_missing_grant() {
    let (mut d, mut history) = song();
    history.observe_ids(&d);
    let w = World::new(Arc::new(d.clone()), history.incarnations().snapshot());
    let id = d.track(0).unwrap().id();
    let sub = w.subchannel(id, SubchannelId(1)).unwrap();
    let host = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    host.set_world(Some(w.clone()));
    host.set_global_handle("sub", sub.clone()).unwrap();
    assert_eq!(
        host.run(
            "invalid-color.lua",
            r#"
      for _,v in ipairs({0,128,-1,1.5,100.0,'100',false,{}})do
        local ok,e=pcall(function()sub:note_color(v)end)
        assert(not ok and tostring(e):find('integer velocity',1,true),tostring(e))
      end
      local ok,e=pcall(function()plum.project.current():track(1):note_color()end)
      assert(not ok and tostring(e):find('not available',1,true));return true
    "#
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    let denied = Host::new(Limits::default(), []).unwrap();
    denied.set_world(Some(w));
    denied.set_global_handle("sub", sub).unwrap();
    assert_eq!(
        denied
            .run("denied.lua", "return sub:note_color()")
            .unwrap_err()
            .category,
        Category::Capability
    );
    history
        .apply(&mut d, Operation::DeleteTrack { index: 0 })
        .unwrap();
    history.observe_ids(&d);
    host.set_world(Some(World::new(
        Arc::new(d),
        history.incarnations().snapshot(),
    )));
    assert_eq!(
        host.run("expired-color.lua", "return sub:note_color()")
            .unwrap_err()
            .category,
        Category::Expired
    );
}
