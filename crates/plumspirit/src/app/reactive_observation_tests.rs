use super::super::commands::Command;
use super::super::test_support::Harness;
use super::super::track_list::ClipIntent;
use super::*;
use crate::lua::Scripts;
use crate::lua::plugins::{PluginManager, PluginPrefs, Status};
use plumspirit_engine::history::Operation;
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::path::PathBuf;
use std::sync::Arc;

const ID: &str = "plumspirit.example.reactive-monitor";
struct Rig {
    h: Harness,
    root: PathBuf,
}
impl Rig {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "plumspirit-reactive-app-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut doc = PlumDocument::new_project("Reactive integration".into(), Bpm::DEFAULT, 960);
        let records: Vec<_> = (0..2)
            .map(|i| NoteRecord {
                start_tick: i * 960,
                duration_ticks: 120,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&records, false)
            .unwrap();
        let mut h = Harness::new(doc);
        let mut manager = PluginManager::new(Some(root.join("plugins")), Scripts::default());
        let mut prefs = PluginPrefs {
            folder: Some(root.join("plugins")),
            ..Default::default()
        };
        manager
            .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
            .unwrap();
        assert_eq!(
            prefs.installed[ID].origin,
            crate::lua::package::Origin::Example
        );
        for cap in ["project.read", "project.observe"] {
            manager.set_granted(ID, cap, true, &mut prefs);
        }
        manager.set_enabled(ID, true, &mut prefs);
        h.app.settings.lua_plugins = prefs;
        h.app.lua_plugins = Some(manager);
        h.app.lua_plugins_started = true;
        let mut rig = Self { h, root };
        rig.until(|r| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(ID)
                .unwrap()
                .status
                == Status::Ready
                && !r.h.app.lua_plugins.as_ref().unwrap().is_busy()
        });
        // Initialize a baseline after subscriptions finish loading; no event replay assumption.
        rig.h.app.lua_observation.sampled = None;
        rig.h.app.sample_lua_observation();
        rig
    }
    fn poll(&mut self) {
        self.h.poll_background_jobs();
        self.h.app.poll_lua_plugins(&self.h.ctx);
    }
    fn until(&mut self, test: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.poll();
            if test(self) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "native observation source timed out; log: {:?}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    fn log(&self) -> Vec<String> {
        self.h
            .app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .log
            .lines()
            .map(|l| l.text.clone())
            .collect()
    }
    fn count(&self, kind: &str) -> usize {
        let prefix = format!("OBS {kind} ");
        self.log().iter().filter(|s| s.starts_with(&prefix)).count()
    }
    fn wait_new(&mut self, kind: &str, previous: usize) {
        self.until(|r| r.count(kind) > previous);
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut m) = self.h.app.lua_plugins.take() {
            m.stop_all(&self.h.app.settings.lua_plugins);
            drop(m);
        }
        let root = self.root.canonicalize().unwrap();
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plumspirit-reactive-app-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn actual_app_sources_deliver_selection_content_tracks_clips_view_and_history() {
    let mut r = Rig::new();
    assert_eq!(Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc), 1);
    let before = r.count("selection_changed");
    Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 1, 60, 60);
    r.wait_new("selection_changed", before);
    assert!(
        r.log()
            .iter()
            .any(|s| s.starts_with("OBS selection_changed ") && s.ends_with("notes=1"))
    );
    // Same selected count, different rank must still invalidate.
    let before = r.count("selection_changed");
    let notes = &mut Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes;
    notes.clear_selection();
    notes.select_in_region(960, 961, 60, 60);
    r.wait_new("selection_changed", before);
    let content = r.count("content_changed");
    let tracks = r.count("tracks_changed");
    assert!(r.h.app.apply_op(Operation::AddTrack).is_some());
    r.wait_new("content_changed", content);
    r.wait_new("tracks_changed", tracks);
    let undo = r.count("undo");
    r.h.app.undo();
    r.wait_new("undo", undo);
    assert_eq!(r.h.doc().track_count(), 1);
    let redo = r.count("redo");
    r.h.app.redo();
    r.wait_new("redo", redo);
    assert_eq!(r.h.doc().track_count(), 2);
    let clips = r.count("clips_changed");
    r.h.clip(ClipIntent::Add {
        track: 0,
        start: 1920,
        length: 960,
    });
    r.wait_new("clips_changed", clips);
    let view = r.count("view_changed");
    r.h.app.roll.view.time.origin_tick = u64::MAX - 33;
    r.h.app.roll.view.time.pixels_per_tick = 0.125;
    r.h.app.roll.view.pitch.scroll_rows = 32.25;
    r.h.app.roll.view.pitch.row_height = 17.5;
    r.wait_new("view_changed", view);
    assert!(
        r.log()
            .iter()
            .any(|s| s.contains("origin=18446744073709551582 zoom=0.125"))
    );
    assert_eq!(Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc), 1);
}

