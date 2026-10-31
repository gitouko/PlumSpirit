//! Spiderweb port against the original (T8-E-*): every case of
//! `fixtures/spiderweb_cases.json` run through the ORIGINAL Python engine
//! (`tools/dev/spiderweb_oracle/oracle.py`) gives `fixtures/spiderweb_golden.json`;
//! the Rust port must give exactly the same notes, tracks and render.
//!
//! Oracle platform policy (B2-S27 gate, E0069): the goldens were made by
//! CPython on Linux x86-64 with glibc, and both CPython and Rust's std hand
//! `sin`, `cos`, `pow`, `exp`, `log`, `log2`, `atan2`, `acos` and the C
//! `hypot` to the platform's C math library. On that reference platform the
//! port must match the goldens bit for bit (the cloud gates run there). On
//! any other platform the original Python Spiderweb itself does not reproduce
//! those bits, so only what never reaches the C math library stays exact
//! there (shape cleaning, which formulas are read or refused, their names,
//! CPython's own `math.hypot`); differences in the values computed through
//! it are counted and printed, not failed. Checked with a shim that moves
//! every such result by one ulp (`docs/BUILTIN_PLUGINS.md`).

use plumspirit_engine::json::{self, Value};
use plumspirit_engine::spiderweb::{self, engine};

