//! B1-S35 (B1-0075 – B1-0077, contract §010): Note Properties' math
//! expressions (`note_props` n12) — documented examples with their tested
//! outputs, every out-of-range policy, rounding, exact huge ticks, seeded
//! `rnd`, constants, the controller value and the preview counts.

use plumspirit_engine::model::TrackId;
use plumspirit_engine::transform::output::Lane;
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::source::SourceTrack;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, Source, TNote, TransformError, TransformRun, run_kernel, tool,
};

fn tn(start: u64, dur: u64, pitch: u8, vel: u8) -> TNote {
    TNote {
        start,
        dur,
        pitch,
        vel,
        rel: 64,
        ..Default::default()
    }
}

/// Runs Note Properties with n12 on and `set` on one track with
/// subchannels 0 – 3: the result or the build / run error.
fn run(notes: Vec<TNote>, set: &[(&str, ParamValue)]) -> Result<TransformRun, TransformError> {
    let track = SourceTrack {
        id: TrackId(1),
        index: 0,
        content: None,
        revision: 0,
        subs: vec![(0, 0), (1, 1), (2, 2), (3, 3)],
        all: false,
        insert_only: false,
        frame: 0,
    };
    let src = Source::from_notes(notes, vec![track]);
    let t = tool("note_props").unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    ps.set("n12", ParamValue::Bool(true));
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b)?;
    let est = k.estimate_source(&src);
    let r = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)?;
    assert!(r.notes.len() as u64 <= est.notes);
    Ok(r)
}

fn x(k: &str, f: &str) -> (String, ParamValue) {
    (k.to_owned(), ParamValue::Text(f.to_owned()))
}

fn ok(notes: Vec<TNote>, set: &[(String, ParamValue)]) -> TransformRun {
    let s: Vec<(&str, ParamValue)> = set.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    run(notes, &s).unwrap_or_else(|e| panic!("{e}"))
}

fn err(notes: Vec<TNote>, set: &[(String, ParamValue)]) -> String {
    let s: Vec<(&str, ParamValue)> = set.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    match run(notes, &s) {
        Ok(r) => panic!("expected an error, got {:?}", r.message),
        Err(e) => e.user_message(),
    }
}

/// Notes sorted by (start, key, subchannel).
fn sorted(r: &TransformRun) -> Vec<TNote> {
    let mut v = r.notes.clone();
    v.sort_by_key(|n| (n.start, n.pitch, n.sub));
    v
}

/// Eight notes, one per eighth (480 ticks), keys 60 … 67, velocity 100,
/// channel 1, subchannel 0.
fn row() -> Vec<TNote> {
    (0..8)
        .map(|i| tn(i * 480, 240, 60 + i as u8, 100))
        .collect()
}

fn policy(p: u32) -> (String, ParamValue) {
    ("n12.policy".into(), ParamValue::Choice(p))
}

