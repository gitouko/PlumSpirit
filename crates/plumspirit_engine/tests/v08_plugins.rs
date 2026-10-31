//! v0.8 built-in plugins (docs/v0_8/TEST_MATRIX.md, T8-C-*): HZ-Chop against
//! the original Lua (golden output of the unmodified script), SAFOR against a
//! literal per-tick model of the upstream algorithm.

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::tools::plugins::{
    HzMode, hz_base_gate, hz_chop_note, hz_of, safor_overlaps, safor_sustains,
};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, Source, SourceTrack, TNote, TrackScope, TransformError,
    TransformRun, run_kernel, tool,
};

// ------------------------------------------------------------------ helpers

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

/// A one-track project at `ppq` with `notes`, all selected.
fn doc_with(ppq: u32, notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("v08".into(), Bpm::DEFAULT, ppq);
    let mut v = notes.to_vec();
    v.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

fn run_tool(
    d: &PlumDocument,
    id: &str,
    set: &[(&str, ParamValue)],
    scopes: &[TrackScope],
) -> Result<TransformRun, TransformError> {
    let def = tool(id).unwrap();
    let mut values = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in set {
        values.set(k, v.clone());
    }
    let src = collect_for(def, d, scopes, 10_000_000)?;
    let ppq = d.division().musical_ppqn().unwrap_or(960);
    let ctx = Ctx::simple(ppq);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&values, &b)?;
    let est = k.estimate(&src.stats);
    let r = run_kernel(
        k.as_ref(),
        &src,
        def.boundary(&values),
        &Limits::default(),
        None,
    )?;
    assert!(
        r.notes.len() as u64 <= est.notes,
        "{id}: {} out > preflight {}",
        r.notes.len(),
        est.notes
    );
    Ok(r)
}

fn all_selected(d: &PlumDocument) -> Vec<TrackScope> {
    (0..d.track_count())
        .filter(|&i| d.track(i).unwrap().notes.selected_count() > 0)
        .map(|index| TrackScope {
            index,
            all: false,
            content: None,
        })
        .collect()
}

// ------------------------------------------------------------------ HZ-Chop

struct GoldenCase {
    name: String,
    ppq: u64,
    bpm: f64,
    inote: i64,
    fstep: f64,
    /// (start, length, channel, key, velocity)
    sources: Vec<[u64; 5]>,
    created: Vec<[u64; 5]>,
}

fn golden() -> Vec<GoldenCase> {
    let text = include_str!("fixtures/hz_chop_golden.txt");
    let mut cases: Vec<GoldenCase> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("case ") {
            let mut parts = rest.split_whitespace();
            let name = parts.next().unwrap().to_string();
            let mut c = GoldenCase {
                name,
                ppq: 0,
                bpm: 0.0,
                inote: 0,
                fstep: 0.0,
                sources: Vec::new(),
                created: Vec::new(),
            };
            for kv in parts {
                let (k, v) = kv.split_once('=').unwrap();
                match k {
                    "ppq" => c.ppq = v.parse().unwrap(),
                    "bpm" => c.bpm = v.parse().unwrap(),
                    "iNote" => c.inote = v.parse().unwrap(),
                    "fStep" => c.fstep = v.parse().unwrap(),
                    _ => panic!("unknown field {k}"),
                }
            }
            cases.push(c);
            continue;
        }
        let (is_source, nums) = match line.strip_prefix("source ") {
            Some(r) => (true, r),
            None => (false, line),
        };
        let v: Vec<u64> = nums
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        let row = [v[0], v[1], v[2], v[3], v[4]];
        let c = cases.last_mut().unwrap();
        if is_source {
            c.sources.push(row);
        } else {
            c.created.push(row);
        }
    }
    cases
}

/// The chops of `c`'s sources computed by PlumSpirit's kernel function, in the
/// Lua's order, with the mocked Andromeda tempo (`ticks_to_secs(t) = t · 60 /
/// (bpm · ppq)`).
fn chop_case(c: &GoldenCase, mode: HzMode) -> Vec<[u64; 5]> {
    let secs = c.ppq as f64 * 60.0 / (c.bpm * c.ppq as f64);
    let mut out = Vec::new();
    for s in &c.sources {
        let key = if c.inote < 0 {
            s[3] as u8
        } else {
            c.inote as u8
        };
        let g = hz_base_gate(c.ppq, secs, hz_of(key));
        hz_chop_note(s[0], s[1], g, c.fstep, mode, |start, len| {
            out.push([start, len, s[2], s[3], s[4]]);
            true
        });
    }
    out
}

#[test]
fn hz_chop_reproduces_every_note_of_the_original_lua() {
    let cases = golden();
    assert_eq!(cases.len(), 13);
    let mut total = 0;
    for c in &cases {
        let mine = chop_case(c, HzMode::OriginalLua);
        assert_eq!(
            mine.len(),
            c.created.len(),
            "{}: {} chops, the Lua made {}",
            c.name,
            mine.len(),
            c.created.len()
        );
        for (i, (a, b)) in mine.iter().zip(&c.created).enumerate() {
            assert_eq!(a, b, "{}: chop {i} differs", c.name);
        }
        total += mine.len();
    }
    assert!(total > 5000, "{total}");
}

