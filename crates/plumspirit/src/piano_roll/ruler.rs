//! The ruler's time gestures and spans, shared by the Piano Roll and the
//! Track List (v0.9 §220: one transport — both rulers seek, select time and
//! set the loop the same way, and draw the loop and the time selection the
//! same way).
//!
//! A press seeks on release (a click); moving [`CLICK_SLOP`] points or more
//! turns it into a time selection; Shift+press drags a loop range. The caller
//! snaps the ticks it passes in (the roll's Snap).

use super::RollAction;
use crate::ui::theme::Palette;
use egui::{Painter, Pos2, Rect, Stroke};
use plumspirit_engine::view::TimeAxis;

/// Pointer movement (points) below which a press on the ruler is a click.
pub const CLICK_SLOP: f32 = super::CLICK_SLOP;

/// A gesture that started on a ruler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RulerDrag {
    /// Pressed: a click moves the edit position to `anchor`; a drag becomes a
    /// time selection.
    Press { press_x: f32, anchor: u64 },
    /// Drag (v0.8): the time selection `[min, max)`.
    Range { anchor: u64, now: u64 },
    /// Shift+drag: a loop range.
    Loop { anchor: u64, now: u64 },
}

impl RulerDrag {
    /// A press at screen x `x` on (snapped) tick `tick`.
    pub fn press(tick: u64, x: f32, shift: bool) -> RulerDrag {
        if shift {
            RulerDrag::Loop {
                anchor: tick,
                now: tick,
            }
        } else {
            RulerDrag::Press {
                press_x: x,
                anchor: tick,
            }
        }
    }

    /// The pointer is now at screen x `x`, (snapped) tick `tick`.
    pub fn drag(&mut self, tick: u64, x: f32) {
        match self {
            RulerDrag::Press { press_x, anchor } => {
                if (x - *press_x).abs() >= CLICK_SLOP {
                    *self = RulerDrag::Range {
                        anchor: *anchor,
                        now: tick,
                    };
                }
            }
            RulerDrag::Range { now, .. } | RulerDrag::Loop { now, .. } => *now = tick,
        }
    }

    /// What the release requests (the app applies them like the roll's).
    pub fn release(self) -> Vec<RollAction> {
        match self {
            RulerDrag::Press { anchor, .. } => vec![RollAction::Seek(anchor)],
            RulerDrag::Range { anchor, now } => {
                let (a, b) = (anchor.min(now), anchor.max(now));
                let mut out = Vec::with_capacity(2);
                if b > a {
                    out.push(RollAction::SetTimeSelection(Some((a, b))));
                }
                out.push(RollAction::Seek(a));
                out
            }
            RulerDrag::Loop { anchor, now } if anchor != now => {
                vec![RollAction::SetLoop(anchor.min(now), anchor.max(now))]
            }
            RulerDrag::Loop { .. } => Vec::new(),
        }
    }

    /// Still a press (no drag yet): the edit position follows the press.
    pub fn is_press(&self) -> bool {
        matches!(self, RulerDrag::Press { .. })
    }

    /// A loop range being dragged (drawn instead of the project's).
    pub fn loop_shown(&self) -> Option<(u64, u64)> {
        match *self {
            RulerDrag::Loop { anchor, now } if anchor != now => {
                Some((anchor.min(now), anchor.max(now)))
            }
            _ => None,
        }
    }

    /// A time selection being dragged (drawn instead of the current one).
    pub fn range_shown(&self) -> Option<(u64, u64)> {
        match *self {
            RulerDrag::Range { anchor, now } if anchor != now => {
                Some((anchor.min(now), anchor.max(now)))
            }
            _ => None,
        }
    }
}

/// What a ruler shows besides its bars: the loop range (on / off) and the
/// time selection, after any drag in progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spans {
    pub loop_range: Option<(u64, u64)>,
    pub loop_on: bool,
    pub range: Option<(u64, u64)>,
}

impl Spans {
    /// The project's spans with `drag` (if any) drawn instead.
    pub fn with_drag(
        loop_range: Option<(u64, u64)>,
        loop_enabled: bool,
        range: Option<(u64, u64)>,
        drag: Option<&RulerDrag>,
    ) -> Spans {
        let dragging_loop = matches!(drag, Some(RulerDrag::Loop { .. }));
        Spans {
            loop_range: drag.and_then(RulerDrag::loop_shown).or(loop_range),
            loop_on: loop_enabled || dragging_loop,
            range: drag.and_then(RulerDrag::range_shown).or(range),
        }
    }
}

fn span_x(left: f32, time: &TimeAxis, (a, b): (u64, u64)) -> (f32, f32) {
    (
        left + time.tick_to_x(a) as f32,
        left + time.tick_to_x(b) as f32,
    )
}