#[test]
fn successful_native_save_delivers_event_and_failed_save_does_not() {
    let mut r = Rig::new();
    r.h.app.open.as_mut().unwrap().source =
        super::super::DocSource::Project(r.root.join("actual.lup"));
    let count = r.count("saved");
    assert!(r.h.command(Command::Save));
    r.wait_new("saved", count);
    assert!(r.root.join("actual.lup").metadata().unwrap().len() > 0);
    assert!(
        r.log()
            .iter()
            .any(|s| s.contains("Observed successful lup save."))
    );
    let count = r.count("saved");
    r.h.app.open.as_mut().unwrap().source =
        super::super::DocSource::Project(r.root.join("missing/failed.lup"));
    assert!(r.h.command(Command::Save));
    r.until(|r| r.h.app.job.is_none());
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(350) {
        r.poll();
        std::thread::sleep(Duration::from_millis(4));
    }
    assert_eq!(r.count("saved"), count);
    assert!(!r.root.join("missing/failed.lup").exists());
}

#[test]
fn app_delivery_rechecks_revision_and_lifecycle_after_manager_poll() {
    let mut r = Rig::new();
    let stamp = r.h.app.lua_observation_stamp().unwrap();
    let delivery = Delivery {
        views: [None, None],
        playback: None,
        id: "monitor".into(),
        event: crate::lua::reactive::Event::new(
            Kind::ContentChanged,
            stamp,
            1,
            Data::Content {
                previous_revision: 0,
            },
        )
        .unwrap(),
        fence: crate::lua::reactive::Fence::live(),
        lifecycle: crate::lua::reactive::Fence::live(),
        dropped: 0,
    };
    assert!(r.h.app.lua_observation_delivery_valid(&delivery));
    r.h.app.apply_op(Operation::AddTrack).unwrap();
    assert!(!r.h.app.lua_observation_delivery_valid(&delivery));
    let mut closed = delivery.clone();
    closed.event.kind = Kind::ProjectClosed;
    closed.event.data = Data::Closed;
    assert!(!r.h.app.lua_observation_delivery_valid(&closed));
    r.h.app.open = None;
    assert!(r.h.app.lua_observation_delivery_valid(&closed));
    closed.fence.invalidate();
    assert!(!r.h.app.lua_observation_delivery_valid(&closed));
}

#[test]
fn bounded_fact_sampling_reads_metadata_without_owning_the_document_or_lua() {
    let mut h = Harness::new(super::super::context_matrix::fixture());
    let first = h.app.lua_observation_facts().unwrap();
    h.app.roll.view.time.origin_tick = (1u64 << 63) + 123;
    let next = h.app.lua_observation_facts().unwrap();
    assert_eq!(first.content, next.content);
    assert_ne!(first.view, next.view);
    assert_eq!(next.viewport.origin_tick, (1u64 << 63) + 123);
    assert_eq!(Arc::strong_count(&h.app.open.as_ref().unwrap().doc), 1);
    h.app.sample_lua_observation(); // Disabled path is inert.
    assert!(h.app.lua_observation.last.is_none());
    assert!(Viewport::new(0, f64::NAN, 0., 1., 127).is_none());
    assert!(Viewport::new(0, 1., f64::INFINITY, 1., 127).is_none());
    assert!(Viewport::new(0, 1., 0., 0., 127).is_none());
}

