//! B1-0045 – B1-0048: custom tool declarations (schema checks), runs
//! (parameters exactly as declared, the selection, 256 keys, one
//! transaction = one Undo step, Preview = Apply, Cancel changes nothing) and
//! hot refresh with rollback.

use super::custom::{ToolParamKind, ToolValue, params_literal};
use super::plugin_api::PluginMeta;
use super::plugins::{PluginEvent, PluginManager, PluginPrefs, Status, ToolJob};
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOAD: &str = "plum.plugin.__phase('load') if type(on_load) == 'function' then on_load() end \
    plum.plugin.__phase('register') if type(register) == 'function' then register(plum.plugin) end \
    plum.plugin.__phase('ready')";

fn plugin_host(caps: &[Capability], entry: &str) -> Result<Host, ScriptError> {
    let h = Host::new(Limits::default(), caps.iter().copied())?;
    h.set_plugin(PluginMeta {
        id: "test.tools".into(),
        name: "Tools".into(),
        version: "1.0".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })?;
    h.run("main.lua", entry)?;
    h.run("=load", LOAD)?;
    Ok(h)
}

const SCHEMA: &str = r#"
function register(p)
  p.tool{ id = "echo", name = "Echo Params", tooltip = "Returns its parameters.", needs_selection = false,
    params = {
      { key = "n", kind = "int", min = -5, max = 5, default = 2 },
      { key = "f", kind = "float", min = 0, max = 1, default = 0.25, unit = "%" },
      { key = "b", kind = "bool", default = true },
      { key = "c", kind = "choice", choices = { "Up", "Down" }, default = "Down" },
      { key = "k", kind = "pitch", default = 200 },
      { key = "t", kind = "ticks", min = 1, max = 960, default = 240 },
      { key = "s", kind = "seed" },
      { key = "col", kind = "color", default = { r = 1, g = 2, b = 3 } },
      { key = "cv", kind = "curve", default = { { 0, 0 }, { 0.5, 1 }, { 1, 0 } } },
      { key = "r", kind = "range", min = 0, max = 127, default = { 10, 20 } },
      { key = "m", kind = "subchannel_map", default = { { 0, 3 }, { 1, 4 } } },
      { key = "nf", kind = "note_filter", default = { keys = { 60, 72 }, velocity = { 2, 100 }, channel = 9 } },
      { key = "txt", kind = "text", max_len = 20, default = "a \"quoted\"\nline" },
    },
    run = function(ctx)
      local p = ctx.params
      return table.concat({ p.n, p.f, tostring(p.b), p.c, p.k, p.t, p.s,
        p.col.r .. "/" .. p.col.g .. "/" .. p.col.b,
        p.cv.at(0.25) .. "/" .. #p.cv.points,
        p.r.lo .. "-" .. p.r.hi, p.m[2].from .. ">" .. p.m[2].to,
        p.nf.keys[1] .. "-" .. p.nf.keys[2] .. "/" .. p.nf.velocity[2] .. "/" .. p.nf.channel,
        p.txt, tostring(ctx.preview), ctx.seed, ctx.keys, tostring(ctx.selection) }, "|")
    end }
end
"#;