/// The loop band and edges along the ruler's bottom and the time selection's
/// wash over its upper part (`p` clipped to `ruler`).
pub fn draw_spans(p: &Painter, ruler: Rect, time: &TimeAxis, spans: &Spans, pal: &Palette) {
    if let Some(lr) = spans.loop_range {
        let (x0, x1) = span_x(ruler.left(), time, lr);
        let on = spans.loop_on;
        let band = Rect::from_min_max(
            Pos2::new(x0.max(ruler.left() - 2.0), ruler.bottom() - 5.0),
            Pos2::new(x1.min(ruler.right() + 2.0), ruler.bottom()),
        );
        let c = if on { pal.accent } else { pal.text_weak };
        p.rect_filled(band, 0.0, c.gamma_multiply(if on { 0.8 } else { 0.35 }));
        for x in [x0, x1] {
            p.vline(x.round() + 0.5, ruler.y_range(), Stroke::new(1.0, c));
        }
    }
    if let Some(r) = spans.range {
        let (x0, x1) = span_x(ruler.left(), time, r);
        let band = Rect::from_min_max(
            Pos2::new(x0.max(ruler.left() - 2.0), ruler.top()),
            Pos2::new(x1.min(ruler.right() + 2.0), ruler.bottom() - 6.0),
        );
        if band.width() > 0.5 {
            p.rect_filled(band, 0.0, pal.accent.gamma_multiply(0.22));
        }
    }
}

/// The time selection's faint wash and the loop's edge lines over a content
/// area under a ruler (`p` clipped to `area`; `left` is the screen x of the
/// time axis' origin).
pub fn draw_area_spans(
    p: &Painter,
    area: Rect,
    left: f32,
    time: &TimeAxis,
    spans: &Spans,
    pal: &Palette,
) {
    if let Some(r) = spans.range {
        let (x0, x1) = span_x(left, time, r);
        let band = Rect::from_min_max(
            Pos2::new(x0.max(area.left() - 2.0), area.top()),
            Pos2::new(x1.min(area.right() + 2.0), area.bottom()),
        );
        if band.width() > 0.5 {
            p.rect_filled(band, 0.0, pal.accent.gamma_multiply(0.05));
        }
    }
    if let Some(lr) = spans.loop_range {
        let (x0, x1) = span_x(left, time, lr);
        let on = spans.loop_on;
        let c = if on { pal.accent } else { pal.text_weak };
        for x in [x0, x1] {
            p.vline(
                x.round() + 0.5,
                area.y_range(),
                Stroke::new(1.0, c.gamma_multiply(if on { 0.55 } else { 0.3 })),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_seeks_a_drag_selects_time_and_shift_drags_a_loop() {
        let mut d = RulerDrag::press(960, 100.0, false);
        d.drag(970, 100.0 + CLICK_SLOP - 0.5);
        assert!(d.is_press(), "jitter below the slop is still a click");
        assert_eq!(d.release(), vec![RollAction::Seek(960)]);
        // Leftwards: the selection is ordered; the edit position goes to its start.
        let mut d = RulerDrag::press(960, 100.0, false);
        d.drag(480, 40.0);
        assert_eq!(d.range_shown(), Some((480, 960)));
        assert_eq!(
            d.release(),
            vec![
                RollAction::SetTimeSelection(Some((480, 960))),
                RollAction::Seek(480)
            ]
        );
        // Dragged back to the anchor: no selection, a seek.
        let mut d = RulerDrag::press(960, 100.0, false);
        d.drag(1200, 140.0);
        d.drag(960, 101.0);
        assert_eq!(d.release(), vec![RollAction::Seek(960)]);
        // Shift: a loop; an empty one requests nothing.
        let mut d = RulerDrag::press(1920, 300.0, true);
        assert_eq!(d.loop_shown(), None);
        d.drag(480, 10.0);
        assert_eq!(d.loop_shown(), Some((480, 1920)));
        assert_eq!(d.release(), vec![RollAction::SetLoop(480, 1920)]);
        assert!(RulerDrag::press(5, 0.0, true).release().is_empty());
    }

    #[test]
    fn spans_show_the_drag_in_progress_instead_of_the_project() {
        let project = Spans::with_drag(Some((0, 10)), false, Some((3, 4)), None);
        assert_eq!(
            project,
            Spans {
                loop_range: Some((0, 10)),
                loop_on: false,
                range: Some((3, 4))
            }
        );
        let l = RulerDrag::Loop {
            anchor: 50,
            now: 20,
        };
        let s = Spans::with_drag(Some((0, 10)), false, Some((3, 4)), Some(&l));
        assert_eq!(
            (s.loop_range, s.loop_on, s.range),
            (Some((20, 50)), true, Some((3, 4)))
        );
        let r = RulerDrag::Range { anchor: 9, now: 7 };
        let s = Spans::with_drag(None, true, None, Some(&r));
        assert_eq!(
            (s.loop_range, s.loop_on, s.range),
            (None, true, Some((7, 9)))
        );
    }
}
