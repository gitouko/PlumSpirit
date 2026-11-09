//! Manual isolated native UI CPU baseline; not an OS window/GPU/audio benchmark.
use super::test_support::Harness;
use crate::lua::plugins::{PluginManager, PluginPrefs, Status};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use std::path::PathBuf;
use std::time::{Duration, Instant};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-api02-frame-perf-{}-{tick}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let path = self.0.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(path.parent(), Some(base.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plum-api02-frame-perf-")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
fn memory() -> serde_json::Value {
    serde_json::json!({"working_set_bytes":crate::platform::working_set_bytes(),"private_commit_bytes":crate::platform::private_commit_bytes(),
        "os_process_peak_working_set_bytes":crate::platform::peak_working_set_bytes()})
}
#[test]
#[ignore = "Manual native frame baseline: PLUMSPIRIT_FRAME_PERF_CASE=0|22:1000000|10000000, fresh exact process"]
fn isolated_disabled_plugin_frames() {
    let case =
        std::env::var("PLUMSPIRIT_FRAME_PERF_CASE").expect("Set an explicit disabled:notes case");
    let (disabled, notes) = case.split_once(':').unwrap();
    let disabled: usize = disabled.parse().unwrap();
    let notes: usize = notes.parse().unwrap();
    assert!([0, 22].contains(&disabled));
    assert!([1_000_000, 10_000_000].contains(&notes));
    let per_pass = std::env::var("PLUMSPIRIT_FRAME_PERF_SAMPLES")
        .map_or(300, |v| v.parse::<usize>().expect("Integer frame samples"));
    assert!((10..=300).contains(&per_pass));
    let frames = 3 * per_pass;
    let temp = Temp::new();
    let process_before = memory();
    let t = Instant::now();
    let doc = generate(FixtureShape::Dense, notes, 960, 89451);
    let source = doc
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap();
    let original = doc.export_fingerprint();
    let fixture_ms = t.elapsed().as_secs_f64() * 1000.;
    let t = Instant::now();
    let mut h = Harness::new(doc);
    let construct_ms = t.elapsed().as_secs_f64() * 1000.;
    h.set_track(source);
    let folder = temp.0.join("plugins");
    std::fs::create_dir(&folder).unwrap();
    let mut prefs = PluginPrefs {
        folder: Some(folder.clone()),
        enable_on_install: false,
        ..Default::default()
    };
    let t = Instant::now();
    if disabled != 0 {
        let mut installer = PluginManager::new(Some(folder.clone()), h.app.scripts.clone());
        for example in &crate::lua::examples::EXAMPLES {
            installer.install_example(example, &mut prefs).unwrap();
        }
        assert!(!installer.is_busy());
        drop(installer);
    }
    let install_ms = t.elapsed().as_secs_f64() * 1000.;
    assert_eq!(prefs.installed.len(), disabled);
    assert!(
        prefs
            .installed
            .values()
            .all(|s| !s.enabled && s.granted.is_empty())
    );
    // New manager, not the installer's warmed manifest registry.
    let t = Instant::now();
    let mut manager = PluginManager::new(Some(folder), h.app.scripts.clone());
    manager.start(&prefs);
    let fresh_scan_ms = t.elapsed().as_secs_f64() * 1000.;
    assert_eq!(manager.plugins().len(), disabled);
    assert!(
        manager
            .plugins()
            .iter()
            .all(|p| p.status == Status::Disabled && !p.is_busy())
    );
    assert!(manager.plugins().iter().all(|p| p.decls.commands.is_empty()
        && p.decls.tools.is_empty()
        && p.decls.observers.is_empty()
        && p.decls.services.is_empty()));
    h.app.settings.lua_plugins = prefs;
    h.app.lua_plugins = Some(manager);
    h.app.lua_plugins_started = true;
    // The first native frame initializes a newly opened project's Track List.
    // Follow real commands afterward and establish the roll's layout before Fit.
    let startup = Instant::now();
    h.app_frame(egui::vec2(1366., 768.), 1.);
    assert!(h.command(super::commands::Command::MainView(
        super::track_list::MainView::PianoRoll
    )));
    h.app_frame(egui::vec2(1366., 768.), 1.);
    assert!(h.command(super::commands::Command::FitSong));
    let initial_ui_ms = startup.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let mut warm_frames = 0;
    loop {
        h.app_frame(egui::vec2(1366., 768.), 1.);
        warm_frames += 1;
        let stats = &h.app.roll.stats;
        if warm_frames >= 16
            && stats.primitives > 0
            && (!stats.lod
                || (stats.dense.ready == stats.dense.visible_tiles && stats.dense.pending == 0))
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "Native paint never ready: {:?}",
            h.app.roll.stats
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let warm_ms = start.elapsed().as_secs_f64() * 1000.;
    eprintln!("Native warm complete: {warm_frames} frames, {warm_ms:.3}ms");
    let (t0, visible_end) = h
        .app
        .roll
        .view
        .time
        .visible_range(f64::from(h.app.roll_width));
    let span = visible_end.saturating_sub(t0).max(1);
    let mut passes = Vec::new();
    let mut times = Vec::with_capacity(frames);
    let mut lane_times = Vec::with_capacity(frames);
    let mut maximum_pending = 0;
    let mut nonblank = 0;
    let mut dense = 0;
    for pass in 0..3 {
        let mut samples = Vec::with_capacity(per_pass);
        for i in 0..per_pass {
            let off = ((i as f64) * 0.05).sin() * span as f64 / 4.;
            h.app.roll.view.time.origin_tick = (t0 as f64 + span as f64 / 4. + off).max(0.) as u64;
            let t = Instant::now();
            let shapes = h.app_frame(egui::vec2(1366., 768.), 1.);
            std::hint::black_box(shapes);
            let ms = t.elapsed().as_secs_f64() * 1000.;
            samples.push(ms);
            times.push(ms);
            let stats = &h.app.roll.stats;
            lane_times.push(stats.lane_time.as_secs_f64() * 1000.);
            nonblank += usize::from(stats.primitives > 0);
            dense += usize::from(stats.lod);
            maximum_pending = maximum_pending.max(stats.dense.pending);
        }
        let (mean, median, p95, p99, max) = super::bench::summary(&samples);
        eprintln!("Native pass {pass}: median{median:.3}ms, p95{p95:.3}ms");
        passes.push(serde_json::json!({"pass":pass,"mean_ms":mean,"median_ms":median,"p95_ms":p95,"p99_ms":p99,"max_ms":max,"frames_ms":samples}));
    }
    assert_eq!(
        nonblank, frames,
        "All panned frames must contain native note primitives"
    );
    assert_eq!(h.doc().export_fingerprint(), original);
    assert!(!h.app.open.as_ref().unwrap().history.can_undo());
    assert_eq!(
        h.app.scripts.cancel_all(),
        0,
        "Disabled packages must create no live Lua hosts"
    );
    let manager = h.app.lua_plugins.as_ref().unwrap();
    assert!(!manager.is_busy());
    assert!(
        manager
            .plugins()
            .iter()
            .all(|p| p.status == Status::Disabled && !p.is_busy() && p.decls.observers.is_empty())
    );
    let (mean, median, p95, p99, max) = super::bench::summary(&times);
    let update = h.app.perf.slowest_update().cloned();
    let roll = h.app.roll.stats;
    let (_, lane_median, lane_p95, _, lane_max) = super::bench::summary(&lane_times);
    let pass=crate::diagnostics::FrameClock::take_last(&h.ctx).map(|p|serde_json::json!({"ui_ms":p.ui_ms,"begin_ms":p.begin_ms,"exit_ms":p.exit_ms,"end_ms":p.end_ms}));
    let diagnostic = serde_json::json!({"slowest_native_update":update,"last_pass":pass,
        "native_event_lane":{"median_ms":lane_median,"p95_ms":lane_p95,"max_ms":lane_max,"frames_ms":lane_times},
        "last_roll":{"frame_ms":roll.frame_time.as_secs_f64()*1000.,"lane_ms":roll.lane_time.as_secs_f64()*1000.,"notes_ms":roll.notes_time.as_secs_f64()*1000.,"query_ms":roll.query_time.as_secs_f64()*1000.,"onion_ms":roll.onion_time.as_secs_f64()*1000.,"primitives":roll.primitives,"notes_scanned":roll.notes_scanned,"dense_ready":roll.dense.ready,"dense_visible":roll.dense.visible_tiles}});

    println!(
        "PLUMSPIRIT_API02_FRAME_MEASUREMENT {}",
        serde_json::json!({"schema":1,"case":case,"notes":notes,"installed_disabled":disabled,"profile":if cfg!(debug_assertions){"debug"}else{"optimized"},"debug_assertions":cfg!(debug_assertions),"execution":"native headless CPU UI/paint preparation; no GPU/OS window/audio claim; CRT/toolchain recorded separately",
        "fixture":"existing Dense/960PPQ/seed89451","size":[1366,768],"dpi":1,"fixture_ms":fixture_ms,"app_construct_ms":construct_ms,"package_install_ms_excluded_from_scan":install_ms,"fresh_manager_scan_ms":fresh_scan_ms,"initial_native_ui_ms":initial_ui_ms,
        "warm_frames":warm_frames,"warm_ms":warm_ms,"panned_frames":frames,"nonblank_frames":nonblank,"lod_frames":dense,"maximum_pending_tiles":maximum_pending,"live_lua_hosts":0,
        "mean_ms":mean,"median_ms":median,"p95_ms":p95,"p99_ms":p99,"max_ms":max,"diagnostic":diagnostic,"passes":passes,"process_before":process_before,"process_after":memory()})
    );
    drop(h);
    drop(temp);
}
