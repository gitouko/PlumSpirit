//! B1-0063: one numeric box with a stepper, the same everywhere.
//!
//! The value field comes first and a paired **[−][+]** follows it on its
//! right (the community request: minus and plus on one consistent side of
//! the box, as in Andromeda and Domino). The buttons and the keyboard do the
//! same thing: one *step* per click or Up / Down press, ten with Shift. A
//! step is 1 for whole numbers and the drag speed for decimals unless the box
//! sets its own (`step`). (egui's own arrow keys step by the drag speed, which
//! rounds back to the same value on an integer box with its 0.25 default, and
//! jumps by hundreds on boxes with a fast drag.)
//!
//! Typing keeps working as before (thousands spaces, the − sign) and also
//! takes arithmetic: `960*4`, `2^20`, `(127+1)/2` (the safe expression
//! language of `editor::expr`, no variables). Typed values are checked against
//! the semantic range; integer fields reject fractions instead of rounding.
//!
//! [`Stepper`] is a drop-in for `egui::DragValue` (same builder names), so a
//! box changes by its constructor only. [`buttons`] is the bare pair for the
//! custom text boxes (the toolbar's Zoom X / Y).

use egui::{Key, Modifiers, Response, Ui, Vec2, Widget};
use std::ops::RangeInclusive;

/// Width of each stepper button (the height is the row's interact height).
pub const BUTTON_WIDTH: f32 = 18.0;
/// Space between the field and the pair, and between the two buttons.
pub const GAP: f32 = 2.0;
/// Steps per click / key press with Shift held.
pub const SHIFT_STEPS: f64 = 10.0;

enum NumberEdit<'a> {
    Read,
    Float(f64),
    Text(&'a str),
}
struct Number {
    value: f64,
    text: String,
    changed: bool,
}
type GetSet<'a> = Box<dyn 'a + FnMut(NumberEdit<'_>) -> Result<Number, String>>;
type Formatter<'a> = Box<dyn 'a + Fn(f64, RangeInclusive<usize>) -> String>;

/// Which button of a pair was used (with Shift: [`SHIFT_STEPS`] steps).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Steps(pub f64);

/// The paired [−][+] after a custom box: returns the signed number of steps
/// (−1 / +1, ±10 with Shift) when one was clicked.
pub fn buttons(ui: &mut Ui, enabled: bool, what: &str) -> Option<Steps> {
    buttons_with(
        ui,
        enabled,
        &format!("Decrease {what} (Shift: ×10; also Down)"),
        &format!("Increase {what} (Shift: ×10; also Up)"),
    )
}

/// [`buttons`] with the tooltips given (for a pair that runs commands).
pub fn buttons_with(ui: &mut Ui, enabled: bool, dec_tip: &str, inc_tip: &str) -> Option<Steps> {
    let h = ui.spacing().interact_size.y;
    let size = Vec2::new(BUTTON_WIDTH, h);
    let shift = ui.input(|i| i.modifiers.shift);
    let mul = if shift { SHIFT_STEPS } else { 1.0 };
    let mut out = None;
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = GAP;
    if ui
        .add_enabled(enabled, egui::Button::new("−").min_size(size))
        .on_hover_text(dec_tip)
        .clicked()
    {
        out = Some(Steps(-mul));
    }
    if ui
        .add_enabled(enabled, egui::Button::new("+").min_size(size))
        .on_hover_text(inc_tip)
        .clicked()
    {
        out = Some(Steps(mul));
    }
    ui.spacing_mut().item_spacing.x = spacing;
    out
}

/// Plain numbers (whitespace and the − sign allowed, like egui) or an
/// arithmetic expression; `None` when neither gives a finite number.
#[cfg(test)]
pub fn parse(text: &str) -> Option<f64> {
    super::numeric::float(text).ok()
}

/// `value + steps × step`, clamped to `range`, rounded for integers.
pub fn stepped(
    value: f64,
    steps: f64,
    step: f64,
    range: &RangeInclusive<f64>,
    integral: bool,
) -> f64 {
    let mut v = value + steps * step;
    if integral {
        v = v.round();
    } else {
        // Keep the step's decimals clean (0.1 + 0.2 shows 0.3).
        v = super::numeric::add_decimal(value, steps * step);
    }
    v.clamp(*range.start(), *range.end())
}

/// A round step for a decimal range: a power of ten near a hundredth of it
/// (0..1 → 0.01, 0..360 → 1, -1000..1000 → 10).
pub fn nice_step(min: f64, max: f64) -> f64 {
    let span = (max - min).abs();
    if !span.is_finite() || span <= 0.0 {
        return 1.0;
    }
    10f64.powf((span / 100.0).log10().floor())
}

/// A practical pointer slider beside the shared checked numeric entry. The
/// semantic domain is shared, while typing keeps the native representation.
pub fn slider<Num: egui::emath::Numeric + std::fmt::Display + std::str::FromStr>(
    ui: &mut Ui,
    value: &mut Num,
    range: RangeInclusive<Num>,
    unit: &str,
) -> Response {
    ui.horizontal(|ui| {
        let step = if Num::INTEGRAL {
            1.0
        } else {
            nice_step(range.start().to_f64(), range.end().to_f64())
        };
        let pointer = ui.add(
            egui::Slider::new(value, range.clone())
                .show_value(false)
                .clamping(egui::SliderClamping::Edits),
        );
        let typed = ui.add(Stepper::new(value).range(range).speed(step).suffix(
            if unit.is_empty() {
                String::new()
            } else {
                format!(" {unit}")
            },
        ));
        pointer | typed
    })
    .inner
}

/// A numeric box with the stepper pair (see the module docs).
pub struct Stepper<'a> {
    get_set: GetSet<'a>,
    integral: bool,
    integer_range: Option<(String, String)>,
    range: RangeInclusive<f64>,
    speed: Option<f64>,
    step: Option<f64>,
    prefix: Option<String>,
    suffix: Option<String>,
    min_decimals: Option<usize>,
    max_decimals: Option<usize>,
    formatter: Option<Formatter<'a>>,
    what: String,
    representation: String,
    /// B2-F20: the sentence for a number outside the range (instead of
    /// "Enter a number from … to …", which is unreadable for a domain such as
    /// "any positive number").
    range_sentence: Option<String>,
}

