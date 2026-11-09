use super::*;
use crate::lua::{Limits, Plain, World, handles::ProjectInfo};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
    view::{PitchAxis, TimeAxis},
};
use std::time::Duration;
#[test]
fn published_view_example_executes_verbatim_and_retained_snapshots_own_no_document_root() {
    let docs = include_str!("../../../../docs/LUA_API.md");
    let section = docs
        .split("## Immutable native view snapshots (API 0.2)")
        .nth(1)
        .unwrap();
    let example = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let (h, budget) = fixture(127, 0);
    h.run("published-view.lua", example).unwrap();
    h.run("held-view.lua", "held=plum.ui.view_snapshot('piano_roll')")
        .unwrap();
    let doc = Arc::clone(
        &h.lua
            .app_data_ref::<crate::lua::handles::WorldSlot>()
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .doc,
    );
    h.set_world(None);
    h.lua.gc_collect().unwrap();
    assert_eq!(Arc::strong_count(&doc), 1);
    h.run(
        "expire.lua",
        "assert(not pcall(function()held:info()end));held=nil;collectgarbage('collect')",
    )
    .unwrap();
    assert_eq!(budget.used(), 0);
}
fn fixture(top: u8, shift: i128) -> (Host, NativeBudget) {
    let mut doc = PlumDocument::new_project("View fixture".into(), Bpm::DEFAULT, 960);
    if top == 255 {
        doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let stamp = ProjectStamp {
        serial: world.project_serial(),
        revision: 0,
    };
    let rect = Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(800.0, 400.0));
    let frame = ViewFrame {
        rect,
        view: crate::piano_roll::viewport::RollView {
            time: TimeAxis::new(9_007_199_254_740_993, 0.5),
            pitch: PitchAxis {
                top,
                scroll_rows: if top == 255 { 128.25 } else { 0.25 },
                row_height: 12.5,
            },
        },
        shift,
        track: doc.track(0).unwrap().id(),
        content: None,
        locked: false,
        conductor: false,
        pixels_per_point: 1.5,
        document: Arc::as_ptr(&doc) as usize,
        revisions: [
            doc.structure_revision(),
            doc.analysis_revision(),
            doc.layout_revision(),
            doc.tempo_revision(),
        ],
    };
    let player = PlayerFrame {
        rect,
        rain: Rect::from_min_max(rect.min, egui::pos2(rect.right(), 500.0)),
        keyboard: Rect::from_min_max(egui::pos2(rect.left(), 500.0), rect.max),
        keys: u16::from(top) + 1,
        mapping: crate::player::rain::Mapping::Time,
        seconds_ahead: 4.0,
        pixels_per_point: 1.5,
        stamp,
    };
    let h = Host::new(
        Limits {
            time: Duration::from_secs(10),
            ..Default::default()
        },
        [
            Capability::UiOverlay,
            Capability::ReadProject,
            Capability::ReadPlayback,
        ],
    )
    .unwrap();
    h.set_world(Some(world.with_info(ProjectInfo {
        views: [
            Some(Snapshot {
                stamp,
                revision: 7,
                facts: Facts::Roll(frame),
            }),
            Some(Snapshot {
                stamp,
                revision: 8,
                facts: Facts::Player(player),
            }),
        ],
        ..Default::default()
    })));
    let budget = NativeBudget::isolated(128 * 1024);
    h.lua.set_app_data(budget.clone());
    (h, budget)
}
#[test]
fn exact_relative_ticks_clip_shifts_and_both_key_modes_roundtrip_native_coordinates() {
    for top in [127, 255] {
        let (h, _) = fixture(top, -9_007_199_254_740_513);
        h.run(
            "coordinates.lua",
            r#"
            local s=assert(plum.ui.view_snapshot('piano_roll'));local info=s:info()
            assert(info.origin_tick==9007199254740993 and math.type(info.origin_tick)=='integer')
            assert(info.song_shift=='-9007199254740513' and info.view_revision==7)
            assert(info.bounds.x==100 and info.bounds.y==200 and info.bounds.width==800)
            assert(info.pixels_per_point==1.5 and info.first_tick==9007199254740993)
            assert(s:local_to_song(9007199254740995)==482)
            assert(s:song_to_local(482)==9007199254740995)
            assert(s:local_to_song(0)==nil)
            for key=0,info.keys-1 do
                local uv=s:note_to_uv(9007199254740996,key)
                assert(uv.x==1.5/800)
                local n=s:uv_to_note(uv.x,uv.y)
                assert(n.local_tick==9007199254740996 and n.key==key)
                assert(math.abs(n.pitch_position-key)<1e-10)
                local screen=s:to_screen(uv.x,uv.y)
                local again=s:from_screen(screen.x,screen.y)
                assert(math.abs(again.x-uv.x)<1e-10 and math.abs(again.y-uv.y)<1e-10)
            end
            info.bounds.width=1;assert(s:info().bounds.width==800)
            assert(not pcall(function()s:note_to_uv(0,info.keys)end))
            assert(not pcall(function()s:note_to_uv(0,60.0)end))
            return 'exact'
        "#,
        )
        .unwrap();
    }
    let (h, _) = fixture(127, 12345);
    h.run("positive-shift.lua","local s=plum.ui.view_snapshot('piano_roll');assert(s:local_to_song(10)==12355);assert(s:song_to_local(1)==nil);assert(s:song_to_local(12355)==10)").unwrap();
}
#[test]
fn player_snapshots_keep_distinct_surface_geometry_and_reject_roll_mapping() {
    let (h, _) = fixture(255, 0);
    assert_eq!(h.run("player.lua",r#"
        local s=plum.ui.view_snapshot('player');local info=s:info()
        assert(info.surface=='player' and info.keys==256 and info.mapping=='time')
        assert(info.seconds_ahead==4 and info.rainfall_bounds.height==300 and info.keyboard_bounds.height==100)
        assert(info.points_per_tick==nil and info.track==nil)
        local p=s:to_screen(0.5,0.5);assert(p.x==500 and p.y==400)
        local uv=s:from_screen(500,400);assert(uv.x==0.5 and uv.y==0.5)
        assert(not pcall(function()s:note_to_uv(0,60)end))
        assert(not pcall(function()s:uv_to_note(0,0)end))
        assert(not pcall(function()s:local_to_song(1)end))
        return 'player'
    "#).unwrap(),Plain::String("player".into()));
}
#[test]
fn geometry_versions_change_only_with_facts_context_or_visibility_and_disable_never_reuses_ids() {
    let (h, _) = fixture(127, 0);
    let base = h
        .lua
        .app_data_ref::<crate::lua::handles::WorldSlot>()
        .unwrap()
        .0
        .as_ref()
        .unwrap()
        .info
        .views[0]
        .unwrap();
    let mut capture = Capture::default();
    assert!(capture.sample(0, base.stamp, Some(base.facts)).is_none());
    capture.enable(true);
    let first = capture.sample(0, base.stamp, Some(base.facts)).unwrap();
    assert_eq!(capture.sample(0, base.stamp, Some(base.facts)), Some(first));
    let Facts::Roll(mut f) = base.facts else {
        unreachable!()
    };
    for change in 0..5 {
        match change {
            0 => f.rect.max.x += 1.0,
            1 => f.view.time.zoom_around(300.0, 2.0),
            2 => f.view.pitch.scroll_rows += 0.5,
            3 => f.shift += 1,
            _ => f.locked = true,
        }
        let next = capture.sample(0, base.stamp, Some(Facts::Roll(f))).unwrap();
        assert!(next.revision > first.revision);
        assert_eq!(
            capture.sample(0, base.stamp, Some(Facts::Roll(f))),
            Some(next)
        );
    }
    capture.sample(0, base.stamp, None);
    let fresh = capture.sample(0, base.stamp, Some(base.facts)).unwrap();
    assert!(fresh.revision > first.revision);
    capture.enable(false);
    capture.enable(true);
    assert!(
        capture
            .sample(0, base.stamp, Some(base.facts))
            .unwrap()
            .revision
            > fresh.revision
    );
    assert!(capture.sample(2, base.stamp, Some(base.facts)).is_none());
    capture.sequence = ticks::MAX_LUA_TICK;
    capture.last = [None, None];
    assert!(capture.sample(0, base.stamp, Some(base.facts)).is_none());
    assert!(!capture.enabled);
}
#[test]
fn view_authority_is_separate_surface_specific_and_hidden_surfaces_return_nil() {
    for (caps, surface) in [
        (vec![], "piano_roll"),
        (vec![Capability::UiOverlay], "piano_roll"),
        (vec![Capability::ReadProject], "piano_roll"),
        (
            vec![Capability::UiOverlay, Capability::ReadProject],
            "player",
        ),
        (
            vec![Capability::UiOverlay, Capability::ReadPlayback],
            "piano_roll",
        ),
    ] {
        let h = Host::new(Limits::default(), caps).unwrap();
        assert_eq!(
            h.run(
                "denied.lua",
                &format!("return plum.ui.view_snapshot('{surface}')")
            )
            .unwrap_err()
            .category,
            Category::Capability
        );
    }
    let h = Host::new(
        Limits::default(),
        [
            Capability::UiOverlay,
            Capability::ReadProject,
            Capability::ReadPlayback,
        ],
    )
    .unwrap();
    h.run("hidden.lua","assert(plum.ui.view_snapshot('piano_roll')==nil);assert(plum.ui.view_snapshot('player')==nil)").unwrap();
    assert!(
        h.run("unknown.lua", "plum.ui.view_snapshot('desktop')")
            .is_err()
    );
}
#[test]
fn bounded_retained_snapshot_accounting_expiry_and_native_preflight_do_not_leak_buffers() {
    let (h, budget) = fixture(127, 0);
    h.run(
        "held.lua",
        r#"
        held=plum.ui.view_snapshot('piano_roll')
        for i=2,32 do local s=plum.ui.view_snapshot('piano_roll')end
        assert(not pcall(function()plum.ui.view_snapshot('piano_roll')end))
    "#,
    )
    .unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), SNAPSHOT_BYTES);
    h.run("expired.lua","assert(not pcall(function()held:info()end));assert(not pcall(function()held:to_screen(0,0)end));held=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), 0);
    let tiny = NativeBudget::isolated(SNAPSHOT_BYTES - 1);
    h.lua.set_app_data(tiny.clone());
    assert_eq!(
        h.run("quota.lua", "plum.ui.view_snapshot('piano_roll')")
            .unwrap_err()
            .category,
        Category::Budget
    );
    assert_eq!(tiny.used(), 0);
}
#[test]
fn invalid_native_facts_non_finite_coordinates_and_foreign_project_frames_fail_cleanly() {
    let (h, _) = fixture(127, 0);
    h.run(
        "bad-input.lua",
        r#"
        local s=plum.ui.view_snapshot('piano_roll')
        for _,x in ipairs{0/0,1/0,-1/0,1e13}do
            assert(not pcall(function()s:to_screen(x,0)end))
            assert(not pcall(function()s:uv_to_note(0,x)end))
        end
        assert(not pcall(function()s:note_to_uv(-1,60)end))
        assert(not pcall(function()s:note_to_uv('9223372036854775808',60)end))
    "#,
    )
    .unwrap();
    let base = h
        .lua
        .app_data_ref::<crate::lua::handles::WorldSlot>()
        .unwrap()
        .0
        .as_ref()
        .unwrap()
        .info
        .views[0]
        .unwrap();
    let Facts::Roll(mut f) = base.facts else {
        unreachable!()
    };
    for change in 0..4 {
        f = match base.facts {
            Facts::Roll(f) => f,
            _ => unreachable!(),
        };
        match change {
            0 => f.view.time.pixels_per_tick = f64::NAN,
            1 => f.view.pitch.row_height = 0.0,
            2 => f.rect.max.x = f32::INFINITY,
            _ => f.shift = i128::MIN,
        }
        assert!(!Facts::Roll(f).valid());
    }
    assert!(!Facts::Roll(f).valid());
    {
        let mut slot = h
            .lua
            .app_data_mut::<crate::lua::handles::WorldSlot>()
            .unwrap();
        let info = Arc::make_mut(&mut slot.0.as_mut().unwrap().info);
        info.views[0].as_mut().unwrap().stamp.revision += 1;
    }
    assert_eq!(
        h.run(
            "foreign-view.lua",
            "return plum.ui.view_snapshot('piano_roll')"
        )
        .unwrap_err()
        .category,
        Category::Expired
    );
}