#[test]
fn documented_examples_give_their_tested_outputs() {
    // Ticks: t + index * 10 staggers the notes.
    let r = ok(row(), &[x("n12.t", "t + index * 10")]);
    let s: Vec<u64> = sorted(&r).iter().map(|n| n.start).collect();
    assert_eq!(s, vec![0, 490, 980, 1470, 1960, 2450, 2940, 3430]);
    // Gates: g * 2; ppqn / 4.
    let r = ok(row(), &[x("n12.g", "g * 2")]);
    assert!(r.notes.iter().all(|n| n.dur == 480));
    let r = ok(row(), &[x("n12.g", "ppqn / 4")]);
    assert!(r.notes.iter().all(|n| n.dur == 240));
    // Keys: k + round(sin(index / 4) * 12).
    let r = ok(row(), &[x("n12.k", "k + round(sin(index / 4) * 12)")]);
    let want: Vec<u8> = (0..8)
        .map(|i| (60 + i + ((i as f64 / 4.0).sin() * 12.0).round() as i64) as u8)
        .collect();
    assert_eq!(sorted(&r).iter().map(|n| n.pitch).collect::<Vec<_>>(), want);
    assert_eq!(want, vec![60, 64, 68, 71, 74, 76, 78, 79]);
    // Velocities: 20 + 100 * pos fades in (pos 0 … 1 over the starts).
    let r = ok(row(), &[x("n12.v", "20 + 100 * pos")]);
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![20, 34, 49, 63, 77, 91, 106, 120]);
    // Channels: index % 16 + 1 cycles them (channel 1 = 0 in a note).
    let r = ok(row(), &[x("n12.c", "index % 16 + 1")]);
    let c: Vec<u8> = sorted(&r).iter().map(|n| n.ch).collect();
    assert_eq!(c, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    // Subchannels: index % 4 cycles the first four.
    let r = ok(row(), &[x("n12.sc", "index % 4")]);
    let sc: Vec<u32> = sorted(&r).iter().map(|n| n.sub).collect();
    assert_eq!(sc, vec![0, 1, 2, 3, 0, 1, 2, 3]);
    // The owner's image16 style: a hashed channel, written with Lua names.
    let f =
        "(c*4 + t/120 + k/8 + ((math.sin(t*12.9898 + k*78.233) * 43758.5453) % 1) * 16) % 16 + 1";
    let r = ok(row(), &[x("n12.c", f)]);
    let a = sorted(&r);
    assert!(a.iter().all(|n| n.ch <= 15));
    assert_eq!(a, sorted(&ok(row(), &[x("n12.c", f)])), "repeatable");
    let distinct: std::collections::BTreeSet<u8> = a.iter().map(|n| n.ch).collect();
    assert!(distinct.len() >= 4, "{distinct:?}");
    // Every field together reads the values before n12 (one simultaneous
    // assignment): v from the old k, k from the old v.
    let r = ok(row(), &[x("n12.v", "k"), x("n12.k", "v - 40")]);
    let a = sorted(&r);
    assert!(a.iter().all(|n| n.pitch == 60));
    let mut vels: Vec<u8> = a.iter().map(|n| n.vel).collect();
    vels.sort_unstable();
    assert_eq!(vels, (60..68).collect::<Vec<u8>>());
    assert!(r.message.as_deref().unwrap().contains("8 note(s) changed"));
}

#[test]
fn empty_fields_and_identity_formulas_change_nothing() {
    let r = ok(row(), &[]);
    assert_eq!(sorted(&r), row());
    assert!(
        r.message
            .as_deref()
            .unwrap()
            .starts_with("Math expressions: 0 note(s) changed.")
    );
    let r = ok(
        row(),
        &[
            x("n12.t", "t"),
            x("n12.g", "g"),
            x("n12.k", "k"),
            x("n12.v", "v"),
            x("n12.c", "c"),
            x("n12.sc", "sc"),
        ],
    );
    assert_eq!(sorted(&r), row());
}

#[test]
fn out_of_range_results_follow_the_policy() {
    // Velocity 30 * index: 0 and 210 are outside 1 – 127.
    let f = [x("n12.v", "30 * index")];
    // Clamp (the default).
    let r = ok(row(), &f);
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![1, 30, 60, 90, 120, 127, 127, 127]);
    assert!(
        r.message.as_deref().unwrap().contains("4 value(s) clamped"),
        "{:?}",
        r.message
    );
    // Wrap: 1 + (x − 1) mod 127.
    let r = ok(row(), &[f[0].clone(), policy(1)]);
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![127, 30, 60, 90, 120, 23, 53, 83]);
    assert!(r.message.as_deref().unwrap().contains("4 value(s) wrapped"));
    // Leave unchanged: those notes keep velocity 100.
    let r = ok(row(), &[f[0].clone(), policy(2)]);
    let v: Vec<u8> = sorted(&r).iter().map(|n| n.vel).collect();
    assert_eq!(v, vec![100, 30, 60, 90, 120, 100, 100, 100]);
    let m = r.message.unwrap();
    assert!(m.contains("4 left unchanged (out of range)"), "{m}");
    assert!(
        m.contains("First: Velocities (v) gives 0 outside 1 – 127 (note at tick 0, key 60)"),
        "{m}"
    );
    // Stop: nothing applies, the message names the note.
    let e = err(row(), &[f[0].clone(), policy(3)]);
    assert!(
        e.contains("Velocities (v) gives 0 for the note at tick 0, key 60"),
        "{e}"
    );
    assert!(e.contains("Nothing was changed"), "{e}");
    // Channels wrap around 1 – 16; keys around the key range; ticks and
    // gates clamp even under Wrap.
    let r = ok(row(), &[x("n12.c", "index + 15"), policy(1)]);
    let c: Vec<u8> = sorted(&r).iter().map(|n| n.ch).collect();
    assert_eq!(c, vec![14, 15, 0, 1, 2, 3, 4, 5]);
    let r = ok(row(), &[x("n12.k", "k + 64"), policy(1)]);
    let k: Vec<u8> = sorted(&r).iter().map(|n| n.pitch).collect();
    assert_eq!(k, vec![124, 125, 126, 127, 0, 1, 2, 3]);
    let r = ok(row(), &[x("n12.t", "t - 1000"), x("n12.g", "0"), policy(1)]);
    assert!(r.notes.iter().all(|n| n.dur == 1));
    assert_eq!(r.notes.iter().filter(|n| n.start == 0).count(), 3);
    // Subchannels clamp to the track's list (4 entries).
    let r = ok(row(), &[x("n12.sc", "index")]);
    let sc: Vec<u32> = sorted(&r).iter().map(|n| n.sub).collect();
    assert_eq!(sc, vec![0, 1, 2, 3, 3, 3, 3, 3]);
}

