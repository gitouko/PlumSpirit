//! Native diagnostics must not bypass the Lua VM's allocation limit.
use super::*;

#[test]
fn print_buffer_is_bounded_even_when_lua_reuses_one_small_string() {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    // Two MiB at most on the old path: this is a small exhaustion regression,
    // never an experiment that deliberately exhausts the process or machine.
    let error = h
        .run(
            "output-limit.lua",
            "local line=string.rep('x',16384); for i=1,128 do print(line) end",
        )
        .expect_err("Host output must have a bound separate from Lua memory");
    assert_eq!(error.category, Category::Budget, "{error:?}");
    let output = h.take_output();
    assert!(output.iter().map(|s| s.len() + 1).sum::<usize>() <= 1024 * 1024);
    h.run("after-output.lua", "print('usable again'); return true")
        .unwrap();
    assert_eq!(h.take_output(), ["usable again"]);
}

#[test]
fn print_lines_are_atomic_utf8_bounded_and_empty_calls_are_counted() {
    let h = Host::new(Limits::default(), []).unwrap();
    let error = h
        .run("line.lua", "print('before'); print(string.rep('x',16385))")
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert_eq!(h.take_output(), ["before"]);
    h.run("utf8.lua", "print(string.rep('😀',4096))").unwrap();
    assert_eq!(h.take_output()[0].len(), MAX_DIAGNOSTIC_LINE_BYTES);
    let error = h
        .run("lossy.lua", "print(string.rep(string.char(255),6000))")
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(h.take_output().is_empty(), "No partial lossy line");
    let error = h
        .run("blank.lua", "for i=1,1001 do print() end")
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert_eq!(h.take_output(), vec![String::new(); MAX_PRINT_LINES]);
}