#[test]
fn hz_chop_frequency_table_is_the_lua_formula() {
    for k in 0..128u8 {
        let f = 440.0 * 2f64.powf((f64::from(k) - 69.0) / 12.0);
        assert!((hz_of(k) - f).abs() <= f * 1e-15, "{k}");
    }
    assert_eq!(hz_of(69), 440.0);
}

/// The 120 BPM cases also go through the whole tool (registry, source,
/// preflight, kernel): the generated notes equal the Lua's, the source notes
/// are kept, channel / key / velocity / subchannel come from each source.
#[test]
fn hz_chop_as_a_tool_equals_the_lua_and_keeps_the_source_notes() {
    for c in golden().iter().filter(|c| c.bpm == 120.0) {
        let notes: Vec<NoteRecord> = c
            .sources
            .iter()
            .map(|s| rec(s[0], s[1], s[3] as u8, s[4] as u8, s[2] as u8, 0))
            .collect();
        let d = doc_with(c.ppq as u32, &notes);
        let r = run_tool(
            &d,
            "hz_chop",
            &[
                ("inote", ParamValue::Int(c.inote)),
                ("fstep", ParamValue::Float(c.fstep)),
            ],
            &all_selected(&d),
        )
        .unwrap();
        let mut got: Vec<[u64; 5]> = r
            .notes
            .iter()
            .map(|n| {
                [
                    n.start,
                    n.dur,
                    u64::from(n.ch),
                    u64::from(n.pitch),
                    u64::from(n.vel),
                ]
            })
            .collect();
        let mut want = c.created.clone();
        want.extend(c.sources.iter().copied());
        got.sort();
        want.sort();
        assert_eq!(got, want, "{}", c.name);
    }
}

#[test]
fn hz_chop_replace_mode_drops_the_sources_and_keeps_track_subchannel_and_group() {
    let mut d = PlumDocument::new_project("hz".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(1), 5)],
        },
    )
    .unwrap();
    let g = d.project_mut().groups.add(Default::default());
    let mut a = rec(0, 960, 69, 100, 5, 1);
    a.group = g;
    let b = rec(1000, 50, 60, 80, 0, 0);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[a, b], true)
        .unwrap();
    let r = run_tool(
        &d,
        "hz_chop",
        &[("replace", ParamValue::Bool(true))],
        &all_selected(&d),
    )
    .unwrap();
    // A4 at 120 BPM, 960 PPQ: BaseGate = 1920 / 440 = 4.3636…
    let from_a: Vec<&TNote> = r.notes.iter().filter(|n| n.pitch == 69).collect();
    assert!(from_a.iter().all(|n| n.dur == 4 || n.dur == 5));
    assert!(from_a.len() > 200);
    assert!(
        from_a
            .iter()
            .all(|n| n.sub == 1 && n.ch == 5 && n.group == g && n.vel == 100)
    );
    assert!(!r.notes.iter().any(|n| n.start == 0 && n.dur == 960));
    // Commit: one undo step; undo restores the notes exactly.
    let before = d.state_fingerprint();
    let mut d2 = d.clone();
    let mut h2 = History::default();
    let op = build_operation(&mut d2, &r, "HZ-Chop").unwrap();
    h2.apply(&mut d2, op).unwrap();
    assert_eq!(d2.track(0).unwrap().notes.len(), r.notes.len());
    h2.undo(&mut d2).unwrap();
    assert_eq!(d2.state_fingerprint(), before);
}

#[test]
fn hz_chop_refuses_invalid_settings_with_sentences() {
    let d = doc_with(960, &[rec(0, 960, 60, 100, 0, 0)]);
    for (k, v) in [
        ("inote", ParamValue::Int(-2)),
        ("inote", ParamValue::Int(128)),
        ("fstep", ParamValue::Float(51.0)),
        ("fstep", ParamValue::Float(f64::NAN)),
    ] {
        match run_tool(&d, "hz_chop", &[(k, v.clone())], &all_selected(&d)) {
            Err(TransformError::Invalid(m)) => assert!(m.ends_with('.'), "{m}"),
            // Out-of-range values may also be clamped by the parameter schema.
            Ok(_) => {}
            Err(e) => panic!("{k} {v:?}: {e}"),
        }
    }
}

