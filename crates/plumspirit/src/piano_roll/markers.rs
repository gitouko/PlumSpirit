//! The marker band above the ruler: point markers and chapter spans of the
//! Conductor, drawn on the roll's horizontal axis.
//!
//! Drawing is bounded by the width of the view, not by the number of markers:
//! markers are sorted by start, so after laying out one marker the next one
//! looked at is found by binary search a few pixels further right. The markers
//! in between are merged: counted ("+N close together"), except the chapter
//! among them that reaches furthest, which is drawn too — so at most two entries
//! per few pixels are laid out and drawn, and each is looked up by position (no
//! search by id). Chapters that began before the view are found through the
//! Conductor's reach index. Overlapping chapters are stacked on up to
//! [`LEVELS`] thin bars.

use crate::ui::theme::{Palette, size};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use plumspirit_engine::model::Conductor;
use plumspirit_engine::model::conductor::Marker;
use plumspirit_engine::view::TimeAxis;

/// Height of the band in points.
pub const BAND_HEIGHT: f32 = 20.0;
/// Chapter bars stacked for overlapping chapters.
const LEVELS: usize = 3;
/// Point markers closer than this (points) are drawn as one.
const MIN_GAP_PX: f32 = 3.0;
/// Pick tolerance around markers and chapter edges.
const PICK_PX: f32 = 5.0;
/// Most chapters that began before the view that are drawn.
const MAX_SPANNING: usize = 64;

/// What part of a marker the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerPart {
    /// A point marker (moves as a whole).
    Point,
    /// A chapter's start edge.
    Start,
    /// A chapter's end edge.
    End,
    /// Inside a chapter (moves both ends).
    Body,
}

#[derive(Clone, Copy, Debug)]
struct Placed {
    id: u32,
    /// Position in `Conductor::markers()` (valid for the frame's Conductor).
    index: usize,
    x0: f32,
    x1: f32,
    chapter: bool,
    level: usize,
    label: Option<Rect>,
}

/// The markers laid out for one frame (for drawing and hit testing).
#[derive(Default)]
pub struct BandLayout {
    placed: Vec<Placed>,
    /// Point markers merged into a neighbour because they were too close.
    pub hidden: usize,
}

impl BandLayout {
    /// Labels are measured while drawing, after this frame's hit tests ran, so
    /// the hit tests use the previous frame's label rectangles for markers that
    /// are laid out exactly as before (the usual case: the view did not move).
    /// O(markers laid out).
    pub fn inherit_labels(&mut self, previous: &BandLayout) {
        if self.placed.len() != previous.placed.len() {
            return;
        }
        for (it, old) in self.placed.iter_mut().zip(&previous.placed) {
            if it.id == old.id && it.x0 == old.x0 && it.x1 == old.x1 {
                it.label = old.label;
            }
        }
    }
}

/// A drag preview: this marker is drawn at `start..end` instead.
#[derive(Clone, Copy, Debug)]
pub struct Preview {
    pub id: u32,
    pub start: u64,
    pub end: Option<u64>,
}

fn x_of(time: &TimeAxis, band: Rect, tick: u64) -> f32 {
    band.left() + time.tick_to_x(tick) as f32
}