impl<'a> Stepper<'a> {
    pub fn new<Num: egui::emath::Numeric + std::fmt::Display + std::str::FromStr>(
        value: &'a mut Num,
    ) -> Self {
        Stepper {
            get_set: Box::new(move |edit| {
                let old = value.to_f64();
                let old_text = value.to_string();
                match edit {
                    NumberEdit::Read => {}
                    NumberEdit::Float(v) => {
                        let next = Num::from_f64(v);
                        if !next.to_f64().is_finite() || (v != 0.0 && next.to_f64() == 0.0) {
                            return Err("The number is outside this field's representation.".into());
                        }
                        *value = next;
                    }
                    NumberEdit::Text(s) => {
                        *value = s.parse::<Num>().map_err(|_| {
                            "The whole number is outside this field's representation.".to_string()
                        })?;
                    }
                }
                let text = value.to_string();
                let raw = value.to_f64();
                let changed = text != old_text || raw.to_bits() != old.to_bits();
                Ok(Number {
                    value: raw,
                    text: if Num::INTEGRAL {
                        text
                    } else {
                        super::numeric::display_float_text(text)
                    },
                    changed,
                })
            }),
            integer_range: Num::INTEGRAL.then(|| (Num::MIN.to_string(), Num::MAX.to_string())),
            integral: Num::INTEGRAL,
            range: Num::MIN.to_f64()..=Num::MAX.to_f64(),
            speed: None,
            step: None,
            prefix: None,
            suffix: None,
            min_decimals: None,
            max_decimals: None,
            formatter: None,
            what: "the value".into(),
            range_sentence: None,
            representation: if Num::INTEGRAL {
                "Whole numbers; plain digits and decimal exponents preserve the integer type exactly. Arithmetic expressions use f64.".into()
            } else {
                format!(
                    "Stored as a {}-bit floating-point value. Full accepted precision is shown; additional digits round to the representation.",
                    std::mem::size_of::<Num>() * 8
                )
            },
        }
    }