#[test]
fn hz_chop_preflight_bounds_every_draw_and_refuses_explosions() {
    let mut r = Rng::new(5);
    for draw in 0..300u64 {
        let n = r.range_i64(1, 6) as usize;
        let notes: Vec<NoteRecord> = (0..n)
            .map(|i| {
                rec(
                    i as u64 * 100,
                    r.range_i64(1, 20_000) as u64,
                    r.range_i64(0, 127) as u8,
                    90,
                    0,
                    0,
                )
            })
            .collect();
        let d = doc_with([96u32, 480, 960, 3840][draw as usize % 4], &notes);
        let step = r.range_f64(-50.0, 50.0);
        let inote = r.range_i64(-1, 127);
        let mode = r.range_i64(0, 1) as u32;
        // run_tool asserts the preflight bound.
        run_tool(
            &d,
            "hz_chop",
            &[
                ("inote", ParamValue::Int(inote)),
                ("fstep", ParamValue::Float(step)),
                ("mode", ParamValue::Choice(mode)),
            ],
            &all_selected(&d),
        )
        .unwrap();
    }
    // A note spanning most of the tick range is refused before allocating.
    let d = doc_with(960, &[rec(0, 1 << 40, 127, 90, 0, 0)]);
    match run_tool(&d, "hz_chop", &[], &all_selected(&d)) {
        Err(TransformError::TooLarge(m)) => assert!(m.contains("limit"), "{m}"),
        other => panic!("{:?}", other.map(|r| r.notes.len())),
    }
}

#[test]
fn hz_chop_high_precision_keeps_the_average_gate_while_the_step_changes_it() {
    // fStep 0: both modes agree on the total length within one chop.
    let secs = 0.5;
    let g = hz_base_gate(960, secs, hz_of(57)); // 8.727…
    let (mut a, mut b) = (Vec::new(), Vec::new());
    hz_chop_note(0, 100_000, g, 0.0, HzMode::OriginalLua, |s, l| {
        a.push((s, l));
        true
    });
    hz_chop_note(0, 100_000, g, 0.0, HzMode::HighPrecision, |s, l| {
        b.push((s, l));
        true
    });
    assert!((a.len() as i64 - b.len() as i64).abs() <= 1);
    // With a step, High Precision follows Σ BaseGate exactly (± 1 tick).
    let step = 0.37;
    let mut ends = Vec::new();
    hz_chop_note(0, 200_000, g, step, HzMode::HighPrecision, |s, l| {
        ends.push(s + l);
        true
    });
    let mut exact = 0.0;
    for (k, e) in ends.iter().enumerate() {
        exact += g + step * (k + 1) as f64;
        assert!((*e as f64 - exact).abs() <= 1.0, "chop {k}");
    }
    // The original mode keeps the first fraction (≈ 0.727) forever: its
    // average drifts from Σ BaseGate — the modes are really different.
    let mut lua_end = 0;
    hz_chop_note(0, 200_000, g, step, HzMode::OriginalLua, |s, l| {
        lua_end = s + l;
        true
    });
    assert_ne!(lua_end, *ends.last().unwrap());
}

// ------------------------------------------------------------------ SAFOR

/// (tick, length, key, velocity, SAFOR track number = slot << 4 | channel)
type Sn = (u64, u64, u8, u8, u64);

/// SAFOR literally (upstream `smart_push` + `priority_predicate`): notes pushed
/// in note-off order per track, compared with every kept note of the same
/// (tick, key).
fn safor_literal_overlaps(notes: &[Sn]) -> Vec<Sn> {
    // (tick, length, key, velocity, track = slot << 4 | channel)
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|&i| {
        let n = notes[i];
        (n.4 >> 4, n.0 + n.1, n.4 & 15, n.2, n.0, i)
    });
    let mut kept: Vec<Sn> = Vec::new();
    for i in order {
        let mut n = notes[i];
        let mut j = 0;
        while j < kept.len() {
            let o = kept[j];
            let replace = if o.0 != n.0 || o.2 != n.2 {
                false
            } else {
                n.4 < o.4 || n.1 >= o.1
            };
            if replace {
                n.3 = n.3.max(o.3);
                kept.remove(j);
            } else {
                j += 1;
            }
        }
        kept.push(n);
    }
    kept.sort();
    kept
}

/// SAFOR's sustains removal literally: per key, per-tick owner array painted
/// in multiset order (tick, insertion), edges where the owner changes or a
/// note starts. Owner = (track number) like upstream.
fn safor_literal_sustains(survivors_in_order: &[Sn]) -> Vec<Sn> {
    let mut out = Vec::new();
    for key in 0..128u8 {
        let mut list: Vec<(usize, Sn)> = survivors_in_order
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, n)| n.2 == key)
            .collect();
        if list.is_empty() {
            continue;
        }
        list.sort_by_key(|(i, n)| (n.0, *i));
        let furthest = list.iter().map(|(_, n)| n.0 + n.1).max().unwrap() + 1;
        // (owner + 1, velocity, start flag)
        let mut data = vec![(0u64, 0u8, false); furthest as usize];
        for (_, n) in &list {
            let cur = data[n.0 as usize];
            data[n.0 as usize] = (n.4 + 1, cur.1.max(n.3), true);
            for t in n.0 + 1..n.0 + n.1 {
                data[t as usize] = (n.4 + 1, 0, false);
            }
        }
        let mut last = 0usize;
        for i in 1..data.len() {
            if data[i].0 != data[i - 1].0 || data[i].2 {
                let owner = data[i - 1].0;
                if owner != 0 {
                    let v = data[last].1;
                    out.push((
                        last as u64,
                        (i - last) as u64,
                        key,
                        if v == 0 { 1 } else { v },
                        owner - 1,
                    ));
                }
                last = i;
            }
        }
    }
    out.sort();
    out
}

