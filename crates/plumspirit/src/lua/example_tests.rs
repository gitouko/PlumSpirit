//! B1-0049 / B1-0040: the example plugins that ship with PlumSpirit (Graph
//! Shift, CC Batch) installed through the plugin manager exactly as the Lua
//! Plugins window does, and the plugin / API conformance items no other
//! `lua::*_tests` covers (1-note runs, Redo, multi-tool plugins, CC events
//! through `.lup` and MIDI export). `docs/v1_0_beta1/LUA_TEST_MATRIX.md`
//! maps every matrix item to its test.

use super::custom::{ToolValue, params_literal, values_literal};
use super::examples::{EXAMPLES, by_id};
use super::handles::ProjectInfo;
use super::package::{Manifest, Origin};
use super::plugins::{PluginEvent, PluginManager, PluginPrefs, Status, ToolJob};
use super::tx::Commits;
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RankSetBuilder, RawKind,
};
use plumspirit_engine::transform::param::Curve;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GRAPH_SHIFT: &str = "plumspirit.example.graph-shift";
const CC_BATCH: &str = "plumspirit.example.cc-batch";

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-example-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `n` notes of key `key`, 120 ticks apart, the first `selected` selected.
fn project(n: u64, key: u8, selected: u64) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..n)
        .map(|k| NoteRecord {
            start_tick: k * 120,
            duration_ticks: 100,
            attrs: NoteAttrs::new(key, 64, 3, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    let t = d.track_mut(0).unwrap();
    t.notes.insert_sorted_batch(&recs, false).unwrap();
    let mut b = RankSetBuilder::default();
    for r in 0..selected {
        b.push(r as usize);
    }
    t.notes.select_ranks(&b.finish()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

/// The snapshot a run reads, with the Piano Roll's track active (as the app
/// builds it in `lua_world`).
fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    let info = ProjectInfo {
        active_track: doc.track(0).map(|t| t.id()),
        ..ProjectInfo::default()
    };
    World::new(Arc::clone(doc), h.incarnations().snapshot()).with_info(info)
}

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
        assert!(t.elapsed() < Duration::from_secs(120), "never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A manager with example `id` installed from PlumSpirit's own copy,
/// allowed and enabled (Enable Plugins When Installed on), loaded.
fn with_example(
    name: &str,
    id: &str,
    doc: &Arc<PlumDocument>,
    h: &mut History,
) -> (PluginManager, PluginPrefs, PathBuf) {
    let root = temp(name);
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs {
        enable_on_install: true,
        ..PluginPrefs::default()
    };
    let r = m.install_example(by_id(id).unwrap(), &mut prefs).unwrap();
    assert_eq!(r.manifest.id, id);
    assert_eq!(prefs.installed[id].origin, Origin::Example);
    settle(&mut m, &prefs, &mut || world(doc, h));
    let p = m.plugin(id).unwrap();
    assert_eq!(
        p.status,
        Status::Ready,
        "{:?}",
        p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
    );
    (m, prefs, root)
}

fn tool_job(
    m: &PluginManager,
    values: &[ToolValue],
    preview: bool,
    generation: u64,
    doc: &Arc<PlumDocument>,
    h: &mut History,
) -> ToolJob {
    let decl = &m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0];
    ToolJob {
        stroke: None,
        tool: decl.id.clone(),
        params: params_literal(decl, values),
        track: doc.track(0).unwrap().id().0,
        content: None,
        preview,
        seed: 1,
        keys: 128,
        generation,
        world: Some(world(doc, h)),
    }
}

/// Runs Graph Shift once; its commits (`None`: it changed nothing) and
/// its returned text.
fn run_shift(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    values: &[ToolValue],
    preview: bool,
    generation: u64,
    doc: &Arc<PlumDocument>,
    h: &mut History,
) -> Option<Commits> {
    let job = tool_job(m, values, preview, generation, doc, h);
    m.run_tool(GRAPH_SHIFT, job).unwrap();
    let ev = settle(m, prefs, &mut || world(doc, h));
    ev.into_iter()
        .find_map(|e| match e {
            PluginEvent::ToolResult {
                generation: g,
                result,
                ..
            } if g == generation => Some(result.unwrap_or_else(|e| panic!("{e}"))),
            _ => None,
        })
        .expect("a tool result")
}

fn keys_of(d: &PlumDocument) -> Vec<(u64, u8)> {
    let mut v = Vec::new();
    d.track(0)
        .unwrap()
        .notes
        .query(0, u64::MAX, |_, n| v.push((n.start_tick, n.attrs.pitch())));
    v
}

fn curve_of(values: &[ToolValue], i: usize) -> Curve {
    match &values[i] {
        ToolValue::Curve(c) => c.clone(),
        v => panic!("{v:?}"),
    }
}

#[test]
fn the_examples_are_valid_packages_for_this_api() {
    assert_eq!(EXAMPLES.len(), 22);
    for ex in &EXAMPLES {
        let toml = ex
            .files
            .iter()
            .find(|(p, _)| *p == package::MANIFEST)
            .map(|(_, t)| *t)
            .expect("a manifest");
        let m = Manifest::parse(toml).unwrap_or_else(|e| panic!("{}: {e}", ex.id));
        assert_eq!(m.id, ex.id);
        assert!(m.compatible().is_ok());
        assert_eq!(m.entry, "main.lua");
        let expected = match ex.id {
            "plumspirit.example.file-transform" => {
                vec!["file.read_user_choice", "file.write_user_choice"]
            }
            "plumspirit.example.note-service" => vec!["services.provide"],
            "plumspirit.example.service-composer" => {
                vec!["notes.edit", "project.read", "services.call"]
            }
            "plumspirit.example.metadata-notebook" => vec!["metadata.edit", "project.read"],
            "plumspirit.example.reactive-monitor" => vec!["project.observe", "project.read"],
            "plumspirit.example.reactive-velocity" => vec![
                "notes.edit",
                "project.automate",
                "project.observe",
                "project.read",
            ],
            "plumspirit.example.font-resources" => {
                vec!["file.read_user_choice", "fonts.list", "ui.panels"]
            }
            "plumspirit.example.midi-input-laboratory" => {
                vec!["midi.input", "notes.edit", "project.read"]
            }
            "plumspirit.example.note-exchange" => vec![
                "file.read_user_choice",
                "file.write_user_choice",
                "notes.edit",
                "project.read",
            ],
            "plumspirit.example.text-art-studio" => {
                vec!["fonts.list", "notes.edit", "project.read", "ui.panels"]
            }
            "plumspirit.example.image-art-studio" => vec![
                "file.read_user_choice",
                "notes.edit",
                "project.read",
                "ui.panels",
            ],
            "plumspirit.example.editor-overlay-laboratory" => {
                vec![
                    "file.read_user_choice",
                    "playback.read",
                    "project.read",
                    "ui.overlay",
                ]
            }
            "plumspirit.example.midi-pattern-laboratory" => {
                vec!["playback.emit_midi", "playback.read"]
            }
            "plumspirit.example.transport-controls" => vec!["playback.control", "playback.read"],
            "plumspirit.example.transport-monitor" => vec!["playback.read"],
            "plumspirit.example.ui-layout-laboratory" => vec!["ui.panels"],
            "plumspirit.example.ui-image-laboratory" => vec!["file.read_user_choice", "ui.panels"],
            "plumspirit.example.custom-graph-laboratory" => {
                vec!["notes.edit", "project.read", "ui.panels"]
            }
            "plumspirit.example.custom-drawing-studio" => {
                vec!["notes.edit", "project.read", "tools.draw"]
            }
            _ => vec!["notes.edit", "project.read"],
        };
        assert_eq!(m.capabilities, expected);
        // The script compiles in the sandbox.
        let h = Host::new(Limits::default(), []).unwrap();
        h.run(
            "=check",
            &format!("return load({:?}, 'main.lua', 't') ~= nil", ex.main_lua()),
        )
        .unwrap_or_else(|e| panic!("{}: {e}", ex.id));
    }
    assert!(by_id("nope").is_none());
}

#[test]
fn graph_shift_follows_its_graph_exactly_previews_like_apply_and_is_one_undo_step() {
    // 11 of 12 notes selected: starts 0 – 1200, so x = 0.5 is the start at 600.
    let (mut doc, mut h) = project(12, 60, 11);
    let before = doc.export_fingerprint();
    let (mut m, prefs, root) = with_example("shift", GRAPH_SHIFT, &doc, &mut h);
    let decl = m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0].clone();
    assert_eq!(
        (decl.id.as_str(), decl.name.as_str()),
        ("graph-shift", "Graph Shift")
    );
    let values = decl.defaults();
    assert_eq!(values[0], ToolValue::Int(12));
    // Preview twice: the same result (deterministic), the project untouched.
    let p1 = run_shift(&mut m, &prefs, &values, true, 1, &doc, &mut h).expect("it shifts");
    let p2 = run_shift(&mut m, &prefs, &values, true, 2, &doc, &mut h).expect("it shifts");
    let shown = p1.result().export_fingerprint();
    assert_eq!(
        p2.result().export_fingerprint(),
        shown,
        "same input, same result"
    );
    drop(p2); // a preview shares the project it read
    assert_eq!(
        doc.export_fingerprint(),
        before,
        "a preview changes nothing"
    );
    // Exactly the graph: key + round(12 × Shape(x)), x from the first to the
    // last selected start; unselected notes stay.
    let shape = curve_of(&values, 1);
    let expect: Vec<(u64, u8)> = (0..12u64)
        .map(|k| {
            let s = k * 120;
            if k >= 11 {
                return (s, 60);
            }
            let x = s as f64 / 1200.0;
            (s, (60.0 + (12.0 * shape.eval(x) + 0.5).floor()) as u8)
        })
        .collect();
    assert_eq!(keys_of(p1.result()), expect);
    assert_eq!(expect[0].1, 60, "the graph starts at 0");
    assert!(
        expect.iter().any(|&(_, k)| k == 72),
        "the top is the full shift"
    );
    // Apply = the preview, one Undo step named after the tool; Undo / Redo.
    let mut apply = p1;
    assert_eq!(apply.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.export_fingerprint(), shown, "Preview = Apply");
    assert_eq!(h.undo_label(), Some("Graph Shift"));
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    assert_eq!(d.export_fingerprint(), before, "Undo restores");
    h.redo(d).unwrap().unwrap();
    assert_eq!(d.export_fingerprint(), shown, "Redo repeats it");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn graph_shift_handles_one_note_and_the_keyboards_edges() {
    // One selected note: x = 0, so a flat graph at the top moves it by Shift.
    let flat = ToolValue::Curve(Curve::new(vec![(0.0, 1.0), (1.0, 1.0)]));
    for (edge, expect) in [(0usize, Some(127u8)), (1, Some(120)), (2, None)] {
        let (doc, mut h) = project(1, 120, 1);
        let (mut m, prefs, root) = with_example(&format!("edge{edge}"), GRAPH_SHIFT, &doc, &mut h);
        let decl = m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0].clone();
        let mut values = decl.defaults();
        values[1] = flat.clone();
        values[2] = ToolValue::Choice(edge);
        let r = run_shift(&mut m, &prefs, &values, false, 1, &doc, &mut h);
        let keys = r
            .as_ref()
            .map_or_else(|| keys_of(&doc), |c| keys_of(c.result()));
        match expect {
            Some(k) => assert_eq!(
                keys,
                if k == 120 {
                    vec![(0, 120)]
                } else {
                    vec![(0, k)]
                },
                "edge {edge}"
            ),
            None => assert!(keys.is_empty(), "Remove Note"),
        }
        if edge == 1 {
            assert!(
                r.is_none(),
                "Keep Key changes nothing, so nothing is committed"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    // Without a selection the tool asks for one.
    let (doc, mut h) = project(3, 60, 0);
    let (mut m, prefs, root) = with_example("nosel", GRAPH_SHIFT, &doc, &mut h);
    let decl = m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0].clone();
    let job = tool_job(&m, &decl.defaults(), false, 9, &doc, &mut h);
    m.run_tool(GRAPH_SHIFT, job).unwrap();
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    assert!(ev.iter().any(|e| matches!(e,
        PluginEvent::ToolResult { result: Err(e), .. } if e.contains("select some first"))));
    let _ = std::fs::remove_dir_all(&root);
}

/// CC 11 events of every track: (tick, channel, value).
fn cc11(d: &PlumDocument) -> Vec<(u64, u8, u8)> {
    let mut v = Vec::new();
    for i in 0..d.track_count() {
        for e in d.track(i).unwrap().events.events() {
            if let RawKind::Channel { status, data } = e.kind
                && status & 0xF0 == 0xB0
                && data[0] == 11
            {
                v.push((e.tick, status & 0x0F, data[1]));
            }
        }
    }
    v.sort_unstable();
    v
}

fn run_command(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    command: &str,
    params: String,
    doc: &Arc<PlumDocument>,
    h: &mut History,
) -> (Option<Commits>, Option<String>) {
    m.run_command_with(CC_BATCH, command, params, Some(world(doc, h)))
        .unwrap();
    let mut commits = None;
    let mut notice = None;
    for e in settle(m, prefs, &mut || world(doc, h)) {
        match e {
            PluginEvent::Commits { commits: c, .. } => commits = Some(c),
            PluginEvent::Notice { text, .. } => notice = Some(text),
            _ => {}
        }
    }
    (commits, notice)
}

#[test]
fn cc_batch_writes_exact_events_in_one_undo_step_that_survive_lup_and_midi_export() {
    // 8 notes 0 – 940 selected (the span is 0 – 940), channel 4.
    let (mut doc, mut h) = project(8, 60, 8);
    let before = doc.export_fingerprint();
    let (mut m, prefs, root) = with_example("cc", CC_BATCH, &doc, &mut h);
    let cmds: Vec<String> = m
        .plugin(CC_BATCH)
        .unwrap()
        .decls
        .commands
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(
        cmds,
        vec!["write-cc", "clear-cc"],
        "a plugin with two commands"
    );
    let decl = m.plugin(CC_BATCH).unwrap().decls.commands[0].clone();
    let values = decl
        .params
        .iter()
        .map(|p| p.default.clone())
        .collect::<Vec<_>>();
    let params = values_literal(&decl.params, &values);
    let (c1, n1) = run_command(&mut m, &prefs, "write-cc", params.clone(), &doc, &mut h);
    let (c2, _) = run_command(&mut m, &prefs, "write-cc", params.clone(), &doc, &mut h);
    let mut c1 = c1.expect("it wrote events");
    let shown = c1.result().export_fingerprint();
    assert_eq!(
        c2.unwrap().result().export_fingerprint(),
        shown,
        "deterministic"
    );
    assert_eq!(doc.export_fingerprint(), before);
    // Exactly the linear graph 0 → 127 every 60 ticks from 0 to 940 (and the
    // end value at 940), repeated values left out, on the notes' channel.
    let graph = curve_of(&values, 2);
    let mut expect: Vec<(u64, u8, u8)> = Vec::new();
    let mut t = 0u64;
    loop {
        let v = (127.0 * graph.eval(t as f64 / 940.0) + 0.5)
            .floor()
            .clamp(0.0, 127.0) as u8;
        if expect.last().is_none_or(|l| l.2 != v) {
            expect.push((t, 3, v));
        }
        if t >= 940 {
            break;
        }
        t = (t + 60).min(940);
    }
    assert_eq!(cc11(c1.result()), expect);
    assert_eq!(expect.first().unwrap().2, 0);
    assert_eq!(expect.last().unwrap(), &(940, 3, 127));
    assert_eq!(
        n1.as_deref(),
        Some(format!("CC Batch (Example): Wrote {} CC 11 events.", expect.len()).as_str())
    );
    // One Undo step; Undo / Redo exact.
    assert_eq!(c1.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(h.undo_label(), Some("Write CC 11"));
    assert_eq!(doc.export_fingerprint(), shown, "Preview = Apply");
    {
        let d = Arc::get_mut(&mut doc).unwrap();
        h.undo(d).unwrap().unwrap();
        assert_eq!(d.export_fingerprint(), before);
        assert!(cc11(d).is_empty());
        h.redo(d).unwrap().unwrap();
        assert_eq!(cc11(d), expect);
    }
    // Running it again replaces its own events (no duplicates).
    let (again, n2) = run_command(&mut m, &prefs, "write-cc", params, &doc, &mut h);
    let mut again = again.unwrap();
    assert_eq!(cc11(again.result()), expect, "replaced, not doubled");
    assert!(n2.unwrap().contains(&format!("replaced {}", expect.len())));
    again.apply(&mut h, &mut doc).unwrap();
    // The events are saved in the project and come back on reopening.
    let bytes = plumspirit_engine::native::lup::project_to_bytes(&doc).unwrap();
    let (reopened, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(cc11(&reopened), expect, ".lup round trip");
    assert_eq!(reopened.export_fingerprint(), doc.export_fingerprint());
    // … and in the exported MIDI file.
    let mid = plumspirit_engine::midi::export_to_bytes(&doc).unwrap();
    let (imported, _) = plumspirit_engine::midi::import_bytes(&mid).unwrap();
    assert_eq!(cc11(&imported), expect, "MIDI export round trip");
    // Clear CC Over Selection removes exactly them.
    let clear = m.plugin(CC_BATCH).unwrap().decls.commands[1].clone();
    let cv = clear
        .params
        .iter()
        .map(|p| p.default.clone())
        .collect::<Vec<_>>();
    let (cleared, n3) = run_command(
        &mut m,
        &prefs,
        "clear-cc",
        values_literal(&clear.params, &cv),
        &doc,
        &mut h,
    );
    assert!(cc11(cleared.unwrap().result()).is_empty());
    assert!(
        n3.unwrap()
            .contains(&format!("Removed {} CC 11", expect.len()))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cc_batch_lfo_shapes_and_the_time_selection() {
    // No notes selected: the ruler's time selection 0 – 1920 and the track's
    // default channel.
    let (doc, mut h) = project(4, 60, 0);
    let (mut m, prefs, root) = with_example("lfo", CC_BATCH, &doc, &mut h);
    let decl = m.plugin(CC_BATCH).unwrap().decls.commands[0].clone();
    let mut values = decl
        .params
        .iter()
        .map(|p| p.default.clone())
        .collect::<Vec<_>>();
    values[1] = ToolValue::Choice(4); // Saw
    values[3] = ToolValue::Float(2.0);
    values[5] = ToolValue::Int(240);
    let tw = |doc: &Arc<PlumDocument>, h: &mut History| {
        let mut w = world(doc, h);
        let mut info = (*w.info).clone();
        info.time_selection = Some((0, 1920));
        w.info = Arc::new(info);
        w
    };
    m.run_command_with(
        CC_BATCH,
        "write-cc",
        values_literal(&decl.params, &values),
        Some(tw(&doc, &mut h)),
    )
    .unwrap();
    let ev = settle(&mut m, &prefs, &mut || tw(&doc, &mut h));
    let c = ev
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::Commits { commits, .. } => Some(commits),
            _ => None,
        })
        .expect("written");
    let ch = doc.track(0).unwrap().props().default_channel;
    // A saw twice over 0 – 1920 every 240 ticks: 0, ¼, ½, ¾ of 127, then again.
    let want: Vec<(u64, u8, u8)> = [0u64, 240, 480, 720, 960, 1200, 1440, 1680, 1920]
        .iter()
        .map(|&t| {
            let p = 2.0 * t as f64 / 1920.0;
            (t, ch, (127.0 * (p - p.floor()) + 0.5).floor() as u8)
        })
        .collect();
    assert_eq!(cc11(c.result()), want);
    // Nothing selected and no time selection: a warning, nothing written.
    let (none, note) = run_command(&mut m, &prefs, "write-cc", "nil".into(), &doc, &mut h);
    assert!(none.is_none());
    assert!(
        note.unwrap()
            .contains("Select notes in the Piano Roll or a time range")
    );
    let _ = std::fs::remove_dir_all(&root);
}

const TWO_TOOLS: &str = r#"
function register(p)
  p.tool{ id = "up", name = "Up", run = function(ctx) ctx.tx:modify_notes(ctx.notes, { transpose = 1 }) end }
  p.tool{ id = "down", name = "Down", run = function(ctx) ctx.tx:modify_notes(ctx.notes, { transpose = -1 }) end }
  p.command{ id = "count", name = "Count", run = function() return tostring(plum.project.current():note_count()) end }
end
"#;

#[test]
fn a_plugin_with_several_tools_and_a_command_runs_each_on_its_own() {
    let root = temp("multi");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("plum-plugin.toml"),
        "id = \"test.multi\"\nname = \"Multi\"\nversion = \"1.0\"\napi = \"0.1\"\ncapabilities = [\"project.read\", \"notes.edit\"]\n",
    )
    .unwrap();
    std::fs::write(src.join("main.lua"), TWO_TOOLS).unwrap();
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs {
        enable_on_install: true,
        ..PluginPrefs::default()
    };
    m.install(&src, false, &mut prefs).unwrap();
    let (doc, mut h) = project(5, 60, 5);
    settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let p = m.plugin("test.multi").unwrap();
    let tools: Vec<&str> = p.decls.tools.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(tools, vec!["up", "down"]);
    assert_eq!(p.decls.commands.len(), 1);
    for (tool, key, g) in [("up", 61u8, 1u64), ("down", 59, 2)] {
        let job = ToolJob {
            stroke: None,
            tool: tool.into(),
            params: "{}".into(),
            track: doc.track(0).unwrap().id().0,
            content: None,
            preview: true,
            seed: 1,
            keys: 128,
            generation: g,
            world: Some(world(&doc, &mut h)),
        };
        m.run_tool("test.multi", job).unwrap();
        let r = settle(&mut m, &prefs, &mut || world(&doc, &mut h))
            .into_iter()
            .find_map(|e| match e {
                PluginEvent::ToolResult {
                    generation, result, ..
                } if generation == g => Some(result.unwrap().unwrap()),
                _ => None,
            })
            .unwrap();
        assert!(keys_of(r.result()).iter().all(|&(_, k)| k == key), "{tool}");
    }
    m.run_command("test.multi", "count", Some(world(&doc, &mut h)))
        .unwrap();
    let ev = settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    assert!(
        ev.iter()
            .any(|e| matches!(e, PluginEvent::Notice { text, .. } if text == "Multi: 5"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn installing_an_example_again_updates_it_and_keeps_it_an_example() {
    let (doc, mut h) = project(1, 60, 0);
    let (mut m, mut prefs, root) = with_example("again", CC_BATCH, &doc, &mut h);
    let r = m
        .install_example(by_id(CC_BATCH).unwrap(), &mut prefs)
        .unwrap();
    assert_eq!(r.replaced.as_deref(), Some("1.0.0"));
    assert_eq!(prefs.installed[CC_BATCH].origin, Origin::Example);
    assert_eq!(m.plugins().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_example_comes_back_after_a_restart_as_the_user_left_it() {
    // PlumSpirit closes (the manager is dropped) and opens again with the
    // same plugins folder and settings: Graph Shift loads again, enabled and
    // allowed, and gives the same result; CC Batch, disabled before closing,
    // stays disabled.
    let (doc, mut h) = project(6, 60, 6);
    let (mut m, mut prefs, root) = with_example("restart", GRAPH_SHIFT, &doc, &mut h);
    m.install_example(by_id(CC_BATCH).unwrap(), &mut prefs)
        .unwrap();
    m.set_enabled(CC_BATCH, false, &mut prefs);
    settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    let decl = m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0].clone();
    let before = run_shift(&mut m, &prefs, &decl.defaults(), true, 1, &doc, &mut h)
        .unwrap()
        .result()
        .export_fingerprint();
    drop(m);
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    m.start(&prefs);
    settle(&mut m, &prefs, &mut || world(&doc, &mut h));
    assert_eq!(m.plugin(GRAPH_SHIFT).unwrap().status, Status::Ready);
    assert_eq!(m.plugin(CC_BATCH).unwrap().status, Status::Disabled);
    assert_eq!(prefs.installed[GRAPH_SHIFT].origin, Origin::Example);
    let after = run_shift(&mut m, &prefs, &decl.defaults(), true, 2, &doc, &mut h)
        .unwrap()
        .result()
        .export_fingerprint();
    assert_eq!(after, before, "the same result after reopening");
    let _ = std::fs::remove_dir_all(&root);
}

/// B1-0040 measurement: Graph Shift (one Lua call per note) over 10,000,000
/// selected notes as a plugin tool run (install, preview run, result), time
/// and peak RAM → docs/PERFORMANCE.md (native batch edits at 10M: E0162). `cargo test --release -p plumspirit
/// example_tool_10m -- --ignored --nocapture`.
#[test]
#[ignore]
fn example_tool_10m_notes_cost() {
    let n = 10_000_000u64;
    let t0 = Instant::now();
    let (doc, mut h) = project(n, 60, n);
    eprintln!("built {n} notes in {:.2} s", t0.elapsed().as_secs_f64());
    let rss0 = crate::diagnostics::process_memory_bytes().unwrap_or(0);
    let (mut m, prefs, root) = with_example("10m", GRAPH_SHIFT, &doc, &mut h);
    let decl = m.plugin(GRAPH_SHIFT).unwrap().decls.tools[0].clone();
    let t = Instant::now();
    let r = run_shift(&mut m, &prefs, &decl.defaults(), true, 1, &doc, &mut h).unwrap();
    let lua = t.elapsed();
    let rss1 = crate::diagnostics::process_memory_bytes().unwrap_or(0);
    assert_eq!(r.result().track(0).unwrap().notes.len() as u64, n);
    drop(r);
    eprintln!(
        "graph-shift 10M (one Lua call per note): {:.2} s, RSS {} -> {} MB, peak {} MB",
        lua.as_secs_f64(),
        rss0 >> 20,
        rss1 >> 20,
        peak_mb()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The process's peak resident memory in MB (Linux `VmHWM`; 0 elsewhere).
fn peak_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb >> 10)
}

const IMAGE_STUDIO: &str = "plumspirit.example.image-art-studio";
fn image_tool(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    tool: usize,
    values: &[ToolValue],
    (preview, generation): (bool, u64),
    doc: &Arc<PlumDocument>,
    h: &mut History,
) -> Result<Option<Commits>, String> {
    let decl = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[tool].clone();
    m.run_tool(
        IMAGE_STUDIO,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(&decl, values),
            track: doc.track(0).unwrap().id().0,
            content: None,
            preview,
            seed: 1,
            keys: doc.project().keys.max_pitch() as u16 + 1,
            generation,
            world: Some(world(doc, h)),
        },
    )
    .unwrap();
    settle(m, prefs, &mut || world(doc, h))
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::ToolResult {
                generation: g,
                result,
                ..
            } if g == generation => Some(result),
            _ => None,
        })
        .expect("image tool result")
}
fn image_events(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    doc: &Arc<PlumDocument>,
    h: &mut History,
    choice: Option<PathBuf>,
    close_panel: bool,
) -> Vec<PluginEvent> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        for e in m.poll(prefs) {
            if matches!(e, PluginEvent::NeedWorld) {
                m.supply_world(world(doc, h));
            }
            out.push(e);
        }
        if let Some(request) = m.poll_ui_request() {
            let stamp = request.lease.project;
            request.respond(stamp, choice.clone()).unwrap();
        }
        if close_panel && let Some(panel) = m.poll_panel_request() {
            let snapshot = panel.snapshot().expect("validated source frame");
            assert_eq!(snapshot.title, "Image Art Studio — Captured Source");
            assert!(panel.push(super::ui::Event {
                kind: "activate",
                id: "close".into(),
                value: super::ui::InputValue::None,
                pointer: None,
                revision: panel.revision(),
                sequence: 0
            }));
        }
        if !m.is_busy() {
            out.extend(m.poll(prefs));
            return out;
        }
        assert!(Instant::now() < deadline, "image command did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn image_data(doc: &mut Arc<PlumDocument>, events: Vec<PluginEvent>) -> Option<String> {
    let mut latest = None;
    for e in events {
        match e {
            PluginEvent::ProjectData { plugin, text } => {
                assert_eq!(plugin, IMAGE_STUDIO);
                let data = &mut Arc::make_mut(doc).project_mut().plugin_data;
                data.retain(|(id, _)| id != &plugin);
                if !text.is_empty() {
                    data.push((plugin, text.clone()));
                }
                latest = Some(text);
            }
            PluginEvent::Notice {
                text, error: true, ..
            } => panic!("image command error: {text}"),
            _ => {}
        }
    }
    latest
}
#[test]
fn image_studio_installs_and_authored_artwork_has_deterministic_preview_apply_undo_both_key_modes()
{
    for keys in [128, 256] {
        let (mut doc, mut h) = project(0, 60, 0);
        if keys == 256 {
            Arc::get_mut(&mut doc).unwrap().project_mut().keys =
                plumspirit_engine::model::project::KeyRange::Extended256;
        }
        let (mut m, prefs, _root) =
            with_example(&format!("image-art-{keys}"), IMAGE_STUDIO, &doc, &mut h);
        let plugin = m.plugin(IMAGE_STUDIO).unwrap();
        assert_eq!(plugin.decls.commands.len(), 5);
        assert_eq!(plugin.decls.tools.len(), 2);
        let mut values = plugin.decls.tools[0].defaults();
        if keys == 256 {
            values[1] = ToolValue::Int(200);
        }
        let preview = image_tool(&mut m, &prefs, 0, &values, (true, 1), &doc, &mut h)
            .unwrap()
            .unwrap();
        assert!(preview.result().note_count() > 0 && preview.result().note_count() <= 4096);
        let expected = preview.result().export_fingerprint();
        let count = preview.result().note_count();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        let mut apply = image_tool(&mut m, &prefs, 0, &values, (false, 2), &doc, &mut h)
            .unwrap()
            .unwrap();
        assert_eq!(apply.result().export_fingerprint(), expected);
        apply.apply(&mut h, &mut doc).unwrap();
        drop(apply);
        assert_eq!(doc.note_count(), count);
        h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
        assert_eq!(doc.note_count(), 0);
        assert!(!h.can_undo());
        h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
    }
}
#[test]
fn image_studio_all_authored_mask_operations_run_as_installed_tools() {
    for operation in 0..7 {
        let (mut doc, mut h) = project(20, 60, 20);
        let (mut m, prefs, _root) = with_example(
            &format!("image-mask-{operation}"),
            IMAGE_STUDIO,
            &doc,
            &mut h,
        );
        let decl = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[1].clone();
        let mut values = decl.defaults();
        values[1] = ToolValue::Int(60);
        values[3] = ToolValue::Int(1);
        values[9] = ToolValue::Choice(operation);
        let before = doc.export_fingerprint();
        let preview = image_tool(&mut m, &prefs, 1, &values, (true, 1), &doc, &mut h)
            .unwrap()
            .unwrap();
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.export_fingerprint(), before);
        drop(preview);
        let mut apply = image_tool(&mut m, &prefs, 1, &values, (false, 2), &doc, &mut h)
            .unwrap()
            .unwrap();
        assert_eq!(apply.result().export_fingerprint(), expected);
        apply.apply(&mut h, &mut doc).unwrap();
        drop(apply);
        assert_eq!(doc.export_fingerprint(), expected);
        if operation != 6 && expected != before {
            h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
            assert_eq!(doc.export_fingerprint(), before);
            h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
            assert_eq!(doc.export_fingerprint(), expected);
        }
    }
}
#[test]
fn image_studio_real_file_capture_cancel_source_preset_and_panel_use_bounded_plain_pixels() {
    use image::{DynamicImage, ImageFormat, RgbaImage};
    use std::io::Cursor;
    let (mut doc, mut h) = project(0, 60, 0);
    let (mut m, prefs, root) = with_example("image-capture", IMAGE_STUDIO, &doc, &mut h);
    m.run_command(IMAGE_STUDIO, "choose_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, false);
    assert!(image_data(&mut doc, events).is_none());
    let raw = vec![
        255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 10, 20, 30, 255,
    ];
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_raw(2, 2, raw).unwrap())
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let path = root.join("captured-source.png");
    std::fs::write(&path, bytes.into_inner()).unwrap();
    m.run_command(IMAGE_STUDIO, "choose_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, Some(path.clone()), false);
    let captured = image_data(&mut doc, events).unwrap();
    assert!(captured.contains("ff0000ff00ff00800000ff000a141eff"));
    assert!(!captured.contains("captured-source.png") && !captured.contains("file_token"));
    m.run_command(IMAGE_STUDIO, "save_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, false);
    image_data(&mut doc, events);
    m.run_command(IMAGE_STUDIO, "reset_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, false);
    let reset = image_data(&mut doc, events).unwrap();
    assert!(!reset.contains("ff0000ff"));
    std::fs::remove_file(&path).unwrap(); // Future tools/presets must not reopen the source.
    m.run_command(IMAGE_STUDIO, "load_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, false);
    assert_eq!(image_data(&mut doc, events).unwrap(), captured);
    let values = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[0].defaults();
    assert!(
        image_tool(&mut m, &prefs, 0, &values, (true, 7), &doc, &mut h)
            .unwrap()
            .unwrap()
            .result()
            .note_count()
            > 0
    );
    m.run_command(IMAGE_STUDIO, "show_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, true);
    assert!(image_data(&mut doc, events).is_none());
    assert_eq!(doc.note_count(), 0);
}

#[test]
fn image_studio_bounds_capture_and_refuses_malformed_pixels_then_recovers() {
    use image::{DynamicImage, ImageFormat, RgbaImage};
    use std::io::Cursor;
    let (mut doc, mut h) = project(0, 60, 0);
    let (mut m, prefs, root) = with_example("image-bounded-capture", IMAGE_STUDIO, &doc, &mut h);
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_raw(128, 128, vec![255; 128 * 128 * 4]).unwrap())
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let path = root.join("large-capture.png");
    std::fs::write(&path, png.into_inner()).unwrap();
    m.run_command(IMAGE_STUDIO, "choose_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, Some(path), false);
    let data = image_data(&mut doc, events).unwrap();
    let json: serde_json::Value = serde_json::from_str(&data).unwrap();
    let capture = &json["values"]["image_art_capture"];
    assert_eq!(capture["width"], 64);
    assert_eq!(capture["height"], 64);
    assert_eq!(capture["hex"].as_str().unwrap().len(), 32768);
    let mut values = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[0].defaults();
    values[1] = ToolValue::Int(0);
    values[2] = ToolValue::Int(64);
    values[3] = ToolValue::Int(64);
    values[8] = ToolValue::Bool(true);
    assert_eq!(
        image_tool(&mut m, &prefs, 0, &values, (true, 1), &doc, &mut h)
            .unwrap()
            .unwrap()
            .result()
            .note_count(),
        4096
    );
    Arc::make_mut(&mut doc).project_mut().plugin_data=vec![(IMAGE_STUDIO.into(),"{\"version\":1,\"values\":{\"image_art_capture\":{\"version\":1,\"width\":64,\"height\":64,\"hex\":\"00\"}}}".into())];
    assert!(image_tool(&mut m, &prefs, 0, &values, (true, 2), &doc, &mut h).is_err());
    assert_eq!(doc.note_count(), 0);
    m.run_command(IMAGE_STUDIO, "reset_source", Some(world(&doc, &mut h)))
        .unwrap();
    let events = image_events(&mut m, &prefs, &doc, &mut h, None, false);
    image_data(&mut doc, events);
    assert!(
        image_tool(&mut m, &prefs, 0, &values, (true, 3), &doc, &mut h)
            .unwrap()
            .is_some()
    );
}
#[test]
fn image_studio_installed_art_and_mask_keep_owned_linked_clip_local_target() {
    let (mut doc, mut h) = project(0, 60, 0);
    Arc::get_mut(&mut doc).unwrap().project_mut().keys =
        plumspirit_engine::model::project::KeyRange::Extended256;
    let setup = Host::new(
        Limits::default(),
        [
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    )
    .unwrap();
    setup.set_world(Some(world(&doc, &mut h)));
    setup.run("image-clip-fixture.lua","local t=plum.project.current():track(1);local tx=plum.transaction{};local child=tx:add_subtracks(t,1)[1];local c=tx:add_clip(child,10000,1000);tx:duplicate_clips(c,{start=20000,linked=true});tx:insert_notes(c:content(),{{start=1,gate=2,key=200,velocity=70}});tx:commit()").unwrap();
    let mut prepared = setup.take_commits().unwrap();
    setup.set_world(None);
    prepared.apply(&mut h, &mut doc).unwrap();
    drop(prepared);
    h.reset(false);
    let content = doc.clips().contents().first().unwrap().id;
    let track = doc.track(1).unwrap().id().0;
    let (mut m, prefs, _root) = with_example("image-owned-clips", IMAGE_STUDIO, &doc, &mut h);
    let mut values = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[0].defaults();
    values[1] = ToolValue::Int(200);
    let decl = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[0].clone();
    m.run_tool(
        IMAGE_STUDIO,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(&decl, &values),
            track,
            content: Some(content.0),
            preview: true,
            seed: 1,
            keys: 256,
            generation: 1,
            world: Some(world(&doc, &mut h)),
        },
    )
    .unwrap();
    let preview = settle(&mut m, &prefs, &mut || world(&doc, &mut h))
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::ToolResult { result, .. } => Some(result.unwrap().unwrap()),
            _ => None,
        })
        .unwrap();
    assert!(
        preview
            .result()
            .notes_of(plumspirit_engine::model::ContentRef::Owned(content))
            .unwrap()
            .len()
            > 1
    );
    assert_eq!(preview.result().track(0).unwrap().notes.len(), 0);
    assert_eq!(preview.result().track(1).unwrap().notes.len(), 0);
    drop(preview);
    assert_eq!(doc.stored_note_count(), 1);
    let decl = m.plugin(IMAGE_STUDIO).unwrap().decls.tools[1].clone();
    let mut values = decl.defaults();
    values[1] = ToolValue::Int(200);
    values[3] = ToolValue::Int(1);
    m.run_tool(
        IMAGE_STUDIO,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(&decl, &values),
            track,
            content: Some(content.0),
            preview: true,
            seed: 1,
            keys: 256,
            generation: 2,
            world: Some(world(&doc, &mut h)),
        },
    )
    .unwrap();
    let mut commits = settle(&mut m, &prefs, &mut || world(&doc, &mut h))
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::ToolResult { result, .. } => Some(result.unwrap().unwrap()),
            _ => None,
        })
        .unwrap();
    let rec = commits
        .result()
        .notes_of(plumspirit_engine::model::ContentRef::Owned(content))
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .record();
    assert_eq!({ rec.start_tick }, 1);
    assert_eq!(rec.attrs.pitch(), 200);
    assert_eq!(rec.attrs.velocity(), 100);
    commits.apply(&mut h, &mut doc).unwrap();
    drop(commits);
    assert_eq!(doc.clips().explicit_placements().len(), 2);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
    assert_eq!(
        doc.clips()
            .content(content)
            .unwrap()
            .notes
            .iter()
            .next()
            .unwrap()
            .attrs
            .velocity(),
        70
    );
}

#[path = "midi_input_laboratory_tests.rs"]
mod midi_input_laboratory;
#[path = "text_art_studio_tests.rs"]
mod text_art_studio;

#[test]
fn catalogue_keeps_original_order_and_installs_every_owned_package_file_exactly() {
    let original = [
        "plumspirit.example.graph-shift",
        "plumspirit.example.cc-batch",
        "plumspirit.example.custom-graph-laboratory",
        "plumspirit.example.custom-drawing-studio",
        "plumspirit.example.ui-layout-laboratory",
        "plumspirit.example.ui-image-laboratory",
        "plumspirit.example.transport-monitor",
        "plumspirit.example.transport-controls",
        "plumspirit.example.midi-pattern-laboratory",
        "plumspirit.example.editor-overlay-laboratory",
        "plumspirit.example.image-art-studio",
        "plumspirit.example.text-art-studio",
        "plumspirit.example.note-exchange",
        "plumspirit.example.midi-input-laboratory",
        "plumspirit.example.font-resources",
    ];
    assert_eq!(
        EXAMPLES[..15].iter().map(|e| e.id).collect::<Vec<_>>(),
        original
    );
    let sources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples-lua");
    let mut packages = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&sources).unwrap() {
        let folder = entry.unwrap().path();
        let manifest = folder.join(package::MANIFEST);
        if !manifest.is_file() {
            continue;
        }
        let m = Manifest::parse(&std::fs::read_to_string(manifest).unwrap()).unwrap();
        assert!(packages.insert(m.id, folder).is_none());
    }
    assert_eq!(
        packages.len(),
        EXAMPLES.len(),
        "Every authored sample must ship through the native installer"
    );
    let root = temp("catalogue");
    for ex in &EXAMPLES {
        let folder = packages
            .remove(ex.id)
            .expect("An embedded sample must have canonical source");
        let installed = ex.write_to(&root).unwrap();
        let mut names = std::collections::BTreeSet::new();
        for (name, contents) in ex.files {
            assert!(names.insert(*name));
            assert_eq!(
                std::fs::read(folder.join(name)).unwrap(),
                contents.as_bytes()
            );
            assert_eq!(
                std::fs::read(installed.join(name)).unwrap(),
                contents.as_bytes()
            );
        }
        let source_names = std::fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            source_names,
            names.into_iter().map(str::to_owned).collect(),
            "No missing helper, manifest or README in {}",
            ex.id
        );
    }
    assert!(packages.is_empty());
    let resolved = root.canonicalize().unwrap();
    let base = std::env::temp_dir().canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(base.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("plumspirit-example-test-catalogue-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
}
