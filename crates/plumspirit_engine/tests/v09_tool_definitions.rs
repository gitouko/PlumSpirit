//! v0.9 Tool Definition System (contract §095, §208, §209): every tool and
//! one-click command is tested from its definition — the declared Preview
//! recipe, growth kinds and Subchannel policy against what its kernels do on a
//! minimal fixture (defaults, each function alone, seeded random settings),
//! Preview builds on that fixture, presets round-trip, and the definitions
//! agree with docs/v0_9/TOOL_CATALOG.toml and LEGACY_MAP.toml.

mod tool_support;

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::transform::definition::{
    Category, FunctionStatus, GrowthKind, LEGACY, SubchannelPolicy, ToolDefinition, definitions,
};
use plumspirit_engine::transform::param::{ParamKind, ParamValue};
use plumspirit_engine::transform::preset::{self, Preset};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ToolDef, TrackScope, command_tool, run_kernel, tool,
};
use std::collections::{BTreeMap, BTreeSet};
use tool_support::random_value;

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// The minimal fixture: one track with four subchannels and eight selected
/// notes, all in its first subchannel (a chord, a line, a long bass note),
/// and a second, empty track. A tool that keeps subchannels leaves every
/// note in subchannel 0; one that routes can reach 1–3.
fn minimal() -> PlumDocument {
    let mut d = PlumDocument::new_project("defs".into(), Bpm::DEFAULT, 960);
    let subs: Vec<Subchannel> = (1..4)
        .map(|i| Subchannel::new(SubchannelId(i), i as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    d.add_track().unwrap();
    let mut v = vec![
        rec(0, 480, 60),
        rec(0, 480, 64),
        rec(0, 480, 67),
        rec(0, 1920, 48),
        rec(480, 240, 62),
        rec(960, 240, 65),
        rec(1440, 240, 69),
        rec(1920, 240, 72),
    ];
    v.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

/// The context, with a pointer stroke for tools that read one.
fn ctx(d: &PlumDocument, stroke: bool) -> Ctx {
    let mut c = Ctx::simple(960);
    c.keys = d.project().keys;
    c.groups = std::sync::Arc::new(d.project().groups.clone());
    if stroke {
        c.stroke = Some(std::sync::Arc::new(tool_support::stroke(0)));
    }
    c
}

fn tool_def(id: &str) -> &'static ToolDef {
    tool(id).or_else(|| command_tool(id)).unwrap()
}

/// What one run on the minimal fixture showed.
#[derive(Debug, Default)]
struct Seen {
    pointwise: bool,
    growth: Option<GrowthKind>,
    /// Some result left subchannel 0.
    routes: bool,
    error: Option<String>,
}

fn measure(d: &PlumDocument, def: &ToolDefinition, set: &ParamSet) -> Seen {
    let td = tool_def(def.id);
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = match collect_for(td, d, &sc, 1_000_000) {
        Ok(s) => s,
        Err(e) => {
            return Seen {
                error: Some(format!("collect: {e}")),
                ..Seen::default()
            };
        }
    };
    let c = ctx(d, def.capture(set).is_some() || def.pointer.base.is_some());
    let b = BuildCtx {
        ctx: &c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = match td.kernel(set, &b) {
        Ok(k) => k,
        Err(e) => {
            return Seen {
                error: Some(format!("build: {e}")),
                ..Seen::default()
            };
        }
    };
    let mut s = Seen {
        pointwise: k.as_point().is_some(),
        growth: Some(GrowthKind::of(k.growth())),
        ..Seen::default()
    };
    match run_kernel(k.as_ref(), &src, td.boundary(set), &Limits::default(), None) {
        Ok(r) => s.routes = r.notes.iter().any(|n| n.sub != 0),
        Err(e) => s.error = Some(format!("run: {e}")),
    }
    s
}

/// Named settings of a definition: defaults, each function alone, and
/// seeded random draws (each with one function or none on).
///
/// `mandatory`: also the platform's mandatory functions (§098) — they
/// change what the tool's own declared policies describe (they route
/// subchannels, move pitches, make the run whole-source), so the policy
/// checks leave them off.
fn settings(
    def: &ToolDefinition,
    keys: KeyRange,
    draws: u64,
    mandatory: bool,
) -> Vec<(String, ParamSet)> {
    let specs = def.specs();
    let base = ParamSet::defaults_in(&specs, keys);
    let mut out = vec![("defaults".to_owned(), base.clone())];
    let functions: Vec<_> = def
        .functions
        .iter()
        .filter(|f| mandatory || !f.mandatory)
        .collect();
    for f in &functions {
        let mut s = base.clone();
        s.set(f.switch, ParamValue::Bool(true));
        out.push((f.id.to_owned(), s.clone()));
        // V09-S36: and every option of the function's own menus, one at a
        // time (a policy may hold only for some options — Group n10's
        // Subchannel target).
        let prefix = format!("{}.", f.switch);
        for p in specs.iter().filter(|p| p.key.starts_with(&prefix)) {
            if let ParamKind::Choice(options) = &p.kind {
                for c in 0..options.len() as u32 {
                    let mut s = s.clone();
                    s.set(p.key, ParamValue::Choice(c));
                    out.push((format!("{} {}={c}", f.id, p.key), s));
                }
            }
        }
    }
    // V09-S37: the shared geometry Fill with copies cycling through the
    // track's subchannels (what routes a geometry tool's subchannels when
    // asked), stretched so copies are made — every geometry tool, not only
    // when a random draw happens to reach it.
    if specs.iter().any(|p| p.key == "g.keep_sub") {
        let mut s = base.clone();
        s.set("g.fill", ParamValue::Bool(true));
        s.set("g.keep_sub", ParamValue::Bool(false));
        s.set("g.scale_y", ParamValue::Float(300.0));
        out.push(("g.fill with subchannel copies".to_owned(), s));
    }
    // V09-S38 step 3: Clipping Mask's Split result moves the outside parts
    // by a subchannel offset where the clipped pieces go; on this fixture
    // only the art's track has subchannels to move to (the base track has
    // one), so the routing shows with Add to the art's track.
    if def.id == "clip_mask" {
        let mut s = base.clone();
        s.set("n08", ParamValue::Bool(true));
        s.set("result", ParamValue::Choice(2));
        out.push(("n08 with the result on the art's track".to_owned(), s));
    }
    let n = functions.len();
    for draw in 0..draws {
        let mut r = Rng::new(0x5EED_0000 + draw);
        let mut s = base.clone();
        for p in &specs {
            if p.key.starts_with("b.") || def.functions.iter().any(|f| f.switch == p.key) {
                continue;
            }
            if let Some(v) = random_value(p, &mut r, keys) {
                s.set(p.key, v);
            }
        }
        let on = (n > 0).then(|| draw as usize % (n + 1)).filter(|&i| i < n);
        for f in &def.functions {
            let k = functions.iter().position(|g| g.id == f.id);
            s.set(f.switch, ParamValue::Bool(k.is_some() && k == on));
        }
        out.push((format!("draw {draw}"), s));
    }
    out
}

/// §209 "Preview build succeeds on a minimal fixture", and the declared
/// Preview recipe, growth kinds and Subchannel policy hold for every
/// setting tried.
#[test]
fn declared_preview_growth_and_subchannel_policies_hold_on_a_minimal_fixture() {
    let d = minimal();
    let mut failures = Vec::new();
    for def in definitions() {
        let mut routed = Vec::new();
        for (name, set) in settings(&def, KeyRange::Standard, 24, false) {
            let s = measure(&d, &def, &set);
            if let Some(e) = s.error {
                // Random settings may be refused with a sentence (a boundary
                // policy of Refuse, an impossible mode); the defaults and
                // each function alone must build and run (a menu option
                // may be refused like a draw).
                if !name.starts_with("draw") && !name.contains('=') {
                    failures.push(format!("{} {name}: {e}", def.id));
                }
                continue;
            }
            if !def.preview.allows(s.pointwise) {
                failures.push(format!(
                    "{} {name}: pointwise {} but declared {:?}",
                    def.id, s.pointwise, def.preview
                ));
            }
            let g = s.growth.unwrap();
            if !def.growth.contains(&g) {
                failures.push(format!(
                    "{} {name}: growth {g:?} not in {:?}",
                    def.id, def.growth
                ));
            }
            if s.routes {
                routed.push(name);
            }
        }
        let by_default = routed.iter().any(|n| n == "defaults");
        let ok = match def.subchannels {
            SubchannelPolicy::Keep => routed.is_empty(),
            SubchannelPolicy::KeepUnlessAsked => !by_default && !routed.is_empty(),
            SubchannelPolicy::Assign => by_default,
        };
        if !ok {
            failures.push(format!(
                "{}: declared {:?}, routed in {routed:?}",
                def.id, def.subchannels
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Runs tool `id` with `set` on `d` (selected notes of track 0): the result
/// notes, and the lazy per-note preview's notes when the kernel is pointwise.
fn run_tool(
    d: &PlumDocument,
    id: &str,
    set: &ParamSet,
) -> (
    Vec<plumspirit_engine::transform::TNote>,
    Option<Vec<plumspirit_engine::transform::TNote>>,
    String,
) {
    let td = tool_def(id);
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(td, d, &sc, 1_000_000).unwrap();
    let c = ctx(d, false);
    let b = BuildCtx {
        ctx: &c,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = td.kernel(set, &b).unwrap();
    let lazy = k.as_point().map(|p| {
        let mut out =
            plumspirit_engine::transform::Output::new(td.boundary(set).within(src.keys), 1 << 20);
        for n in &src.notes {
            p.map(n, &mut out);
        }
        out.finish().unwrap().0
    });
    let r = run_kernel(k.as_ref(), &src, td.boundary(set), &Limits::default(), None).unwrap();
    let summary = r.summary();
    (r.notes, lazy, summary)
}

fn with(id: &str, pairs: &[(&str, ParamValue)]) -> ParamSet {
    let def = plumspirit_engine::transform::definition::find(id).unwrap();
    let mut s = ParamSet::defaults(&def.specs());
    for (k, v) in pairs {
        assert!(def.param(k).is_some(), "{id} has no {k}");
        s.set(k, v.clone());
    }
    s
}

/// §098 functions 1–2 through the shared drivers: Transpose by 0 (identity)
/// with the Subchannel Offset Driver (time, one subchannel every quarter)
/// and the Pitch Offset Driver (note index, +0 / +12 in turn); the lazy
/// preview equals the run while the drivers are per-note.
#[test]
fn offset_drivers_move_subchannels_and_pitches_by_their_inputs() {
    let d = minimal();
    let on = ParamValue::Bool(true);
    let sub = with("transpose", &[("m.sub", on.clone())]);
    let (notes, lazy, _) = run_tool(&d, "transpose", &sub);
    // 960 PPQN, every 1/4 = 960 ticks: starts 0 / 480 → +0, 960 / 1440 → +1,
    // 1920 → +2 (the track's subchannels 0, 1, 2, 3 in turn).
    for n in &notes {
        assert_eq!(u64::from(n.sub), n.start / 960, "{n:?}");
    }
    assert_eq!(lazy.as_ref(), Some(&notes), "per-note preview = run");
    // Note index: not per-note, so no lazy preview; +0 / +12 alternate in
    // start order (ties by pitch).
    let pitch = with(
        "transpose",
        &[
            ("m.pitch", on.clone()),
            ("m.pitch.by", ParamValue::Choice(2)),
            ("m.pitch.map", ParamValue::Choice(1)),
            ("m.pitch.cycle", ParamValue::List(vec![0, 12])),
        ],
    );
    let (notes, lazy, _) = run_tool(&d, "transpose", &pitch);
    assert!(lazy.is_none());
    let mut src: Vec<(u64, u8)> = vec![
        (0, 48),
        (0, 60),
        (0, 64),
        (0, 67),
        (480, 62),
        (960, 65),
        (1440, 69),
        (1920, 72),
    ];
    for (i, s) in src.iter_mut().enumerate() {
        s.1 += if i % 2 == 1 { 12 } else { 0 };
    }
    let mut got: Vec<(u64, u8)> = notes.iter().map(|n| (n.start, n.pitch)).collect();
    got.sort_unstable();
    src.sort_unstable();
    assert_eq!(got, src);
    // Off: exactly the tool (the identity here), unwrapped.
    let (notes, lazy, _) = run_tool(&d, "transpose", &with("transpose", &[]));
    assert!(notes.iter().all(|n| n.sub == 0) && lazy.is_some());
}

/// §098 function 3 after the drivers (§235 order): every note moved onto the
/// lowest pitch collides at tick 0; Remove Note Overlaps keeps the user's own
/// bass note (identical to the source) and removes the moved ones.
#[test]
fn overlaps_run_after_the_drivers_and_keep_the_users_notes() {
    let d = minimal();
    let on = ParamValue::Bool(true);
    let set = with(
        "transpose",
        &[
            ("m.pitch", on.clone()),
            ("m.pitch.by", ParamValue::Choice(1)),
            ("m.pitch.every.n", ParamValue::Int(1)),
            ("m.pitch.step", ParamValue::Int(-1)),
            ("m.ovl", on.clone()),
            ("m.ovl.mode", ParamValue::Choice(1)),
        ],
    );
    let (notes, lazy, summary) = run_tool(&d, "transpose", &set);
    assert!(lazy.is_none(), "overlaps make the run whole-source");
    assert!(notes.iter().all(|n| n.pitch == 48), "{notes:?}");
    let at0: Vec<_> = notes.iter().filter(|n| n.start == 0).collect();
    assert_eq!(at0.len(), 1);
    assert_eq!(at0[0].dur, 1920, "the user's own bass note stays");
    assert!(summary.contains("Overlaps: 3 removed"), "{summary}");
}

/// §098 function 4 / §234: the Control Sweep drives Transpose's semitones
/// over time — each note gets the value at its place along the graph; off,
/// or with no target switched on, nothing changes.
#[test]
fn the_control_sweep_varies_a_parameter_along_its_domain() {
    let d = minimal();
    let on = ParamValue::Bool(true);
    let set = with(
        "transpose",
        &[
            ("m.sweep", on.clone()),
            ("m.sweep.semis", on.clone()),
            (
                "m.sweep.semis.graph",
                ParamValue::Curve(plumspirit_engine::transform::Curve::line(0.5, 1.0)),
            ),
        ],
    );
    let (notes, lazy, _) = run_tool(&d, "transpose", &set);
    assert!(lazy.is_none());
    // Range −24 … 24 added to 0: the graph 0.5 → 1 over the selection's
    // starts 0 … 1920 gives +0 at 0, +6 at 480, +12 at 960, +18 at 1440 and
    // +24 at 1920.
    let shift = |start: u64| (start / 480) as u8 * 6;
    let mut want: Vec<(u64, u8)> = [
        (0, 48),
        (0, 60),
        (0, 64),
        (0, 67),
        (480, 62),
        (960, 65),
        (1440, 69),
        (1920, 72),
    ]
    .iter()
    .map(|&(s, p)| (s, p + shift(s)))
    .collect();
    let mut got: Vec<(u64, u8)> = notes.iter().map(|n| (n.start, n.pitch)).collect();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    // The sweep switched on but no target: the tool alone.
    let idle = with("transpose", &[("m.sweep", on.clone())]);
    let (notes, _, _) = run_tool(&d, "transpose", &idle);
    let (plain, _, _) = run_tool(&d, "transpose", &with("transpose", &[]));
    assert_eq!(notes, plain);
}

/// Every eligible tool runs with each mandatory function alone and with all
/// of them on (defaults) on the minimal fixture.
#[test]
fn mandatory_functions_run_on_every_eligible_tool() {
    let d = minimal();
    let mut failures = Vec::new();
    for def in definitions() {
        let mandatory: Vec<_> = def.functions.iter().filter(|f| f.mandatory).collect();
        if mandatory.is_empty() {
            continue;
        }
        let specs = def.specs();
        let base = ParamSet::defaults(&specs);
        let mut all = base.clone();
        for f in &mandatory {
            all.set(f.switch, ParamValue::Bool(true));
        }
        let mut cases: Vec<(String, ParamSet)> = mandatory
            .iter()
            .map(|f| {
                let mut s = base.clone();
                s.set(f.switch, ParamValue::Bool(true));
                (f.id.to_owned(), s)
            })
            .collect();
        cases.push(("all".into(), all));
        for (name, set) in cases {
            if let Some(e) = measure(&d, &def, &set).error {
                failures.push(format!("{} {name}: {e}", def.id));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Two values are the same preset content (curve points are stored with six
/// decimals).
fn same_value(a: &ParamValue, b: &ParamValue) -> bool {
    match (a, b) {
        (ParamValue::Curve(x), ParamValue::Curve(y)) => {
            x.points.len() == y.points.len()
                && x.outside == y.outside
                && (x.smoothness - y.smoothness).abs() <= 5e-7
                && x.points
                    .iter()
                    .zip(&y.points)
                    .all(|(p, q)| (p.0 - q.0).abs() <= 5e-7 && (p.1 - q.1).abs() <= 5e-7)
        }
        _ => a == b,
    }
}

/// §209 "preset serialization": every definition's defaults (both key
/// ranges), all functions on, and random settings survive a preset save and
/// load, and a second save is byte-identical.
#[test]
fn every_definition_round_trips_through_presets() {
    for def in definitions() {
        let specs = def.specs();
        let mut cases = Vec::new();
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            cases.extend(settings(&def, keys, 6, true));
        }
        let mut all_on = ParamSet::defaults(&specs);
        for f in &def.functions {
            all_on.set(f.switch, ParamValue::Bool(true));
        }
        cases.push(("all functions".into(), all_on));
        for (name, values) in cases {
            let p = Preset {
                tool: def.id.into(),
                name: format!("{} {name}", def.name),
                values: values.clone(),
            };
            let text = preset::to_text(&p);
            let (back, skipped) = preset::parse(&text, def.id, &specs)
                .unwrap_or_else(|e| panic!("{} {name}: {e}", def.id));
            assert!(skipped.is_empty(), "{} {name}: {skipped:?}", def.id);
            assert_eq!(back.tool, def.id);
            assert_eq!(back.values.len(), values.len(), "{} {name}", def.id);
            for (k, v) in values.iter() {
                let got = back.values.get_raw(k).unwrap();
                assert!(same_value(v, got), "{} {name} {k}: {v:?} → {got:?}", def.id);
            }
            assert_eq!(
                preset::to_text(&back),
                text,
                "{} {name}: second save",
                def.id
            );
        }
    }
}

/// §208 / §209 "editor type valid": every parameter kind the tool windows
/// render is one the generic UI knows, and every list has the editor of its
/// meaning (`transform/editors.rs`).
/// V09-0143 (the S41 gate): every eligible tool (the platform's mandatory
/// functions, not Legacy) declares ten Control Sweep targets —
/// distinct, each a numeric parameter of its own.
#[test]
fn every_eligible_tool_declares_ten_control_sweep_targets() {
    use plumspirit_engine::transform::definition::sweep_targets;
    let mut checked = 0;
    for def in definitions() {
        if !def.functions.iter().any(|f| f.mandatory) {
            continue;
        }
        if matches!(
            def.category,
            Category::Legacy | Category::LegacyExperimental
        ) {
            continue;
        }
        let t = sweep_targets(def.id);
        assert_eq!(t.len(), 10, "{}: {} Control Sweep targets", def.id, t.len());
        let ids: BTreeSet<&str> = t.iter().map(|x| x.id).collect();
        assert_eq!(ids.len(), 10, "{}: duplicate target ids", def.id);
        for x in t {
            let p = def
                .params
                .iter()
                .find(|p| p.spec.key == x.param)
                .unwrap_or_else(|| {
                    panic!(
                        "{}: target {} drives {} — no such parameter",
                        def.id, x.id, x.param
                    )
                });
            assert!(
                matches!(
                    p.spec.kind,
                    ParamKind::Int { .. }
                        | ParamKind::Float { .. }
                        | ParamKind::Ticks { .. }
                        | ParamKind::Pitch
                ),
                "{}: {} is not numeric",
                def.id,
                x.param
            );
        }
        checked += 1;
    }
    assert!(checked > 50, "{checked}");
}

#[test]
fn every_parameter_has_a_generic_or_semantic_editor() {
    use plumspirit_engine::transform::editors::Editor;
    let mut kinds = BTreeSet::new();
    for def in definitions() {
        for p in &def.params {
            kinds.insert(match p.spec.kind {
                ParamKind::Int { .. } => "integer",
                ParamKind::Float { .. } => "float",
                ParamKind::Bool => "switch",
                ParamKind::Choice(_) => "enum",
                ParamKind::Ticks { .. } => "ticks",
                ParamKind::Pitch => "pitch",
                ParamKind::Curve => "curve",
                ParamKind::List { .. } => "list",
                ParamKind::Seed => "seed",
                ParamKind::Text { .. } => "text",
            });
            if matches!(p.spec.kind, ParamKind::List { .. }) {
                assert_ne!(p.editor, Editor::Auto, "{} {}", def.id, p.spec.key);
            }
        }
    }
    for k in ["integer", "float", "enum", "curve", "list"] {
        assert!(kinds.contains(k), "{k}");
    }
}

/// `key = "value"` / `key = 12` lines of the `[[table]]` blocks of a
/// planning file (the catalog and the legacy map use nothing else).
fn blocks(text: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut out: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if let Some(h) = t.strip_prefix("[[").and_then(|h| h.strip_suffix("]]")) {
            out.push((h.to_owned(), BTreeMap::new()));
        } else if let (false, Some((k, v)), Some(b)) =
            (t.starts_with('#'), t.split_once('='), out.last_mut())
        {
            b.1.insert(k.trim().to_owned(), v.trim().trim_matches('"').to_owned());
        }
    }
    out
}

fn doc(name: &str) -> String {
    let path = format!("{}/../../docs/v0_9/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

type CatalogTool = (BTreeMap<String, String>, Vec<BTreeMap<String, String>>);

/// The definitions and the tool catalog describe the same tools: name,
/// family, category, advanced functions by stable id with names and
/// statuses; the legacy map's whole-tool entries are the One-Click tools.
#[test]
fn definitions_agree_with_the_tool_catalog_and_the_legacy_map() {
    let mut catalog: BTreeMap<String, CatalogTool> = BTreeMap::new();
    let mut current = None;
    for (h, kv) in blocks(&doc("TOOL_CATALOG.toml")) {
        match h.as_str() {
            "tool" => {
                let id = kv["id"].clone();
                current = Some(id.clone());
                catalog.insert(id, (kv, Vec::new()));
            }
            "tool.function" => {
                let id = current.clone().expect("function before tool");
                catalog.get_mut(&id).unwrap().1.push(kv);
            }
            _ => {}
        }
    }
    let mut problems = Vec::new();
    for def in definitions() {
        if def.category == Category::Command {
            assert!(!catalog.contains_key(def.id), "{} is a command", def.id);
            continue;
        }
        let Some((t, fs)) = catalog.get(def.id) else {
            problems.push(format!("{}: not in the catalog", def.id));
            continue;
        };
        let fam = format!("{:?}", def.family);
        for (k, want) in [
            ("name", def.name.to_owned()),
            ("family", fam),
            ("category", def.category.key().to_owned()),
        ] {
            if t[k] != want {
                problems.push(format!(
                    "{}: catalog {k} {:?}, definition {want:?}",
                    def.id, t[k]
                ));
            }
        }
        // The functions the registry implements are the catalog's
        // implemented ones, matched by stable id (never by place): name,
        // status and composition order (§235) agree. Designed functions of
        // later waves are not implemented yet.
        let implemented: BTreeMap<&str, &BTreeMap<String, String>> = fs
            .iter()
            .filter(|f| f["state"] == "IMPLEMENTED" || f["state"] == "VERIFIED")
            .map(|f| (f["id"].as_str(), f))
            .collect();
        if implemented.len() != def.functions.len() {
            problems.push(format!(
                "{}: {} functions, catalog has {} implemented",
                def.id,
                def.functions.len(),
                implemented.len()
            ));
        }
        for f in &def.functions {
            let Some(c) = implemented.get(f.id) else {
                problems.push(format!(
                    "{} {}: not implemented in the catalog",
                    def.id, f.id
                ));
                continue;
            };
            let order = f.order.to_string();
            if c["name"] != f.name || c["status"] != f.status.key() || c["order"] != order {
                problems.push(format!(
                    "{} {}: catalog {:?} {} order {}, definition {:?} {} order {order}",
                    def.id,
                    f.id,
                    c["name"],
                    c["status"],
                    c["order"],
                    f.name,
                    f.status.key()
                ));
            }
        }
    }
    // Whole-tool legacy entries ⇔ One-Click and Legacy tools; entries
    // naming a function ⇔ the Legacy functions of the other tools (a
    // One-Click tool's functions are Legacy through its whole-tool entry).
    let map = blocks(&doc("LEGACY_MAP.toml"));
    let legacy: BTreeSet<String> = map
        .iter()
        .filter(|(h, kv)| h == "legacy" && !kv.contains_key("function"))
        .map(|(_, kv)| kv["tool"].clone())
        .collect();
    let whole: BTreeSet<String> = definitions()
        .into_iter()
        .filter(|d| d.category == Category::OneClick || d.category.is_legacy())
        .map(|d| d.id.to_owned())
        .collect();
    assert_eq!(legacy, whole);
    let declared: BTreeSet<String> = LEGACY.tools.iter().map(|t| (*t).to_owned()).collect();
    let moved: BTreeSet<String> = definitions()
        .into_iter()
        .filter(|d| d.category.is_legacy())
        .map(|d| d.id.to_owned())
        .collect();
    assert_eq!(declared, moved);
    let functions: BTreeSet<(String, String)> = map
        .iter()
        .filter(|(h, _)| h == "legacy")
        .filter_map(|(_, kv)| Some((kv["tool"].clone(), kv.get("function")?.clone())))
        .collect();
    let demoted: BTreeSet<(String, String)> = definitions()
        .iter()
        .filter(|d| d.category != Category::OneClick)
        .flat_map(|d| {
            d.functions
                .iter()
                .filter(|f| f.status == FunctionStatus::Legacy)
                .map(|f| (d.id.to_owned(), f.id.to_owned()))
        })
        .collect();
    assert_eq!(functions, demoted);
    assert!(LEGACY.problems().is_empty(), "{:?}", LEGACY.problems());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The Track List Paste Repeat schema (`editor::clip_repeat::params`, not a
/// note tool) and the catalog's `paste_repeat_clips` agree: the ten names
/// in order, and every implemented function has its switch and the
/// catalog's number of controls.
#[test]
fn the_clip_repeat_schema_agrees_with_the_tool_catalog() {
    use plumspirit_engine::editor::clip_repeat::{FUNCTIONS, TOOL_ID, params};
    let mut fs = Vec::new();
    let mut current = String::new();
    for (h, kv) in blocks(&doc("TOOL_CATALOG.toml")) {
        match h.as_str() {
            "tool" => current = kv["id"].clone(),
            "tool.function" if current == TOOL_ID => fs.push(kv),
            _ => {}
        }
    }
    assert_eq!(fs.len(), 10);
    let specs = params();
    for (i, f) in fs.iter().enumerate() {
        let n = (i + 1) as u8;
        assert_eq!(f["name"], FUNCTIONS[i], "function {n}");
        let switch = specs
            .iter()
            .find(|s| s.adv == n && matches!(s.kind, ParamKind::Bool) && s.key == format!("a{n}"));
        if f["state"] == "IMPLEMENTED" || f["state"] == "VERIFIED" {
            assert!(switch.is_some(), "function {n} has its switch");
            let controls = specs.iter().filter(|s| s.adv == n).count() - 1;
            assert_eq!(f["params"], controls.to_string(), "function {n}");
        } else {
            assert!(switch.is_none(), "function {n} is not implemented yet");
        }
    }
    // Unique keys, defaults inside their ranges (presets read them back).
    let keys: BTreeSet<&str> = specs.iter().map(|s| s.key).collect();
    assert_eq!(keys.len(), specs.len());
}

/// Prints what every kernel does on the minimal fixture: the measurement the
/// declared table in `transform/definition.rs` is written from (run it after
/// changing a kernel's preview path, growth or subchannel behaviour).
#[test]
#[ignore]
fn print_measured_tool_behaviour() {
    let d = minimal();
    for def in definitions() {
        let mut point = BTreeSet::new();
        let mut growth = BTreeSet::new();
        let mut routes = Vec::new();
        for (name, set) in settings(&def, KeyRange::Standard, 24, false) {
            let s = measure(&d, &def, &set);
            if s.error.is_some() {
                continue;
            }
            point.insert(s.pointwise);
            growth.insert(s.growth.unwrap());
            if s.routes {
                routes.push(name);
            }
        }
        println!(
            "{:20} pointwise={point:?} growth={growth:?} routes={routes:?}",
            def.id
        );
    }
}

/// Prints every eligible tool's numeric parameters (what Control Sweep
/// targets can drive): the source the sweep table is chosen from.
#[test]
#[ignore]
fn print_sweepable_parameters() {
    for def in definitions() {
        if !def.functions.iter().any(|f| f.mandatory) {
            continue;
        }
        println!("## {} ({})", def.id, def.name);
        for p in def.params.iter().filter(|p| !p.spec.key.starts_with("m.")) {
            let r = match &p.spec.kind {
                ParamKind::Int { min, max } => format!("int {min}..{max}"),
                ParamKind::Float { min, max, unit } => format!("float {min}..{max} {unit}"),
                ParamKind::Ticks { min, max } => format!("ticks {min}..{max}"),
                ParamKind::Pitch => "pitch".into(),
                _ => continue,
            };
            println!(
                "  {:16} a{:<2} {:28} {:?} — {}",
                p.spec.key, p.spec.adv, r, p.spec.default, p.spec.label
            );
        }
    }
}

#[test]
fn chopper_v2_keeps_its_id_presets_and_legacy_name_aliases() {
    use plumspirit_engine::transform::{registry::Family, tool};
    let d = tool("repeat").unwrap();
    assert_eq!((d.spec)().name, "Chopper V2");
    assert_eq!((d.spec)().family, Family::Iterations);
    for alias in ["Note Repeat", "Notes Repeat", "Chopper V2"] {
        assert!(std::ptr::eq(tool(alias).unwrap(), d));
    }
    assert_eq!(
        (tool("paste_repeat_notes").unwrap().spec)().name,
        "Duplicate V2"
    );
    assert_eq!((tool("chop").unwrap().spec)().name, "Chopper");
}