fn safor_source(notes: &[Sn]) -> Source {
    let slots = notes.iter().map(|n| n.4 >> 4).max().unwrap_or(0) + 1;
    let tracks: Vec<SourceTrack> = (0..slots)
        .map(|i| SourceTrack {
            id: plumspirit_engine::model::TrackId(i as u32),
            index: i as usize,
            content: None,
            revision: 0,
            subs: (0..16).map(|c| (c as u32, c as u8)).collect(),
            all: false,
            insert_only: false,
            frame: 0,
        })
        .collect();
    let tn: Vec<TNote> = notes
        .iter()
        .map(|n| TNote {
            start: n.0,
            dur: n.1,
            pitch: n.2,
            vel: n.3,
            track: (n.4 >> 4) as u16,
            ch: (n.4 & 15) as u8,
            sub: (n.4 & 15) as u32,
            rel: 64,
            ..Default::default()
        })
        .collect();
    Source::from_notes(tn, tracks)
}

fn as_tuples(v: &[TNote]) -> Vec<Sn> {
    let mut o: Vec<_> = v
        .iter()
        .map(|n| {
            (
                n.start,
                n.dur,
                n.pitch,
                n.vel,
                (u64::from(n.track) << 4) | u64::from(n.ch),
            )
        })
        .collect();
    o.sort();
    o
}

fn random_safor_notes(r: &mut Rng, n: usize) -> Vec<Sn> {
    (0..n)
        .map(|_| {
            (
                r.range_i64(0, 40) as u64 * 10,
                r.range_i64(1, 12) as u64 * 10 + r.range_i64(0, 3) as u64,
                r.range_i64(60, 63) as u8,
                r.range_i64(1, 127) as u8,
                ((r.range_i64(0, 3) as u64) << 4) | r.range_i64(0, 3) as u64,
            )
        })
        .collect()
}

#[test]
fn safor_overlaps_equals_the_upstream_algorithm() {
    let mut r = Rng::new(42);
    for _ in 0..400 {
        let n = r.range_i64(1, 60) as usize;
        let notes = random_safor_notes(&mut r, n);
        let src = safor_source(&notes);
        let (mine, removed) = safor_overlaps(&src, &src.notes);
        let lit = safor_literal_overlaps(&notes);
        assert_eq!(as_tuples(&mine), lit, "{notes:?}");
        assert_eq!(removed as usize, notes.len() - lit.len());
    }
}

#[test]
fn safor_sustains_equals_the_per_tick_model() {
    let mut r = Rng::new(7);
    for _ in 0..400 {
        let n = r.range_i64(1, 50) as usize;
        let notes = random_safor_notes(&mut r, n);
        let src = safor_source(&notes);
        let (survivors, _) = safor_overlaps(&src, &src.notes);
        // Owner identity = SAFOR's track number: subchannel = channel here.
        let order: Vec<Sn> = survivors
            .iter()
            .map(|n| {
                (
                    n.start,
                    n.dur,
                    n.pitch,
                    n.vel,
                    (u64::from(n.track) << 4) | u64::from(n.ch),
                )
            })
            .collect();
        let (pieces, _, _) = safor_sustains(&survivors);
        assert_eq!(
            as_tuples(&pieces),
            safor_literal_sustains(&order),
            "{notes:?}"
        );
    }
}

#[test]
fn safor_examples_match_the_documented_rules() {
    // Same tick and key in tracks 0 and 1: the later track replaces the earlier
    // one unless it is shorter; the survivor keeps the louder velocity.
    let notes = [
        (0, 100, 60, 120, 0 << 4),
        (0, 100, 60, 30, 1 << 4),
        (0, 300, 62, 50, 0 << 4),
        (0, 100, 62, 90, 1 << 4),
        (10, 50, 64, 40, 0 << 4),
        (11, 50, 64, 40, 1 << 4),
    ];
    let src = safor_source(&notes);
    let (out, removed) = safor_overlaps(&src, &src.notes);
    assert_eq!(removed, 1);
    let t = as_tuples(&out);
    assert!(t.contains(&(0, 100, 60, 120, 1 << 4)));
    assert!(t.contains(&(0, 300, 62, 50, 0)) && t.contains(&(0, 100, 62, 90, 1 << 4)));
    assert_eq!(t.len(), 5);
    // Sustains: the long note is cut around the shorter one laid over it.
    let (pieces, cut, from) = safor_sustains(&out);
    let k62: Vec<_> = as_tuples(&pieces)
        .into_iter()
        .filter(|p| p.2 == 62)
        .collect();
    assert_eq!(k62, vec![(0, 100, 62, 90, 1 << 4), (100, 200, 62, 1, 0)]);
    // Changed: the long key-62 note (lost its first 100 ticks and its velocity)
    // and the first key-64 note (cut where the second starts, tick 11).
    assert_eq!((cut, from), (2, 2));
    assert!(as_tuples(&pieces).contains(&(10, 1, 64, 40, 0)));
}