/// Lays out the markers visible in `band`.
pub fn layout(
    conductor: &Conductor,
    time: &TimeAxis,
    band: Rect,
    preview: Option<Preview>,
) -> BandLayout {
    let mut out = BandLayout::default();
    let (t0, t1) = time.visible_range(f64::from(band.width()));
    let markers = conductor.markers();
    let span_of = |m: &Marker| -> (u64, Option<u64>) {
        match preview {
            Some(p) if p.id == m.id => (p.start, p.end),
            _ => (m.start, m.end),
        }
    };
    let mut level_end = [f32::NEG_INFINITY; LEVELS];
    let mut place = |out: &mut BandLayout, index: usize| {
        let m = &markers[index];
        let (s, e) = span_of(m);
        let x0 = x_of(time, band, s);
        match e {
            Some(e) => {
                let x1 = x_of(time, band, e).max(x0 + 1.0);
                let level = (0..LEVELS)
                    .find(|&l| level_end[l] < x0 - 2.0)
                    .unwrap_or(LEVELS - 1);
                level_end[level] = level_end[level].max(x1);
                out.placed.push(Placed {
                    id: m.id,
                    index,
                    x0,
                    x1,
                    chapter: true,
                    level,
                    label: None,
                });
            }
            None => out.placed.push(Placed {
                id: m.id,
                index,
                x0,
                x1: x0,
                chapter: false,
                level: 0,
                label: None,
            }),
        }
    };
    // Chapters that began before the view and reach into it (at most
    // MAX_SPANNING of them are drawn).
    let first_inside = markers.partition_point(|m| m.start < t0);
    for (index, _) in conductor
        .markers_in_range_indexed(t0, t1)
        .take_while(|(_, m)| m.start < t0)
        .take(MAX_SPANNING)
    {
        place(&mut out, index);
    }
    // Markers starting inside the view, skipping ahead by a few pixels each time.
    let mut i = first_inside;
    let end = markers.partition_point(|m| m.start <= t1);
    let gap_ticks = (f64::from(MIN_GAP_PX) / time.pixels_per_tick).max(1.0) as u64;
    while i < end {
        let m = &markers[i];
        place(&mut out, i);
        let next_tick = m.start.saturating_add(gap_ticks);
        let j = markers
            .partition_point(|x| x.start < next_tick)
            .clamp(i + 1, end);
        // Of the markers merged into this one, the chapter reaching furthest is
        // drawn as well (a long chapter is not hidden by a short neighbour).
        let reach = m.end.unwrap_or(m.start);
        let longest = (i + 1..j)
            .filter(|&k| markers[k].end.is_some_and(|e| e > reach))
            .max_by_key(|&k| markers[k].end);
        out.hidden += (j - i - 1) - usize::from(longest.is_some());
        if let Some(k) = longest {
            place(&mut out, k);
        }
        i = j;
    }
    // A marker being dragged is always shown, wherever it came from.
    if let Some(p) = preview
        && !out.placed.iter().any(|x| x.id == p.id)
        && let Some((index, _)) = conductor.marker_by_id(p.id)
    {
        place(&mut out, index);
    }
    out
}