#[test]
fn ordinary_formatting_and_reentrant_tostring_remain_compatible() {
    let h = Host::new(Limits::default(), []).unwrap();
    h.run(
        "format.lua",
        r#"
        local t=setmetatable({}, {__tostring=function() print('nested'); return 'table text' end})
        local bad=setmetatable({}, {__tostring=function() error('bad formatter') end})
        tostring=function() error('replacement global is not the native formatter') end
        print('π', nil, true, 17, 1.0, string.char(255), t)
        print(bad)
    "#,
    )
    .unwrap();
    assert_eq!(
        h.take_output(),
        ["nested", "π\tnil\ttrue\t17\t1\t�\ttable text", "table"]
    );
    let error = h
        .run(
            "large-meta.lua",
            r#"
        print(setmetatable({}, {__tostring=function() return string.rep('x',16385) end}))
    "#,
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(h.take_output().is_empty());
}

#[test]
fn persistent_log_bounds_bytes_work_and_unicode_while_retaining_recent_lines() {
    use super::plugins::{LOG_BYTES, LOG_LINES, Level, PluginLog};
    let mut log = PluginLog::default();
    let line = "😀".repeat(4096);
    for _ in 0..1000 {
        log.push(Level::Output, &line);
    }
    assert!(log.lines().map(|l| l.text.len()).sum::<usize>() <= LOG_BYTES);
    assert!(log.len() <= LOG_LINES);
    log.push(Level::Error, "π".repeat(50_000));
    let last = &log.lines().last().unwrap().text;
    assert!(last.len() <= MAX_DIAGNOSTIC_LINE_BYTES);
    assert!(last.ends_with(" [truncated]"));
    assert!(last.starts_with('π'));
    log.clear();
    assert!(log.is_empty());
    for i in 0..=LOG_LINES {
        log.push(Level::Info, i.to_string());
    }
    assert_eq!(log.len(), LOG_LINES);
    assert_eq!(log.lines().next().unwrap().text, "1");
    assert_eq!(log.lines().last().unwrap().text, LOG_LINES.to_string());
    log.clear();
    let many = "x\n".repeat(200_000);
    log.push(Level::Output, &many);
    assert!(log.len() <= LOG_LINES);
    assert!(log.lines().map(|l| l.text.len()).sum::<usize>() <= LOG_BYTES);
}

#[test]
fn progress_and_notices_keep_unicode_truncation_and_existing_rate_limits() {
    let h = Host::new(Limits::default(), []).unwrap();
    h.set_plugin(plugin_api::PluginMeta {
        id: "test.diagnostics".into(),
        name: "Diagnostics".into(),
        version: "1.0".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    h.run(
        "messages.lua",
        r#"
        local s=string.rep('π',200000)
        plum.progress(.5,s); plum.progress(2)
        for i=1,20 do plum.ui.notify(s,'warning') end
    "#,
    )
    .unwrap();
    let progress = h.progress_handle();
    let progress = progress.lock().unwrap();
    assert_eq!(progress.fraction, Some(1.0));
    assert_eq!(progress.message, "π".repeat(200));
    let notices = h.take_plugin_out().notices;
    assert_eq!(notices.len(), plugin_api::MAX_NOTICES);
    assert!(
        notices
            .iter()
            .all(|n| n.0 == "π".repeat(300) && n.1 == plugin_api::NoticeKind::Warning)
    );
    drop(progress);
    assert!(
        h.run("invalid.lua", "plum.progress(.5,string.char(255))")
            .is_err()
    );
    assert!(
        h.run("invalid.lua", "plum.ui.notify(string.char(255))")
            .is_err()
    );
    let error = h
        .run("kind.lua", "plum.ui.notify('x',string.rep('x',200000))")
        .unwrap_err();
    assert!(error.message.len() < 1024);
}

#[test]
fn diagnostic_exhaustion_discards_already_committed_private_musical_changes() {
    use plumspirit_engine::history::{History, HistoryLimits};
    use plumspirit_engine::model::{Bpm, PlumDocument};
    let doc = Arc::new(PlumDocument::new_project(
        "Diagnostics".into(),
        Bpm::DEFAULT,
        960,
    ));
    let before = doc.fingerprint();
    let mut history = History::new(HistoryLimits::default());
    history.reset(false);
    history.observe_ids(&doc);
    let h = Host::new(
        Limits::default(),
        [Capability::ReadProject, Capability::EditTracks],
    )
    .unwrap();
    h.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    let error = h
        .run(
            "rollback.lua",
            r#"
        local tx=plum.transaction{label='Private diagnostic test'}
        tx:add_track{name='Never published'}; tx:commit()
        local line=string.rep('x',16384); for i=1,128 do print(line) end
    "#,
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(h.take_commits().is_none());
    assert_eq!(doc.fingerprint(), before);
    assert!(!history.can_undo());
}

#[test]
fn worker_drains_bounded_output_and_recovers_for_the_next_job() {
    use super::worker::{ScriptJob, ScriptWorker};
    let mut w = ScriptWorker::spawn(Limits::default(), vec![], &Scripts::default()).unwrap();
    w.submit(ScriptJob {
        file: "worker-output.lua".into(),
        source: "local s=string.rep('x',16384);for i=1,128 do print(s) end".into(),
        world: None,
        time_limit: None,
    })
    .unwrap();
    let result = w
        .wait(Duration::from_secs(10))
        .expect("Bounded output job completes");
    assert_eq!(result.result.unwrap_err().category, Category::Budget);
    assert!(result.output.iter().map(|s| s.len() + 1).sum::<usize>() <= MAX_PRINT_BYTES);
    assert!(result.commits.is_none());
    w.submit(ScriptJob {
        file: "recovery.lua".into(),
        source: "print('recovered');return 7".into(),
        world: None,
        time_limit: None,
    })
    .unwrap();
    let result = w
        .wait(Duration::from_secs(10))
        .expect("The worker remains usable");
    assert_eq!(result.result.unwrap(), Plain::Integer(7));
    assert_eq!(result.output, ["recovered"]);
}