#[test]
fn safor_tool_keeps_tracks_subchannels_groups_and_undoes_exactly() {
    let mut d = PlumDocument::new_project("safor".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(1), 3)],
        },
    )
    .unwrap();
    d.add_track().unwrap();
    let g = d.project_mut().groups.add(Default::default());
    let mut a = rec(0, 480, 60, 100, 3, 1);
    a.group = g;
    let mut dup = a;
    dup.attrs = NoteAttrs::new(60, 20, 3, 64, NoteOffKind::NoteOff).unwrap();
    let quiet = rec(960, 100, 70, 5, 0, 0);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[a, dup, quiet], true)
        .unwrap();
    // Track 2 has a longer note on the same tick and key: it wins.
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 960, 60, 50, 0, 0)], true)
        .unwrap();
    let before = d.state_fingerprint();
    let r = run_tool(&d, "safor", &[], &all_selected(&d)).unwrap();
    assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
    let winner = r.notes.iter().find(|n| n.pitch == 60).unwrap();
    assert_eq!((winner.track, winner.dur, winner.vel), (1, 960, 100));
    assert!(
        r.message
            .as_deref()
            .unwrap()
            .contains("2 doubled note(s) removed")
    );
    // Within one track: the stacked duplicate goes, subchannel and group stay.
    let one = run_tool(
        &d,
        "safor",
        &[],
        &[TrackScope {
            index: 0,
            all: false,
            content: None,
        }],
    )
    .unwrap();
    let kept = one.notes.iter().find(|n| n.pitch == 60).unwrap();
    assert_eq!((kept.sub, kept.group, kept.ch, kept.vel), (1, g, 3, 100));
    // Velocity threshold mode removes only the quiet note.
    let q = run_tool(
        &d,
        "safor",
        &[
            ("mode", ParamValue::Choice(2)),
            ("threshold", ParamValue::Int(10)),
        ],
        &all_selected(&d),
    )
    .unwrap();
    assert_eq!(q.notes.len(), 3);
    assert!(q.notes.iter().all(|n| n.vel >= 10));
    // Commit, save / reopen, undo.
    let mut d2 = d.clone();
    let mut h2 = History::default();
    let op = build_operation(&mut d2, &r, "SAFOR").unwrap();
    h2.apply(&mut d2, op).unwrap();
    let total: usize = (0..d2.track_count())
        .map(|i| d2.track(i).unwrap().notes.len())
        .sum();
    assert_eq!(total, 2);
    let bytes = project_to_bytes(&d2).unwrap();
    let back = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.0.state_fingerprint(), d2.state_fingerprint());
    h2.undo(&mut d2).unwrap();
    assert_eq!(d2.state_fingerprint(), before);
}

#[test]
fn safor_leaves_muted_notes_alone() {
    let mut a = rec(0, 100, 60, 100, 0, 0);
    a.attrs = a.attrs.with_muted(true);
    let b = rec(0, 100, 60, 90, 0, 0);
    let d = doc_with(960, &[a, b]);
    let r = run_tool(&d, "safor", &[], &all_selected(&d));
    // Nothing doubled among the sounding notes: unchanged.
    match r {
        Ok(r) => assert_eq!(r.notes.len(), 2),
        Err(e) => panic!("{e}"),
    }
}

/// 1M notes (100k in debug): measured, no O(N²).
#[test]
fn safor_scales_to_large_projects() {
    let n: usize = if cfg!(debug_assertions) {
        100_000
    } else {
        1_000_000
    };
    let mut r = Rng::new(3);
    let mut recs: Vec<NoteRecord> = (0..n)
        .map(|i| {
            rec(
                (i as u64 / 4) * 30 + r.range_i64(0, 2) as u64 * 30,
                r.range_i64(10, 400) as u64,
                r.range_i64(40, 90) as u8,
                r.range_i64(1, 127) as u8,
                r.range_i64(0, 3) as u8,
                0,
            )
        })
        .collect();
    recs.sort_by_key(|r| r.start_tick);
    let d = doc_with(960, &recs);
    for mode in 0..2u32 {
        let t = std::time::Instant::now();
        let out = run_tool(
            &d,
            "safor",
            &[("mode", ParamValue::Choice(mode))],
            &all_selected(&d),
        )
        .unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "SAFOR mode {mode}: {n} notes in, {} out, {ms:.1} ms",
            out.notes.len()
        );
        assert!(ms < 30_000.0);
    }
}

// ------------------------------------------------------------------ paste foundation

use plumspirit_engine::transform::paste::{
    self, BelowZero, ChannelRule, ColorInfo, ColorNote, Dest, ExistingDest, Generated, PasteMode,
    PasteSpec,
};