/// Draws the band. `selected` is highlighted.
pub fn draw(
    p: &Painter,
    band: Rect,
    conductor: &Conductor,
    layout: &mut BandLayout,
    pal: &Palette,
    selected: Option<u32>,
    hovered: Option<u32>,
) {
    p.rect_filled(band, 0.0, pal.panel_bg);
    p.hline(
        band.x_range(),
        band.bottom() - 0.5,
        Stroke::new(1.0, pal.panel_border),
    );
    let font = FontId::proportional(size::SMALL);
    let bar_top = band.top() + 14.0;
    let mut label_right = f32::NEG_INFINITY;
    // Chapters first (bars under the point markers), then point markers.
    let mut order: Vec<usize> = (0..layout.placed.len()).collect();
    order.sort_by(|&a, &b| {
        let (pa, pb) = (&layout.placed[a], &layout.placed[b]);
        pb.chapter.cmp(&pa.chapter).then(pa.x0.total_cmp(&pb.x0))
    });
    let markers = conductor.markers();
    for k in order {
        let it = layout.placed[k];
        let Some(m) = markers.get(it.index).filter(|m| m.id == it.id) else {
            continue;
        };
        let active = selected == Some(it.id);
        let hot = hovered == Some(it.id);
        let color = if active {
            pal.accent
        } else if hot {
            pal.text
        } else if it.chapter {
            pal.ruler_text
        } else {
            pal.text_weak
        };
        if it.chapter {
            let y = bar_top + it.level as f32 * 2.0;
            let x0 = it.x0.max(band.left() - 2.0);
            let x1 = it.x1.min(band.right() + 2.0);
            let fill = if active {
                pal.accent
            } else {
                color.gamma_multiply(0.7)
            };
            p.rect_filled(
                Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + 2.0)),
                0.0,
                fill,
            );
            for (x, visible) in [
                (it.x0, it.x0 >= band.left() - 1.0),
                (it.x1, it.x1 <= band.right() + 1.0),
            ] {
                if visible {
                    p.vline(
                        x.round() + 0.5,
                        (band.top() + 3.0)..=(band.bottom() - 1.0),
                        Stroke::new(1.0, fill),
                    );
                }
            }
        } else {
            let x = it.x0.round() + 0.5;
            p.vline(
                x,
                (band.top() + 2.0)..=(band.bottom() - 1.0),
                Stroke::new(1.0, color),
            );
            p.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x, band.top() + 2.0),
                    Pos2::new(x + 5.0, band.top() + 5.0),
                    Pos2::new(x, band.top() + 8.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
        // Label: after the flag / at the chapter start (or the band's left edge for
        // a chapter that began earlier); skipped where it would overlap another.
        let lx = if it.chapter {
            it.x0.max(band.left() + 2.0) + 3.0
        } else {
            it.x0 + 7.0
        };
        if lx >= label_right + 4.0
            && lx < band.right() - 12.0
            && (!it.chapter || it.level == 0 || active)
        {
            let max_w = if it.chapter {
                (it.x1 - lx - 2.0).clamp(24.0, 240.0)
            } else {
                160.0
            };
            let mut job =
                egui::text::LayoutJob::simple_singleline(m.title.clone(), font.clone(), color);
            job.wrap =
                egui::text::TextWrapping::truncate_at_width(max_w.min(band.right() - lx - 2.0));
            let galley = p.layout_job(job);
            let r = Rect::from_min_size(Pos2::new(lx, band.top() + 1.0), galley.size());
            p.galley(r.min, galley, color);
            label_right = r.right();
            layout.placed[k].label = Some(r);
        }
    }
    if layout.hidden > 0 {
        p.text(
            band.right_top() + Vec2::new(-4.0, 1.0),
            Align2::RIGHT_TOP,
            format!("+{} close together", layout.hidden),
            font,
            pal.text_weak,
        );
    }
}

/// Marker under `pos`: point markers first, then the selected chapter, then the
/// innermost chapter.
pub fn hit(layout: &BandLayout, pos: Pos2, selected: Option<u32>) -> Option<(u32, MarkerPart)> {
    let mut best: Option<(u32, MarkerPart, i32)> = None;
    for it in &layout.placed {
        let on_label = it.label.is_some_and(|r| r.expand(1.0).contains(pos));
        let (part, rank) = if !it.chapter {
            if (pos.x - it.x0).abs() <= PICK_PX || on_label {
                (MarkerPart::Point, 3)
            } else {
                continue;
            }
        } else if (pos.x - it.x0).abs() <= PICK_PX {
            (MarkerPart::Start, 2)
        } else if (pos.x - it.x1).abs() <= PICK_PX {
            (MarkerPart::End, 2)
        } else if (pos.x >= it.x0 && pos.x <= it.x1) || on_label {
            (MarkerPart::Body, 1)
        } else {
            continue;
        };
        let rank = rank * 2 + i32::from(selected == Some(it.id));
        if best.is_none_or(|b| rank > b.2) {
            best = Some((it.id, part, rank));
        }
    }
    best.map(|(id, part, _)| (id, part))
}

/// Short description of a marker for tooltips.
pub fn describe(m: &Marker, position: impl Fn(u64) -> String) -> String {
    match m.end {
        Some(e) => format!(
            "Chapter “{}”\n{} → {} ({} ticks)\nDrag the edges to change its range; double-click to edit.",
            m.title,
            position(m.start),
            position(e),
            plumspirit_engine::midi::group_digits(e - m.start)
        ),
        None => format!(
            "Marker “{}”\n{}\nClick to go there; drag to move; double-click to edit.",
            m.title,
            position(m.start)
        ),
    }
}

/// The ruler's time-signature labels: (id, rect) for hit testing, and the bar
/// numbers drawn next to them (the ruler does not draw those again).
pub struct SignatureLabels {
    pub rects: Vec<(u32, Rect)>,
    pub bar_labels: Vec<Rect>,
}