#[test]
fn results_that_are_no_number_skip_or_stop() {
    // 1 / (k − 61): division by zero on key 61; sqrt of a negative below.
    let r = ok(row(), &[x("n12.v", "100 + 1 / (k - 61)")]);
    let a = sorted(&r);
    assert_eq!(a[1].vel, 100, "the note at key 61 keeps its velocity");
    assert_eq!(a[0].vel, 99);
    let m = r.message.unwrap();
    assert!(m.contains("1 left unchanged (no number)"), "{m}");
    assert!(m.contains("Division by zero"), "{m}");
    let e = err(row(), &[x("n12.v", "100 + 1 / (k - 61)"), policy(3)]);
    assert!(e.starts_with("Velocities (v): Division by zero"), "{e}");
    assert!(e.contains("(note at tick 480, key 61)"), "{e}");
    let r = ok(row(), &[x("n12.g", "sqrt(k - 64) * 100")]);
    assert_eq!(r.notes.iter().filter(|n| n.dur == 240).count(), 4);
    // NaN never reaches a note: math.huge and math.random are refused.
    assert!(err(row(), &[x("n12.k", "math.huge")]).contains("infinity"));
    assert!(err(row(), &[x("n12.k", "math.random()")]).contains("rnd"));
}

#[test]
fn parse_errors_name_the_field_and_code_is_refused() {
    let e = err(row(), &[x("n12.k", "k +")]);
    assert!(
        e.starts_with("Keys (k): The expression ends too early"),
        "{e}"
    );
    for bad in [
        "os.execute(\"x\")",
        "require(\"io\")",
        "k; v",
        "load(k)",
        "x = 1",
        "\"k\"",
    ] {
        let e = err(row(), &[x("n12.k", bad)]);
        assert!(e.starts_with("Keys (k): "), "{bad}: {e}");
    }
}

#[test]
fn rounding_modes_and_rounding_noise() {
    let f = x("n12.v", "v / 3");
    let want = [(0, 33), (1, 33), (2, 34), (3, 33)];
    for (mode, v) in want {
        let r = ok(
            row(),
            &[f.clone(), ("n12.round".into(), ParamValue::Choice(mode))],
        );
        assert!(r.notes.iter().all(|n| n.vel == v), "mode {mode}");
    }
    // 0.1 * 30 is 3 in every mode, not 2.9999999999999996 floored.
    for mode in 0..4 {
        let r = ok(
            row(),
            &[
                x("n12.g", "0.1 * 30"),
                ("n12.round".into(), ParamValue::Choice(mode)),
            ],
        );
        assert!(r.notes.iter().all(|n| n.dur == 3), "mode {mode}");
    }
    // Halves: 57.5 is 58 nearest (away from zero) and up, 57 down and
    // toward zero.
    for (mode, want) in [(0, 58), (1, 57), (2, 58), (3, 57)] {
        let r = ok(
            row(),
            &[
                x("n12.k", "k - 2.5"),
                ("n12.round".into(), ParamValue::Choice(mode)),
            ],
        );
        assert_eq!(sorted(&r)[0].pitch, want, "mode {mode}");
    }
}