/// Four colours, colour c on pitch 60 + c, 3 notes each.
fn art() -> Generated {
    let mut g = Generated {
        notes: Vec::new(),
        colors: (0..4u8)
            .map(|c| ColorInfo::new([c * 60, 255 - c * 60, 128]))
            .collect(),
    };
    for c in 0..4u32 {
        for k in 0..3u64 {
            g.notes.push(ColorNote {
                start: k * 120,
                dur: 100,
                pitch: 60 + c as u8,
                vel: 90,
                color: c,
            });
        }
    }
    g.sort();
    g
}

fn paste_doc() -> PlumDocument {
    let mut d = PlumDocument::new_project("paste".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    // Destination already has Subchannel id 11 on MIDI channel 7.
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(11), 7)],
        },
    )
    .unwrap();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 10, 40, 50, 0, 0)], true)
        .unwrap();
    d.add_track().unwrap();
    d
}

fn apply_paste(d: &mut PlumDocument, h: &mut History, a: &Generated, spec: &PasteSpec) {
    let p = paste::plan(d, a, spec);
    let op = paste::build(d, a, spec, &p, "Paste Artwork").unwrap();
    h.apply(d, op).unwrap();
}

#[test]
fn paste_to_subchannels_uses_the_signed_offset_and_reuses_existing_ones() {
    let mut d = paste_doc();
    let before = d.state_fingerprint();
    let dest = d.track(0).unwrap().id();
    let a = art();
    let mut spec = PasteSpec::new(dest, 3840);
    spec.sub_offset = 10;
    let p = paste::plan(&d, &a, &spec);
    assert!(p.error.is_none(), "{:?}", p.error);
    let ids: Vec<(u32, u8, bool)> = p
        .colors
        .iter()
        .map(|c| match c.dest {
            Dest::Subchannel { id, channel, new } => (id.0, channel, new),
            ref other => panic!("{other:?}"),
        })
        .collect();
    // 0..3 + 10 = 10, 11, 12, 13; 11 exists (channel 7) and is reused; new
    // ones take the next channels after the table's last (7): 8, 9, 10.
    assert_eq!(
        ids,
        vec![(10, 8, true), (11, 7, false), (12, 9, true), (13, 10, true)]
    );
    let mut h = History::default();
    apply_paste(&mut d, &mut h, &a, &spec);
    let t = d.track(0).unwrap();
    assert_eq!(t.notes.len(), 13);
    assert_eq!(t.notes.selected_count(), 12);
    for v in t.notes.iter() {
        if v.start_tick >= 3840 {
            let want = SubchannelId(u32::from(v.attrs.pitch() - 60) + 10);
            assert_eq!(v.sub, want);
            let ch = t.subchannels().get(want).unwrap().midi_channel;
            assert_eq!(v.attrs.channel(), ch);
        }
    }
    assert!(
        t.subchannels()
            .get(SubchannelId(12))
            .unwrap()
            .display_name()
            .contains('#')
    );
    // Save / reopen, then one undo restores everything.
    let back = project_from_bytes(&project_to_bytes(&d).unwrap())
        .unwrap()
        .0;
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert_eq!(h.undo_len(), 1);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
}

#[test]
fn negative_offsets_refuse_clamp_or_drop_as_chosen() {
    let d = paste_doc();
    let dest = d.track(0).unwrap().id();
    let a = art();
    let mut spec = PasteSpec::new(dest, 0);
    spec.sub_offset = -2;
    // Refuse (default): colours 0 and 1 would be below 0.
    let p = paste::plan(&d, &a, &spec);
    let e = p.error.clone().unwrap();
    assert!(e.contains("0, 1") && e.contains("at least 0"), "{e}");
    assert!(paste::build(&d, &a, &spec, &p, "Paste").is_err());
    // Clamp: colours 0, 1, 2 share Subchannel 0 (the main one, existing).
    spec.below_zero = BelowZero::Clamp;
    let p = paste::plan(&d, &a, &spec);
    assert!(p.error.is_none());
    let ids: Vec<u32> = p
        .colors
        .iter()
        .map(|c| match c.dest {
            Dest::Subchannel { id, .. } => id.0,
            _ => u32::MAX,
        })
        .collect();
    assert_eq!(ids, vec![0, 0, 0, 1]);
    assert_eq!(p.notes, 12);
    // Drop: colours 0 and 1 are left out.
    spec.below_zero = BelowZero::Drop;
    let p = paste::plan(&d, &a, &spec);
    assert_eq!((p.notes, p.skipped), (6, 6));
    let mut d2 = d.clone();
    let mut h = History::default();
    apply_paste(&mut d2, &mut h, &a, &spec);
    assert_eq!(d2.track(0).unwrap().notes.len(), 7);
}