fn project(notes: u64, keys256: bool) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    if keys256 {
        d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let recs: Vec<NoteRecord> = (0..notes)
        .map(|k| NoteRecord {
            start_tick: k * 120,
            duration_ticks: 100,
            attrs: NoteAttrs::new(60, 64, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    World::new(Arc::clone(doc), h.incarnations().snapshot())
}

#[test]
fn every_parameter_kind_reaches_the_script_exactly_as_declared() {
    let (doc, mut h) = project(4, true);
    let host = plugin_host(&[Capability::ReadProject, Capability::EditNotes], SCHEMA)
        .unwrap_or_else(|e| panic!("{e}"));
    let decl = host.take_decls().expect("decls").tools.remove(0);
    assert_eq!(decl.params.len(), 13);
    assert_eq!(decl.params[4].kind, ToolParamKind::Pitch);
    let values = decl.defaults();
    assert_eq!(values[3], ToolValue::Choice(1));
    let lit = params_literal(&decl, &values);
    let track = doc.track(0).unwrap().id().0;
    host.set_world(Some(world(&doc, &mut h)));
    let r = host
        .run(
            "=run",
            &format!(
                "return plum.plugin.__run_tool('echo', {lit}, {{ track = {track}, preview = true, seed = 77, keys = 256 }})"
            ),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r,
        Plain::String(
            "2|0.25|true|Down|200|240|1|1/2/3|0.5/3|10-20|1>4|60-72/100/9|a \"quoted\"\nline|true|77|256|false"
                .into()
        )
    );
    // A run that changes nothing commits nothing.
    assert!(host.take_commits().is_none());
}
#[test]
fn optional_multiline_text_is_strict_and_preserves_legacy_defaults_and_literal_bytes() {
    let entry = r#"function register(p)p.tool{id='text',name='Text',needs_selection=false,params={
      {key='old',kind='text',max_len=100,default='legacy'},
      {key='lines',kind='text',multiline=true,max_len=100,default='é\r\nTwo👨‍👩‍👧‍👦'}},
      run=function(ctx)assert(ctx.params.old=='legacy');assert(ctx.owner_track==ctx.track);return ctx.params.lines end}end"#;
    let host = plugin_host(&[Capability::ReadProject, Capability::EditNotes], entry).unwrap();
    let decl = host.take_decls().unwrap().tools.remove(0);
    assert_eq!(
        decl.params[0].kind,
        ToolParamKind::Text {
            max_len: 100,
            multiline: false
        }
    );
    assert_eq!(
        decl.params[1].kind,
        ToolParamKind::Text {
            max_len: 100,
            multiline: true
        }
    );
    let (doc, mut history) = project(0, false);
    host.set_world(Some(world(&doc, &mut history)));
    let values = decl.defaults();
    let literal = params_literal(&decl, &values);
    let result=host.run("multiline-values.lua",&format!("return plum.plugin.__run_tool('text',{literal},{{track={},preview=true,keys=128}})",doc.track(0).unwrap().id().0)).unwrap();
    assert_eq!(result, Plain::String("e\u{301}\r\nTwo👨‍👩‍👧‍👦".into()));
    assert!(host.take_commits().is_none());
    for value in ["1", "'true'", "{}"] {
        let invalid = entry.replace("multiline=true", &format!("multiline={value}"));
        assert!(plugin_host(&[], &invalid).is_err());
    }
    let invalid = entry.replace("default='é\\r\\nTwo👨‍👩‍👧‍👦'", "default=string.char(255)");
    assert!(plugin_host(&[], &invalid).is_err());
}

#[test]
fn declarations_are_checked_and_only_while_loading() {
    for (src, words) in [
        (
            "p.tool{ id = 'Bad Id', name = 'X', run = function() end }",
            "tool id \u{201C}Bad Id",
        ),
        (
            "p.tool{ id = 'x', run = function() end }",
            "name is missing",
        ),
        ("p.tool{ id = 'x', name = 'X' }", "run must be a function"),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end, params = { { key = 'a', kind = 'nope' } } }",
            "unknown kind",
        ),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end, params = { { key = 'a', kind = 'int' }, { key = 'a', kind = 'bool' } } }",
            "used twice",
        ),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end, params = { { key = 'a', kind = 'int', min = 0, max = 5, default = 9 } } }",
            "0 – 5",
        ),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end, params = { { key = 'a', kind = 'choice', choices = {} } } }",
            "1 – 64 choices",
        ),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end, params = { { key = 'a', kind = 'curve', default = { { 0, 2 }, { 1, 1 } } } } }",
            "0 – 1",
        ),
        (
            "p.tool{ id = 'x', name = 'X', run = function() end } p.tool{ id = 'x', name = 'Y', run = function() end }",
            "already registered",
        ),
    ] {
        let Err(e) = plugin_host(&[], &format!("function register(p) {src} end")) else {
            panic!("{src} was accepted");
        };
        assert_eq!(e.category, Category::Runtime, "{src}: {e}");
        assert!(e.message.contains(words), "{words}: {}", e.message);
    }
    // After loading: refused.
    let h = plugin_host(
        &[],
        "function later() plum.plugin.tool{ id = 'x', name = 'X', run = function() end } end",
    )
    .unwrap();
    let e = h.run("=late", "later()").unwrap_err();
    assert!(e.message.contains("while the plugin loads"), "{e}");
}

const SHIFT: &str = r#"
function register(p)
  p.tool{ id = "up", name = "Raise", tooltip = "Raises the selected notes.",
    params = { { key = "by", kind = "int", min = -24, max = 24, default = 12 } },
    run = function(ctx)
      ctx.tx:modify_notes(ctx.notes, { transpose = ctx.params.by })
    end }
end
"#;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-custom-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const MANIFEST: &str = "id = \"test.shift\"\nname = \"Shift\"\nversion = \"1.0\"\napi = \"0.1\"\ncapabilities = [\"project.read\", \"notes.edit\"]\n";