#[test]
fn huge_ticks_stay_exact_below_two_to_the_53() {
    let base: u64 = (1 << 52) + 12_345;
    let notes = vec![
        tn(base, 1 << 40, 60, 90),
        tn(base + 1, (1 << 40) + 1, 61, 90),
    ];
    let r = ok(
        notes.clone(),
        &[x("n12.t", "t + 1"), x("n12.g", "g * 2 + 1")],
    );
    let a = sorted(&r);
    assert_eq!(a[0].start, base + 1);
    assert_eq!(a[1].start, base + 2);
    assert_eq!(a[0].dur, (1 << 41) + 1);
    assert_eq!(a[1].dur, (1 << 41) + 3);
    // Beyond 2^53 − 1 the result clamps (or stops).
    let r = ok(notes.clone(), &[x("n12.t", "t * 4")]);
    assert!(r.notes.iter().all(|n| n.start == (1 << 53) - 1));
    assert!(err(notes, &[x("n12.t", "t * 4"), policy(3)]).contains("outside 0 – 9007199254740991"));
    // A source tick beyond 2^53 − 1 is refused when ticks are computed.
    let far = vec![tn(1 << 54, 10, 60, 90)];
    let e = err(far.clone(), &[x("n12.t", "t + 1")]);
    assert!(e.contains("only up to 9,007,199,254,740,991"), "{e}");
    // …but the other fields still work there.
    let r = ok(far, &[x("n12.v", "64")]);
    assert_eq!(r.notes[0].vel, 64);
    assert_eq!(r.notes[0].start, 1 << 54);
}

#[test]
fn rnd_is_seeded_and_repeatable() {
    let f = x("n12.v", "1 + floor(rnd * 127)");
    let seed = |s: i64| ("n12.seed".to_owned(), ParamValue::Int(s));
    let a = sorted(&ok(row(), &[f.clone(), seed(7)]));
    let b = sorted(&ok(row(), &[f.clone(), seed(7)]));
    let c = sorted(&ok(row(), &[f.clone(), seed(8)]));
    assert_eq!(a, b);
    assert_ne!(a, c);
    let distinct: std::collections::BTreeSet<u8> = a.iter().map(|n| n.vel).collect();
    assert!(distinct.len() >= 5);
}

#[test]
fn constants_are_named_values() {
    let consts = (
        "n12.consts".to_owned(),
        ParamValue::Text("a = 12; b = a * 2\nstep = ppq".into()),
    );
    // `ppq` is not a constant expression (no variables in constants).
    assert!(err(row(), std::slice::from_ref(&consts)).starts_with("Constants: step: "));
    let consts = (
        "n12.consts".to_owned(),
        ParamValue::Text("a = 12; B = a * 2;".into()),
    );
    let r = ok(row(), &[consts, x("n12.k", "k + a - b")]);
    assert_eq!(sorted(&r)[0].pitch, 48);
    for (bad, why) in [
        ("k = 1", "cannot be a constant's name"),
        ("sin = 1", "cannot be a constant's name"),
        ("a = 1; a = 2", "defined twice"),
        ("a 1", "name = value"),
        ("a = 1/0", "Division by zero"),
    ] {
        let e = err(
            row(),
            &[("n12.consts".into(), ParamValue::Text(bad.into()))],
        );
        assert!(e.contains(why), "{bad}: {e}");
    }
}