#[test]
fn paste_to_new_subtracks_creates_one_coloured_subtrack_per_colour() {
    let mut d = paste_doc();
    let before = d.state_fingerprint();
    let base_fp = d.fingerprint();
    let dest = d.track(0).unwrap().id();
    let a = art();
    let mut spec = PasteSpec::new(dest, 960);
    spec.mode = PasteMode::Subtracks;
    spec.channel = ChannelRule::ByColor;
    spec.prefix = "Image".into();
    let mut h = History::default();
    apply_paste(&mut d, &mut h, &a, &spec);
    // Track 0, its 4 new subtracks, then the second track.
    assert_eq!(d.track_count(), 6);
    for c in 0..4usize {
        let t = d.track(1 + c).unwrap();
        assert_eq!(t.parent(), Some(dest));
        assert_eq!(t.notes.len(), 3);
        assert_eq!(t.props().onion_color, Some(a.colors[c].rgb));
        assert!(t.name().unwrap().starts_with("Image #"));
        assert!(t.notes.iter().all(|v| v.attrs.channel() == c as u8
            && v.attrs.pitch() == 60 + c as u8
            && v.start_tick >= 960));
    }
    let back = project_from_bytes(&project_to_bytes(&d).unwrap())
        .unwrap()
        .0;
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    // Track ids are never reused: undo removes the subtracks and their notes
    // exactly; the next free id stays advanced (by design).
    assert_ne!(before, d.state_fingerprint());
    h.undo(&mut d).unwrap();
    assert_eq!(d.track_count(), 2);
    assert_eq!(d.fingerprint(), base_fp);
}

#[test]
fn explicit_mapping_sends_colours_to_chosen_tracks_and_subchannels() {
    let mut d = paste_doc();
    let dest = d.track(0).unwrap().id();
    let other = d.track(1).unwrap().id();
    let a = art();
    let mut spec = PasteSpec::new(dest, 0);
    spec.mode = PasteMode::Existing;
    spec.existing = vec![
        ExistingDest::Track(other),
        ExistingDest::Subchannel(SubchannelId(11)),
        ExistingDest::Skip,
        ExistingDest::Subchannel(SubchannelId(5)), // missing → refused
    ];
    let p = paste::plan(&d, &a, &spec);
    assert!(
        p.error
            .as_deref()
            .unwrap()
            .contains("not in the destination")
    );
    spec.existing[3] = ExistingDest::Skip;
    let mut h = History::default();
    apply_paste(&mut d, &mut h, &a, &spec);
    assert_eq!(d.track(1).unwrap().notes.len(), 3);
    let t0 = d.track(0).unwrap();
    assert_eq!(t0.notes.len(), 4);
    assert!(
        t0.notes
            .iter()
            .filter(|v| v.attrs.pitch() == 61)
            .all(|v| v.sub == SubchannelId(11) && v.attrs.channel() == 7)
    );
    assert_eq!(h.undo_len(), 1);
}

#[test]
fn a_paste_planned_before_the_destination_changed_is_refused_as_stale() {
    let mut d = paste_doc();
    let dest = d.track(0).unwrap().id();
    let a = art();
    let mut spec = PasteSpec::new(dest, 0);
    spec.sub_offset = 10;
    let p = paste::plan(&d, &a, &spec);
    // A subchannel is added meanwhile: the plan no longer matches.
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![Subchannel::new(SubchannelId(12), 3)],
        },
    )
    .unwrap();
    assert_eq!(
        paste::build(&d, &a, &spec, &p, "Paste").unwrap_err(),
        TransformError::Stale
    );
}

// ------------------------------------------------------------------ Image to MIDI

use plumspirit_engine::transform::image_midi::{self as im, ImageTransform, Rgba, Settings};

fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Rgba {
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.push(f(x, y));
        }
    }
    Rgba::new(w, h, px).unwrap()
}

/// Two coloured blocks over a transparent background, 64 keys high.
fn blocks() -> Rgba {
    picture(64, 32, |x, y| {
        if (4..20).contains(&x) && (4..28).contains(&y) {
            [220, 30, 30, 255]
        } else if (30..60).contains(&x) && (10..20).contains(&y) {
            [30, 60, 220, 255]
        } else {
            [0, 0, 0, 0]
        }
    })
}

fn convert(img: &Rgba, s: &Settings) -> Generated {
    im::convert(&im::prepare(img, s).unwrap(), s, 1_000_000).unwrap()
}