#[test]
fn actual_project_close_reopen_and_midi_export_have_native_owner_events() {
    let mut r = Rig::new();
    let saved = r.count("saved");
    r.h.dialog_outcome(super::super::dialogs::DialogOutcome::PathEntered {
        purpose: super::super::dialogs::PromptPurpose::Export,
        path: r.root.join("actual.mid"),
    });
    r.wait_new("saved", saved);
    assert!(r.root.join("actual.mid").metadata().unwrap().len() > 14);
    assert!(
        r.log()
            .iter()
            .any(|s| s.contains("Observed successful midi save."))
    );
    let closed = r.count("project_closed");
    r.h.app.request_close_project();
    assert!(r.h.app.open.is_none());
    r.wait_new("project_closed", closed);
    let opened = r.count("project_opened");
    let mut doc = PlumDocument::new_project("Replacement".into(), Bpm::DEFAULT, 960);
    doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    r.h.app
        .install_document(doc, super::super::DocSource::New, true);
    r.wait_new("project_opened", opened);
    assert_eq!(r.h.app.lua_observation_facts().unwrap().keys, 256);
}

#[test]
#[ignore = "manual metadata timing fixture; run explicitly for source performance changes"]
fn metadata_sampling_cost_is_independent_of_dense_note_payloads() {
    fn measure(h: &Harness) -> Duration {
        let start = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(h.app.lua_observation_facts().unwrap());
        }
        start.elapsed() / 100
    }
    let mut doc = PlumDocument::new_project("Metadata timing".into(), Bpm::DEFAULT, 960);
    let empty = Harness::new(doc.clone());
    println!("empty one-track metadata sample {:?}", measure(&empty));
    let records: Vec<_> = (0..1_000_000)
        .map(|i| NoteRecord {
            start_tick: i,
            duration_ticks: 1,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, false)
        .unwrap();
    drop(records);
    let dense = Harness::new(doc);
    println!(
        "one-million-note one-track metadata sample {:?}",
        measure(&dense)
    );
    assert_eq!(Arc::strong_count(&dense.app.open.as_ref().unwrap().doc), 1);
    let mut tracks = PlumDocument::new_project("Track metadata timing".into(), Bpm::DEFAULT, 960);
    for _ in 1..65_535 {
        tracks.add_track().unwrap();
    }
    let tracks = Harness::new(tracks);
    println!(
        "65,535 empty-track complete metadata scan {:?}",
        measure(&tracks)
    );
    let mut sampler = Sampler::new(Context::read(&tracks.app).unwrap());
    let mut steps = 0;
    let mut maximum = Duration::ZERO;
    loop {
        let start = Instant::now();
        let done = sampler.step(&tracks.app).is_some();
        maximum = maximum.max(start.elapsed());
        steps += 1;
        assert!(sampler.visited <= SAMPLE_STEP_ITEMS);
        if done {
            break;
        }
    }
    println!("bounded metadata scan steps={steps}, maximum step={maximum:?}");
    assert!(steps >= 32);
}

#[test]
fn metadata_steps_bound_track_and_selection_visits_and_keep_no_document_reference() {
    let mut doc = PlumDocument::new_project("Bounded metadata".into(), Bpm::DEFAULT, 960);
    for _ in 1..8193 {
        doc.add_track().unwrap();
    }
    let mut h = Harness::new(doc);
    for t in h.app.open.as_ref().unwrap().doc.tracks() {
        h.app.selected_tracks.insert(t.id());
    }
    let context = Context::read(&h.app).unwrap();
    let mut sampler = Sampler::new(context);
    let mut steps = 0;
    let facts = loop {
        let sample = sampler.step(&h.app);
        steps += 1;
        assert!(sampler.visited <= SAMPLE_STEP_ITEMS);
        assert_eq!(Arc::strong_count(&h.app.open.as_ref().unwrap().doc), 1);
        if let Some(facts) = sample {
            break facts;
        }
    };
    assert_eq!(facts.track_count, 8193);
    assert!(steps >= 8);
    h.app.apply_op(Operation::AddTrack).unwrap();
    assert_ne!(Context::read(&h.app).unwrap(), context);
    assert!(
        std::mem::size_of::<Sampler>() < 512,
        "sampler has fixed metadata state"
    );
}