    pub fn range<Num: egui::emath::Numeric + std::fmt::Display>(
        mut self,
        range: RangeInclusive<Num>,
    ) -> Self {
        if self.integral {
            self.integer_range = Some(if Num::INTEGRAL {
                (range.start().to_string(), range.end().to_string())
            } else {
                (
                    range.start().to_f64().ceil().to_string(),
                    range.end().to_f64().floor().to_string(),
                )
            });
        }
        self.range = range.start().to_f64()..=range.end().to_f64();
        self
    }

    /// The sentence shown when a typed number is outside the range.
    pub fn range_sentence(mut self, sentence: impl ToString) -> Self {
        self.range_sentence = Some(sentence.to_string());
        self
    }

    /// Drag sensitivity (points → value), and the step unless [`Self::step`].
    pub fn speed(mut self, speed: impl Into<f64>) -> Self {
        self.speed = Some(speed.into());
        self
    }

    /// The change of one click / Up / Down press.
    pub fn step(mut self, step: impl Into<f64>) -> Self {
        self.step = Some(step.into());
        self
    }

    pub fn prefix(mut self, prefix: impl ToString) -> Self {
        self.prefix = Some(prefix.to_string());
        self
    }

    pub fn suffix(mut self, suffix: impl ToString) -> Self {
        self.suffix = Some(suffix.to_string());
        self
    }

    pub fn min_decimals(mut self, n: usize) -> Self {
        self.min_decimals = Some(n);
        self
    }

    pub fn max_decimals(mut self, n: usize) -> Self {
        self.max_decimals = Some(n);
        self
    }

    pub fn custom_formatter(
        mut self,
        f: impl 'a + Fn(f64, RangeInclusive<usize>) -> String,
    ) -> Self {
        self.formatter = Some(Box::new(f));
        self
    }

    /// What the buttons' tooltips name ("the value" by default).
    pub fn what(mut self, what: impl ToString) -> Self {
        self.what = what.to_string();
        self
    }

    /// The change of one step.
    pub fn step_size(&self) -> f64 {
        let s = match (self.step, self.integral) {
            (Some(s), _) => s,
            (None, true) => 1.0,
            (None, false) => self.speed.unwrap_or(1.0),
        }
        .abs();
        let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
        if self.integral { s.round().max(1.0) } else { s }
    }
}

