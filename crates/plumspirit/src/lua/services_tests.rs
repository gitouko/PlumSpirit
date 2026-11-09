use super::*;
use crate::lua::plugin_api::PluginMeta;
use crate::lua::worker::{ScriptJob, ScriptResult, ScriptWorker};
use crate::lua::{Limits, Plain, Scripts};
use std::time::Duration;
fn worker(caps: Vec<Capability>) -> ScriptWorker {
    ScriptWorker::spawn_plugin(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        caps,
        &Scripts::default(),
        Some(PluginMeta {
            id: "test.services".into(),
            name: "Services".into(),
            version: "1".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        }),
    )
    .unwrap()
}
fn script(w: &mut ScriptWorker, source: &str) -> ScriptResult {
    w.submit(ScriptJob {
        file: "services.lua".into(),
        source: source.into(),
        world: None,
        time_limit: None,
    })
    .unwrap();
    w.wait(Duration::from_secs(3)).unwrap()
}
fn payload() -> Payload {
    let host = Host::new(Limits::default(), []).unwrap();
    Payload::read(
        &host.lua,
        host.lua
            .load("return {tick=9007199254740993,value=21}")
            .eval()
            .unwrap(),
    )
    .unwrap()
}
fn job() -> Job {
    Job {
        id: "double".into(),
        version: Version { major: 1, minor: 1 },
        data: payload(),
        lifecycle: Fence::live(),
        seed: 17,
        call: None,
    }
}
fn provider(w: &mut ScriptWorker, body: &str) {
    let source = format!(
        "plum.plugin.service{{id='double',version='1.2',run=function(data,ctx) {body} end}};plum.plugin.__phase('ready')"
    );
    let result = script(w, &source);
    assert!(result.result.is_ok(), "{:?}", result.result);
    let decls = result.decls.unwrap();
    assert_eq!(decls.services[0].namespace, "test.services/double");
}
#[test]
fn provider_version_registration_and_dense_plain_dispatch_are_real_worker_operations() {
    let mut w = worker(vec![Capability::ProvideServices]);
    provider(
        &mut w,
        "assert(ctx.service=='test.services/double' and ctx.version=='1.2' and ctx.seed==17);return {tick=data.tick,value=data.value*2}",
    );
    w.submit_service(job()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(result.commits.is_none() && result.ui.is_none());
    let host = Host::new(Limits::default(), []).unwrap();
    host.lua
        .globals()
        .set(
            "reply",
            result.service_reply.unwrap().to_lua(&host.lua).unwrap(),
        )
        .unwrap();
    host.lua.load("assert(reply.tick==9007199254740993 and math.type(reply.tick)=='integer' and reply.value==42)").exec().unwrap();
    assert!(
        script(&mut w, "return plum.plugin.__service()")
            .result
            .is_err()
    );
    assert_eq!(
        script(&mut w, "return 'ordinary job'").result.unwrap(),
        Plain::String("ordinary job".into())
    );
}
#[test]
fn service_scope_denies_project_ui_settings_and_lifecycle_phase_escalation() {
    let mut w = worker(Capability::ALL.to_vec());
    provider(
        &mut w,
        r#"
        assert(plum.project.current()==nil)
        local forbidden={
            function() plum.transaction{} end,
            function() plum.selection.set_time(0,1) end,
            function() plum.ui.notify('forbidden') end,
            function() plum.config.set('x',1) end,
            function() plum.config.get('x') end,
            function() plum.presets.save('x',{}) end,
            function() plum.files.open{} end,
            function() plum.plugin.unobserve('any') end,
            function() plum.plugin.__phase('load');plum.plugin.command{id='invented',name='Invented',run=function()end} end,
            function() plum.plugin.__phase('load');plum.plugin.service{id='nested',version='1.0',run=function()end} end,
        }
        for i,f in ipairs(forbidden) do assert(not pcall(f),'service authority '..i) end
        return data
    "#,
    );
    w.submit_service(job()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(result.service_reply.is_some());
    assert!(
        result.plugin.project_data.is_none()
            && result.plugin.notices.is_empty()
            && result.commits.is_none()
    );
    assert!(w.poll_ui_request().is_none());
    assert!(
        script(
            &mut w,
            "return plum.plugin.command{id='late',name='Late',run=function()end}"
        )
        .result
        .is_err(),
        "native service exit restores registration phase"
    );
}
#[test]
fn service_versions_caps_reply_validation_and_expiry_are_bounded() {
    for version in [
        "",
        "1",
        "1.0.0",
        "65536.0",
        "-1.0",
        "1.nan",
        "1.9999999999999999999999999",
    ] {
        assert!(Version::parse(version).is_none(), "{version}");
    }
    assert_eq!(
        Version::parse("65535.65535"),
        Some(Version {
            major: 65535,
            minor: 65535
        })
    );
    let mut denied = worker(Vec::new());
    assert_eq!(
        script(
            &mut denied,
            "plum.plugin.service{id='double',version='1.0',run=function()end}"
        )
        .result
        .unwrap_err()
        .category,
        Category::Capability
    );
    let mut w = worker(vec![Capability::ProvideServices]);
    provider(&mut w, "return data");
    for version in [
        Version { major: 2, minor: 0 },
        Version { major: 1, minor: 3 },
    ] {
        let mut j = job();
        j.version = version;
        w.submit_service(j).unwrap();
        let result = w.wait(Duration::from_secs(3)).unwrap();
        assert_eq!(result.result.unwrap_err().category, Category::Compatibility);
        assert!(result.service_reply.is_none());
    }
    let j = job();
    j.lifecycle.invalidate();
    w.submit_service(j).unwrap();
    assert_eq!(
        w.wait(Duration::from_secs(3))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Expired
    );
    let mut bad = worker(vec![Capability::ProvideServices]);
    provider(&mut bad, "local t={};t.self=t;return t");
    bad.submit_service(job()).unwrap();
    let result = bad.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_err() && result.service_reply.is_none());
}
#[test]
fn service_declaration_caps_duplicates_and_cancellation_preserve_the_worker() {
    let mut w = worker(vec![Capability::ProvideServices]);
    let result = script(
        &mut w,
        r#"
        for i=1,32 do plum.plugin.service{id='s'..i,version='1.0',run=function()end} end
        assert(not pcall(function() plum.plugin.service{id='s1',version='1.0',run=function()end} end))
        assert(not pcall(function() plum.plugin.service{id='extra',version='1.0',run=function()end} end))
        plum.plugin.__phase('ready');assert(not pcall(function() plum.plugin.service{id='late',version='1.0',run=function()end} end))
    "#,
    );
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(result.decls.unwrap().services.len(), 32);
    let mut slow = worker(vec![Capability::ProvideServices]);
    provider(
        &mut slow,
        "plum.progress(.5,'service');while true do plum.yield() end",
    );
    let j = job();
    let life = Arc::clone(&j.lifecycle);
    slow.submit_service(j).unwrap();
    let start = std::time::Instant::now();
    while slow.progress().fraction != Some(0.5) {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        slow.submit_service(job()).is_err(),
        "one native service job per worker"
    );
    life.invalidate();
    slow.cancel();
    let result = slow.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_err() && result.service_reply.is_none());
    assert!(script(&mut slow, "return 1").result.is_ok());
    slow.submit_service(job()).unwrap();
    let timeout = slow.wait(Duration::from_secs(2)).unwrap();
    assert_eq!(timeout.result.unwrap_err().category, Category::TimeLimit);
    assert!(timeout.service_reply.is_none());
}