fn fnv(path: &[[f64; 2]]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for q in path {
        for v in q {
            h = (h ^ v.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The platform the goldens were made on (see the module comment).
const REFERENCE_LIBM: bool = cfg!(all(
    target_os = "linux",
    target_env = "gnu",
    target_arch = "x86_64"
));

/// Fails on any `exact` difference everywhere and on any `libm` difference
/// on the reference platform; elsewhere prints how many values computed
/// through the platform's C math library differ from the reference.
fn settle(what: &str, exact: &[String], libm: &[String]) {
    let show = |v: &[String]| v[..v.len().min(60)].join("\n");
    assert!(
        exact.is_empty(),
        "{what}: {} differences that never reach the C math library:\n{}",
        exact.len(),
        show(exact)
    );
    if REFERENCE_LIBM {
        assert!(
            libm.is_empty(),
            "{what}: {} differences:\n{}",
            libm.len(),
            show(libm)
        );
    } else if !libm.is_empty() {
        // The whole list for review (CARGO_TARGET_TMPDIR, one file per test).
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "spiderweb_platform_differences_{}.txt",
            what.replace(' ', "_")
        ));
        let _ = std::fs::write(&path, libm.join("\n"));
        eprintln!(
            "{what}: {} values computed through this platform's C math library differ from the \
             Linux / glibc reference (not a failure here; exact on the reference platform; all \
             listed in {}):\n{}",
            libm.len(),
            path.display(),
            show(libm)
        );
    }
}

fn rows(v: &Value) -> Vec<Vec<i64>> {
    v.as_arr()
        .unwrap_or(&[])
        .iter()
        .map(|r| {
            r.as_arr()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap() as i64)
                .collect()
        })
        .collect()
}

#[test]
fn every_case_equals_the_original_spiderweb() {
    let cases = json::parse(include_str!("fixtures/spiderweb_cases.json")).unwrap();
    let golden = json::parse(include_str!("fixtures/spiderweb_golden.json")).unwrap();
    let (cases, golden) = (cases.as_arr().unwrap(), golden.as_arr().unwrap());
    assert_eq!(cases.len(), golden.len());
    let (mut exact, mut libm) = (Vec::new(), Vec::new());
    let mut checked = 0;
    let mut strokes = 0;
    let mut custom = 0;
    for (c, g) in cases.iter().zip(golden) {
        let name = c.get("name").and_then(Value::as_str).unwrap();
        let ppq = c.get("ppq").and_then(Value::as_f64).unwrap_or(960.0) as u32;
        let keys = c.get("keys").and_then(Value::as_f64).unwrap_or(128.0) as i64;
        let cleaned: Vec<Option<Value>> = c
            .get("shapes")
            .unwrap()
            .as_arr()
            .unwrap()
            .iter()
            .map(spiderweb::clean_shape)
            .collect();
        let want_clean: Vec<bool> = g
            .get("clean")
            .unwrap()
            .as_arr()
            .unwrap()
            .iter()
            .map(|b| b.as_bool().unwrap())
            .collect();
        let got_clean: Vec<bool> = cleaned.iter().map(Option::is_some).collect();
        if got_clean != want_clean {
            exact.push(format!("{name}: clean {got_clean:?} != {want_clean:?}"));
            continue;
        }
        let shapes: Vec<Value> = cleaned.into_iter().flatten().collect();
        let mut lists = Vec::new();
        let mut tracks = Vec::new();
        let mut ok = true;
        for (k, (sh, gs)) in shapes
            .iter()
            .zip(g.get("shapes").unwrap().as_arr().unwrap())
            .enumerate()
        {
            let (n, t) = spiderweb::notes_tracks(sh, ppq, keys);
            let want = rows(gs.get("notes").unwrap());
            let got: Vec<Vec<i64>> = n.iter().map(|r| r.to_vec()).collect();
            if got != want {
                let i = got.iter().zip(&want).position(|(a, b)| a != b);
                libm.push(format!(
                    "{name} shape {k}: {} notes vs {} (first difference at {i:?}: {:?} vs {:?})",
                    got.len(),
                    want.len(),
                    i.map(|i| &got[i]),
                    i.map(|i| &want[i])
                ));
                ok = false;
            }
            let want_t = gs.get("tracks").unwrap();
            let want_t: Option<Vec<i64>> = want_t
                .as_arr()
                .map(|a| a.iter().map(|x| x.as_f64().unwrap() as i64).collect());
            if t != want_t {
                libm.push(format!("{name} shape {k}: tracks differ"));
                ok = false;
            }
            // the geometry, bit for bit (FNV-1a over every coordinate's bits)
            let want_s: Vec<String> = gs
                .get("strokes")
                .unwrap()
                .as_arr()
                .unwrap()
                .iter()
                .map(|p| {
                    let p = p.as_arr().unwrap();
                    format!("{}:{}", p[0].as_f64().unwrap(), p[1].as_str().unwrap())
                })
                .collect();
            let got_s: Vec<String> = spiderweb::shape_strokes(sh)
                .iter()
                .map(|p| format!("{}:{}", p.len(), fnv(p)))
                .collect();
            if got_s != want_s {
                libm.push(format!("{name} shape {k}: strokes {got_s:?} vs {want_s:?}"));
            }
            strokes += got_s.len();
            lists.push(n);
            // (pasted notes keep every track; other shapes keep 15 colours)
            tracks.push(if sh.has("notes") {
                t
            } else {
                t.map(|t| spiderweb::capped_colours(&t))
            });
        }
        if !ok {
            continue;
        }
        let mode = match c.get("mode").and_then(Value::as_str).unwrap_or("single") {
            "raw" => engine::ChannelMode::Raw,
            "auto" => engine::ChannelMode::Auto,
            _ => engine::ChannelMode::Single,
        };
        let split = match c.get("split").and_then(Value::as_str).unwrap_or("key") {
            "time" => engine::Split::Time,
            _ => engine::Split::Key,
        };
        let apart: Vec<bool> = shapes.iter().map(spiderweb::tracks_apart).collect();
        let (r, count) = engine::render(&lists, mode, split, &tracks, &apart);
        let want = rows(g.get("rendered").unwrap());
        let got: Vec<Vec<i64>> = r.iter().map(|x| x.to_vec()).collect();
        if got != want || count != g.get("count").unwrap().as_f64().unwrap() as usize {
            libm.push(format!(
                "{name}: render differs ({} vs {} notes)",
                got.len(),
                want.len()
            ));
            continue;
        }
        checked += 1;
        if name.starts_with("custom") {
            custom += 1;
        }
    }
    settle("Spiderweb cases", &exact, &libm);
    assert!(cases.len() > 150 && strokes > 700);
    if REFERENCE_LIBM {
        assert!(checked > 150 && custom > 190);
    }
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// The formula language (T8-E-formula): ~2,800 hand-made and seeded random
/// formulas read and worked out by the ORIGINAL `files/mathexpr.formula`
/// (`tools/dev/spiderweb_oracle/formula_oracle.py`); the port must accept
/// exactly the same texts, find the same names and give the same float (or
/// fail) at every position. Also CPython's `math.hypot` bit for bit.
#[test]
fn every_formula_reads_and_works_out_like_spiderweb() {
    use plumspirit_engine::spiderweb::formula::Formula;
    let g = json::parse(include_str!("fixtures/spiderweb_formula_golden.json")).unwrap();
    let (mut exact, mut libm) = (Vec::new(), Vec::new());
    let cases = g.get("formulas").unwrap().as_arr().unwrap();
    for c in cases {
        let text = c.get("text").and_then(Value::as_str).unwrap();
        let var = c.get("var").and_then(Value::as_str).unwrap();
        let named = c.get("named").and_then(Value::as_bool).unwrap();
        let got = Formula::parse(text, named, var);
        let refused = c.get("error").is_some();
        match (&got, refused) {
            (Err(_), true) => continue,
            (Ok(_), true) => {
                exact.push(format!("{text:?}: read, Spiderweb refuses it"));
                continue;
            }
            (Err(e), false) => {
                exact.push(format!("{text:?}: refused ({e}), Spiderweb reads it"));
                continue;
            }
            (Ok(_), false) => {}
        }
        let f = got.unwrap();
        let names: Vec<&str> = c
            .get("names")
            .unwrap()
            .as_arr()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        if f.names != names {
            exact.push(format!("{text:?}: names {:?} vs {names:?}", f.names));
            continue;
        }
        let vars: Vec<(String, f64)> = c
            .get("vars")
            .unwrap()
            .as_arr()
            .unwrap()
            .iter()
            .map(|p| {
                let p = p.as_arr().unwrap();
                (p[0].as_str().unwrap().to_owned(), p[1].as_f64().unwrap())
            })
            .collect();
        let vals = f.bind(&vars).unwrap();
        for p in c.get("at").unwrap().as_arr().unwrap() {
            let p = p.as_arr().unwrap();
            let x = p[0].as_f64().unwrap();
            let want = p[1].as_f64();
            let have = f.eval(x, &vals).ok();
            let ok = match (want, have) {
                (None, None) => true,
                (Some(a), Some(b)) => same(a, b),
                _ => false,
            };
            if !ok {
                libm.push(format!("{text:?} at {x}: {have:?} vs {want:?}"));
            }
        }
    }
    let mut hyp = 0;
    for h in g.get("hypot").unwrap().as_arr().unwrap() {
        let h = h.as_arr().unwrap();
        let (x, y, want) = (
            h[0].as_f64().unwrap(),
            h[1].as_f64().unwrap(),
            h[2].as_f64().unwrap(),
        );
        let have = spiderweb::np::hypot(x, y);
        if !same(have, want) {
            exact.push(format!("hypot({x:e}, {y:e}) = {have:e} vs {want:e}"));
        }
        hyp += 1;
    }
    assert!(cases.len() > 2000 && hyp >= 4000);
    settle("Spiderweb formulas", &exact, &libm);
}

#[test]
fn the_preflight_estimate_is_never_far_below_the_notes_made() {
    let cases = json::parse(include_str!("fixtures/spiderweb_cases.json")).unwrap();
    let mut worst = (0.0f64, String::new());
    for c in cases.as_arr().unwrap() {
        let name = c.get("name").and_then(Value::as_str).unwrap();
        let ppq = c.get("ppq").and_then(Value::as_f64).unwrap_or(960.0) as u32;
        for sh in c.get("shapes").unwrap().as_arr().unwrap() {
            let Some(sh) = spiderweb::clean_shape(sh) else {
                continue;
            };
            let est = spiderweb::output::estimate_notes(&sh, ppq);
            let got = spiderweb::notes_tracks(&sh, ppq, 128).0.len() as u64;
            let ratio = got as f64 / (est.max(1)) as f64;
            if ratio > worst.0 {
                worst = (ratio, format!("{name}: {got} made, {est} estimated"));
            }
        }
    }
    assert!(worst.0 <= 1.0, "worst: {}", worst.1);
}