fn settle(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    w: &mut dyn FnMut() -> World,
) -> Vec<PluginEvent> {
    let t = Instant::now();
    let mut out = Vec::new();
    loop {
        for e in m.poll(prefs) {
            if matches!(e, PluginEvent::NeedWorld) {
                m.supply_world(w());
            }
            out.push(e);
        }
        if !m.is_busy() {
            out.extend(m.poll(prefs));
            return out;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_tool_previews_applies_as_one_undo_step_and_cancel_changes_nothing() {
    let root = temp("apply");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("plum-plugin.toml"), MANIFEST).unwrap();
    std::fs::write(src.join("main.lua"), SHIFT).unwrap();
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs {
        enable_on_install: true,
        ..PluginPrefs::default()
    };
    m.install(&src, false, &mut prefs).unwrap();
    let (mut doc, mut h) = project(10, false);
    // Select notes 0 – 4.
    {
        let d = Arc::get_mut(&mut doc).unwrap();
        let mut b = plumspirit_engine::model::RankSetBuilder::default();
        for r in 0..5 {
            b.push(r);
        }
        d.track_mut(0)
            .unwrap()
            .notes
            .select_ranks(&b.finish())
            .unwrap();
    }
    let before = doc.export_fingerprint();
    settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let p = m.plugin("test.shift").unwrap();
    assert_eq!(p.status, Status::Ready);
    let decl = p.decls.tools[0].clone();
    assert_eq!((decl.id.as_str(), decl.name.as_str()), ("up", "Raise"));
    let track = doc.track(0).unwrap().id().0;
    let job = |preview: bool, generation: u64, w: World| ToolJob {
        stroke: None,
        tool: "up".into(),
        params: params_literal(&decl, &decl.defaults()),
        track,
        content: None,
        preview,
        seed: 1,
        keys: 128,
        generation,
        world: Some(w),
    };
    // Preview: transactions come back; the document is untouched.
    m.run_tool("test.shift", job(true, 1, world(&doc, &mut h)))
        .unwrap();
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let mut preview = None;
    for e in ev {
        if let PluginEvent::ToolResult {
            generation: 1,
            preview: true,
            result,
            ..
        } = e
        {
            preview = result.unwrap();
        }
    }
    let mut commits = preview.expect("a preview result");
    assert_eq!(
        doc.export_fingerprint(),
        before,
        "previewing changes nothing (Cancel = nothing to undo)"
    );
    let shown = commits.result().export_fingerprint();
    let mut pitches = Vec::new();
    commits
        .result()
        .track(0)
        .unwrap()
        .notes
        .query(0, u64::MAX, |_, v| pitches.push(v.attrs.pitch()));
    assert_eq!(
        pitches.iter().filter(|&&p| p == 72).count(),
        5,
        "only the selection"
    );
    // Apply = the previewed transactions: one Undo step named after the tool.
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.export_fingerprint(), shown, "Preview = Apply");
    assert_eq!(h.undo_label(), Some("Raise"));
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    assert_eq!(d.export_fingerprint(), before);
    // Without a selection a needs_selection tool refuses with a sentence.
    {
        let d = Arc::get_mut(&mut doc).unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .select_ranks(&plumspirit_engine::model::RankSetBuilder::default().finish())
            .unwrap();
    }
    m.run_tool("test.shift", job(false, 2, world(&doc, &mut h)))
        .unwrap();
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let err = ev.into_iter().find_map(|e| match e {
        PluginEvent::ToolResult {
            generation: 2,
            result: Err(e),
            ..
        } => Some(e),
        _ => None,
    });
    assert!(err.is_some_and(|e| e.contains("select some first")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn refreshing_an_edited_plugin_keeps_the_old_version_when_the_new_one_fails() {
    let root = temp("refresh");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("plum-plugin.toml"), MANIFEST).unwrap();
    std::fs::write(src.join("main.lua"), SHIFT).unwrap();
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs {
        enable_on_install: true,
        ..PluginPrefs::default()
    };
    m.install(&src, false, &mut prefs).unwrap();
    let (doc, mut h) = project(1, false);
    settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    assert_eq!(m.plugin("test.shift").unwrap().decls.tools[0].name, "Raise");
    // The user edits the installed script and breaks it.
    let installed = root.join("plugins/test.shift/main.lua");
    std::fs::write(
        &installed,
        "function register(p) p.tool{ id = 'up', name = 'Broken'",
    )
    .unwrap();
    m.refresh(&prefs);
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let p = m.plugin("test.shift").unwrap();
    assert_eq!(
        p.status,
        Status::Ready,
        "the previous version keeps running"
    );
    assert_eq!(p.decls.tools[0].name, "Raise");
    assert!(
        ev.iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
    assert!(
        p.log
            .lines()
            .any(|l| l.text.contains("previous version keeps running"))
    );
    // A good edit replaces it.
    std::fs::write(&installed, SHIFT.replace("\"Raise\"", "\"Raise Higher\"")).unwrap();
    m.refresh(&prefs);
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let p = m.plugin("test.shift").unwrap();
    assert_eq!(p.decls.tools.len(), 1, "no double registration");
    assert_eq!(p.decls.tools[0].name, "Raise Higher");
    assert!(ev.iter().any(|e| matches!(e, PluginEvent::DeclsChanged)));
    assert!(
        p.log
            .lines()
            .any(|l| l.text.contains("Reloaded after its files changed"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn custom_linked_content_writer_uses_editing_tracks_default_channel_and_subchannel_table() {
    use plumspirit_engine::editor::clips::{add_clip, duplicate_clips};
    use plumspirit_engine::history::Operation;
    use plumspirit_engine::model::{ContentRef, Subchannel, SubchannelId};
    for extended in [false, true] {
        let (mut d, mut history) = project(0, extended);
        let d = Arc::make_mut(&mut d);
        d.add_track().unwrap();
        let second = d.track(1).unwrap().id();
        let first_track = d.track(0).unwrap().id();
        let mut props = d.track(1).unwrap().props().clone();
        props.default_channel = 9;
        history
            .apply(
                d,
                Operation::TrackProps {
                    track: second,
                    props,
                    end: None,
                    label: "Fixture channel",
                },
            )
            .unwrap();
        history
            .apply(
                d,
                Operation::AddSubchannels {
                    track: 1,
                    subs: vec![Subchannel::new(SubchannelId(7), 9)],
                },
            )
            .unwrap();
        let op = add_clip(d, 0, 100, 960).unwrap();
        history.apply(d, op).unwrap();
        let c = d.clips().contents()[0].id;
        let first = d.clips().placements_of(ContentRef::Owned(c))[0].id;
        let op = duplicate_clips(d, &[(first, 1, 2000)], true).unwrap();
        history.apply(d, op).unwrap();
        let mut doc = Arc::new(d.clone());
        let before = doc.export_fingerprint();
        let source = r#"function register(p)
          p.tool{id='owner',name='Owner writer',needs_selection=false,run=function(ctx)
            assert(ctx.owner_track==plum.project.current():track(2))
            assert(ctx.owner_track~=ctx.track and ctx.track:kind()=='clip_content')
            assert(ctx.owner_track:subchannels()[2]:id()==7)
            local c=ctx.owner_track:subchannels()[2]:note_color()
            assert(math.type(c.r)=='integer'and c.r>=0 and c.r<=255)
            local w=ctx.tx:note_writer(ctx.track)
            w:append{start={17},gate={20},key={60},subchannel={7}}
            w:finish()
            local b=ctx.track:notes():batches{columns={'track_id','content_id'}}()
            assert(b.track_id[1]==plum.project.current():track(2):id())
            assert(b.content_id[1]==ctx.track:id())
          end}
        end"#;
        let host = plugin_host(&[Capability::ReadProject, Capability::EditNotes], source).unwrap();
        host.set_world(Some(world(&doc, &mut history)));
        let script = format!(
            "return plum.plugin.__run_tool('owner',{{}},{{track={},content={},keys={}}})",
            second.0,
            c.0,
            if extended { 256 } else { 128 }
        );
        host.run("owner.lua", &script)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(doc.export_fingerprint(), before);
        let mut commit = host.take_commits().unwrap();
        let staged = commit.result().clips().content(c).unwrap();
        assert_eq!(staged.notes.len(), 1);
        let n = staged.notes.iter().next().unwrap();
        assert_eq!(n.attrs.channel(), 9);
        assert_eq!(n.sub, SubchannelId(7));
        commit.apply(&mut history, &mut doc).unwrap();
        let after = doc.export_fingerprint();
        assert_ne!(before, after);
        history.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        history.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), after);
        assert_eq!(doc.track_by_id(first_track).unwrap().notes.len(), 0);
        host.set_world(Some(world(&doc, &mut history)));
        let wrong = script.replace(
            &format!("track={}", second.0),
            &format!("track={}", first_track.0),
        );
        assert!(
            host.run("wrong-owner.lua", &wrong).is_err(),
            "first track must not borrow second-only subchannel"
        );
        assert!(host.take_commits().is_none());
    }
}