/// Draws time-signature labels in the ruler at their ticks (collision-aware:
/// a label that would overlap the previous one is skipped). Returns the drawn
/// labels so bar numbers can avoid them.
pub fn draw_signatures(
    p: &Painter,
    ruler: Rect,
    time: &TimeAxis,
    conductor: &Conductor,
    meter: &plumspirit_engine::view::grid::MeterMap,
    pal: &Palette,
    selected: Option<u32>,
) -> SignatureLabels {
    let mut out = SignatureLabels {
        rects: Vec::new(),
        bar_labels: Vec::new(),
    };
    let (t0, t1) = time.visible_range(f64::from(ruler.width()));
    let sigs = conductor.time_signatures();
    let font = FontId::proportional(size::SMALL);
    let mut i = sigs.partition_point(|s| s.tick < t0);
    // The signature in effect at the left edge is shown too (pinned to the edge).
    if i > 0 && sigs.get(i).is_none_or(|s| s.tick > t0) {
        i -= 1;
    }
    let mut right = f32::NEG_INFINITY;
    let gap_ticks = (24.0 / time.pixels_per_tick).max(1.0) as u64;
    while let Some(s) = sigs.get(i) {
        if s.tick > t1 {
            break;
        }
        let line_x = ruler.left() + time.tick_to_x(s.tick) as f32;
        let x = line_x.max(ruler.left() + 1.0);
        // The bar number of the change comes first (the ruler skips its own
        // label there), then the signature.
        let bar = if line_x >= ruler.left() {
            meter.bar_beat(s.tick).map(|b| b.0.to_string())
        } else {
            None
        };
        let bar_galley = bar.map(|b| p.layout_no_wrap(b, font.clone(), pal.ruler_text));
        let bar_w = bar_galley.as_ref().map_or(0.0, |g| g.size().x + 4.0);
        let text = s.label();
        let galley = p.layout_no_wrap(text, font.clone(), pal.accent);
        let r = Rect::from_min_size(
            Pos2::new(x + 2.0 + bar_w, ruler.top() + 2.0),
            galley.size() + Vec2::new(6.0, 1.0),
        );
        if r.left() - bar_w >= right + 2.0 && r.right() < ruler.right() - 2.0 {
            if let Some(g) = bar_galley {
                let br = Rect::from_min_size(Pos2::new(x + 3.0, ruler.top() + 3.0), g.size());
                p.galley(br.min, g, pal.ruler_text);
                out.bar_labels.push(br);
            }
            let active = selected == Some(s.id);
            let (bg, fg) = if active {
                (pal.accent, pal.panel_bg)
            } else {
                (pal.accent_weak, pal.text)
            };
            p.rect_filled(r, 0.0, bg);
            p.galley(r.min + Vec2::new(3.0, 0.5), galley, fg);
            right = r.right();
            out.rects.push((s.id, r));
        }
        let next = s.tick.saturating_add(gap_ticks);
        i = sigs.partition_point(|x| x.tick < next).max(i + 1);
    }
    out
}