impl Rig {
    fn install_response(&mut self, id: &str, source: &str) {
        let folder = self.root.join(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("plum-plugin.toml"),
            format!(
                r#"id="{id}"
name="Response integration"
version="1.0.0"
api="0.2"
entry="main.lua"
capabilities=["project.read","project.observe","project.automate","notes.edit"]
"#
            ),
        )
        .unwrap();
        std::fs::write(folder.join("main.lua"), source).unwrap();
        self.install_package(&folder, id, true);
    }
    fn install_package(&mut self, folder: &std::path::Path, id: &str, automate: bool) {
        let app = &mut self.h.app;
        let prefs = &mut app.settings.lua_plugins;
        let manager = app.lua_plugins.as_mut().unwrap();
        if let Some(example) = crate::lua::examples::by_id(id) {
            manager.install_example(example, prefs).unwrap();
            assert_eq!(
                prefs.installed[id].origin,
                crate::lua::package::Origin::Example
            );
        } else {
            manager.install(folder, false, prefs).unwrap();
        }
        for cap in ["project.read", "project.observe", "notes.edit"] {
            manager.set_granted(id, cap, true, prefs);
        }
        if automate {
            manager.set_granted(id, "project.automate", true, prefs);
        }
        manager.set_enabled(id, true, prefs);
        self.until(|r| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(id)
                .unwrap()
                .status
                == Status::Ready
                && !r
                    .h
                    .app
                    .lua_plugins
                    .as_ref()
                    .unwrap()
                    .plugin(id)
                    .unwrap()
                    .is_busy()
        });
    }
    fn poll_for(&mut self, time: Duration) {
        let start = Instant::now();
        while start.elapsed() < time {
            self.poll();
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    fn all_velocities(&self, value: u8) -> bool {
        self.h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .all(|n| n.attrs.velocity() == value)
    }
}

#[test]
fn installable_velocity_response_applies_once_and_undo_redo_do_not_retrigger_it() {
    let mut r = Rig::new();
    let example =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples-lua/reactive-velocity-guard");
    r.install_package(&example, "plumspirit.example.reactive-velocity", true);
    Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .select_all();
    r.h.app
        .apply_op(Operation::SetVelocity {
            track: 0.into(),
            velocity: 121,
        })
        .unwrap();
    r.until(|r| r.all_velocities(100));
    let applied = r.h.app.open.as_ref().unwrap().history.current_state();
    r.poll_for(Duration::from_millis(700));
    assert_eq!(
        r.h.app.open.as_ref().unwrap().history.current_state(),
        applied
    );
    r.h.app.undo();
    assert!(r.all_velocities(121));
    r.poll_for(Duration::from_millis(650));
    assert!(r.all_velocities(121));
    r.h.app.redo();
    assert!(r.all_velocities(100));
    r.poll_for(Duration::from_millis(650));
    assert!(r.all_velocities(100));
    r.h.app
        .apply_op(Operation::SetVelocity {
            track: 0.into(),
            velocity: 122,
        })
        .unwrap();
    r.until(|r| r.all_velocities(100));
    assert_eq!(Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc), 1);
}
fn recursive_plugin() -> &'static str {
    r#"
    function register(p)
        p.command{id='respond',name='Respond',run=function(ctx)
            local tx=plum.transaction{label='Reactive insertion'}
            tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=ctx.request.key}});tx:commit()
        end}
        p.observe{id='observe',events={'content_changed'},debounce_ms=20,run=function(e)
            print('child allowed='..tostring(e.transaction_allowed))
            p.request_transaction{command='respond',data={key=62}}
        end}
    end