impl Widget for Stepper<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let step = self.step_size();
        let (range, integral) = (self.range.clone(), self.integral);
        let range_sentence = self.range_sentence.clone();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            use std::cell::{Cell, RefCell};
            let get_set = RefCell::new(self.get_set);
            let snapshot = get_set.borrow_mut()(NumberEdit::Read).unwrap();
            let old = snapshot.value;
            let mut v = old;
            let old_text = snapshot.text;
            let exact_changed = Cell::new(false);
            let field_id = ui.next_auto_id();
            let error =
                RefCell::new(ui.data(|d| d.get_temp::<String>(field_id.with("number-error"))));
            let exact_range = self.integer_range.clone();

            // The field's id is the next one: when it has the keyboard, Up /
            // Down (and with Shift) step here, before egui's own handling.
            let keyed = ui.is_enabled() && ui.memory(|m| m.has_focus(field_id));
            if keyed {
                let n = ui.input_mut(|i| {
                    // Shift first: a pattern without Shift also matches it.
                    let up10 = i.count_and_consume_key(Modifiers::SHIFT, Key::ArrowUp) as f64
                        - i.count_and_consume_key(Modifiers::SHIFT, Key::ArrowDown) as f64;
                    let up = i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as f64
                        - i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as f64;
                    up + up10 * SHIFT_STEPS
                });
                if n != 0.0 {
                    if let Some((lo, hi)) = &exact_range {
                        match super::numeric::add_integer(
                            &old_text,
                            (n * step).round() as i128,
                            lo,
                            hi,
                        )
                        .and_then(|s| get_set.borrow_mut()(NumberEdit::Text(&s)))
                        {
                            Ok(next) => {
                                v = next.value;
                                exact_changed.set(next.changed);
                                *error.borrow_mut() = None;
                            }
                            Err(e) => *error.borrow_mut() = Some(e),
                        }
                    } else {
                        v = stepped(v, n, step, &range, false);
                    }
                }
            }
            let mut drag = egui::DragValue::new(&mut v)
                .range(range.clone())
                .clamp_existing_to_range(false)
                .speed(self.speed.unwrap_or(if integral { 0.25 } else { 1.0 }))
                .custom_parser(|text| {
                    let result = if let Some((lo, hi)) = &exact_range {
                        super::numeric::integer(text).and_then(|s| {
                            if super::numeric::compare_integer(&s, lo).is_lt()
                                || super::numeric::compare_integer(&s, hi).is_gt()
                            {
                                return Err(range_sentence.clone().unwrap_or_else(|| {
                                    format!("Enter a whole number from {lo} to {hi}.")
                                }));
                            }
                            get_set.borrow_mut()(NumberEdit::Text(&s))
                        })
                    } else {
                        super::numeric::float(text).and_then(|v| {
                            if !range.contains(&v) {
                                return Err(range_sentence.clone().unwrap_or_else(|| {
                                    format!(
                                        "Enter a number from {} to {}.",
                                        range.start(),
                                        range.end()
                                    )
                                }));
                            }
                            get_set.borrow_mut()(NumberEdit::Float(v))
                        })
                    };
                    match result {
                        Ok(n) => {
                            exact_changed.set(exact_changed.get() || n.changed);
                            *error.borrow_mut() = None;
                            Some(n.value)
                        }
                        Err(e) => {
                            *error.borrow_mut() = Some(e);
                            None
                        }
                    }
                });
            if integral {
                drag = drag.max_decimals(0);
            }
            if let Some(n) = self.min_decimals {
                drag = drag.min_decimals(n);
            }
            if let Some(n) = self.max_decimals {
                drag = drag.max_decimals(n);
            }
            if let Some(p) = &self.prefix {
                drag = drag.prefix(p.as_str());
            }
            if let Some(s) = &self.suffix {
                drag = drag.suffix(s.as_str());
            }
            if let Some(f) = self.formatter.take() {
                drag = drag.custom_formatter(f);
            } else {
                drag = drag.custom_formatter(|n, _| {
                    if n.to_bits() == old.to_bits() {
                        old_text.clone()
                    } else {
                        n.to_string()
                    }
                });
            }
            let mut r = ui.add(drag).on_hover_text(self.representation);
            if let Some(Steps(n)) = buttons(ui, ui.is_enabled(), &self.what) {
                if let Some((lo, hi)) = &exact_range {
                    let now = get_set.borrow_mut()(NumberEdit::Read).unwrap();
                    match super::numeric::add_integer(&now.text, (n * step).round() as i128, lo, hi)
                        .and_then(|s| get_set.borrow_mut()(NumberEdit::Text(&s)))
                    {
                        Ok(next) => {
                            v = next.value;
                            exact_changed.set(exact_changed.get() || next.changed);
                            *error.borrow_mut() = None;
                        }
                        Err(e) => *error.borrow_mut() = Some(e),
                    }
                } else {
                    v = stepped(v, n, step, &range, false);
                }
            }
            if v.to_bits() != old.to_bits() && !exact_changed.get() {
                match get_set.borrow_mut()(NumberEdit::Float(v)) {
                    Ok(n) => exact_changed.set(n.changed),
                    Err(e) => *error.borrow_mut() = Some(e),
                }
            }
            r.flags
                .set(egui::response::Flags::CHANGED, exact_changed.get());
            if let Some(e) = error.borrow().as_ref() {
                ui.data_mut(|d| d.insert_temp(field_id.with("number-error"), e.clone()));
                ui.label(egui::RichText::new("!").color(ui.visuals().error_fg_color))
                    .on_hover_text(e);
                r = r.on_hover_text(e);
            } else {
                ui.data_mut(|d| d.remove::<String>(field_id.with("number-error")));
            }
            r
        })
        .inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};

    #[test]
    fn typed_numbers_and_expressions_are_read() {
        assert_eq!(parse("1 000 000"), Some(1_000_000.0));
        assert_eq!(parse("−12.5"), Some(-12.5));
        assert_eq!(parse("960*4"), Some(3840.0));
        assert_eq!(parse("2^20"), Some(1_048_576.0));
        assert_eq!(parse("(127+1)/2"), Some(64.0));
        assert_eq!(parse("1e12"), Some(1e12));
        assert_eq!(parse("0.1"), Some(0.1));
        for bad in ["", "abc", "t", "1/0", "sqrt(-1)", "1+"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_step_is_one_for_whole_numbers_and_the_speed_for_decimals() {
        let mut i = 5u8;
        let mut f = 1.0f64;
        assert_eq!(Stepper::new(&mut i).step_size(), 1.0);
        assert_eq!(Stepper::new(&mut i).speed(0.25).step_size(), 1.0);
        assert_eq!(
            Stepper::new(&mut i).speed(10.0).step_size(),
            1.0,
            "drag speed is not the step"
        );
        assert_eq!(Stepper::new(&mut i).step(12).step_size(), 12.0);
        assert_eq!(Stepper::new(&mut f).step_size(), 1.0);
        assert_eq!(Stepper::new(&mut f).speed(0.1).step_size(), 0.1);
        assert_eq!(Stepper::new(&mut f).speed(0.1).step(0.5).step_size(), 0.5);
        assert_eq!(stepped(0.2, 1.0, 0.1, &(0.0..=1.0), false), 0.3);
        assert_eq!(stepped(126.0, 10.0, 1.0, &(1.0..=127.0), true), 127.0);
        assert_eq!(stepped(1.0, -1.0, 1.0, &(1.0..=127.0), true), 1.0);
        assert_eq!(nice_step(0.0, 1.0), 0.01);
        assert_eq!(nice_step(0.0, 360.0), 1.0);
        assert_eq!(nice_step(-1000.0, 1000.0), 10.0);
        assert_eq!(nice_step(5.0, 5.0), 1.0);
        // Large values stay exact where f64 holds them.
        assert_eq!(stepped(1e12, 1.0, 1.0, &(0.0..=f64::MAX), true), 1e12 + 1.0);
    }

    /// Draws `add` in a 600-point panel with `events`; returns the shapes.
    fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        mods: Modifiers,
        add: &mut dyn FnMut(&mut Ui),
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut all = vec![egui::Event::ModifiersChanged(mods)];
        all.extend(events);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 200.0))),
            events: all,
            ..Default::default()
        };
        let mut o = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| add(ui));
        });
        o.textures_delta.clear();
        o.shapes
    }

    fn click(p: Pos2, mods: Modifiers) -> Vec<egui::Event> {
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: mods,
        };
        vec![egui::Event::PointerMoved(p), b(true), b(false)]
    }

    fn type_value(ctx: &egui::Context, pos: Pos2, text: &str, add: &mut dyn FnMut(&mut Ui)) {
        frame(ctx, click(pos, Modifiers::NONE), Modifiers::NONE, add);
        frame(ctx, vec![], Modifiers::NONE, add);
        let mods = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        let key = |key, mods| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        };
        frame(
            ctx,
            vec![key(Key::A, mods), egui::Event::Text(text.into())],
            mods,
            add,
        );
        frame(
            ctx,
            vec![key(Key::Enter, Modifiers::NONE)],
            Modifiers::NONE,
            add,
        );
        frame(ctx, vec![], Modifiers::NONE, add);
    }
    #[test]
    fn typed_precision_and_exact_large_integers_survive_the_real_widget() {
        let ctx = egui::Context::default();
        let mut v = 0.123456789012345f64;
        let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
            ui.add(Stepper::new(&mut v).max_decimals(3).speed(0.01));
        });
        let shown = crate::ui::testkit::texts(&shapes);
        let pos = shown
            .iter()
            .find(|(s, _)| s == "0.123456789012345")
            .unwrap()
            .1;
        type_value(&ctx, pos, "1e-9", &mut |ui| {
            ui.add(Stepper::new(&mut v).max_decimals(3).speed(0.01));
        });
        assert_eq!(v, 1e-9);
        let ctx = egui::Context::default();
        let mut n = u64::MAX - 1;
        let mut changed = false;
        let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
            changed |= ui.add(Stepper::new(&mut n)).changed();
        });
        let shown = crate::ui::testkit::texts(&shapes);
        let pos = shown
            .iter()
            .find(|(s, _)| s == &(u64::MAX - 1).to_string())
            .unwrap()
            .1;
        type_value(&ctx, pos, &u64::MAX.to_string(), &mut |ui| {
            changed |= ui.add(Stepper::new(&mut n)).changed();
        });
        assert_eq!(n, u64::MAX);
        assert!(changed);
        let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
            ui.add(Stepper::new(&mut n));
        });
        let minus = crate::ui::testkit::texts(&shapes)
            .iter()
            .find(|(s, _)| s == "−")
            .unwrap()
            .1;
        frame(
            &ctx,
            click(minus, Modifiers::NONE),
            Modifiers::NONE,
            &mut |ui| {
                ui.add(Stepper::new(&mut n));
            },
        );
        assert_eq!(n, u64::MAX - 1);
    }
    #[test]
    fn slider_keeps_precise_values_until_explicitly_edited() {
        let ctx = egui::Context::default();
        for expected in [0.123456789012345, 1e-9, 0.999999999999999] {
            let mut value = expected;
            frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
                assert!(!slider(ui, &mut value, 0.0..=1.0, "").changed());
            });
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn invalid_typed_values_do_not_change_the_value_or_report_a_numeric_edit() {
        for text in ["1.5", "NaN", "101", "1e400"] {
            let ctx = egui::Context::default();
            let mut n = 12i64;
            let mut changed = false;
            let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
                changed |= ui.add(Stepper::new(&mut n).range(0..=100)).changed();
            });
            let pos = crate::ui::testkit::texts(&shapes)
                .iter()
                .find(|(s, _)| s == "12")
                .unwrap()
                .1;
            type_value(&ctx, pos, text, &mut |ui| {
                changed |= ui.add(Stepper::new(&mut n).range(0..=100)).changed();
            });
            assert_eq!(n, 12, "{text}");
            assert!(!changed, "{text}");
        }
    }

    /// B2-F20: a "positive number" field (the MIDI Player's falling time)
    /// keeps any finite positive value typed — a microsecond, ten hours,
    /// 1e300 — and answers 0 or a negative with its own sentence, the value
    /// unchanged and no edit reported.
    #[test]
    fn a_positive_field_takes_any_positive_number_and_says_why_it_refuses_zero() {
        const SENTENCE: &str = "The falling time must be a positive number of seconds (above 0).";
        fn field(v: &mut f64) -> Stepper<'_> {
            Stepper::new(v)
                .range(f64::MIN_POSITIVE..=f64::MAX)
                .range_sentence(SENTENCE)
                .min_decimals(2)
                .suffix(" s")
        }
        for (text, want) in [("1e-6", 1e-6), ("36000", 36_000.0), ("1e300", 1e300)] {
            let ctx = egui::Context::default();
            let mut v = 2.0f64;
            let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
                ui.add(field(&mut v));
            });
            let all = crate::ui::testkit::texts(&shapes);
            let pos = all
                .iter()
                .find(|(s, _)| s == "2")
                .unwrap_or_else(|| panic!("{all:?}"))
                .1;
            type_value(&ctx, pos, text, &mut |ui| {
                ui.add(field(&mut v));
            });
            assert_eq!(v, want, "{text}");
        }
        for text in ["0", "-3", "0.0"] {
            let ctx = egui::Context::default();
            ctx.global_style_mut(|s| {
                s.interaction.tooltip_delay = 0.0;
                s.interaction.show_tooltips_only_when_still = false;
            });
            let mut v = 2.0f64;
            let mut changed = false;
            let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
                changed |= ui.add(field(&mut v)).changed();
            });
            let pos = crate::ui::testkit::texts(&shapes)
                .iter()
                .find(|(s, _)| s == "2")
                .unwrap()
                .1;
            type_value(&ctx, pos, text, &mut |ui| {
                changed |= ui.add(field(&mut v)).changed();
            });
            assert_eq!(v, 2.0, "{text}");
            assert!(!changed, "{text}");
            let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
                ui.add(field(&mut v));
            });
            let bang = crate::ui::testkit::texts(&shapes)
                .iter()
                .find(|(s, _)| s == "!")
                .map(|t| t.1)
                .unwrap_or_else(|| panic!("{text}: the field shows that it refused"));
            let mut tip = Vec::new();
            for _ in 0..3 {
                let shapes = frame(
                    &ctx,
                    vec![egui::Event::PointerMoved(bang)],
                    Modifiers::NONE,
                    &mut |ui| {
                        ui.add(field(&mut v));
                    },
                );
                tip = crate::ui::testkit::texts(&shapes);
            }
            assert!(tip.iter().any(|(s, _)| s == SENTENCE), "{text}: {tip:?}");
        }
    }

    /// Minus is drawn before plus, right of the field; a click steps by one
    /// (ten with Shift), within the range; the response reports the change.
    #[test]
    fn minus_then_plus_follow_the_field_and_step_the_value() {
        let ctx = egui::Context::default();
        let mut v = 100u32;
        let mut changed = false;
        let add = |ui: &mut Ui, v: &mut u32, changed: &mut bool| {
            *changed |= ui.add(Stepper::new(v).range(0..=105)).changed();
        };
        let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
            add(ui, &mut v, &mut changed)
        });
        let shown = crate::ui::testkit::texts(&shapes);
        let at = |t: &str| shown.iter().find(|s| s.0 == t).map(|s| s.1).unwrap();
        let (field, minus, plus) = (at("100"), at("−"), at("+"));
        assert!(
            field.x < minus.x && minus.x < plus.x,
            "{field:?} {minus:?} {plus:?}"
        );
        frame(
            &ctx,
            click(plus, Modifiers::NONE),
            Modifiers::NONE,
            &mut |ui| add(ui, &mut v, &mut changed),
        );
        assert_eq!(v, 101);
        assert!(changed);
        frame(
            &ctx,
            click(plus, Modifiers::SHIFT),
            Modifiers::SHIFT,
            &mut |ui| add(ui, &mut v, &mut changed),
        );
        assert_eq!(v, 105, "Shift: ten steps, clamped to the range");
        frame(
            &ctx,
            click(minus, Modifiers::SHIFT),
            Modifiers::SHIFT,
            &mut |ui| add(ui, &mut v, &mut changed),
        );
        assert_eq!(v, 95);
        frame(
            &ctx,
            click(minus, Modifiers::NONE),
            Modifiers::NONE,
            &mut |ui| add(ui, &mut v, &mut changed),
        );
        assert_eq!(v, 94);
    }

    /// With the field focused, Up / Down step like the buttons (an integer
    /// box too) and Shift+Up / Down take ten steps.
    #[test]
    fn up_and_down_step_a_focused_field_like_the_buttons() {
        let ctx = egui::Context::default();
        let mut v = 10i64;
        let mut f = 0.5f64;
        let add = |ui: &mut Ui, v: &mut i64, f: &mut f64| {
            ui.add(Stepper::new(v).range(-1_000..=1_000));
            ui.add(Stepper::new(f).speed(0.1).range(0.0..=1.0));
        };
        let shapes = frame(&ctx, vec![], Modifiers::NONE, &mut |ui| {
            add(ui, &mut v, &mut f)
        });
        let shown = crate::ui::testkit::texts(&shapes);
        let field = shown.iter().find(|s| s.0 == "10").unwrap().1;
        // Click the field: it goes into text editing with the keyboard focus.
        frame(
            &ctx,
            click(field, Modifiers::NONE),
            Modifiers::NONE,
            &mut |ui| add(ui, &mut v, &mut f),
        );
        let key = |k: Key, m: Modifiers| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        frame(
            &ctx,
            vec![key(Key::ArrowUp, Modifiers::NONE)],
            Modifiers::NONE,
            &mut |ui| add(ui, &mut v, &mut f),
        );
        assert_eq!(v, 11);
        frame(
            &ctx,
            vec![key(Key::ArrowDown, Modifiers::SHIFT)],
            Modifiers::SHIFT,
            &mut |ui| add(ui, &mut v, &mut f),
        );
        assert_eq!(v, 1);
        assert_eq!(f, 0.5, "only the focused box steps");
    }
}
