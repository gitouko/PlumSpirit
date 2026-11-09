//! Real example entries with controlled native UI events (not Windows input).
use super::*;
use crate::lua::{Limits, World};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, project::KeyRange},
};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
static NEXT: AtomicUsize = AtomicUsize::new(1);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "plum-api02-graph-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let p = self.0.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            p.starts_with(&base)
                && p != base
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("plum-api02-graph-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
#[derive(Clone)]
struct Action {
    kind: &'static str,
    id: &'static str,
    value: InputValue,
    pointer: Option<Pointer>,
    wait: bool,
}
fn click(id: &'static str) -> Action {
    Action {
        kind: "activate",
        id,
        value: InputValue::None,
        pointer: None,
        wait: true,
    }
}
fn change(id: &'static str, value: InputValue) -> Action {
    Action {
        kind: "change",
        id,
        value,
        pointer: None,
        wait: true,
    }
}
fn point(kind: &'static str, x: f64, y: f64, buttons: u8) -> Action {
    Action {
        kind,
        id: "graph",
        value: InputValue::None,
        pointer: Some(Pointer {
            x,
            y,
            width: 600.,
            height: 320.,
            buttons,
            shift: false,
            ctrl: false,
            alt: false,
            command: false,
            scroll_x: 0.,
            scroll_y: 0.,
            scroll_unit: "points",
            time: 1.,
        }),
        wait: true,
    }
}
fn node<'a>(n: &'a schema::Node, id: &str) -> Option<&'a schema::Node> {
    if n.id == id {
        return Some(n);
    }
    match &n.kind {
        schema::Kind::Column(v) | schema::Kind::Row(v) | schema::Kind::Group(v) => {
            v.iter().find_map(|n| node(n, id))
        }
        _ => None,
    }
}
fn setup(
    temp: &Temp,
    doc: &Arc<PlumDocument>,
    history: &mut History,
) -> (Host, Receiver<Arc<Shared>>) {
    let manifest = crate::lua::package::Manifest::parse(include_str!(
        "../../examples-lua/custom-graph-laboratory/plum-plugin.toml"
    ))
    .unwrap();
    let h = Host::new(
        Limits {
            memory: 32 * 1024 * 1024,
            ..Default::default()
        },
        [
            Capability::UiPanels,
            Capability::ReadProject,
            Capability::EditNotes,
        ],
    )
    .unwrap();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: manifest.id.clone(),
        name: manifest.name,
        version: manifest.version,
        root: temp.0.clone(),
        data_dir: Some(temp.0.join("data")),
        config_version: 1,
    })
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    let (client, rx) = channel(&manifest.id);
    h.lua.app_data_mut::<Slot>().unwrap().client = Some(client);
    let (file, _) = crate::lua::bridge::channel(&manifest.id);
    {
        let mut s = h.lua.app_data_mut::<Session>().unwrap();
        s.client = Some(file);
        s.interactive = true;
    }
    history.observe_ids(doc);
    h.set_world(Some(World::new(
        Arc::clone(doc),
        history.incarnations().snapshot(),
    )));
    (h, rx)
}
fn execute(
    h: &Host,
    rx: Receiver<Arc<Shared>>,
    command: &str,
    actions: Vec<Action>,
) -> Result<(), crate::lua::ScriptError> {
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut revision = 0;
        for action in actions {
            let start = std::time::Instant::now();
            let snapshot = loop {
                if let Some(s) = shared.snapshot()
                    && (s.revision > revision || !action.wait)
                {
                    break s;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "Example did not publish a new graph snapshot; closed={}",
                    shared.closed()
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            revision = snapshot.revision;
            if action.kind == "activate" && action.id == "apply" {
                assert!(
                    node(&snapshot.root, "apply").unwrap().enabled,
                    "Apply requires the preview"
                );
            }
            assert!(shared.push(Event {
                kind: action.kind,
                id: action.id.into(),
                value: action.value,
                pointer: action.pointer,
                revision,
                sequence: 0
            }));
        }
    });
    let source = include_str!("../../examples-lua/custom-graph-laboratory/main.lua");
    let result=h.run("graph-laboratory.lua",&format!("{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('{command}',{{}})"));
    let joined = driver.join();
    result?;
    joined.unwrap();
    Ok(())
}
fn document(keys: KeyRange, selected: bool) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Graph".into(), Bpm::DEFAULT, 960);
    d.project_mut().keys = keys;
    if selected {
        let notes = (0..5)
            .map(|i| NoteRecord {
                start_tick: i * 120,
                duration_ticks: 90,
                attrs: NoteAttrs::new(60 + i as u8, 80, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect::<Vec<_>>();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, true)
            .unwrap();
    }
    let mut history = History::new(HistoryLimits::default());
    history.reset(true);
    (Arc::new(d), history)
}
#[test]
fn three_original_graph_commands_preview_replay_apply_undo_redo_in_both_key_modes() {
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut fingerprints = Vec::new();
        for command in ["bezier", "polar", "midi"] {
            let t = Temp::new();
            let (mut doc, mut history) = document(keys, false);
            let (h, rx) = setup(&t, &doc, &mut history);
            execute(&h, rx, command, vec![click("preview"), click("apply")]).unwrap();
            let preview = h.take_commits().unwrap();
            assert_eq!(doc.note_count(), 0);
            assert_eq!(preview.result().note_count(), 64);
            let hash = preview.result().export_fingerprint();
            drop(preview);
            drop(h);
            let (h, rx) = setup(&t, &doc, &mut history);
            execute(&h, rx, command, vec![click("preview"), click("apply")]).unwrap();
            let mut commits = h.take_commits().unwrap();
            assert_eq!(commits.result().export_fingerprint(), hash);
            assert_eq!(commits.steps.len(), 1);
            commits.apply(&mut history, &mut doc).unwrap();
            drop(commits);
            drop(h);
            assert_eq!(doc.export_fingerprint(), hash);
            history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
            assert_eq!(doc.note_count(), 0);
            history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
            assert_eq!(doc.export_fingerprint(), hash);
            fingerprints.push(hash);
        }
        assert_ne!(fingerprints[0], fingerprints[1]);
        assert_ne!(fingerprints[1], fingerprints[2]);
    }
}
#[test]
fn bezier_generated_notes_match_independent_de_casteljau_reference() {
    let t = Temp::new();
    let (doc, mut history) = document(KeyRange::Extended256, false);
    let (h, rx) = setup(&t, &doc, &mut history);
    execute(&h, rx, "bezier", vec![click("preview"), click("apply")]).unwrap();
    let commits = h.take_commits().unwrap();
    let mut notes = Vec::new();
    commits
        .result()
        .track(0)
        .unwrap()
        .notes
        .iter()
        .for_each(|n| notes.push(n));
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    for (i, n) in notes.iter().enumerate() {
        let t = i as f64 / 63.;
        let x = lerp(
            lerp(lerp(0., 0.25, t), lerp(0.25, 0.75, t), t),
            lerp(lerp(0.25, 0.75, t), lerp(0.75, 1., t), t),
            t,
        );
        let y = lerp(
            lerp(lerp(0., 1., t), lerp(1., 0., t), t),
            lerp(lerp(1., 0., t), lerp(0., 1., t), t),
            t,
        );
        assert_eq!(n.start_tick, (x * 63. * 120.).round() as u64);
        assert_eq!(n.attrs.pitch(), (y * 255.).round() as u8);
        assert_eq!(n.duration_ticks, 96);
        assert_eq!(n.attrs.velocity(), (32. + t * 95.).round() as u8);
    }
}
#[test]
fn graph_numeric_edits_and_plain_presets_restore_without_losing_original_values() {
    let t = Temp::new();
    let (doc, mut history) = document(KeyRange::Standard, false);
    let (h, rx) = setup(&t, &doc, &mut history);
    execute(
        &h,
        rx,
        "bezier",
        vec![
            change("pointx", InputValue::Number(0.37)),
            change("pointy", InputValue::Number(0.89)),
            change("seed", InputValue::Number(314.)),
            change("jitter", InputValue::Number(0.7)),
            click("save"),
            click("preview"),
            click("apply"),
        ],
    )
    .unwrap();
    let hash = h.take_commits().unwrap().result().export_fingerprint();
    drop(h);
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(t.0.join("data/presets/Graph Laboratory.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["value"]["bezier"][1]["x"].as_f64(), Some(0.37));
    assert_eq!(value["value"]["bezier"][1]["y"].as_f64(), Some(0.89));
    let (h, rx) = setup(&t, &doc, &mut history);
    execute(
        &h,
        rx,
        "bezier",
        vec![click("load"), click("preview"), click("apply")],
    )
    .unwrap();
    assert_eq!(
        h.take_commits().unwrap().result().export_fingerprint(),
        hash
    );
}
#[test]
fn graph_pointer_edit_zoom_view_changes_and_cancel_never_publish_notes() {
    let t = Temp::new();
    let (doc, mut history) = document(KeyRange::Standard, false);
    let (h, rx) = setup(&t, &doc, &mut history);
    let down = point(
        "pointer_down",
        0.5 + (0.25 - 0.5) / 1.3,
        0.5 - (1. - 0.5) / 1.3,
        1,
    );
    let movement = point("pointer_move", 0.4, 0.2, 1);
    let up = point("pointer_up", 0.4, 0.2, 0);
    let mut wheel = point("scroll", 0.5, 0.5, 0);
    let p = wheel.pointer.as_mut().unwrap();
    p.scroll_y = 2.;
    p.scroll_unit = "lines";
    execute(
        &h,
        rx,
        "bezier",
        vec![
            down,
            movement,
            up,
            wheel,
            click("save"),
            click("preview"),
            click("cancel"),
        ],
    )
    .unwrap_err();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 0);
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(t.0.join("data/presets/Graph Laboratory.json")).unwrap(),
    )
    .unwrap();
    let p = &value["value"]["bezier"][1];
    assert!((p["x"].as_f64().unwrap() - 0.37).abs() < 1e-12);
    assert!((p["y"].as_f64().unwrap() - 0.89).abs() < 1e-12);
    assert!(value["value"]["view"]["sx"].as_f64().unwrap() < 1.3);
}
#[test]
fn selected_velocity_preview_uses_actual_notes_and_apply_keeps_positions_pitch_and_gate() {
    let t = Temp::new();
    let (mut doc, mut history) = document(KeyRange::Standard, true);
    let original = doc.export_fingerprint();
    let (h, rx) = setup(&t, &doc, &mut history);
    execute(
        &h,
        rx,
        "midi",
        vec![
            change("operation", InputValue::Choice(1)),
            click("preview"),
            click("apply"),
        ],
    )
    .unwrap();
    let mut commits = h.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 5);
    let mut rows = Vec::new();
    commits
        .result()
        .track(0)
        .unwrap()
        .notes
        .iter()
        .for_each(|n| rows.push(n));
    for (i, n) in rows.iter().enumerate() {
        let x = i as f64 / 4.;
        let y = if x <= 0.5 {
            -0.5 + 1.3 * x / 0.5
        } else {
            0.8 - (x - 0.5) * 2.
        };
        let expected = ((y + 2.) / 4. * 127.).clamp(1., 127.).round() as u8;
        assert_eq!(n.attrs.velocity(), expected);
        assert_eq!(n.start_tick, i as u64 * 120);
        assert_eq!(n.duration_ticks, 90);
        assert_eq!(n.attrs.pitch(), 60 + i as u8);
    }
    commits.apply(&mut history, &mut doc).unwrap();
    drop(commits);
    drop(h);
    history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.export_fingerprint(), original);
}
