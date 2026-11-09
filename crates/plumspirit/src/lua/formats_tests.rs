//! Real native worker/filesystem tests; file picker replies are controlled.
use super::*;
use crate::lua::worker::{ScriptJob, ScriptResult, ScriptWorker};
use crate::lua::{Limits, Plain, Scripts, World};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, PlumDocument};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-format-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn count(&self) -> usize {
        std::fs::read_dir(&self.0).unwrap().count()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let p = self.0.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            p.parent() == Some(base.as_path())
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("plum-format-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
fn meta(root: &Temp) -> super::super::plugin_api::PluginMeta {
    super::super::plugin_api::PluginMeta {
        id: "test.formats".into(),
        name: "Format Test".into(),
        version: "1.0.0".into(),
        root: root.0.clone(),
        data_dir: None,
        config_version: 1,
    }
}
fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    World::new(Arc::clone(doc), h.incarnations().snapshot())
}
fn project(keys: u16) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Format source".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    if keys == 256 {
        d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}
fn wait(w: &mut ScriptWorker, choice: Option<Option<PathBuf>>) -> (ScriptResult, usize) {
    let start = Instant::now();
    let mut prompts = 0;
    loop {
        if let Some(request) = w.poll_ui_request() {
            prompts += 1;
            assert_eq!(prompts, 1, "one format job can select only one stream");
            assert_eq!(request.spec.filters.len(), 1);
            assert_eq!(request.spec.filters[0].1, vec!["pnotes"]);
            assert!(
                ["Author Notes", "Note Exchange (four columns)"]
                    .contains(&request.spec.filters[0].0.as_str())
            );
            let stamp = request.lease.project;
            request
                .respond(stamp, choice.clone().expect("unexpected file prompt"))
                .unwrap();
        }
        if let Some(r) = w.poll() {
            return (r, prompts);
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "format worker did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn worker(root: &Temp, source: &str, caps: Vec<Capability>) -> ScriptWorker {
    worker_memory(root, source, caps, Limits::default().memory)
}
fn worker_memory(root: &Temp, source: &str, caps: Vec<Capability>, memory: usize) -> ScriptWorker {
    let mut w = ScriptWorker::spawn_plugin(
        Limits {
            memory,
            time: Duration::from_secs(5),
            instructions: None,
            ..Default::default()
        },
        caps,
        &Scripts::default(),
        Some(meta(root)),
    )
    .unwrap();
    w.submit(ScriptJob {
        file: "=format-register".into(),
        source: format!("{source}\nregister(plum.plugin);plum.plugin.__phase('ready')"),
        world: None,
        time_limit: None,
    })
    .unwrap();
    let (r, n) = wait(&mut w, None);
    assert_eq!(n, 0);
    r.result.unwrap();
    let decl = r.decls.unwrap();
    assert_eq!(decl.formats.len(), 1);
    assert!(!decl.is_empty());
    w
}
fn caps() -> Vec<Capability> {
    Capability::ALL.to_vec()
}
fn job(w: &mut ScriptWorker, mode: Mode, world: World, path: Option<PathBuf>) -> ScriptResult {
    w.submit_format(
        Request {
            id: "notes".into(),
            mode,
        },
        Some(world),
    )
    .unwrap();
    let (r, n) = wait(w, Some(path));
    assert_eq!(n, 1);
    r
}
const SOURCE: &str = r#"
function register(p)
 p.format{id='notes',name='Author Notes',extensions={'PNOTES'},
 import=function(ctx)
   assert(ctx.mode=='import'and ctx.format=='notes'and ctx.project and ctx.tx)
   assert(not plum.has('file.write_user_choice')and not plum.has('ui.panels')and not plum.has('playback.control'))
   assert(not pcall(plum.files.open)and not pcall(plum.plugin.__run_format))
   assert(not pcall(plum.config.project.set,'early',1)and not pcall(plum.config.set,'early',1)and not pcall(plum.presets.save,'early',{}))
   assert(not pcall(plum.transaction,{label='extra'}))
   local s=ctx.file:read_all();saved=ctx.file
   if s=='fail' then error('Author decode refused')end
   local writer=ctx.tx:note_writer(ctx.project:track(1))
   writer:append{start={tonumber(s)},gate={29},key={ctx.keys==256 and 200 or 60},velocity={93}}
   writer:finish();ctx.tx:commit()
   assert(not pcall(plum.transaction,{label='second'}))
 end,
 export=function(ctx)
   assert(ctx.mode=='export'and ctx.tx==nil and ctx.project)
   assert(not plum.has('notes.edit')and not plum.has('tracks.edit')and not plum.has('metadata.edit'))
   assert(not pcall(plum.transaction,{label='must not edit'}))
   assert(not pcall(plum.files.save)and not pcall(plum.files.open)and not pcall(plum.plugin.__run_format))
   ctx.file:write('AUTHOR\0\r\n');saved=ctx.file
   assert(not pcall(ctx.file.commit,ctx.file))
 end}
end
"#;
#[test]
fn strict_registration_reserved_extensions_freeze_caps_and_callback_snapshots() {
    let root = Temp::new();
    let h = Host::new(Limits::default(), []).unwrap();
    h.set_plugin(meta(&root)).unwrap();
    h.run("format-schema.lua",r#"
      local p=plum.plugin;assert(p.max_formats==32)
      local function valid()return{id='x',name='Test',extensions={'abc'},import=function()end}end
      for _,ext in ipairs({{}, {'mid'},{'MIDI'},{'lup'},{'rmi'},{'x','X'},{'../x'},{'.abc'},{'*'},{string.rep('a',17)},{1},{[1]='x',[3]='y'},{[1]='x',extra='y'},setmetatable({'x'},{})})do
        local d=valid();d.extensions=ext;assert(not pcall(p.format,d))
      end
      for _,change in ipairs({{key='id',value='BAD'},{key='id',value=1},{key='id',value=string.rep('x',65)},{key='name',value=string.rep('x',257)},{key='name',value=string.char(255)},{key='name',value='bad\nname'},{key='extra',value=1},{key='import',value=true}})do
        local d=valid();d[change.key]=change.value;assert(not pcall(p.format,d))
      end
      local d=valid();d.import=nil;assert(not pcall(p.format,d));assert(not pcall(p.format,setmetatable(valid(),{})))
      p.format(valid());assert(not pcall(p.format,valid()))
      for i=2,32 do local d=valid();d.id='format'..i;p.format(d)end
      local d=valid();d.id='overflow';assert(not pcall(p.format,d))
      p.__phase('ready');d.id='late';assert(not pcall(p.format,d));assert(not pcall(p.__run_format))
    "#).unwrap();
    let d = h.take_decls().unwrap();
    assert_eq!(d.formats.len(), 32);
    assert_eq!(d.formats[0].extensions, vec!["abc"]);
    assert!(d.formats[0].import && !d.formats[0].export);
}
#[test]
fn real_worker_import_is_private_exact_and_one_undo_redo_in_both_modes() {
    for keys in [128, 256] {
        let root = Temp::new();
        let path = root.file("notes.pnotes");
        std::fs::write(&path, b"9007199254741001").unwrap();
        let mut w = worker(&root, SOURCE, caps());
        let (mut doc, mut h) = project(keys);
        let before = doc.export_fingerprint();
        let r = job(&mut w, Mode::Import, world(&doc, &mut h), Some(path));
        assert!(matches!(r.result, Ok(Plain::Boolean(true))));
        let mut c = r.commits.unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        assert_eq!(c.steps.len(), 1);
        let n = c.result().track(0).unwrap().notes.iter().next().unwrap();
        assert_eq!({ n.start_tick }, 9007199254741001);
        assert_eq!({ n.duration_ticks }, 29);
        assert_eq!(n.attrs.pitch(), if keys == 256 { 200 } else { 60 });
        let expected = c.result().export_fingerprint();
        c.apply(&mut h, &mut doc).unwrap();
        drop(c);
        h.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        h.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        w.submit(ScriptJob{file:"=restored".into(),source:"assert(plum.has('file.write_user_choice')and plum.has('ui.panels')and not pcall(saved.info,saved))".into(),world:Some(world(&doc,&mut h)),time_limit:None}).unwrap();
        wait(&mut w, None).0.result.unwrap();
        drop(w);
    }
}
#[test]
fn real_export_publishes_only_at_success_and_restores_scoped_authority() {
    let root = Temp::new();
    let path = root.file("notes.PNOTES");
    std::fs::write(&path, b"old").unwrap();
    let mut w = worker(&root, SOURCE, caps());
    let (doc, mut h) = project(128);
    let before = doc.export_fingerprint();
    let r = job(
        &mut w,
        Mode::Export,
        world(&doc, &mut h),
        Some(path.clone()),
    );
    r.result.unwrap();
    assert!(r.commits.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"AUTHOR\0\r\n");
    assert_eq!(root.count(), 1);
    assert_eq!(doc.export_fingerprint(), before);
    w.submit(ScriptJob{file:"=restored".into(),source:"assert(plum.has('notes.edit')and plum.has('metadata.edit')and not pcall(saved.write,saved,'late'))".into(),world:Some(world(&doc,&mut h)),time_limit:None}).unwrap();
    wait(&mut w, None).0.result.unwrap();
    drop(w);
}
#[test]
fn failed_canceled_and_wrong_extension_selection_leave_file_and_project_unchanged() {
    let root = Temp::new();
    let path = root.file("notes.pnotes");
    std::fs::write(&path, b"fail").unwrap();
    let (doc, mut h) = project(128);
    let before = doc.export_fingerprint();
    let mut w = worker(&root, SOURCE, caps());
    let r = job(
        &mut w,
        Mode::Import,
        world(&doc, &mut h),
        Some(path.clone()),
    );
    assert!(r.result.is_err() && r.commits.is_none());
    assert_eq!(doc.export_fingerprint(), before);
    let r = job(&mut w, Mode::Import, world(&doc, &mut h), None);
    assert!(matches!(r.result, Ok(Plain::Boolean(false))) && r.commits.is_none());
    let wrong = root.file("native.lup");
    std::fs::write(&wrong, b"native").unwrap();
    let r = job(
        &mut w,
        Mode::Export,
        world(&doc, &mut h),
        Some(wrong.clone()),
    );
    assert!(r.result.is_err() && r.commits.is_none());
    assert_eq!(std::fs::read(wrong).unwrap(), b"native");
    drop(w);
    let source = SOURCE.replace(
        "ctx.file:write('AUTHOR\\0\\r\\n');saved=ctx.file",
        "ctx.file:write('private');error('Author encoder refused');saved=ctx.file",
    );
    let mut w = worker(&root, &source, caps());
    let r = job(
        &mut w,
        Mode::Export,
        world(&doc, &mut h),
        Some(path.clone()),
    );
    assert!(r.result.is_err() && r.commits.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"fail");
    assert_eq!(root.count(), 2);
    drop(w);
}
#[test]
fn missing_capabilities_refuse_before_prompt_and_native_dispatch_cannot_be_forged() {
    let root = Temp::new();
    let (doc, mut h) = project(128);
    for (mode, caps) in [
        (
            Mode::Import,
            vec![Capability::ReadProject, Capability::ReadUserFile],
        ),
        (Mode::Export, vec![Capability::ReadProject]),
        (
            Mode::Import,
            vec![Capability::ReadUserFile, Capability::EditNotes],
        ),
    ] {
        let mut w = worker(&root, SOURCE, caps);
        w.submit_format(
            Request {
                id: "notes".into(),
                mode,
            },
            Some(world(&doc, &mut h)),
        )
        .unwrap();
        let (r, n) = wait(&mut w, None);
        assert_eq!(n, 0);
        assert_eq!(r.result.unwrap_err().category, Category::Capability);
        assert!(r.commits.is_none());
    }
    let mut w = worker(&root, SOURCE, caps());
    w.submit(ScriptJob {
        file: "=forged".into(),
        source: "return plum.plugin.__run_format()".into(),
        world: Some(world(&doc, &mut h)),
        time_limit: None,
    })
    .unwrap();
    let (r, n) = wait(&mut w, None);
    assert_eq!(n, 0);
    assert_eq!(r.result.unwrap_err().category, Category::Capability);
    drop(w);
}

#[test]
fn cancel_and_destination_conflict_discard_private_output_and_worker_recovers() {
    let source = r#"function register(p)
      p.format{id='notes',name='Author Notes',extensions={'pnotes'},export=function(ctx)
        ctx.file:write('private');plum.progress(0.5,'staged')
        local finish=os.clock()+0.5;while os.clock()<finish do end
      end}
    end"#;
    for cancel in [true, false] {
        let root = Temp::new();
        let path = root.file("notes.pnotes");
        std::fs::write(&path, b"original").unwrap();
        let (doc, mut h) = project(128);
        let mut w = worker(&root, source, caps());
        w.submit_format(
            Request {
                id: "notes".into(),
                mode: Mode::Export,
            },
            Some(world(&doc, &mut h)),
        )
        .unwrap();
        let start = Instant::now();
        let mut selected = false;
        loop {
            if let Some(request) = w.poll_ui_request() {
                assert!(!selected);
                selected = true;
                let stamp = request.lease.project;
                request.respond(stamp, Some(path.clone())).unwrap();
            }
            if w.progress().fraction == Some(0.5) {
                break;
            }
            assert!(
                w.poll().is_none(),
                "format finished before staged-output checkpoint"
            );
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(selected);
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        if cancel {
            w.cancel();
        } else {
            std::fs::write(&path, b"outside replacement with different length").unwrap();
        }
        let (r, n) = wait(&mut w, None);
        assert_eq!(n, 0);
        let err = r.result.unwrap_err();
        assert_eq!(
            err.category,
            if cancel {
                Category::Canceled
            } else {
                Category::Runtime
            }
        );
        assert!(r.commits.is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            if cancel {
                b"original".as_slice()
            } else {
                b"outside replacement with different length".as_slice()
            }
        );
        assert_eq!(root.count(), 1);
        w.submit(ScriptJob {
            file: "=post-format-failure".into(),
            source: "assert(plum.has('notes.edit')and plum.has('ui.panels'))".into(),
            world: Some(world(&doc, &mut h)),
            time_limit: None,
        })
        .unwrap();
        wait(&mut w, None).0.result.unwrap();
        drop(w);
    }
}
#[test]
fn vm_and_output_budget_errors_after_staging_keep_existing_destination() {
    for body in [
        "local huge=string.rep('x',8*1024*1024)",
        "ctx.file:write(string.rep('x',1024*1024+1))",
    ] {
        let root = Temp::new();
        let path = root.file("notes.pnotes");
        std::fs::write(&path, b"existing").unwrap();
        let source = format!(
            "function register(p)p.format{{id='notes',name='Author Notes',extensions={{'pnotes'}},export=function(ctx)ctx.file:write('private');{body} end}}end"
        );
        let mut w = worker_memory(&root, &source, caps(), 4 * 1024 * 1024);
        let (doc, mut h) = project(128);
        let r = job(
            &mut w,
            Mode::Export,
            world(&doc, &mut h),
            Some(path.clone()),
        );
        let err = r.result.unwrap_err();
        assert!(matches!(err.category, Category::Memory | Category::Budget));
        assert!(r.commits.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
        assert_eq!(root.count(), 1);
        drop(w);
    }
}
#[test]
fn registered_functions_and_extensions_are_snapshots_of_the_authored_declaration() {
    let root = Temp::new();
    let path = root.file("notes.pnotes");
    let source = r#"function register(p)
      local d={id='notes',name='Author Notes',extensions={'PNOTES'},export=function(ctx)ctx.file:write('original callback')end}
      p.format(d);d.name='Changed';d.extensions[1]='mid';d.export=function()error('mutated callback')end
    end"#;
    let mut w = worker(&root, source, caps());
    let (doc, mut h) = project(128);
    job(
        &mut w,
        Mode::Export,
        world(&doc, &mut h),
        Some(path.clone()),
    )
    .result
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"original callback");
    drop(w);
}

#[test]
fn installed_manager_routes_import_only_to_review_result_and_disable_invalidates_its_fence() {
    use crate::lua::plugins::{PluginEvent, PluginManager, PluginPrefs, Status};
    let root = Temp::new();
    let src = root.file("source");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(
        src.join("plum-plugin.toml"),
        r#"id="test.formats"
name="Format Test"
version="1.0.0"
api="0.2"
entry="main.lua"
capabilities=["project.read","notes.edit","file.read_user_choice","file.write_user_choice"]
"#,
    )
    .unwrap();
    std::fs::write(src.join("main.lua"), SOURCE).unwrap();
    let input = root.file("input.pnotes");
    std::fs::write(&input, b"123").unwrap();
    let mut m = PluginManager::new(Some(root.file("plugins")), Scripts::default());
    let mut prefs = PluginPrefs::default();
    m.install(&src, false, &mut prefs).unwrap();
    for c in [
        Capability::ReadProject,
        Capability::EditNotes,
        Capability::ReadUserFile,
        Capability::WriteUserFile,
    ] {
        m.set_granted("test.formats", c.name(), true, &mut prefs);
    }
    let (doc, mut h) = project(256);
    let before = doc.export_fingerprint();
    m.project_opened(world(&doc, &mut h));
    m.set_enabled("test.formats", true, &mut prefs);
    let settle = |m: &mut PluginManager, h: &mut History, choice: Option<PathBuf>| {
        let start = Instant::now();
        let mut out = Vec::new();
        let mut prompts = 0;
        loop {
            for e in m.poll(&prefs) {
                if matches!(e, PluginEvent::NeedWorld) {
                    m.supply_world(world(&doc, h));
                }
                out.push(e);
            }
            if let Some(request) = m.poll_ui_request() {
                prompts += 1;
                let stamp = request.lease.project;
                request.respond(stamp, choice.clone()).unwrap();
            }
            if !m.is_busy() {
                out.extend(m.poll(&prefs));
                return (out, prompts);
            }
            assert!(start.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    settle(&mut m, &mut h, None);
    assert_eq!(m.plugin("test.formats").unwrap().status, Status::Ready);
    assert_eq!(m.plugin("test.formats").unwrap().decls.formats.len(), 1);
    m.run_format(
        "test.formats",
        "notes",
        Mode::Import,
        Some(world(&doc, &mut h)),
    )
    .unwrap();
    assert!(
        m.run_command("test.formats", "missing", Some(world(&doc, &mut h)))
            .is_err()
    );
    let (events, prompts) = settle(&mut m, &mut h, Some(input));
    assert_eq!(prompts, 1);
    assert!(!events.iter().any(|e| matches!(
        e,
        PluginEvent::Commits { .. } | PluginEvent::Select { .. } | PluginEvent::ProjectData { .. }
    )));
    let (preview, fence) = events
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::FormatResult {
                plugin,
                format,
                mode,
                selected,
                result,
                lifecycle,
            } => {
                assert_eq!(plugin, "test.formats");
                assert_eq!(format, "notes");
                assert_eq!(mode, Mode::Import);
                assert!(selected);
                Some((result.unwrap().unwrap(), lifecycle))
            }
            _ => None,
        })
        .expect("native review-only result");
    assert_eq!(doc.export_fingerprint(), before);
    assert_eq!(preview.result().note_count(), 1);
    assert!(fence.active());
    m.set_enabled("test.formats", false, &mut prefs);
    assert!(!fence.active());
    assert_eq!(doc.export_fingerprint(), before);
    drop(preview);
    drop(m);
}

#[test]
fn published_author_codec_roundtrips_exact_ticks_through_native_writer_and_streamed_columns() {
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Registered format adapters (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    for keys in [128, 256] {
        let root = Temp::new();
        let input = root.file("input.pnotes");
        let output = root.file("output.pnotes");
        let pitch = if keys == 256 { 200 } else { 60 };
        let expected = format!("PNOTES1\n0,24,{pitch},96\n9007199254741001,29,48,93\n");
        std::fs::write(&input, &expected).unwrap();
        let mut w = worker(&root, source, caps());
        let (mut doc, mut h) = project(keys);
        let before = doc.export_fingerprint();
        let r = job(&mut w, Mode::Import, world(&doc, &mut h), Some(input));
        r.result.unwrap();
        let mut c = r.commits.unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        assert_eq!(c.steps.len(), 1);
        assert_eq!(c.result().note_count(), 2);
        c.apply(&mut h, &mut doc).unwrap();
        drop(c);
        let after = doc.export_fingerprint();
        let r = job(
            &mut w,
            Mode::Export,
            world(&doc, &mut h),
            Some(output.clone()),
        );
        r.result.unwrap();
        assert!(r.commits.is_none());
        assert_eq!(std::fs::read_to_string(output).unwrap(), expected);
        assert_eq!(doc.export_fingerprint(), after);
        h.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        h.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), after);
        drop(w);
    }
}

#[test]
fn authored_streaming_codec_handles_hundred_thousand_notes_and_split_crlf_in_eight_mib_vm() {
    let root = Temp::new();
    let input = root.file("large.pnotes");
    let output = root.file("roundtrip.pnotes");
    let source = super::super::examples::by_id("plumspirit.example.note-exchange")
        .unwrap()
        .main_lua();
    let mut bytes = String::from("PNOTES1\r\n");
    bytes.push_str(&"\r\n".repeat(8188));
    let mut expected = String::from("PNOTES1\n");
    for i in 0..100_000 {
        let row = format!("{},24,200,96", i * 2);
        bytes.push_str(&row);
        bytes.push_str("\r\n");
        expected.push_str(&row);
        expected.push('\n');
    }
    assert_eq!(bytes.as_bytes()[16383], b'\r');
    assert_eq!(bytes.as_bytes()[16384], b'\n');
    std::fs::write(&input, bytes).unwrap();
    let mut w = worker_memory(&root, source, caps(), 8 * 1024 * 1024);
    let (mut doc, mut h) = project(256);
    let before = doc.export_fingerprint();
    let r = job(&mut w, Mode::Import, world(&doc, &mut h), Some(input));
    r.result.unwrap();
    let mut c = r.commits.unwrap();
    assert_eq!(c.result().note_count(), 100_000);
    assert_eq!(c.steps.len(), 1);
    assert_eq!(doc.export_fingerprint(), before);
    let after = c.result().export_fingerprint();
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    let r = job(
        &mut w,
        Mode::Export,
        world(&doc, &mut h),
        Some(output.clone()),
    );
    r.result.unwrap();
    assert!(r.commits.is_none());
    assert_eq!(std::fs::read_to_string(output).unwrap(), expected);
    assert_eq!(doc.export_fingerprint(), after);
    h.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
    assert_eq!(doc.export_fingerprint(), before);
    h.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
    assert_eq!(doc.export_fingerprint(), after);
    drop(w);
}
#[test]
fn authored_codec_failure_after_accepted_batches_discards_import_and_retries_cleanly() {
    let root = Temp::new();
    let input = root.file("broken.pnotes");
    let source = super::super::examples::by_id("plumspirit.example.note-exchange")
        .unwrap()
        .main_lua();
    let data = format!("PNOTES1\n{}malformed\n", "0,24,60,96\n".repeat(4097));
    std::fs::write(&input, data).unwrap();
    let mut w = worker_memory(&root, source, caps(), 8 * 1024 * 1024);
    let (doc, mut h) = project(128);
    let before = doc.export_fingerprint();
    let r = job(
        &mut w,
        Mode::Import,
        world(&doc, &mut h),
        Some(input.clone()),
    );
    assert!(r.result.is_err() && r.commits.is_none());
    assert_eq!(doc.export_fingerprint(), before);
    std::fs::write(&input, b"PNOTES1\n9007199254741001,24,60,96").unwrap();
    let r = job(&mut w, Mode::Import, world(&doc, &mut h), Some(input));
    r.result.unwrap();
    let c = r.commits.unwrap();
    assert_eq!(c.result().note_count(), 1);
    let n = c.result().track(0).unwrap().notes.iter().next().unwrap();
    assert_eq!({ n.start_tick }, 9007199254741001);
    assert_eq!(doc.export_fingerprint(), before);
    drop(c);
    drop(w);
}