/// A faint outline colour for a marker being dragged.
pub fn drag_color(pal: &Palette) -> Color32 {
    pal.accent.gamma_multiply(0.8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::model::conductor::ConductorScope;

    fn conductor(markers: &[(u64, Option<u64>)]) -> Conductor {
        let base = Conductor::with_defaults(ConductorScope::Timeline);
        let list: Vec<Marker> = markers
            .iter()
            .enumerate()
            .map(|(i, &(s, e))| Marker {
                id: 10 + i as u32,
                start: s,
                end: e,
                title: format!("M{i}"),
                raw_title: None,
                cue: false,
                origin: None,
            })
            .collect();
        Conductor::from_parts(
            ConductorScope::Timeline,
            base.tempo_points().to_vec(),
            base.time_signatures().to_vec(),
            list,
            10 + markers.len() as u32,
        )
        .unwrap()
    }

    #[test]
    fn dense_markers_are_merged_and_work_is_bounded() {
        // 200,000 point markers one tick apart, viewed at 0.01 px per tick.
        let many: Vec<(u64, Option<u64>)> = (0..200_000u64).map(|t| (t, None)).collect();
        let c = conductor(&many);
        let band = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, BAND_HEIGHT));
        let time = TimeAxis::new(0, 0.01);
        let l = layout(&c, &time, band, None);
        assert!(l.placed.len() <= 400, "{}", l.placed.len());
        assert_eq!(l.placed.len() + l.hidden, 100_001);
        // Dense chapters are merged too (it once laid out and looked up every
        // chapter by id: 7 s per frame for 50,000 chapters); the longest of each
        // merged group stays visible.
        let chapters: Vec<(u64, Option<u64>)> = (0..200_000u64)
            .map(|t| (t, Some(t + 1 + (t % 7) * 1000)))
            .collect();
        let c = conductor(&chapters);
        let l = layout(&c, &time, band, None);
        assert!(l.placed.len() <= 800, "{}", l.placed.len());
        assert_eq!(l.placed.len() + l.hidden, 100_001);
        assert!(l.placed.iter().all(|p| c.markers()[p.index].id == p.id));
        let longest = l
            .placed
            .iter()
            .filter(|p| p.chapter)
            .map(|p| c.markers()[p.index].end.unwrap() - c.markers()[p.index].start)
            .max();
        assert_eq!(longest, Some(6001));
    }

    #[test]
    fn overlapping_chapters_stack_and_edges_are_picked() {
        // Ids: 10 = point at 50, 11 = chapter 100–600, 12 = chapter 300–900.
        let c = conductor(&[(50, None), (100, Some(600)), (300, Some(900))]);
        let band = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, BAND_HEIGHT));
        let time = TimeAxis::new(0, 1.0);
        let l = layout(&c, &time, band, None);
        let levels: Vec<usize> = l
            .placed
            .iter()
            .filter(|p| p.chapter)
            .map(|p| p.level)
            .collect();
        assert_eq!(levels, vec![0, 1]);
        assert_eq!(
            hit(&l, Pos2::new(50.0, 10.0), None),
            Some((10, MarkerPart::Point))
        );
        assert_eq!(
            hit(&l, Pos2::new(101.0, 10.0), None),
            Some((11, MarkerPart::Start))
        );
        assert_eq!(
            hit(&l, Pos2::new(598.0, 10.0), None),
            Some((11, MarkerPart::End))
        );
        assert_eq!(
            hit(&l, Pos2::new(800.0, 10.0), None),
            Some((12, MarkerPart::Body))
        );
        assert_eq!(hit(&l, Pos2::new(950.0, 10.0), None), None);
        // A chapter that started before the view is still found.
        let time = TimeAxis::new(400, 1.0);
        let l = layout(&c, &time, band, None);
        assert_eq!(
            hit(&l, Pos2::new(10.0, 10.0), None).map(|h| h.1),
            Some(MarkerPart::Body)
        );
    }

    #[test]
    fn a_point_marker_label_can_be_clicked_in_the_next_frame() {
        let c = conductor(&[(100, None)]);
        let band = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, BAND_HEIGHT));
        let time = TimeAxis::new(0, 1.0);
        // Frame 1: drawing measured the label "M0" right of the flag.
        let mut drawn = layout(&c, &time, band, None);
        drawn.placed[0].label = Some(Rect::from_min_size(
            Pos2::new(107.0, 1.0),
            Vec2::new(30.0, 12.0),
        ));
        let on_label = Pos2::new(125.0, 8.0);
        // Frame 2: a fresh layout has no labels until drawn again, but inherits
        // them for hit testing while nothing moved.
        let mut next = layout(&c, &time, band, None);
        assert_eq!(hit(&next, on_label, None), None);
        next.inherit_labels(&drawn);
        assert_eq!(hit(&next, on_label, None), Some((10, MarkerPart::Point)));
        // After scrolling, stale label rectangles are not used.
        let mut scrolled = layout(&c, &TimeAxis::new(50, 1.0), band, None);
        scrolled.inherit_labels(&drawn);
        assert_eq!(hit(&scrolled, on_label, None), None);
    }
}