#[test]
fn the_controller_value_is_written_at_each_note_start() {
    let set = [
        x("n12.cc", "round(pos * 127)"),
        ("n12.ccn".into(), ParamValue::Int(74)),
        x("n12.c", "3"),
    ];
    let r = ok(row(), &set);
    let pts: Vec<(u8, u64, i64)> = r
        .events
        .iter()
        .filter(|e| e.lane == Lane::Cc(74))
        .map(|e| (e.ch, e.tick, i64::from(e.value)))
        .collect();
    let want: Vec<(u8, u64, i64)> = (0..8)
        .map(|i| (2, i * 480, (i as f64 / 7.0 * 127.0).round() as i64))
        .collect();
    assert_eq!(pts, want);
    assert!(
        r.message
            .as_deref()
            .unwrap()
            .contains("Controller events are written")
    );
    // On the lane of the base controller / n09 it gives no way: one point
    // per note.
    let mut set2 = set.to_vec();
    set2[1] = ("n12.ccn".into(), ParamValue::Int(10));
    set2.push(("ctl".into(), ParamValue::Choice(1)));
    set2.push(("n09".into(), ParamValue::Bool(true)));
    let r = ok(row(), &set2);
    let on10: Vec<i64> = r
        .events
        .iter()
        .filter(|e| e.lane == Lane::Cc(10))
        .map(|e| i64::from(e.value))
        .collect();
    assert_eq!(on10.len(), 8);
    assert_eq!(on10[7], 127);
    // Out of range: clamped to 0 – 127.
    let r = ok(row(), &[x("n12.cc", "200")]);
    assert!(r.events.iter().all(|e| e.value == 127));
}

/// B1-S72 (release sweep, note_props set 229): a channel-mode controller
/// (CC 120 – 127) is refused when the tool is built — before any preview —
/// instead of previewing and then failing at Apply; CC 119 is the highest
/// controller offered and written.
#[test]
fn channel_mode_controllers_are_refused_before_the_preview() {
    let spec = (tool("note_props").unwrap().spec)();
    let ccn = spec.params.iter().find(|p| p.key == "n12.ccn").unwrap();
    assert!(format!("{:?}", ccn.kind).contains("119"), "{:?}", ccn.kind);
    for n in [120, 123, 127] {
        let e = err(
            row(),
            &[x("n12.cc", "64"), ("n12.ccn".into(), ParamValue::Int(n))],
        );
        assert!(e.contains("channel-mode message"), "{n}: {e}");
    }
    let r = ok(
        row(),
        &[x("n12.cc", "64"), ("n12.ccn".into(), ParamValue::Int(119))],
    );
    assert_eq!(
        r.events.iter().filter(|e| e.lane == Lane::Cc(119)).count(),
        8
    );
    // Without a Controller Value formula the number is not used: no refusal.
    let r = ok(
        row(),
        &[x("n12.v", "v"), ("n12.ccn".into(), ParamValue::Int(127))],
    );
    assert!(r.events.is_empty());
}

#[test]
fn the_lock_mask_still_protects_locked_properties() {
    let r = ok(
        row(),
        &[
            x("n12.v", "1"),
            x("n12.k", "k + 1"),
            ("a7".into(), ParamValue::Bool(true)),
        ],
    );
    let a = sorted(&r);
    assert!(a.iter().all(|n| n.vel == 100), "velocity locked by default");
    assert_eq!(a[0].pitch, 61);
}

/// B1-0077: a bounded multi-million batch — 2,000,000 notes with every
/// field set (release: `cargo test --release -p plumspirit_engine --test
/// b1_note_expressions -- --ignored --nocapture`).
#[test]
#[ignore = "release measurement"]
fn two_million_notes_with_every_field_run_within_bounds() {
    let n = 2_000_000u64;
    let notes: Vec<TNote> = (0..n)
        .map(|i| tn(i * 7, 30, 20 + (i % 88) as u8, 1 + (i % 127) as u8))
        .collect();
    let set = [
        x("n12.t", "t + (index % 3)"),
        x("n12.g", "g + k % 5"),
        x("n12.k", "k + round(sin(index / 50) * 6)"),
        x("n12.v", "20 + 100 * pos"),
        x(
            "n12.c",
            "(c*4 + t/120 + k/8 + ((math.sin(t*12.9898 + k*78.233) * 43758.5453) % 1) * 16) % 16 + 1",
        ),
        x("n12.sc", "index % 4"),
        x("n12.cc", "round(rnd * 127)"),
    ];
    let t0 = std::time::Instant::now();
    let r = ok(notes, &set);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(r.notes.len() as u64, n);
    assert_eq!(r.events.len() as u64, n);
    println!(
        "B1-0077 2,000,000 notes, 7 fields: {ms:.0} ms ({:.1} ns/note); {}",
        ms * 1e6 / n as f64,
        r.message.unwrap_or_default()
    );
}