"#
}
#[test]
fn two_plugins_cannot_form_a_recursive_automatic_edit_loop() {
    let mut r = Rig::new();
    r.install_response("test.reactive-a", recursive_plugin());
    r.install_response("test.reactive-b", recursive_plugin());
    let notes = r.h.doc().note_count();
    r.h.app.apply_op(Operation::AddTrack).unwrap();
    r.until(|r| r.h.doc().note_count() > notes);
    r.poll_for(Duration::from_millis(1200));
    assert_eq!(
        r.h.doc().note_count(),
        notes + 1,
        "only one child edit is accepted across all plugins"
    );
    for id in ["test.reactive-a", "test.reactive-b"] {
        assert!(
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(id)
                .unwrap()
                .log
                .lines()
                .any(|l| l.text.contains("child allowed=false"))
        );
    }
}
#[test]
fn actual_edit_and_disable_while_response_calculates_discard_the_staged_result() {
    let mut r = Rig::new();
    r.install_response("test.reactive-slow",r#"
        function register(p)
            p.command{id='respond',name='Respond',run=function(ctx)
                local tx=plum.transaction{};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=65}});tx:commit()
                plum.progress(.5,'detached response');pcall(function() while true do plum.yield() end end)
                print('STALE RESPONSE OUTPUT')
            end}
            p.observe{id='observe',events={'content_changed'},debounce_ms=20,run=function(e)
                p.request_transaction{command='respond'}
            end}
        end
    "#);
    let notes = r.h.doc().note_count();
    r.h.app.apply_op(Operation::AddTrack).unwrap();
    r.until(|r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin("test.reactive-slow")
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    assert_eq!(Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc), 1);
    assert!(
        r.h.app.apply_op(Operation::AddTrack).is_some(),
        "native edits proceed during detached Lua calculation"
    );
    let app = &mut r.h.app;
    app.lua_plugins.as_mut().unwrap().set_enabled(
        "test.reactive-slow",
        false,
        &mut app.settings.lua_plugins,
    );
    r.poll_for(Duration::from_millis(450));
    assert_eq!(r.h.doc().note_count(), notes);
    assert!(
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin("test.reactive-slow")
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("STALE RESPONSE OUTPUT"))
    );
}

#[test]
fn a_later_external_selection_can_start_another_response_without_a_music_revision() {
    let mut r = Rig::new();
    let source =
        recursive_plugin().replace("events={'content_changed'}", "events={'selection_changed'}");
    r.install_response("test.reactive-selection", &source);
    let before = r.h.doc().note_count();
    Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 1, 60, 60);
    r.until(|r| r.h.doc().note_count() == before + 1);
    r.poll_for(Duration::from_millis(700));
    assert_eq!(r.h.doc().note_count(), before + 1);
    let state = r.h.app.open.as_ref().unwrap().history.current_state();
    let notes = &mut Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes;
    notes.clear_selection();
    notes.select_in_region(960, 961, 60, 60);
    assert_eq!(
        r.h.app.open.as_ref().unwrap().history.current_state(),
        state
    );
    r.until(|r| r.h.doc().note_count() == before + 2);
    r.poll_for(Duration::from_millis(700));
    assert_eq!(r.h.doc().note_count(), before + 2);
}

#[test]
fn same_revision_selection_change_discards_both_response_edits_and_notices() {
    let mut r = Rig::new();
    r.install_response("test.reactive-stale-selection",r#"
        function register(p)
            p.command{id='respond',name='Respond',run=function()
                local tx=plum.transaction{};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=66}});tx:commit()
                plum.progress(.5,'staged selection fixture')
                local until_time=os.clock()+0.08;while os.clock()<until_time do plum.yield() end
                plum.ui.notify('SAME-REVISION STALE NOTICE')
            end}
            p.observe{id='observe',events={'content_changed'},debounce_ms=20,run=function()
                p.request_transaction{command='respond'}
            end}
        end
    "#);
    let notes = r.h.doc().note_count();
    r.h.app.apply_op(Operation::AddTrack).unwrap();
    r.until(|r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin("test.reactive-stale-selection")
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .select_all();
    r.poll_for(Duration::from_millis(450));
    assert_eq!(r.h.doc().note_count(), notes);
    assert!(
        !r.h.app
            .status
            .as_ref()
            .is_some_and(|s| s.text.contains("SAME-REVISION STALE NOTICE"))
    );
    assert!(
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin("test.reactive-stale-selection")
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("captured project or selection changed"))
    );
}