#[test]
fn image_blocks_become_run_length_notes_and_paste_as_one_undo_step() {
    let s = Settings {
        first_key: 40,
        last_key: 71, // 32 keys = one key per pixel row
        time_pixels: Some(64),
        ticks_per_pixel: 30,
        palette: vec![[220, 30, 30], [30, 60, 220]],
        ..Default::default()
    };
    let g = convert(&blocks(), &s);
    // Red: 24 rows × one note of 16 pixels; blue: 10 rows × 30 pixels.
    let red: Vec<_> = g.notes.iter().filter(|n| n.color == 0).collect();
    let blue: Vec<_> = g.notes.iter().filter(|n| n.color == 1).collect();
    assert_eq!((red.len(), blue.len()), (24, 10));
    assert!(red.iter().all(|n| n.start == 4 * 30 && n.dur == 16 * 30));
    assert!(blue.iter().all(|n| n.start == 30 * 30 && n.dur == 30 * 30));
    // Row y = 4 is key 71 − 4 = 67 (top row = highest key).
    assert_eq!(red.iter().map(|n| n.pitch).max(), Some(67));
    // Paste: colours → Subchannels 5, 6.
    let mut d = paste_doc();
    let before = d.state_fingerprint();
    let dest = d.track(0).unwrap().id();
    let mut spec = PasteSpec::new(dest, 1920);
    spec.sub_offset = 5;
    let mut h = History::default();
    apply_paste(&mut d, &mut h, &g, &spec);
    let t = d.track(0).unwrap();
    assert_eq!(t.notes.len(), 35);
    assert!(t.subchannels().contains(SubchannelId(5)) && t.subchannels().contains(SubchannelId(6)));
    assert_eq!(h.undo_len(), 1);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
}

#[test]
fn image_rotation_flip_and_scale_change_the_notes_as_expected() {
    let base = Settings {
        first_key: 40,
        last_key: 71,
        time_pixels: Some(64),
        ticks_per_pixel: 1,
        palette: vec![[220, 30, 30], [30, 60, 220]],
        ..Default::default()
    };
    let g0 = convert(&blocks(), &base);
    // Flip X: every note mirrored in time within the 64-pixel picture.
    let flipped = Settings {
        transform: ImageTransform {
            flip_x: true,
            ..Default::default()
        },
        ..base.clone()
    };
    let g1 = convert(&blocks(), &flipped);
    let mirror = |v: &Generated| -> Vec<(u64, u64, u8, u32)> {
        let mut o: Vec<_> = v
            .notes
            .iter()
            .map(|n| (64 - n.start - n.dur, n.dur, n.pitch, n.color))
            .collect();
        o.sort();
        o
    };
    let plain = |v: &Generated| -> Vec<(u64, u64, u8, u32)> {
        let mut o: Vec<_> = v
            .notes
            .iter()
            .map(|n| (n.start, n.dur, n.pitch, n.color))
            .collect();
        o.sort();
        o
    };
    assert_eq!(mirror(&g1), plain(&g0));
    // Rotating by 180° = flip X + flip Y.
    let r180 = Settings {
        transform: ImageTransform {
            rotate: 180.0,
            ..Default::default()
        },
        ..base.clone()
    };
    let both = Settings {
        transform: ImageTransform {
            flip_x: true,
            flip_y: true,
            ..Default::default()
        },
        ..base.clone()
    };
    assert_eq!(
        plain(&convert(&blocks(), &r180)),
        plain(&convert(&blocks(), &both))
    );
    // Scale X −100 % equals Flip X.
    let neg = Settings {
        transform: ImageTransform {
            scale_x: -1.0,
            ..Default::default()
        },
        ..base.clone()
    };
    assert_eq!(plain(&convert(&blocks(), &neg)), plain(&g1));
}

#[test]
fn image_gradients_lines_and_repeated_runs_are_deterministic() {
    // A horizontal gradient quantised to 4 colours: 4 runs per row.
    let grad = picture(64, 4, |x, _| [(x * 4) as u8, 0, 255 - (x * 4) as u8, 255]);
    let mut s = Settings {
        first_key: 60,
        last_key: 63,
        time_pixels: Some(64),
        ticks_per_pixel: 1,
        ..Default::default()
    };
    let img = im::prepare(&grad, &s).unwrap();
    s.palette = im::median_cut(&img, 4, 128);
    assert_eq!(s.palette.len(), 4);
    let g = im::convert(&img, &s, 1000).unwrap();
    assert_eq!(g.notes.len(), 16);
    assert_eq!(g, im::convert(&img, &s, 1000).unwrap());
    // A one-pixel line: one note.
    let line = picture(50, 1, |x, _| {
        if x == 10 {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let s1 = Settings {
        first_key: 60,
        last_key: 60,
        time_pixels: Some(50),
        palette: vec![[255, 255, 255]],
        ..Default::default()
    };
    let g1 = convert(&line, &s1);
    assert_eq!(g1.notes.len(), 1);
    assert_eq!((g1.notes[0].start, g1.notes[0].dur), (50, 5));
    // Repeated runs: alternating 3-pixel stripes → one note per stripe.
    let stripes = picture(30, 1, |x, _| {
        if (x / 3) % 2 == 0 {
            [255, 0, 0, 255]
        } else {
            [0, 255, 0, 255]
        }
    });
    let s2 = Settings {
        first_key: 60,
        last_key: 60,
        time_pixels: Some(30),
        ticks_per_pixel: 1,
        palette: vec![[255, 0, 0], [0, 255, 0]],
        ..Default::default()
    };
    let g2 = convert(&stripes, &s2);
    assert_eq!(g2.notes.len(), 10);
    assert!(g2.notes.iter().all(|n| n.dur == 3));
}
