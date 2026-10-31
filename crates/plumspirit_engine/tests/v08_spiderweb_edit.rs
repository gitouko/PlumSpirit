//! Spiderweb window operations (T8-E-EDIT): PlumSpirit's geometric
//! expansions (rotation, Scale X / Y ±, Flip X / Y), box-placed shapes, pasted
//! notes, live shapes and their split, fixed colours and text styles.

use plumspirit_engine::json::{self, Value};
use plumspirit_engine::spiderweb::edit::{self, Xform};
use plumspirit_engine::spiderweb::engine::{ChannelMode, Split};
use plumspirit_engine::spiderweb::output::{self, Channels};
use plumspirit_engine::spiderweb::paths::P;
use plumspirit_engine::spiderweb::text::{self, FontSource};
use plumspirit_engine::spiderweb::{self as sw};
use std::sync::atomic::AtomicBool;

const PPQ: u32 = 96;

fn shape(text: &str) -> Value {
    sw::clean_shape(&json::parse(text).unwrap()).expect("valid shape")
}

fn notes(sh: &Value) -> Vec<[i64; 4]> {
    let mut n = sw::shape_notes(sh, PPQ, 128);
    n.sort_unstable();
    n
}

fn render(shapes: &[Value], mode: ChannelMode) -> (Vec<[i64; 6]>, usize) {
    output::render_board(
        shapes,
        PPQ,
        128,
        Channels {
            mode,
            split: Split::Key,
        },
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn near(a: P, b: P) -> bool {
    (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
}

#[test]
fn flip_x_mirrors_a_line_in_time_around_its_centre() {
    let up = shape(r#"{"kind":"line","pts":[[0,60],[1,64]]}"#);
    let down = shape(r#"{"kind":"line","pts":[[0,64],[1,60]]}"#);
    let flipped =
        edit::transform_shapes(std::slice::from_ref(&up), &Xform::flip_x(), None, 0.25).unwrap();
    let p = sw::shape_pts(&flipped[0]);
    assert!(near(p[0], [1.0, 60.0]) && near(p[1], [0.0, 64.0]), "{p:?}");
    assert_eq!(notes(&flipped[0]), notes(&down));
    // twice = the original
    let back = edit::transform_shapes(&flipped, &Xform::flip_x(), None, 0.25).unwrap();
    assert_eq!(notes(&back[0]), notes(&up));
}

#[test]
fn flip_y_mirrors_pitches_and_negative_scale_y_equals_flip() {
    let up = shape(r#"{"kind":"line","pts":[[0,60],[2,66]]}"#);
    let f =
        edit::transform_shapes(std::slice::from_ref(&up), &Xform::flip_y(), None, 0.25).unwrap();
    let neg = Xform {
        sy: -1.0,
        ..Xform::default()
    };
    let g = edit::transform_shapes(&[up], &neg, None, 0.25).unwrap();
    let p = sw::shape_pts(&f[0]);
    assert!(near(p[0], [0.0, 66.0]) && near(p[1], [2.0, 60.0]), "{p:?}");
    assert_eq!(notes(&f[0]), notes(&g[0]));
}

#[test]
fn scale_x_two_doubles_the_time_span_and_keeps_the_keys() {
    let l = shape(r#"{"kind":"line","pts":[[0,60],[1,72]]}"#);
    let xf = Xform {
        sx: 2.0,
        ..Xform::default()
    };
    let s = edit::transform_shapes(std::slice::from_ref(&l), &xf, Some([0.0, 60.0]), 0.25).unwrap();
    let (a, b) = (notes(&l), notes(&s[0]));
    assert_eq!(a.len(), b.len());
    let span = |n: &[[i64; 4]]| {
        n.iter().map(|r| r[1]).max().unwrap() - n.iter().map(|r| r[0]).min().unwrap()
    };
    assert_eq!(span(&b), 2 * span(&a));
    let keys = |n: &[[i64; 4]]| n.iter().map(|r| r[2]).collect::<Vec<_>>();
    let (mut ka, mut kb) = (keys(&a), keys(&b));
    ka.sort_unstable();
    kb.sort_unstable();
    assert_eq!(ka, kb);
}

#[test]
fn negative_scale_x_mirrors_and_stretches() {
    let l = shape(r#"{"kind":"line","pts":[[0,60],[1,64]]}"#);
    let xf = Xform {
        sx: -2.0,
        ..Xform::default()
    };
    let s = edit::transform_shapes(&[l], &xf, Some([1.0, 62.0]), 0.25).unwrap();
    let p = sw::shape_pts(&s[0]);
    assert!(near(p[0], [3.0, 60.0]) && near(p[1], [1.0, 64.0]), "{p:?}");
}

#[test]
fn rotation_keeps_screen_proportions_and_a_full_turn_is_identity() {
    // 4 beats at 0.25 beats per key on screen = 16 keys wide → upright: 16 keys tall
    let l = shape(r#"{"kind":"line","pts":[[0,60],[4,60]]}"#);
    let r = edit::transform_shapes(std::slice::from_ref(&l), &Xform::rotation(90.0), None, 0.25)
        .unwrap();
    let p = sw::shape_pts(&r[0]);
    assert!(near(p[0], [2.0, 52.0]) && near(p[1], [2.0, 68.0]), "{p:?}");
    let full = edit::transform_shapes(
        std::slice::from_ref(&l),
        &Xform::rotation(360.0),
        None,
        0.25,
    )
    .unwrap();
    assert_eq!(notes(&full[0]), notes(&l));
}

#[test]
fn shapes_turned_past_the_start_move_right_to_beat_zero() {
    let l = shape(r#"{"kind":"line","pts":[[0,60],[1,61]]}"#);
    let xf = Xform {
        sx: 3.0,
        ..Xform::default()
    };
    let s = edit::transform_shapes(&[l], &xf, None, 0.25).unwrap();
    let (lo, hi) = edit::bounds(&s).unwrap();
    assert!(lo[0].abs() < 1e-9 && (hi[0] - 3.0).abs() < 1e-9);
}

#[test]
fn a_zero_scale_is_refused() {
    let l = shape(r#"{"kind":"line","pts":[[0,60],[1,61]]}"#);
    let xf = Xform {
        sy: 0.0,
        ..Xform::default()
    };
    assert!(edit::transform_shapes(&[l], &xf, None, 0.25).is_none());
}

#[test]
fn every_shape_kind_survives_rotation_and_flips() {
    let base = Value::obj();
    let mut shapes = vec![
        shape(r#"{"kind":"curve","pts":[[0,60],[0.5,60],[0.5,70],[1,70]]}"#),
        shape(r#"{"kind":"arc","pts":[[0,60],[1,66],[2,60]],"k":0.25}"#),
        shape(r#"{"kind":"free","pts":[[0,60],[0.3,63],[0.6,61],[1,65]],"smooth":40,"k":0.25}"#),
        shape(r#"{"kind":"poly","pts":[[0,60],[1,64],[2,60]],"tumour":{"on":true}}"#),
        shape(r#"{"kind":"funnel","pts":[[0,60],[2,60],[3,50],[3,70]],"starts":[]}"#),
    ];
    shapes.push(edit::circle_shape([0.0, 50.0], [2.0, 60.0], &base).unwrap());
    shapes.push(
        edit::polygon_shape(
            [0.0, 50.0],
            [2.0, 60.0],
            &json::parse(r#"{"points":5,"style":"star"}"#).unwrap(),
            &base,
        )
        .unwrap(),
    );
    for xf in [Xform::rotation(33.0), Xform::flip_x(), Xform::flip_y()] {
        for sh in &shapes {
            let t = edit::transform_shapes(std::slice::from_ref(sh), &xf, None, 0.25)
                .unwrap_or_else(|| panic!("{} did not survive {xf:?}", sw::kind(sh)));
            assert_eq!(sw::kind(&t[0]), sw::kind(sh));
            assert!(
                !sw::shape_notes(&t[0], PPQ, 128).is_empty(),
                "{}",
                sw::kind(sh)
            );
        }
    }
}

#[test]
fn box_shapes_make_notes_and_a_filled_circle_makes_more() {
    let base = Value::obj();
    let c = edit::circle_shape([0.0, 50.0], [4.0, 70.0], &base).unwrap();
    let mut filled = c.clone();
    filled.set("fill", Value::Str("fill".into()));
    let filled = sw::clean_shape(&filled).unwrap();
    let (a, b) = (notes(&c), notes(&filled));
    assert!(!a.is_empty());
    let keys = |n: &[[i64; 4]]| {
        let mut k: Vec<i64> = n.iter().map(|r| r[2]).collect();
        k.dedup();
        k
    };
    // one long note per key inside
    assert_eq!(b.len(), keys(&b).len());
    assert!(b.iter().map(|r| r[1] - r[0]).sum::<i64>() > 0);
    let sq = edit::square_shape([0.0, 60.0], [1.0, 64.0], &base).unwrap();
    assert!(!notes(&sq).is_empty());
    for style in ["polygon", "star", "cross"] {
        let d = json::parse(&format!(r#"{{"points":7,"style":"{style}","skip":3}}"#)).unwrap();
        let p = edit::polygon_shape([0.0, 50.0], [2.0, 60.0], &d, &base).unwrap();
        assert!(!notes(&p).is_empty(), "{style}");
    }
}

#[test]
fn an_hz_bass_box_spams_at_one_wave_of_its_tone() {
    let base = Value::obj();
    let h = edit::hz_shape([0.0, 30.0], [1.0, 36.0], 33, 0.0, 120.0, &base).unwrap();
    let gate = sw::getf(&h, "gate", 0.0);
    assert!((gate - plumspirit_engine::spiderweb::hzbass::hz_gate(33.0, 0.0, 120.0)).abs() < 1e-12);
    let n = notes(&h);
    // 55 Hz at 120 BPM: one wave = 1/27.5 beat → about 27 notes per key per beat
    let per_key = n.iter().filter(|r| r[2] == 33).count();
    assert!((25..=29).contains(&per_key), "{per_key}");
}

#[test]
fn notes_taken_from_the_project_come_back_exactly() {
    let taken = [
        [96, 48, 60, 100, 0],
        [144, 96, 64, 80, 1],
        [192, 24, 67, 127, 2],
        [96, 10, 72, 1, 0],
    ];
    let sh = edit::notes_shape(&taken, PPQ, "Notes").unwrap();
    let (r, slots) = render(&[sh], ChannelMode::Auto);
    let mut got: Vec<[i64; 4]> = r.iter().map(|n| [n[0], n[1] - n[0], n[2], n[3]]).collect();
    got.sort_unstable();
    let mut want: Vec<[i64; 4]> = taken.iter().map(|n| [n[0], n[1], n[2], n[3]]).collect();
    want.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(slots, 3, "the three colours stay apart");
}

#[test]
fn a_live_shape_gives_its_shapes_back_moved_with_it() {
    let a = shape(r#"{"kind":"line","pts":[[0,60],[1,64]]}"#);
    let b = shape(r#"{"kind":"curve","pts":[[1,64],[1.5,64],[1.5,70],[2,70]]}"#);
    let live = edit::to_live(&[a.clone(), b.clone()]).unwrap();
    assert_eq!(sw::kind(&live), "custom");
    assert!(!notes(&live).is_empty());
    let back = edit::originals(&live).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(notes(&back[0]), notes(&a));
    assert_eq!(notes(&back[1]), notes(&b));
    let moved = edit::translate_shape(&live, [2.0, 3.0]).unwrap();
    let back = edit::split(&moved).unwrap();
    let a2 = edit::translate_shape(&a, [2.0, 3.0]).unwrap();
    assert_eq!(notes(&back[0]), notes(&a2));
    // a stretched live shape splits into one shape per stroke
    let wide = edit::transform_shapes(
        &[live],
        &Xform {
            sx: 2.0,
            ..Xform::default()
        },
        Some([0.0, 60.0]),
        0.25,
    )
    .unwrap();
    assert!(edit::originals(&wide[0]).is_none());
    let parts = edit::split(&wide[0]).unwrap();
    assert_eq!(parts.len(), 2);
    let (lo, hi) = edit::bounds(&parts).unwrap();
    assert!(
        (lo[0] - 0.0).abs() < 1e-9 && (hi[0] - 4.0).abs() < 1e-9,
        "{lo:?} {hi:?}"
    );
}

#[test]
fn the_live_shape_of_lines_can_be_filled() {
    let tri = shape(r#"{"kind":"poly","pts":[[0,60],[2,70],[4,60],[0,60]]}"#);
    let mut live = edit::to_live(&[tri]).unwrap();
    live.set("fill", Value::Str("fill".into()));
    let live = sw::clean_shape(&live).unwrap();
    let n = notes(&live);
    assert!(n.len() >= 10, "{}", n.len());
}

#[test]
fn a_fixed_colour_overrides_the_channel_mode() {
    let mut a = shape(r#"{"kind":"line","pts":[[0,60],[1,64]]}"#);
    let b = shape(r#"{"kind":"line","pts":[[0,70],[1,74]]}"#);
    edit::set_fixed_colour(&mut a, Some(5));
    let a = sw::clean_shape(&a).unwrap();
    assert_eq!(edit::fixed_colour(&a), Some(5));
    for mode in [ChannelMode::Raw, ChannelMode::Single, ChannelMode::Auto] {
        let (r, slots) = render(&[a.clone(), b.clone()], mode);
        assert_eq!(slots, 6);
        assert!(r.iter().filter(|n| n[5] == 0).all(|n| n[4] == 5));
        assert!(r.iter().filter(|n| n[5] == 1).all(|n| n[4] != 5));
    }
    let mut c = a.clone();
    edit::set_fixed_colour(&mut c, None);
    assert_eq!(edit::fixed_colour(&c), None);
    assert!(!c.has(edit::PS));
}

/// A test font: every letter is a box 0.5 em wide, cap height 0.7.
struct Boxes;
impl FontSource for Boxes {
    fn glyph(&self, ch: char) -> (f64, Vec<Vec<P>>) {
        if ch == ' ' {
            return (0.3, Vec::new());
        }
        let r = [[0.0, 0.0], [0.5, 0.0], [0.5, 0.7], [0.0, 0.7], [0.0, 0.0]];
        let mut c = vec![r[0]];
        for w in r.windows(2) {
            c.push(w[0]);
            c.push(w[1]);
            c.push(w[1]);
        }
        (0.6, vec![c])
    }
    fn kerning(&self, _: char, _: char) -> f64 {
        0.0
    }
    fn line_height(&self) -> f64 {
        1.2
    }
    fn cap(&self) -> f64 {
        0.7
    }
}

fn tx(extra: &str) -> Value {
    json::parse(&format!(
        r#"{{"text":"ab\nc","font":"Boxes","size":10,"bbox":[0,0,1,1]{extra}}}"#
    ))
    .unwrap()
}

#[test]
fn text_styles_survive_cleaning_and_shape_the_layout() {
    let plain = text::layout(&text::clean_text(&tx("")).unwrap(), &Boxes);
    assert_eq!(plain.len(), 3);
    let t = text::clean_text(&tx(r#","underline":true,"strike":true,"caps":"upper""#)).unwrap();
    assert_eq!(t.get("underline"), Some(&Value::Bool(true)));
    assert_eq!(t.get("caps").and_then(Value::as_str), Some("upper"));
    // two lines × (underline + strikeout) more contours
    assert_eq!(text::layout(&t, &Boxes).len(), 3 + 4);
    assert_eq!(text::shown_text(&t), "AB\nC");
    let title = text::clean_text(
        &json::parse(r#"{"text":"hello wORLD it's","caps":"title","bbox":[0,0,1,1]}"#).unwrap(),
    )
    .unwrap();
    assert_eq!(text::shown_text(&title), "Hello World It's");
    let none = text::clean_text(&tx(r#","caps":"none","underline":false"#)).unwrap();
    assert!(!none.has("caps") && !none.has("underline"));
}

#[test]
fn a_built_text_shape_makes_notes_without_the_font() {
    let mut sh = edit::custom_shape(
        edit::box_frame([0.0, 60.0], [1.0, 61.0]),
        vec![edit::ellipse_stroke()],
        "Text",
        &Value::obj(),
    );
    let t = text::clean_text(&tx(r#","underline":true"#)).unwrap();
    assert!(text::build(
        &mut sh,
        &t,
        ([0.0, 60.0], [0.5, 0.0], [0.0, 10.0]),
        &Boxes
    ));
    sh.set("fill", Value::Str("fill".into()));
    let sh = sw::clean_shape(&sh).unwrap();
    assert!(sh.get("text").unwrap().get("underline").is_some());
    assert!(!notes(&sh).is_empty());
}
