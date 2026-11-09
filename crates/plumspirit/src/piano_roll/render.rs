//! Piano-roll painting.
//!
//! Rendering is driven by the engine's range query: only notes overlapping the
//! visible tick range are visited, so frame cost depends on what is on screen, not on
//! the size of the project. Above [`EXACT_RENDER_LIMIT`] visible notes the roll draws
//! a density raster (one texture) instead of one rectangle per note; the raster is
//! cached and rebuilt only when the view or the notes change.

use super::Geometry;
use super::dense::{self, DenseRenderer, Role};
use super::hit_test::note_rect;
use crate::diagnostics::RenderStats;
use crate::ui::theme::{NotePalette, Palette};
use egui::epaint::{Mesh, Shape};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use plumspirit_engine::model::note::is_black_key;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{NoteLoc, NoteStore, TrackId};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::grid::{GridLine, LineKind, MeterMap};
use plumspirit_engine::view::tiles::{Outline, SourcePlace, TileOptions};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
#[path = "store_preview_tests.rs"]
mod store_preview_tests;

/// Reused rendering buffers (no per-frame allocation of large vectors).
#[derive(Default)]
pub struct RenderCache {
    pub grid: Vec<GridLine>,
    /// Subchannel palettes drawn so far.
    pub subs: crate::ui::theme::SubPalettes,
    /// v0.7: the last tool-preview drawing (reused while nothing changed).
    pub tool_preview: Option<ToolPreviewCache>,
}

/// A tool preview's meshes for one (content, track revision, view) key.
pub struct ToolPreviewCache {
    key: (u64, u64, [u64; 5], [u32; 4], bool, u64),
    coverage: std::sync::Arc<Mesh>,
    notes: std::sync::Arc<Mesh>,
    drawn: usize,
    cut: bool,
    aggregated: bool,
}

pub fn fill_background(p: &Painter, g: &Geometry, pal: &Palette) {
    p.rect_filled(g.notes, 0.0, pal.roll_bg);
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    let rh = g.view.pitch.row_height as f32;
    for pitch in lo..=hi {
        let y = g.notes.top() + g.view.pitch.pitch_top_y(pitch) as f32;
        let row = Rect::from_min_max(
            Pos2::new(g.notes.left(), y),
            Pos2::new(g.notes.right(), y + rh),
        );
        if is_black_key(pitch) {
            p.rect_filled(row, 0.0, pal.roll_black_row);
        }
        if pitch % 12 == 0 {
            // Octave boundary below every C row.
            p.hline(
                g.notes.x_range(),
                y + rh - 0.5,
                Stroke::new(1.0, pal.octave_line),
            );
        }
    }
}

pub fn draw_grid(
    p: &Painter,
    g: &Geometry,
    pal: &Palette,
    meter: &MeterMap,
    sub_ticks: Option<u64>,
    lines: &mut Vec<GridLine>,
) -> usize {
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    meter.lines(t0, t1, g.view.time.pixels_per_tick, 7.0, sub_ticks, lines);
    // Draw finer lines first so bar lines sit on top.
    for kind in [LineKind::Sub, LineKind::Minor, LineKind::Major] {
        let color = match kind {
            LineKind::Sub => pal.grid_sub,
            LineKind::Minor => pal.grid_minor,
            LineKind::Major => pal.grid_major,
        };
        for l in lines.iter().filter(|l| l.kind == kind) {
            let x = (g.notes.left() + g.view.time.tick_to_x(l.tick) as f32).round() + 0.5;
            p.vline(x, g.notes.y_range(), Stroke::new(1.0, color));
        }
    }
    lines.len()
}

/// Cache identity of a note host: a track's own content keys by its id (the
/// value before clips), an owned content as `2^32 + id` (the journal's host
/// encoding), so the two never share cached tiles.
pub fn host_key(host: plumspirit_engine::model::ContentRef) -> u64 {
    match host {
        plumspirit_engine::model::ContentRef::Track(t) => u64::from(t.0),
        plumspirit_engine::model::ContentRef::Owned(c) => (1u64 << 32) + u64::from(c.0),
    }
}

/// Parameters for drawing notes of one track (or one owned content).
pub struct NotePass<'a> {
    pub store: &'a NoteStore,
    /// Cache identity of the note host ([`host_key`]): a track's id as
    /// before clips, so the roll's tiles of a track are unchanged.
    pub key: u64,
    pub pal: &'a Palette,
    pub dark: bool,
    /// The note colours (v0.9 §068, `theme::note_palette`).
    pub palette: &'a crate::ui::theme::NotePalette,
    /// Hovered note (drawn with a hover border).
    pub hover: Option<Rect>,
    /// Selected notes are being dragged: draw them as ghosts at their original place
    /// and as previews at `(dt, dp)` / with duration `+ resize`.
    pub move_preview: Option<(i128, i32)>,
    pub resize_preview: Option<i128>,
    /// Notes hit by the Erase gesture in progress (drawn as ghosts).
    pub erasing: Option<&'a HashSet<NoteLoc>>,
    /// Hover border colour (the Erase tool uses the error colour).
    pub hover_color: Color32,
    /// A Preview is shown: selected notes are drawn as faint originals (the
    /// preview overlay draws their results).
    pub previewing: bool,
    /// The preview transforms every note of the track (all drawn faint).
    pub preview_all: bool,
    /// B2-F18 (Track Aggregate): the track's other clips, drawn with the
    /// same colours at their places on this pass's axis; empty = one host.
    pub parts: &'a [PlacedPart<'a>],
}

/// B2-F18: one more clip of the picked track in the Piano Roll's
/// aggregate view — its notes, cache identity and place relative to the
/// roll's content axis (`shift`: roll content tick minus the clip's content
/// tick; `window`: the clip's content window).
#[derive(Clone, Copy)]
pub struct PlacedPart<'a> {
    pub store: &'a NoteStore,
    pub key: u64,
    pub place: SourcePlace,
}

fn push_rect(mesh: &mut Mesh, r: Rect, c: Color32) {
    mesh.add_colored_rect(r, c);
}

/// Adds a note with a 1 px border when large enough.
fn push_note(mesh: &mut Mesh, r: Rect, fill: Color32, border: Color32, prims: &mut usize) {
    if r.width() >= 4.0 && r.height() >= 4.0 {
        push_rect(mesh, r, border);
        push_rect(mesh, r.shrink(1.0), fill);
        *prims += 2;
    } else {
        push_rect(mesh, r, fill);
        *prims += 1;
    }
}

/// Clips very long rectangles to just outside the visible area so huge coordinates
/// never reach the tessellator.
fn clip_x(r: Rect, area: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(r.min.x.max(area.left() - 4.0), r.min.y),
        Pos2::new(
            r.max
                .x
                .min(area.right() + 4.0)
                .max(r.min.x.max(area.left() - 4.0) + 1.0),
            r.max.y,
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn draw_notes(
    p: &Painter,
    ctx: &egui::Context,
    g: &Geometry,
    pass: &NotePass<'_>,
    cache: &mut RenderCache,
    dense: &mut DenseRenderer,
    stats: &mut RenderStats,
) {
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    let estimate = pass.store.estimate_in_range(t0, t1)
        + pass
            .parts
            .iter()
            .filter_map(|q| {
                let (a, b) = q.place.content_range(t0, t1)?;
                Some(q.store.estimate_in_range(a, b))
            })
            .sum::<usize>();
    stats.visible_estimate = estimate;
    stats.lod = dense.wants_dense(Role::Track, estimate);
    let started = Instant::now();
    if stats.lod {
        // The dense subsystem (tiles, never blank; `dense.rs`). B2-D02: rows
        // drawn with the exact renderer's gap and 1-point borders (4 pt rule).
        dense.set_rows(
            Role::Track,
            dense::RowStyle::for_rows(g.view.pitch.row_height, None, Some(4.0)),
        );
        let opt = track_tile_options(KeyRange::from_top(g.view.pitch.top));
        let family = dense::hash_words([
            1,
            pass.key,
            pass.store.revision(),
            pass.store.len() as u64,
            u64::from(pass.dark),
            pass.palette.key(),
            u64::from(u32::from_le_bytes(pass.pal.note_selected_border.to_array())),
            u64::from(u32::from_le_bytes(pass.pal.note_ghost.to_array())),
            tile_options_hash(&opt),
            parts_hash(pass.parts),
        ]);
        // L-G05: tiles still building need no redraw every frame — the
        // workers wake the UI when one is done.
        dense.draw(p, ctx, g, Role::Track, family, || {
            let mut stores = vec![Arc::new(pass.store.clone())];
            let mut places = Vec::new();
            if !pass.parts.is_empty() {
                places.push(SourcePlace::IDENTITY);
                for q in pass.parts {
                    stores.push(Arc::new(q.store.clone()));
                    places.push(q.place);
                }
            }
            dense::Family {
                stores,
                places,
                readings: Vec::new(),
                colors: track_tile_colors(pass),
                opt,
            }
        });
        stats.dense = dense.stats[Role::Track as usize];
        stats.primitives = stats.dense.visible_tiles;
    } else {
        draw_exact(p, g, pass, &mut cache.subs, stats, t0, t1, lo, hi, None);
        // B2-F18: the other clips at their places, in content ticks of each
        // (no drag / hover / preview state: those follow the focus clip);
        // the stats add up over the clips.
        let mut total = (
            stats.notes_queried,
            stats.chunks_visited,
            stats.chunks_total,
            stats.primitives,
        );
        for q in pass.parts {
            let Some((a, b)) = q.place.content_range(t0, t1) else {
                continue;
            };
            let gq = g.song(-q.place.shift);
            let part = NotePass {
                store: q.store,
                key: q.key,
                hover: None,
                move_preview: None,
                resize_preview: None,
                erasing: None,
                parts: &[],
                ..*pass
            };
            draw_exact(
                p,
                &gq,
                &part,
                &mut cache.subs,
                stats,
                a,
                b,
                lo,
                hi,
                Some(q.place.window),
            );
            total.0 += stats.notes_queried;
            total.1 += stats.chunks_visited;
            total.2 += stats.chunks_total;
            total.3 += stats.primitives;
        }
        (
            stats.notes_queried,
            stats.chunks_visited,
            stats.chunks_total,
            stats.primitives,
        ) = total;
    }
    stats.query_time = started.elapsed();
}

/// Cache identity of the aggregate's other clips (B2-F18).
fn parts_hash(parts: &[PlacedPart<'_>]) -> u64 {
    dense::hash_words(parts.iter().flat_map(|q| {
        [
            q.key,
            q.store.revision(),
            q.store.len() as u64,
            q.place.shift as u64,
            (q.place.shift >> 64) as u64,
            q.place.window.0,
            q.place.window.1,
        ]
    }))
}

/// The exact renderer's fill and border of a note of palette `sp`: selected
/// notes in their selected fill (faint when muted) with the selection border
/// — both at the Selection Opacity (v0.9 §119, table lookups) — muted notes
/// hollow (ghost fill) in their border colour (v0.7). The dense tiles draw
/// the same colours (`track_tile_colors`, v0.9 §029).
pub fn exact_paint(
    pal: &Palette,
    sp: &crate::ui::theme::VelocityPalette,
    n: &plumspirit_engine::model::NoteView,
) -> (Color32, Color32) {
    let v = usize::from(n.attrs.velocity());
    let muted = n.attrs.muted();
    if n.selected {
        let fill = if muted {
            sp.selected_muted_fill[v]
        } else {
            sp.selected_fill[v]
        };
        (fill, sp.selected_border[v])
    } else if muted {
        (pal.note_ghost, sp.border[v])
    } else {
        (sp.fill[v], sp.border[v])
    }
}

/// Content identity of tile options (part of a family id).
fn tile_options_hash(o: &TileOptions) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    o.hash(&mut h);
    h.finish()
}

/// The active track's dense colours: exactly the exact renderer's
/// (`draw_exact`) — subchannel × velocity fills, selected fills, the hollow
/// ghost fill of muted notes (v0.9 §029). There is no faint "context" copy:
/// with the Conductor selected no track's notes are drawn by this pass
/// (B2-D01; the Onion Skin shows the tracks in their own colours).
pub fn track_tile_colors(pass: &NotePass<'_>) -> dense::Colors {
    dense::Colors {
        sources: vec![dense::SourceColors::new(dense::Tint::Subchannels {
            dark: pass.dark,
            palette: Arc::new(pass.palette.clone()),
        })],
        selection: true,
        muted_fill: Some(pass.pal.note_ghost.to_srgba_unmultiplied()),
        muted: 255,
        ranked: false,
    }
}

/// The active track's dense tile options (v0.9 §026): opaque like exact
/// notes, outlined in each note's own border colour — selected notes in
/// their selected border at the Selection Opacity (v0.9 §119, from
/// `dense::Colors::border`, so no single selection colour here). B2-D02:
/// tops and bottoms are not texels (a texel is a whole row tall on screen):
/// the tiles carry each texel's border colour and the roll draws a 1-point
/// border on every row edge where rows are at least 4 pt tall
/// ([`dense::RowStyle`]) — the same tiles at every row height.
pub fn track_tile_options(keys: KeyRange) -> TileOptions {
    TileOptions {
        keys,
        fold: None,
        window: None,
        outline: Some(Outline {
            alpha: 255,
            darkness: 75,
            selection: None,
            vertical: false,
            borders: true,
        }),
    }
}

#[allow(clippy::too_many_arguments)] // one call site; all plain values
fn draw_exact(
    p: &Painter,
    g: &Geometry,
    pass: &NotePass<'_>,
    subs: &mut crate::ui::theme::SubPalettes,
    stats: &mut RenderStats,
    t0: u64,
    t1: u64,
    lo: u8,
    hi: u8,
    window: Option<(u64, u64)>,
) {
    let pal = pass.pal;
    // Note colours: hue from the subchannel, saturation from velocity; palettes
    // are looked up (built once per subchannel), never computed per note.
    let vp = crate::ui::theme::velocity_palette(pass.palette, pass.dark);
    let vp = &*vp;
    let palette = pass.palette;
    let dark = pass.dark;
    let gap = if g.view.pitch.row_height >= 6.0 {
        1.0
    } else {
        0.0
    };
    let mut normal = Mesh::default();
    let mut selected = Mesh::default();
    let mut prims = 0usize;
    let dragging = pass.move_preview.is_some() || pass.resize_preview.is_some();
    // v1.0-Launch L-G07 (owner): grouped notes are drawn exactly like
    // ungrouped ones — no outline or corner mark around a group.
    let qs = pass.store.query(t0, t1, |loc, n| {
        let pitch = n.attrs.pitch();
        if pitch < lo || pitch > hi {
            return;
        }
        // A clip's window: only the notes starting in it play (B2-F18).
        if window.is_some_and(|(a, b)| n.start_tick < a || n.start_tick >= b) {
            return;
        }
        let r = note_rect(g, n.start_tick, n.end_tick(), pitch);
        let r = clip_x(
            Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
            g.notes,
        );
        if pass.preview_all || pass.erasing.is_some_and(|e| e.contains(&loc)) {
            push_rect(&mut normal, r, pal.note_ghost);
            prims += 1;
            return;
        }
        let sp = if n.sub.0 == 0 {
            vp
        } else {
            subs.get(palette, n.sub.0, dark)
        };
        let (fill, border) = exact_paint(pal, sp, &n);
        if n.selected && (dragging || pass.previewing) {
            push_rect(&mut normal, r, pal.note_ghost);
            prims += 1;
        } else if n.selected {
            push_note(&mut selected, r, fill, border, &mut prims);
        } else {
            push_note(&mut normal, r, fill, border, &mut prims);
        }
    });
    stats.notes_queried = qs.notes_matched;
    stats.notes_scanned = qs.notes_scanned;
    stats.chunks_visited = qs.chunks_visited;
    stats.chunks_total = qs.chunks_total;

    // Drag previews: selected notes at their prospective position.
    if let Some((dt, dp)) = pass.move_preview {
        let span = |t: u64| -> u64 { (i128::from(t) - dt).clamp(0, i128::from(u64::MAX)) as u64 };
        let qs2 = pass.store.query(span(t0), span(t1), |_, n| {
            if !n.selected {
                return;
            }
            let pitch = i32::from(n.attrs.pitch()) + dp;
            if !(i32::from(lo)..=i32::from(hi)).contains(&pitch) {
                return;
            }
            let s = (i128::from(n.start_tick) + dt).clamp(0, i128::from(u64::MAX)) as u64;
            let e = s.saturating_add(n.duration_ticks);
            let r = note_rect(g, s, e, pitch as u8);
            let r = clip_x(
                Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
                g.notes,
            );
            let sp = if n.sub.0 == 0 {
                vp
            } else {
                subs.get(palette, n.sub.0, dark)
            };
            let v = usize::from(n.attrs.velocity());
            push_note(
                &mut selected,
                r,
                sp.selected_fill[v],
                sp.selected_border[v],
                &mut prims,
            );
        });
        stats.notes_queried += qs2.notes_matched;
    } else if let Some(delta) = pass.resize_preview {
        pass.store.query(t0, t1, |_, n| {
            if !n.selected || n.attrs.pitch() < lo || n.attrs.pitch() > hi {
                return;
            }
            let e = (i128::from(n.end_tick()) + delta)
                .clamp(i128::from(n.start_tick), i128::from(u64::MAX)) as u64;
            let r = note_rect(g, n.start_tick, e, n.attrs.pitch());
            let r = clip_x(
                Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
                g.notes,
            );
            let sp = if n.sub.0 == 0 {
                vp
            } else {
                subs.get(palette, n.sub.0, dark)
            };
            let v = usize::from(n.attrs.velocity());
            push_note(
                &mut selected,
                r,
                sp.selected_fill[v],
                sp.selected_border[v],
                &mut prims,
            );
        });
    }
    p.add(Shape::mesh(normal));
    p.add(Shape::mesh(selected));
    if let Some(h) = pass.hover {
        p.rect_stroke(
            h,
            0.0,
            Stroke::new(1.0, pass.hover_color),
            egui::StrokeKind::Inside,
        );
        prims += 1;
    }
    stats.primitives = prims;
}

pub fn draw_edit_cursor(p: &Painter, g: &Geometry, pal: &Palette, tick: u64) {
    draw_cursor_line(p, g.notes, g.notes.left(), &g.view.time, pal, tick);
}

/// The edit position line through `area` (`left`: the screen x of the time
/// axis' origin); shared with the Track List.
pub fn draw_cursor_line(
    p: &Painter,
    area: Rect,
    left: f32,
    time: &TimeAxis,
    pal: &Palette,
    tick: u64,
) {
    let raw = left + time.tick_to_x(tick) as f32;
    if raw >= area.left() - 1.0 && raw <= area.right() + 1.0 {
        let ppp = p.pixels_per_point();
        let w = marker_width(CURSOR_WIDTH, ppp);
        p.vline(
            marker_x(raw, w, ppp),
            area.y_range(),
            Stroke::new(w, pal.cursor_line),
        );
    }
}

/// The Edit / Anchor playhead (B1-0080): a dashed, dimmer line through
/// `area` and, when `flag` is given (the ruler), a small downward tab at its
/// top — distinct from the solid playhead and edit cursor. Shared with the
/// Track List.
pub fn draw_anchor_line(
    p: &Painter,
    area: Rect,
    left: f32,
    time: &TimeAxis,
    pal: &Palette,
    tick: u64,
    flag: Option<Rect>,
) {
    let raw = left + time.tick_to_x(tick) as f32;
    if raw < area.left() - 1.0 || raw > area.right() + 1.0 {
        return;
    }
    let ppp = p.pixels_per_point();
    let w = marker_width(CURSOR_WIDTH, ppp);
    let x = marker_x(raw, w, ppp);
    let c = pal.cursor_line.gamma_multiply(0.6);
    p.extend(Shape::dashed_line(
        &[Pos2::new(x, area.top()), Pos2::new(x, area.bottom())],
        Stroke::new(w, c),
        4.0,
        3.0,
    ));
    if let Some(r) = flag {
        let (y, s) = (r.top(), 5.0);
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - s, y),
                Pos2::new(x + s, y),
                Pos2::new(x, y + s * 1.2),
            ],
            c,
            Stroke::NONE,
        ));
    }
}

/// Draws the ruler background (called before the signature labels).
/// Shared with the Track List (`ruler` is the ruler's rectangle).
pub fn draw_ruler_background(p: &Painter, ruler: Rect, pal: &Palette) {
    p.rect_filled(ruler, 0.0, pal.ruler_bg);
    p.hline(
        ruler.x_range(),
        ruler.bottom() - 0.5,
        Stroke::new(1.0, pal.panel_border),
    );
}

/// Bar lines, bar numbers (skipping `avoid`, the time-signature labels) and the
/// cursor marker, along `ruler` on the time axis `time` (shared by the Piano
/// Roll and the Track List).
#[allow(clippy::too_many_arguments)] // two call sites; plain values
pub fn draw_ruler(
    p: &Painter,
    ruler: Rect,
    time: &TimeAxis,
    pal: &Palette,
    meter: &MeterMap,
    lines: &mut Vec<GridLine>,
    cursor_tick: u64,
    playing: bool,
    avoid: &[Rect],
) {
    let (t0, t1) = time.visible_range(f64::from(ruler.width()));
    meter.lines(t0, t1, time.pixels_per_tick, 7.0, None, lines);
    let font = FontId::proportional(crate::ui::theme::size::SMALL);
    let mut last_label_x = f32::NEG_INFINITY;
    for l in lines.iter() {
        let x = (ruler.left() + time.tick_to_x(l.tick) as f32).round() + 0.5;
        match l.kind {
            LineKind::Major => {
                p.vline(
                    x,
                    (ruler.bottom() - 9.0)..=ruler.bottom(),
                    Stroke::new(1.0, pal.ruler_text),
                );
                if let Some(v) = l.label
                    && x - last_label_x >= 36.0
                    && x + 2.0 < ruler.right() - 8.0
                {
                    let galley =
                        p.layout_no_wrap(meter.format_label(v), font.clone(), pal.ruler_text);
                    let r =
                        Rect::from_min_size(Pos2::new(x + 3.0, ruler.top() + 3.0), galley.size());
                    if !avoid.iter().any(|a| a.expand(2.0).intersects(r)) {
                        p.galley(r.min, galley, pal.ruler_text);
                        last_label_x = x;
                    }
                }
            }
            LineKind::Minor => {
                p.vline(
                    x,
                    (ruler.bottom() - 4.0)..=ruler.bottom(),
                    Stroke::new(1.0, pal.grid_major),
                );
            }
            LineKind::Sub => {}
        }
    }
    // Edit cursor / playhead marker: the same X as its line (§097).
    let raw = ruler.left() + time.tick_to_x(cursor_tick) as f32;
    if raw >= ruler.left() - 6.0 && raw <= ruler.right() + 6.0 {
        let ppp = p.pixels_per_point();
        let w = marker_width(
            if playing {
                PLAYHEAD_WIDTH
            } else {
                CURSOR_WIDTH
            },
            ppp,
        );
        let x = marker_x(raw, w, ppp);
        let b = ruler.bottom();
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 5.0, b - 8.0),
                Pos2::new(x + 5.0, b - 8.0),
                Pos2::new(x, b),
            ],
            if playing {
                pal.playhead
            } else {
                pal.cursor_line
            },
            Stroke::NONE,
        ));
    }
}

pub fn draw_keys(
    p: &Painter,
    g: &Geometry,
    pal: &Palette,
    labels: &[String],
    hover_pitch: Option<u8>,
    activity: Option<&[crate::app::keys::KeyDisplay; crate::app::keys::KEYS]>,
) {
    p.rect_filled(g.keys, 0.0, pal.key_white);
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.keys.height()));
    let rh = g.view.pitch.row_height as f32;
    let font = FontId::proportional((rh - 2.0).clamp(8.0, crate::ui::theme::size::SMALL));
    for pitch in lo..=hi {
        let y = g.keys.top() + g.view.pitch.pitch_top_y(pitch) as f32;
        let row = Rect::from_min_max(
            Pos2::new(g.keys.left(), y),
            Pos2::new(g.keys.right(), y + rh),
        );
        let black = is_black_key(pitch);
        let key = if black {
            // Black keys are drawn shorter, like a keyboard.
            Rect::from_min_max(
                row.min,
                Pos2::new(g.keys.left() + g.keys.width() * 0.62, row.max.y),
            )
        } else {
            row
        };
        if black {
            p.rect_filled(key, 0.0, pal.key_black);
        }
        if hover_pitch == Some(pitch) {
            p.rect_filled(row, 0.0, pal.accent_weak.gamma_multiply(0.8));
        }
        // Live keyboard colours: the key's area in the colours of the notes
        // sounding on it (bounded bands), inset so the outline and the
        // separation from neighbouring keys stay visible.
        let mut label_bg = None;
        if let Some(d) = activity.map(|a| a[usize::from(pitch)])
            && d.total > 0
            && d.n > 0
        {
            let inset = (rh * 0.18).clamp(0.0, 1.5);
            let area = key.shrink2(egui::vec2(1.0, inset));
            if area.width() > 1.0 && area.height() > 0.5 {
                let n = usize::from(d.n);
                let bw = area.width() / n as f32;
                for (i, c) in d.bands.iter().take(n).enumerate() {
                    let x0 = area.left() + bw * i as f32;
                    let x1 = if i + 1 == n { area.right() } else { x0 + bw };
                    p.rect_filled(
                        Rect::from_min_max(Pos2::new(x0, area.top()), Pos2::new(x1, area.bottom())),
                        0.0,
                        *c,
                    );
                }
                if black {
                    // Keep the key's edge (dimensional contrast with the row).
                    p.rect_stroke(
                        key,
                        0.0,
                        Stroke::new(1.0, pal.key_black),
                        egui::StrokeKind::Inside,
                    );
                }
                label_bg = Some(d.bands[n - 1]);
            }
        }
        let next_is_white_boundary = matches!(pitch % 12, 4 | 11); // E|F and B|C
        if next_is_white_boundary || pitch % 12 == 0 {
            let yb = if pitch % 12 == 0 {
                y + rh - 0.5
            } else {
                y - 0.5
            };
            p.hline(g.keys.x_range(), yb, Stroke::new(1.0, pal.key_separator));
        }
        let show = if pitch % 12 == 0 {
            rh >= 8.0
        } else {
            rh >= 15.0 && !black
        };
        if show {
            let color = match label_bg {
                // Readable over the highlight: dark text on light colours.
                Some(bg) => {
                    let l = 0.2126 * f32::from(bg.r())
                        + 0.7152 * f32::from(bg.g())
                        + 0.0722 * f32::from(bg.b());
                    if l > 140.0 {
                        Color32::from_gray(20)
                    } else {
                        Color32::from_gray(245)
                    }
                }
                None if pitch % 12 == 0 => pal.key_c_text,
                None => pal.key_text,
            };
            p.text(
                Pos2::new(g.keys.right() - 4.0, y + rh / 2.0),
                Align2::RIGHT_CENTER,
                &labels[usize::from(pitch)],
                font.clone(),
                color,
            );
        }
    }
    p.vline(
        g.keys.right() - 0.5,
        g.keys.y_range(),
        Stroke::new(1.0, pal.panel_border),
    );
}

/// Lowest and highest key of an 88-key piano (A0, C8; v0.9 §057).
pub const PIANO_88: (u8, u8) = (21, 108);

/// Y of the 88-Key Guide lines below the area's top: the bottom edge of
/// A0's row and the top edge of C8's row.
pub fn key_guide_ys(axis: &plumspirit_engine::view::PitchAxis) -> [f64; 2] {
    let (lo, hi) = PIANO_88;
    [axis.pitch_top_y(lo) + axis.row_height, axis.pitch_top_y(hi)]
}

/// The 88-Key Guide (v0.9 §057 / §229): one clear line under A0 (21) and one
/// over C8 (108) across `area` (the note area or the keyboard), so keys 21–108
/// lie between them in 128- and 256-key projects alike. Guidance only.
pub fn draw_key_guide(p: &Painter, g: &Geometry, area: Rect, pal: &Palette) {
    let stroke = Stroke::new(1.5, pal.accent);
    for y in key_guide_ys(&g.view.pitch) {
        let y = area.top() + y as f32;
        if (area.top() - 1.0..=area.bottom() + 1.0).contains(&y) {
            p.hline(area.x_range(), y.round() + 0.25, stroke);
        }
    }
}

/// A simple scrollbar: returns the thumb rectangle for interaction.
pub fn scrollbar_thumb(track: Rect, horizontal: bool, start_frac: f64, len_frac: f64) -> Rect {
    let len_frac = len_frac.clamp(0.0, 1.0);
    let start_frac = start_frac.clamp(0.0, 1.0 - len_frac);
    if horizontal {
        let min_len = 24.0f32.min(track.width());
        let len = ((track.width() as f64 * len_frac) as f32).max(min_len);
        let x0 = track.left()
            + ((track.width() - len) as f64 * (start_frac / (1.0 - len_frac).max(1e-9))) as f32;
        Rect::from_min_size(
            Pos2::new(x0, track.top() + 2.0),
            Vec2::new(len, track.height() - 4.0),
        )
    } else {
        let min_len = 24.0f32.min(track.height());
        let len = ((track.height() as f64 * len_frac) as f32).max(min_len);
        let y0 = track.top()
            + ((track.height() - len) as f64 * (start_frac / (1.0 - len_frac).max(1e-9))) as f32;
        Rect::from_min_size(
            Pos2::new(track.left() + 2.0, y0),
            Vec2::new(track.width() - 4.0, len),
        )
    }
}

pub fn draw_scrollbar(p: &Painter, track: Rect, thumb: Rect, hovered: bool, pal: &Palette) {
    p.rect_filled(track, 0.0, pal.scrollbar_track);
    let c = if hovered {
        pal.scrollbar_thumb_hover
    } else {
        pal.scrollbar_thumb
    };
    p.rect_filled(thumb, 0.0, c);
}

// ------------------------------------------------------------------ Onion Skin

/// One ghost source: a track's notes drawn translucently behind the active
/// track — since v0.9 V09-S11 one clip of an Onion track, where it plays
/// (`place`: the placement's `start − offset` and window; the identity for
/// a track's own content where it is).
#[derive(Clone, Copy)]
pub struct OnionSource<'a> {
    pub id: TrackId,
    pub store: &'a NoteStore,
    /// The track's own Onion Skin color (Track Properties), if set.
    pub color: Option<[u8; 3]>,
    /// Where the notes play in song time ([`SourcePlace::IDENTITY`]: where
    /// they are).
    pub place: SourcePlace,
}

pub struct OnionPass<'a> {
    pub sources: &'a [OnionSource<'a>],
    pub dark: bool,
    /// The note colours: ghosts turn per subchannel by its hue offset.
    pub palette: &'a crate::ui::theme::NotePalette,
    /// Ghost opacity, 0–255.
    pub alpha: u8,
    /// Outline ghost notes at a darkness in percent (v0.8 §120–§122: 0 %
    /// white, 50 % the ghost's own colour, 100 % black; exact path: a 1 px
    /// border on every note large enough; dense path: the edges of ghost
    /// shapes shaded towards the outline colour). `None`: no outlines.
    pub outlines: Option<u8>,
}

/// The outline opacity of a ghost source: v1.0-beta1 B1-0150 (contract
/// §022-A, image13) — the Onion opacity applies to the interior only, the
/// dark border stays opaque; invisible only when the ghosts are (0 %). The
/// outline's colour is chosen by the darkness, never by this alpha (§121).
fn outline_alpha(alpha: u8) -> u8 {
    if alpha == 0 { 0 } else { 255 }
}

/// Reused buffers for Onion Skin rendering (exact path tints).
#[derive(Default)]
pub struct OnionCache {
    tints: crate::ui::theme::GhostTints,
}

/// Ghost opacity (0–255) from which ghosts count as opaque (about 80 %).
pub const ONION_OPAQUE_ALPHA: u8 = 204;

fn sources_hash(pass: &OnionPass<'_>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in pass.sources {
        for v in [
            u64::from(s.id.0),
            s.store.content_revision(),
            s.store.len() as u64,
            s.color.map_or(u64::MAX, |[r, g, b]| {
                u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b)
            }),
        ] {
            h ^= v;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A placed source (V09-S11) also by where it plays; identity
        // sources hash as before clips.
        if !s.place.is_identity() {
            for v in [
                s.place.shift as u64,
                (s.place.shift >> 64) as u64,
                s.place.window.0,
                s.place.window.1,
            ] {
                h ^= v;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    h
}

/// Content identity of the Onion tiles (§119): the sources (ids, note
/// revisions, colours, layer order), opacity, outline darkness and theme —
/// never the view. Scrolling, zooming and the playhead moving reuse every
/// built tile; only a real note / colour / order / option change starts a new
/// build.
pub fn onion_family(pass: &OnionPass<'_>, row_height: f64, keys: KeyRange) -> u64 {
    dense::hash_words([
        2,
        sources_hash(pass),
        u64::from(pass.alpha),
        pass.outlines.map_or(u64::MAX, u64::from),
        u64::from(pass.dark),
        pass.palette.key(),
        tile_options_hash(&onion_tile_options(pass, row_height, keys)),
    ])
}

/// Onion tile colours: each source's exact ghost tints (velocity band ×
/// subchannel) at the ghost opacity; muted ghosts like the others, as on the
/// exact path; sources back to front (the later one is in front, §116).
pub fn onion_tile_colors(pass: &OnionPass<'_>) -> dense::Colors {
    let palette = Arc::new(pass.palette.clone());
    dense::Colors {
        sources: pass
            .sources
            .iter()
            .map(|s| {
                let c = pass.palette.track_color(s.id.0, s.color, pass.dark, 255);
                dense::SourceColors::new(dense::Tint::Ghost {
                    base: [c.r(), c.g(), c.b()],
                    alpha: pass.alpha,
                    palette: palette.clone(),
                })
            })
            .collect(),
        selection: false,
        muted_fill: None,
        muted: 255,
        ranked: true,
    }
}

/// Onion tile options: outlines at the chosen darkness (§120–§122); B2-D02:
/// tops / bottoms drawn by the roll from the tiles' border colours where rows
/// are at least 3 pt tall (the exact ghosts' border rule), independent of
/// the row height in the tiles themselves.
pub fn onion_tile_options(pass: &OnionPass<'_>, _row_height: f64, keys: KeyRange) -> TileOptions {
    TileOptions {
        keys,
        fold: None,
        window: None,
        outline: pass.outlines.map(|d| Outline {
            alpha: outline_alpha(pass.alpha),
            darkness: d,
            selection: None,
            vertical: false,
            borders: true,
        }),
    }
}

/// The content ticks of `s` to query for song ticks `[t0, t1)` (identity:
/// the range itself, exactly as before clips).
fn source_range(s: &OnionSource<'_>, t0: u64, t1: u64) -> Option<(u64, u64)> {
    if s.place.is_identity() {
        Some((t0, t1))
    } else {
        s.place.content_range(t0, t1)
    }
}

/// Draws the ghost notes of other tracks: exact translucent rectangles when few
/// are visible, otherwise a density raster built over several frames if needed.
/// Returns true while a raster build is still in progress (the caller repaints).
pub fn draw_onion(
    p: &Painter,
    ctx: &egui::Context,
    g: &Geometry,
    pass: &OnionPass<'_>,
    cache: &mut OnionCache,
    dense: &mut DenseRenderer,
    stats: &mut RenderStats,
) -> bool {
    if pass.sources.is_empty() {
        return false;
    }
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    // The count stops once the view is surely dense — past the enter
    // threshold the decision is dense in either hysteresis state — so the
    // estimate is then a lower bound (V09-S11 for clips: a source per
    // placement; L-G05 for every source: 15,000 Onion tracks were each asked
    // every frame); a placed source counts only what its window plays.
    let t_estimate = std::time::Instant::now();
    let mut total = 0usize;
    stats.onion_estimate_partial = false;
    for s in pass.sources {
        if total > dense.enter_threshold() {
            stats.onion_estimate_partial = true;
            break;
        }
        let (w0, w1) = s.place.window;
        total += source_range(s, t0, t1)
            .map(|(a, b)| (a.max(w0), b.min(w1)))
            .filter(|(a, b)| b > a || s.place.is_identity())
            .map_or(0, |(a, b)| s.store.estimate_in_range(a, b));
    }
    stats.onion_estimate = total;
    stats.onion_estimate_time = t_estimate.elapsed();
    if !dense.wants_dense(Role::Onion, total) {
        let gap = if g.view.pitch.row_height >= 6.0 {
            1.0
        } else {
            0.0
        };
        // Near-opaque ghosts are drawn one pixel thinner at the top so the
        // active track's full-height, bordered notes (drawn above them) stay
        // distinguishable even at 100 % (Section 118).
        let inset = if pass.alpha >= ONION_OPAQUE_ALPHA && g.view.pitch.row_height >= 5.0 {
            1.0
        } else {
            0.0
        };
        // One mesh, layer by layer (back to front): each source's fills, then
        // its outlines — a back layer's outlines never cover a front layer.
        let mut mesh = Mesh::default();
        let mut outline_mesh = Mesh::default();
        let tints = &mut cache.tints;
        let edge_alpha = outline_alpha(pass.alpha);
        for s in pass.sources {
            let tint = pass.palette.track_color(s.id.0, s.color, pass.dark, 255);
            // Last looked-up subchannel's tints and outline colours (runs of
            // one subchannel are the common case).
            let mut last: Option<(u32, [Color32; 16], [Color32; 16])> = None;
            // The source's content ticks sounding in the view (a placed
            // source: moved by its shift, only the notes its window plays).
            let Some((q0, q1)) = source_range(s, t0, t1) else {
                continue;
            };
            let place = s.place;
            s.store.query(q0, q1, |_, n| {
                let pitch = n.attrs.pitch();
                if pitch < lo || pitch > hi {
                    return;
                }
                let Some(start) = place.start(n.start_tick) else {
                    return;
                };
                let r = note_rect(g, start, start.saturating_add(n.duration_ticks), pitch);
                let r = clip_x(
                    Rect::from_min_max(
                        Pos2::new(r.min.x, r.min.y + inset),
                        Pos2::new(r.max.x, r.max.y - gap),
                    ),
                    g.notes,
                );
                if !matches!(&last, Some((sub, ..)) if *sub == n.sub.0) {
                    let row = *tints.get_with_alpha(pass.palette, tint, n.sub.0, pass.alpha);
                    // §121: the outline colour is the source colour at the
                    // darkness, never darkened by the opacity (B2-D02: the
                    // same colour the tiles' borders take; `row` holds
                    // premultiplied colours).
                    let edges = pass.outlines.map_or([Color32::TRANSPARENT; 16], |d| {
                        tints
                            .get_with_alpha(pass.palette, tint, n.sub.0, 255)
                            .map(|c| {
                                let [r, g, b] =
                                    crate::ui::theme::outline_shade([c.r(), c.g(), c.b()], d);
                                Color32::from_rgba_unmultiplied(r, g, b, edge_alpha)
                            })
                    });
                    last = Some((n.sub.0, row, edges));
                }
                let (_, row, edges) = last.as_ref().expect("set");
                let v = usize::from(n.attrs.velocity() >> 3);
                push_rect(&mut mesh, r, row[v]);
                stats.onion_primitives += 1;
                // Every ghost large enough gets its border (deterministic:
                // depends only on the note's own size, never on how many
                // other notes were drawn first — §120).
                if pass.outlines.is_some() && r.width() >= 3.0 && r.height() >= 3.0 {
                    // Four 1 px edges, batched into one mesh.
                    let (x0, x1, y0, y1) = (r.min.x, r.max.x, r.min.y, r.max.y);
                    for e in [
                        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y0 + 1.0)),
                        Rect::from_min_max(Pos2::new(x0, y1 - 1.0), Pos2::new(x1, y1)),
                        Rect::from_min_max(Pos2::new(x0, y0 + 1.0), Pos2::new(x0 + 1.0, y1 - 1.0)),
                        Rect::from_min_max(Pos2::new(x1 - 1.0, y0 + 1.0), Pos2::new(x1, y1 - 1.0)),
                    ] {
                        push_rect(&mut outline_mesh, e, edges[v]);
                    }
                    stats.onion_primitives += 1;
                }
            });
            if !outline_mesh.is_empty() {
                mesh.append(std::mem::take(&mut outline_mesh));
            }
        }
        p.add(Shape::mesh(mesh));
        return false;
    }
    // Dense: the shared tile subsystem (`dense.rs`), rows drawn like the
    // exact ghosts: gap, inset when near-opaque, 1-point borders (B2-D02).
    dense.set_rows(
        Role::Onion,
        dense::RowStyle::for_rows(
            g.view.pitch.row_height,
            (pass.alpha >= ONION_OPAQUE_ALPHA).then_some(5.0),
            pass.outlines.map(|_| 3.0),
        ),
    );
    let keys = KeyRange::from_top(g.view.pitch.top);
    let t_family = std::time::Instant::now();
    let family = onion_family(pass, g.view.pitch.row_height, keys);
    stats.onion_family_time = t_family.elapsed();
    // L-S49: sources whose notes did not change share the clones of the
    // Onion families still held (Onion tiles draw no selection).
    let pool = dense.store_pool(Role::Onion, family);
    let pending = dense.draw(p, ctx, g, Role::Onion, family, move || {
        let fam = dense::Family {
            stores: pass
                .sources
                .iter()
                .map(|s| dense::share_store(pool.as_ref(), s.store))
                .collect(),
            places: pass.sources.iter().map(|s| s.place).collect(),
            readings: Vec::new(),
            colors: onion_tile_colors(pass),
            opt: onion_tile_options(pass, g.view.pitch.row_height, keys),
        };
        // Dropped while the families it came from are still held: the
        // families no role shows are released afterwards, large ones off
        // the UI thread (L-S48) — the pool must not be their last owner.
        drop(pool);
        fam
    });
    stats.onion_dense = dense.stats[Role::Onion as usize];
    stats.onion_primitives += stats.onion_dense.visible_tiles;
    pending
}

/// Playhead line width (points before snapping to whole physical pixels).
pub const PLAYHEAD_WIDTH: f32 = 1.5;
/// Edit cursor line width (points before snapping).
pub const CURSOR_WIDTH: f32 = 1.0;

/// A marker line's stroke width: `points` rounded to whole physical pixels
/// (at least one), so its snapping rule below is exact at every DPI.
pub fn marker_width(points: f32, ppp: f32) -> f32 {
    let ppp = if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    };
    (points * ppp).round().max(1.0) / ppp
}

/// The one X of a time marker (v0.8 §097 / §249): the ruler triangle and the
/// vertical line both use it. `raw` (points: area left + tick → x, scroll and
/// zoom included) is snapped exactly the way egui snaps a vertical line of
/// `width` at `ppp` pixels per point (pixel centre for an odd number of
/// pixels, pixel edge for an even one), so the tessellated line lands where
/// the triangle points — no horizontal offset at any DPI, zoom or scroll.
pub fn marker_x(raw: f32, width: f32, ppp: f32) -> f32 {
    let ppp = if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    };
    let mut x = raw;
    Stroke::new(width, Color32::WHITE).round_center_to_pixel(ppp, &mut x);
    x
}

/// The playback position line (drawn above everything else).
pub fn draw_playhead(
    p: &Painter,
    area: Rect,
    left: f32,
    g_time: &TimeAxis,
    pal: &Palette,
    tick: u64,
) {
    let raw = left + g_time.tick_to_x(tick) as f32;
    if raw >= area.left() - 1.0 && raw <= area.right() + 1.0 {
        let ppp = p.pixels_per_point();
        let w = marker_width(PLAYHEAD_WIDTH, ppp);
        p.vline(
            marker_x(raw, w, ppp),
            area.y_range(),
            Stroke::new(w, pal.playhead),
        );
    }
}

/// Largest number of preview results drawn per frame; beyond it the preview is
/// simplified (the operation itself still applies to every note).
pub const PREVIEW_DRAW_BUDGET: usize = 20_000;

/// Preview overlay (layer above the committed notes, below the playhead): the
/// results of a batch operation for the selected notes that can be visible,
/// computed with the operation's own kernel. Returns (drawn, simplified).
#[allow(clippy::too_many_arguments)]
pub fn draw_preview(
    p: &Painter,
    g: &Geometry,
    track: plumspirit_engine::editor::HostView<'_>,
    kernel: &plumspirit_engine::editor::preview::NoteKernel,
    scope: plumspirit_engine::editor::preview::PreviewScope,
    pal: &Palette,
    dark: bool,
    subs: &mut crate::ui::theme::SubPalettes,
) -> (usize, bool) {
    let palette = crate::ui::theme::note_palette(p.ctx());
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    let gap = if g.view.pitch.row_height >= 6.0 {
        1.0
    } else {
        0.0
    };
    let mut mesh = Mesh::default();
    let mut prims = 0usize;
    let mut removed: Vec<Rect> = Vec::new();
    let (n, cut) = plumspirit_engine::editor::preview::visible_preview(
        track,
        kernel,
        scope,
        t0,
        t1,
        PREVIEW_DRAW_BUDGET,
        |orig, q| {
            let Some(q) = q else {
                // Would stop sounding: a dim, error-coloured outline where it is.
                let pitch = orig.attrs.pitch();
                if pitch < lo || pitch > hi {
                    return;
                }
                let r = note_rect(
                    g,
                    orig.start_tick,
                    orig.end_tick().unwrap_or(u64::MAX),
                    pitch,
                );
                removed.push(clip_x(
                    Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
                    g.notes,
                ));
                return;
            };
            let pitch = q.attrs.pitch();
            if pitch < lo || pitch > hi {
                return;
            }
            let sp = subs.get(&palette, q.sub.0, dark);
            let (fill, border) = NotePalette::selected_paint(sp, q.attrs.velocity());
            let r = note_rect(g, q.start_tick, q.end_tick().unwrap_or(u64::MAX), pitch);
            let r = clip_x(
                Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
                g.notes,
            );
            push_note(&mut mesh, r, fill, border, &mut prims);
        },
    );
    p.add(Shape::mesh(mesh));
    let stroke = Stroke::new(1.0, pal.error.gamma_multiply(0.85));
    for r in removed {
        if r.width() >= 3.0 && r.height() >= 3.0 {
            p.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
        } else {
            p.rect_filled(r, 0.0, pal.error.gamma_multiply(0.5));
        }
    }
    if cut {
        p.text(
            g.notes.right_top() + Vec2::new(-8.0, 8.0),
            Align2::RIGHT_TOP,
            format!(
                "Preview simplified: first {} notes shown",
                PREVIEW_DRAW_BUDGET
            ),
            FontId::proportional(crate::ui::theme::size::SMALL),
            pal.text_weak,
        );
    }
    (n, cut)
}

/// Notes about to be added (import preview): highlighted ghosts from the
/// prepared result itself (no extra copy), bounded like every other layer.
pub fn draw_added(
    p: &Painter,
    g: &Geometry,
    stores: &[&NoteStore],
    records: &[plumspirit_engine::model::NoteRecord],
    longest: u64,
    pal: &Palette,
) -> bool {
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    let (fill, border) = crate::ui::theme::note_palette(p.ctx()).selection_paint(pal);
    let mut mesh = Mesh::default();
    let mut prims = 0usize;
    let mut drawn = 0usize;
    let mut read = 0usize;
    let mut cut = false;
    // L-S46: returns whether to keep reading. Reading stops once the draw
    // budget is spent or `PREVIEW_AGG_BUDGET` notes were read (rows outside
    // the view included): a frame's cost is bounded by the budgets, never by
    // how many notes the import adds (224M: 12.7 s a frame before).
    let mut add = |s: u64, e: u64, pitch: u8, mesh: &mut Mesh| -> bool {
        read += 1;
        if read > PREVIEW_AGG_BUDGET {
            cut = true;
            return false;
        }
        if pitch < lo || pitch > hi {
            return true;
        }
        if drawn >= PREVIEW_DRAW_BUDGET {
            cut = true;
            return false;
        }
        drawn += 1;
        let r = clip_x(note_rect(g, s, e, pitch), g.notes);
        push_note(mesh, r, fill, border, &mut prims);
        true
    };
    let mut more = true;
    for st in stores {
        st.query_while(t0, t1, |_, n| {
            more = add(n.start_tick, n.end_tick(), n.attrs.pitch(), &mut mesh);
            more
        });
        if !more {
            break;
        }
    }
    if !records.is_empty() && more {
        // Sorted by start: visit from the first that may reach the view.
        let from = records.partition_point(|r| r.start_tick.saturating_add(longest) < t0);
        for r in &records[from..] {
            if r.start_tick > t1 {
                break;
            }
            let e = r.start_tick.saturating_add(r.duration_ticks);
            if e >= t0 && !add(r.start_tick, e, r.attrs.pitch(), &mut mesh) {
                break;
            }
        }
    }
    p.add(Shape::mesh(mesh));
    cut
}

/// Preview results beyond [`PREVIEW_DRAW_BUDGET`] are aggregated: each one
/// marks its row's screen columns in a coverage raster (O(1) per note, a
/// difference array per row), drawn as merged runs. At most this many results
/// are visited per frame; beyond it the preview says it is simplified.
pub const PREVIEW_AGG_BUDGET: usize = 2_000_000;
/// Includes native query metadata/checkpoints and rejected/off-row candidates.
/// Each indexed query checkpoint covers at most4096candidate records.
pub const STORE_PREVIEW_CHECKPOINTS: usize = 64;
/// Width of one aggregation column, points.
const AGG_PX: f32 = 2.0;
/// Lazy previews evaluate the kernel in batches of this many results.
const LAZY_BATCH: usize = 4096;

/// Coverage raster of the visible pitch rows (aggregated preview).
struct Coverage {
    lo: u8,
    rows: usize,
    cols: usize,
    /// Per row, `cols + 1` difference counts.
    diff: Vec<i32>,
    used: bool,
}

impl Coverage {
    fn new(g: &Geometry, lo: u8, hi: u8) -> Coverage {
        let cols = ((g.notes.width() / AGG_PX).ceil() as usize).max(1);
        let rows = usize::from(hi.saturating_sub(lo)) + 1;
        Coverage {
            lo,
            rows,
            cols,
            diff: Vec::new(),
            used: false,
        }
    }

    fn col(&self, g: &Geometry, tick: u64) -> isize {
        let x = g.view.time.tick_to_x(tick);
        (x / f64::from(AGG_PX))
            .floor()
            .clamp(-1.0, self.cols as f64) as isize
    }

    fn add(&mut self, g: &Geometry, start: u64, end: u64, pitch: u8) {
        if pitch < self.lo || usize::from(pitch - self.lo) >= self.rows {
            return;
        }
        if self.diff.is_empty() {
            self.diff = vec![0; self.rows * (self.cols + 1)];
        }
        let c0 = self.col(g, start).max(0) as usize;
        let c1 = self.col(g, end.max(start + 1)).max(0) as usize;
        if c0 >= self.cols {
            return;
        }
        let c1 = c1.clamp(c0, self.cols - 1);
        let row = usize::from(pitch - self.lo) * (self.cols + 1);
        self.diff[row + c0] += 1;
        self.diff[row + c1 + 1] -= 1;
        self.used = true;
    }

    /// Merged runs of covered columns per row (a darker fill where denser).
    fn mesh(&self, g: &Geometry, base: Color32, gap: f32) -> Mesh {
        let mut mesh = Mesh::default();
        if !self.used {
            return mesh;
        }
        for r in 0..self.rows {
            let pitch = self.lo + r as u8;
            let row = &self.diff[r * (self.cols + 1)..(r + 1) * (self.cols + 1)];
            let y = note_rect(g, 0, 1, pitch);
            let (mut acc, mut run): (i32, Option<(usize, i32)>) = (0, None);
            for (c, d) in row.iter().enumerate().take(self.cols) {
                acc += d;
                match (acc > 0, run) {
                    (true, None) => run = Some((c, acc)),
                    (true, Some((a, m))) => run = Some((a, m.max(acc))),
                    (false, Some((a, m))) => {
                        Self::push(&mut mesh, g, y, a, c, m, base, gap);
                        run = None;
                    }
                    (false, None) => {}
                }
            }
            if let Some((a, m)) = run {
                Self::push(&mut mesh, g, y, a, self.cols, m, base, gap);
            }
        }
        mesh
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        mesh: &mut Mesh,
        g: &Geometry,
        y: Rect,
        a: usize,
        b: usize,
        depth: i32,
        base: Color32,
        gap: f32,
    ) {
        let x0 = g.notes.left() + a as f32 * AGG_PX;
        let x1 = (g.notes.left() + b as f32 * AGG_PX).min(g.notes.right());
        let k = (depth as f32).ln_1p() / 4.0;
        let fill = base.gamma_multiply((0.45 + k).min(0.95));
        mesh.add_colored_rect(
            Rect::from_min_max(Pos2::new(x0, y.min.y), Pos2::new(x1, y.max.y - gap)),
            fill,
        );
    }
}

/// v0.7 tool preview: the result notes that can be visible (from the computed
/// run, or a pointwise kernel over the viewport). The first
/// [`PREVIEW_DRAW_BUDGET`] are drawn as notes, the rest aggregated (up to
/// [`PREVIEW_AGG_BUDGET`] per frame). Returns (drawn, simplified).
#[allow(clippy::too_many_arguments)]
pub fn draw_tool_preview(
    p: &Painter,
    g: &Geometry,
    track: plumspirit_engine::editor::HostView<'_>,
    tp: &super::ToolPreview<'_>,
    pal: &Palette,
    dark: bool,
    subs: &mut crate::ui::theme::SubPalettes,
    cache: &mut Option<ToolPreviewCache>,
) -> (usize, bool) {
    use plumspirit_engine::transform::{Output, TNote};
    let palette = crate::ui::theme::note_palette(p.ctx());
    let rev = match tp {
        super::ToolPreview::Run { rev, .. }
        | super::ToolPreview::Lazy { rev, .. }
        | super::ToolPreview::Store { rev, .. } => *rev,
    };
    let v = &g.view;
    let key = (
        rev,
        match tp {
            super::ToolPreview::Store { notes, .. } => notes.content_revision(),
            _ => track.notes.revision(),
        },
        [
            v.time.origin_tick,
            v.time.pixels_per_tick.to_bits(),
            v.pitch.scroll_rows.to_bits(),
            v.pitch.row_height.to_bits(),
            u64::from(v.pitch.top),
        ],
        [
            g.notes.left().to_bits(),
            g.notes.top().to_bits(),
            g.notes.width().to_bits(),
            g.notes.height().to_bits(),
        ],
        dark,
        // B1-0092: the colours (incl. the Selection Opacity) are cached too.
        palette.key(),
    );
    if rev != 0
        && let Some(c) = cache.as_ref()
        && c.key == key
    {
        p.add(Shape::Mesh(std::sync::Arc::clone(&c.coverage)));
        p.add(Shape::Mesh(std::sync::Arc::clone(&c.notes)));
        preview_label(p, g, pal, c.cut, c.aggregated);
        return (c.drawn, c.cut);
    }
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
    let gap = if g.view.pitch.row_height >= 6.0 {
        1.0
    } else {
        0.0
    };
    let mut mesh = Mesh::default();
    let mut prims = 0usize;
    let mut drawn = 0usize;
    let mut visited = 0usize;
    let mut cut = false;
    let mut cov = Coverage::new(g, lo, hi);
    // Returns false once the per-frame aggregation budget is spent.
    let mut draw = |q: &TNote, mesh: &mut Mesh, prims: &mut usize, cov: &mut Coverage| -> bool {
        if q.pitch < lo || q.pitch > hi || q.start > t1 || q.end() < t0 {
            return true;
        }
        visited += 1;
        if visited > PREVIEW_AGG_BUDGET {
            return false;
        }
        if drawn >= PREVIEW_DRAW_BUDGET {
            cov.add(g, q.start, q.end(), q.pitch);
            return true;
        }
        let sp = subs.get(&palette, q.sub, dark);
        let (fill, border) = NotePalette::selected_paint(sp, q.vel);
        let r = note_rect(g, q.start, q.end(), q.pitch);
        let r = clip_x(
            Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - gap)),
            g.notes,
        );
        push_note(mesh, r, fill, border, prims);
        drawn += 1;
        true
    };
    match tp {
        super::ToolPreview::Store { notes, .. } => {
            let mut checks = 0usize;
            let mut stopped = false;
            let result = notes.try_query_ranked(
                t0,
                t1,
                || {
                    checks += 1;
                    if checks > STORE_PREVIEW_CHECKPOINTS {
                        Err(())
                    } else {
                        Ok(())
                    }
                },
                |_, v| {
                    let more = draw(
                        &TNote::from_record(&v.record(), 0),
                        &mut mesh,
                        &mut prims,
                        &mut cov,
                    );
                    stopped |= !more;
                    Ok(more)
                },
            );
            cut = stopped || result.is_err();
        }
        super::ToolPreview::Run {
            notes,
            extra,
            max_dur,
            ..
        } => {
            let a = notes.partition_point(|n| n.start < t0.saturating_sub(*max_dur));
            let b = extra.partition_point(|n| n.start < t0.saturating_sub(*max_dur));
            for q in notes[a..].iter().chain(&extra[b..]) {
                if q.start > t1 {
                    continue;
                }
                if !draw(q, &mut mesh, &mut prims, &mut cov) {
                    cut = true;
                    break;
                }
            }
        }
        super::ToolPreview::Lazy {
            kernel,
            boundary,
            slot,
            all,
            ..
        } => {
            // The kernel runs over the source notes that can reach the view,
            // in batches (no buffer of every visible result).
            let (a, b) = kernel.source_window(t0, t1);
            let mut out = Output::new(*boundary, 1 << 20);
            let mut stop = false;
            track.notes.query(a, b, |_, v| {
                if stop || (!*all && !v.selected) {
                    return;
                }
                kernel.map(&TNote::from_record(&v.record(), *slot), &mut out);
                if out.notes.len() >= LAZY_BATCH {
                    for q in &out.notes {
                        if !draw(q, &mut mesh, &mut prims, &mut cov) {
                            stop = true;
                            break;
                        }
                    }
                    out.notes.clear();
                }
            });
            if !stop {
                for q in &out.notes {
                    if !draw(q, &mut mesh, &mut prims, &mut cov) {
                        stop = true;
                        break;
                    }
                }
            }
            cut = stop;
        }
    }
    let coverage = std::sync::Arc::new(cov.mesh(g, palette.selection_paint(pal).0, gap));
    let notes = std::sync::Arc::new(mesh);
    p.add(Shape::Mesh(std::sync::Arc::clone(&coverage)));
    p.add(Shape::Mesh(std::sync::Arc::clone(&notes)));
    preview_label(p, g, pal, cut, cov.used);
    if rev != 0 {
        *cache = Some(ToolPreviewCache {
            key,
            coverage,
            notes,
            drawn,
            cut,
            aggregated: cov.used,
        });
    }
    (drawn, cut)
}

fn preview_label(p: &Painter, g: &Geometry, pal: &Palette, cut: bool, aggregated: bool) {
    if cut || aggregated {
        p.text(
            g.notes.right_top() + Vec2::new(-8.0, 8.0),
            Align2::RIGHT_TOP,
            if cut {
                "Preview simplified (zoom in to see every note)"
            } else {
                "Dense preview aggregated (zoom in to see every note)"
            },
            egui::FontId::proportional(11.0),
            pal.text_weak,
        );
    }
}

#[cfg(test)]
mod tests {

    /// v0.9 §057 / §229: the 88-Key Guide lines bound keys 21–108 in a
    /// standard and a 256-key view alike (the same keys, never cropped).
    #[test]
    fn the_88_key_guide_bounds_a0_to_c8_in_both_key_ranges() {
        use plumspirit_engine::view::PitchAxis;
        for top in [127u8, 255] {
            let a = PitchAxis {
                scroll_rows: 0.0,
                row_height: 6.0,
                top,
            };
            let [below_a0, above_c8] = key_guide_ys(&a);
            assert_eq!(a.y_to_pitch(below_a0 - 1.0), Some(21));
            assert_eq!(a.y_to_pitch(below_a0 + 1.0), Some(20));
            assert_eq!(a.y_to_pitch(above_c8 + 1.0), Some(108));
            assert_eq!(a.y_to_pitch(above_c8 - 1.0), Some(109));
        }
    }

    use super::*;
    use crate::piano_roll::viewport::RollView;
    use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
    use plumspirit_engine::view::PitchAxis;
    use std::time::Duration;

    fn track_pass<'a>(store: &'a NoteStore, pal: &'a Palette, dark: bool) -> NotePass<'a> {
        NotePass {
            store,
            key: 1,
            pal,
            dark,
            palette: crate::ui::theme::default_note_palette(),
            hover: None,
            move_preview: None,
            resize_preview: None,
            erasing: None,
            hover_color: pal.note_hover_border,
            previewing: false,
            preview_all: false,
            parts: &[],
        }
    }

    /// v1.0-Launch L-G07 (owner addition): grouped notes — two groups, some
    /// notes selected — are drawn exactly like the same notes ungrouped:
    /// the same meshes, and no rectangle (stroke or corner mark) around a
    /// group, at a close and a far exact zoom.
    #[test]
    fn grouped_notes_have_no_bounding_box_overlay() {
        let store_with = |grouped: bool| {
            let recs: Vec<NoteRecord> = (0..40u64)
                .map(|i| NoteRecord {
                    start_tick: i * 30,
                    duration_ticks: 60,
                    attrs: NoteAttrs::new(60 + (i % 7) as u8, 100, 0, 64, NoteOffKind::NoteOff)
                        .unwrap(),
                    sub: plumspirit_engine::model::SubchannelId(0),
                    group: if grouped { 1 + (i / 20) as u32 } else { 0 },
                })
                .collect();
            let mut s = NoteStore::from_sorted_records(&recs, false).unwrap();
            s.select_in_region(0, 300, 0, 127);
            s
        };
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        for ppt in [2.0f64, 0.4] {
            let view = RollView {
                time: TimeAxis::new(0, ppt),
                pitch: PitchAxis {
                    scroll_rows: 50.0,
                    row_height: 12.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            };
            let g = Geometry::new(full, view, 1.0);
            let draw = |store: &NoteStore| {
                let ctx = egui::Context::default();
                let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let p = ui.ctx().layer_painter(egui::LayerId::background());
                    let mut subs = crate::ui::theme::SubPalettes::default();
                    let mut stats = RenderStats::default();
                    let pass = track_pass(store, pal, true);
                    draw_exact(&p, &g, &pass, &mut subs, &mut stats, 0, 2_000, 0, 127, None);
                });
                fo.textures_delta.clear();
                let mut rects = 0;
                let mut verts = Vec::new();
                for c in fo.shapes {
                    match c.shape {
                        Shape::Mesh(m) => verts.extend(m.vertices.iter().map(|v| (v.pos, v.color))),
                        Shape::Rect(_) => rects += 1,
                        _ => {}
                    }
                }
                (verts, rects)
            };
            let (grouped, rects) = draw(&store_with(true));
            let (plain, _) = draw(&store_with(false));
            assert_eq!(rects, 0, "zoom {ppt}: no rectangle around a group");
            assert!(!grouped.is_empty());
            assert_eq!(
                grouped, plain,
                "zoom {ppt}: grouped notes look like plain ones"
            );
        }
    }

    /// §030 / §183 / §184 / §263: Dense First / Sparse First decide the
    /// final composite. One dense source (every texel of pitches 50–70) and
    /// 1k / 5k / 10k / 100k sparse ones over it, ranked by `OnionRanking`
    /// from visible counts: Dense First — the dense source owns every texel
    /// it covers; Sparse First — every texel a sparse note covers belongs to
    /// a sparse source, the dense one only shows where nothing sparse is.
    /// At several zoom levels and tiles (pans, seams).
    #[test]
    fn dense_and_sparse_first_win_the_final_composite() {
        check_dense_sparse_first(&[1_000]);
    }

    #[test]
    #[ignore = "scale test: run with --release --ignored (1k – 100k sources)"]
    fn dense_and_sparse_first_win_the_final_composite_at_scale() {
        check_dense_sparse_first(&[1_000, 5_000, 10_000, 100_000]);
    }

    fn check_dense_sparse_first(sizes: &[usize]) {
        use plumspirit_engine::model::project::{OnionOrder, OnionRanking};
        use plumspirit_engine::view::tiles::{self, Scratch};
        let span: u64 = 960 * 4 * 32;
        let rec = |start: u64, dur: u64, pitch: u8, sub: u32| NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: plumspirit_engine::model::SubchannelId(sub),
            group: 0,
        };
        for &n in sizes {
            let mut rng = plumspirit_engine::transform::rng::Rng::new(n as u64);
            // The dense source: 21 rows × back-to-back notes over the span.
            let dense_notes: Vec<NoteRecord> = (0..span / 400)
                .flat_map(|k| (50..=70u8).map(move |p| (k, p)))
                .map(|(k, p)| rec(k * 400, 400, p, (k % 16) as u32))
                .collect();
            let dense = NoteStore::from_sorted_records(&dense_notes, false).unwrap();
            let sparse: Vec<NoteStore> = (0..n)
                .map(|_| {
                    let mut v: Vec<NoteRecord> = (0..2)
                        .map(|_| {
                            rec(
                                rng.range_i64(0, span as i64 - 200) as u64,
                                rng.range_i64(20, 200) as u64,
                                rng.range_i64(50, 70) as u8,
                                rng.range_i64(0, 63) as u32,
                            )
                        })
                        .collect();
                    v.sort_by_key(|r| r.start_tick);
                    NoteStore::from_sorted_records(&v, false).unwrap()
                })
                .collect();
            // Source list: the dense one in the middle of the project order.
            let mid = n / 2;
            let mut stores: Vec<&NoteStore> = sparse.iter().collect();
            stores.insert(mid, &dense);
            let ids: Vec<TrackId> = (0..stores.len()).map(|i| TrackId(i as u32 + 1)).collect();
            let counts: Vec<usize> = stores
                .iter()
                .map(|s| s.count_starting_in(0, span))
                .collect();
            let idx: Vec<usize> = (0..stores.len()).collect();
            for order in [OnionOrder::DenseFirst, OnionOrder::SparseFirst] {
                let mut ranking = OnionRanking::default();
                let ord = ranking.order(order, &idx, &ids, &counts);
                let srcs: Vec<OnionSource<'_>> = ord
                    .iter()
                    .map(|&i| OnionSource {
                        id: ids[i],
                        store: stores[i],
                        color: None,
                        place: SourcePlace::IDENTITY,
                    })
                    .collect();
                let dense_at = ord.iter().position(|&i| i == mid).unwrap();
                let pass = OnionPass {
                    palette: crate::ui::theme::default_note_palette(),
                    sources: &srcs,
                    dark: true,
                    alpha: 255,
                    outlines: Some(75),
                };
                let colors = onion_tile_colors(&pass);
                let opt = onion_tile_options(&pass, 5.0, KeyRange::Standard);
                let refs: Vec<&NoteStore> = srcs.iter().map(|s| s.store).collect();
                let sparse_refs: Vec<&NoteStore> = refs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != dense_at)
                    .map(|(_, s)| *s)
                    .collect();
                let mut sc = Scratch::default();
                let mut cover = Scratch::default();
                let mut sparse_texels = 0usize;
                for (level, tiles_) in [(-6, 0..2), (-2, 3..6), (1, 40..42)] {
                    for index in tiles_ {
                        tiles::build_tile(&refs, &colors, level, index, opt, &mut sc, &|| false)
                            .unwrap();
                        tiles::build_tile(
                            &sparse_refs,
                            &colors,
                            level,
                            index,
                            opt,
                            &mut cover,
                            &|| false,
                        )
                        .unwrap();
                        for row in (tiles::ROWS - 1 - 70)..=(tiles::ROWS - 1 - 50) {
                            for col in 0..tiles::TILE_COLS {
                                let w = sc.winner(row, col).expect("the dense source covers it");
                                let sparse_here = cover.winner(row, col).is_some();
                                sparse_texels += usize::from(sparse_here);
                                match order {
                                    OnionOrder::DenseFirst => assert_eq!(
                                        w.source, dense_at,
                                        "{n} sources, dense first, level {level} tile {index}"
                                    ),
                                    _ => assert_eq!(
                                        w.source == dense_at,
                                        !sparse_here,
                                        "{n} sources, sparse first, level {level} tile {index}"
                                    ),
                                }
                            }
                        }
                    }
                }
                assert!(
                    sparse_texels > 1_000,
                    "{n}: sparse notes overlap the dense one: {sparse_texels}"
                );
            }
        }
    }

    /// §025 / §186 / §264: ghost opacity 0 / 25 / 50 / 75 / 100 % — the
    /// composited alpha of a dense ghost texel equals the exact path's: one
    /// note at exactly the opacity (100 % = fully opaque), n stacked notes as
    /// n stacked translucent rectangles, 1 − (1 − a)^n; no hidden density
    /// factor.
    #[test]
    fn ghost_opacity_is_the_same_in_dense_tiles_as_on_the_exact_path() {
        use plumspirit_engine::view::tiles::{self, Scratch};
        for pct in [0u32, 25, 50, 75, 100] {
            let alpha = (pct * 255 / 100) as u8; // app/mod.rs: opacity % → alpha
            for stacked in [1u64, 2, 3, 40] {
                let s = NoteStore::from_sorted_records(
                    &(0..stacked)
                        .map(|_| NoteRecord {
                            start_tick: 10,
                            duration_ticks: 30,
                            attrs: NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                            sub: plumspirit_engine::model::SubchannelId(0),
                            group: 0,
                        })
                        .collect::<Vec<_>>(),
                    false,
                )
                .unwrap();
                let srcs = [OnionSource {
                    id: TrackId(3),
                    store: &s,
                    color: None,
                    place: SourcePlace::IDENTITY,
                }];
                let pass = OnionPass {
                    palette: crate::ui::theme::default_note_palette(),
                    sources: &srcs,
                    dark: true,
                    alpha,
                    outlines: Some(75),
                };
                // The exact path: each ghost is one rectangle of this colour.
                let mut tints = crate::ui::theme::GhostTints::default();
                let tint = crate::ui::theme::track_color(3, None, true, 255);
                let exact =
                    tints.get_with_alpha(crate::ui::theme::default_note_palette(), tint, 0, alpha)
                        [100 >> 3];
                let a = f64::from(exact.to_srgba_unmultiplied()[3]) / 255.0;
                let want = (255.0 * (1.0 - (1.0 - a).powi(stacked as i32))).round() as u8;
                let colors = onion_tile_colors(&pass);
                let opt = onion_tile_options(&pass, 5.0, KeyRange::Standard);
                let mut sc = Scratch::default();
                let t = tiles::build_tile(&[&s], &colors, 0, 0, opt, &mut sc, &|| false).unwrap();
                let px = t.pixels[(tiles::ROWS - 1 - 60) * tiles::TILE_COLS + 25];
                assert_eq!(px[3], want, "{pct} %, {stacked} stacked: interior alpha");
                if stacked == 1 {
                    // Uploaded as unmultiplied texels (`ColorImage::
                    // from_rgba_unmultiplied`): on the GPU the very colour the
                    // exact path draws.
                    let [r, g, b, a] = px;
                    assert_eq!(
                        Color32::from_rgba_unmultiplied(r, g, b, a),
                        exact,
                        "{pct} %: the exact ghost color"
                    );
                }
                if pct == 100 {
                    assert_eq!(px[3], 255, "100 % is fully opaque");
                }
            }
        }
    }

    /// B2-D02 / §121: an exact ghost's outline has the colour the tiles give
    /// its row borders — the source colour at the outline darkness — at every
    /// opacity (the exact path once shaded the premultiplied tint, so at the
    /// default 35 % its outlines were about three times darker than the
    /// dense ones).
    #[test]
    fn exact_ghost_outlines_match_the_tile_borders_at_every_opacity() {
        use plumspirit_engine::view::tiles::{self, Scratch};
        let s = NoteStore::from_sorted_records(
            &[NoteRecord {
                start_tick: 10,
                duration_ticks: 400,
                attrs: NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: plumspirit_engine::model::SubchannelId(0),
                group: 0,
            }],
            false,
        )
        .unwrap();
        let srcs = [OnionSource {
            id: TrackId(3),
            store: &s,
            color: None,
            place: SourcePlace::IDENTITY,
        }];
        let g = Geometry::new(
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0)),
            RollView {
                time: TimeAxis::new(0, 1.0),
                pitch: PitchAxis {
                    scroll_rows: 127.0 - 70.0,
                    row_height: 12.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            },
            1.0,
        );
        let mut seen = Vec::new();
        for pct in [35u32, 50, 100] {
            let alpha = (pct * 255 / 100) as u8;
            let pass = OnionPass {
                palette: crate::ui::theme::default_note_palette(),
                sources: &srcs,
                dark: true,
                alpha,
                outlines: Some(75),
            };
            // The exact path: one ghost, so exact rectangles; its outline is
            // the opaque colour in the mesh.
            let ctx = egui::Context::default();
            let (mut cache, mut dense) = (OnionCache::default(), DenseRenderer::default());
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                let mut stats = RenderStats::default();
                dense.begin_frame();
                draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
                assert_eq!(stats.onion_dense.visible_tiles, 0, "exact path");
            });
            out.textures_delta.clear();
            let edge: Vec<Color32> = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Mesh(m) => Some(m.vertices.iter().map(|v| v.color)),
                    _ => None,
                })
                .flatten()
                .filter(|c| c.a() == 255)
                .collect();
            let opt = onion_tile_options(&pass, 12.0, KeyRange::Standard);
            let mut sc = Scratch::default();
            let t = tiles::build_tile(
                &[&s],
                &onion_tile_colors(&pass),
                0,
                0,
                opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            let [r, gr, b, a] = t.borders[(tiles::ROWS - 1 - 60) * tiles::TILE_COLS + 200];
            let tile = Color32::from_rgba_unmultiplied(r, gr, b, a);
            assert!(!edge.is_empty(), "{pct} %: outlines drawn");
            if pct < 100 {
                assert!(
                    edge.iter().all(|c| *c == tile),
                    "{pct} %: exact outline {:?} vs tile border {tile:?}",
                    edge[0]
                );
            } else {
                assert!(edge.contains(&tile), "100 %: {tile:?} in {:?}", &edge[..4]);
            }
            seen.push(tile);
        }
        assert!(seen.windows(2).all(|w| w[0] == w[1]), "{seen:?}");
    }

    /// v1.0-beta1 B1-0150 (contract §022-A, image13): the Onion opacity
    /// applies to a ghost's interior only — its dark border stays opaque on
    /// both paths (the exact path's edges and the tiles' edge texels use
    /// `outline_alpha`, B2-D02 also the row borders); 0 % hides the ghost,
    /// border included.
    #[test]
    fn ghost_borders_stay_opaque_while_the_onion_opacity_applies_to_the_interior() {
        use plumspirit_engine::view::tiles::{self, Scratch};
        assert_eq!(outline_alpha(0), 0);
        let s = NoteStore::from_sorted_records(
            &[NoteRecord {
                start_tick: 10,
                duration_ticks: 400,
                attrs: NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: plumspirit_engine::model::SubchannelId(0),
                group: 0,
            }],
            false,
        )
        .unwrap();
        for pct in [0u32, 10, 25, 50, 75, 100] {
            let alpha = (pct * 255 / 100) as u8;
            // 0 % hides the ghost, its border included.
            let edge = if pct == 0 { 0 } else { 255 };
            assert_eq!(outline_alpha(alpha), edge, "{pct} %");
            let srcs = [OnionSource {
                id: TrackId(3),
                store: &s,
                color: None,
                place: SourcePlace::IDENTITY,
            }];
            let pass = OnionPass {
                palette: crate::ui::theme::default_note_palette(),
                sources: &srcs,
                dark: true,
                alpha,
                outlines: Some(75),
            };
            let opt = onion_tile_options(&pass, 5.0, KeyRange::Standard);
            let mut sc = Scratch::default();
            let t = tiles::build_tile(
                &[&s],
                &onion_tile_colors(&pass),
                0,
                0,
                opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            let row = (tiles::ROWS - 1 - 60) * tiles::TILE_COLS;
            // Level 0: one texel per tick; the note covers texels 10..410.
            assert_eq!(
                t.pixels[row + 10][3],
                edge,
                "{pct} %: the border texel is opaque"
            );
            assert_eq!(
                t.pixels[row + 200][3],
                alpha,
                "{pct} %: the interior keeps the opacity"
            );
            // B2-D02: the row's top / bottom lines (the border texture) too.
            assert_eq!(
                t.borders[row + 200][3],
                edge,
                "{pct} %: the row border is opaque"
            );
        }
    }

    /// §027 / §120: Onion tiles are outlined at the chosen darkness when
    /// outlines are on, with tops / bottoms only where rows are ≥ 3 px tall.
    #[test]
    fn dense_outline_options_follow_the_exact_rules() {
        let s = NoteStore::new();
        let srcs = [OnionSource {
            id: TrackId(1),
            store: &s,
            color: None,
            place: SourcePlace::IDENTITY,
        }];
        let mut o = OnionPass {
            palette: crate::ui::theme::default_note_palette(),
            sources: &srcs,
            dark: true,
            alpha: 255,
            outlines: Some(80),
        };
        // B2-D02: the tiles carry border colours and never texel-high
        // tops / bottoms, at every row height (no rebuild while zooming Y).
        for rh in [2.5, 5.0, 12.0] {
            let t = onion_tile_options(&o, rh, KeyRange::Standard)
                .outline
                .unwrap();
            assert_eq!(
                (t.darkness, t.vertical, t.borders, t.selection),
                (80, false, true, None)
            );
        }
        assert_eq!(
            onion_tile_options(&o, 2.5, KeyRange::Standard),
            onion_tile_options(&o, 12.0, KeyRange::Standard)
        );
        // The rows follow the exact rules: borders from 3 pt (Onion) / 4 pt
        // (track) of note height, a 1-point gap from 6 pt rows, the opaque
        // Onion inset from 5 pt.
        use dense::RowStyle;
        let onion =
            |rh: f64, opaque: bool| RowStyle::for_rows(rh, opaque.then_some(5.0), Some(3.0));
        assert!(!onion(2.5, false).borders);
        assert_eq!(
            onion(3.0, false),
            RowStyle {
                gap: 0.0,
                inset: 0.0,
                borders: true,
                grid: None,
            }
        );
        assert_eq!(
            onion(6.0, false),
            RowStyle {
                gap: 1.0,
                inset: 0.0,
                borders: true,
                grid: None,
            }
        );
        assert_eq!(
            onion(6.0, true),
            RowStyle {
                gap: 1.0,
                inset: 1.0,
                borders: true,
                grid: None,
            }
        );
        let track = |rh: f64| RowStyle::for_rows(rh, None, Some(4.0));
        assert!(!track(3.0).borders && track(4.0).borders && track(5.0).gap == 0.0);
        assert_eq!(
            track(6.0),
            RowStyle {
                gap: 1.0,
                inset: 0.0,
                borders: true,
                grid: None,
            }
        );
        assert!(!track(4.5).borders || track(4.5).gap == 0.0);
        assert!(
            !RowStyle::for_rows(12.0, None, None).borders,
            "Onion outlines off"
        );
        o.outlines = None;
        assert!(
            onion_tile_options(&o, 5.0, KeyRange::Standard)
                .outline
                .is_none()
        );
        let t = track_tile_options(KeyRange::Standard).outline.unwrap();
        assert!(!t.vertical && t.borders);
    }

    /// v0.9 §029: a note keeps its exact colours across the dense threshold —
    /// the dense texel inside a note has the exact renderer's fill (opaque,
    /// never pastel), its edge texels the exact border, for every subchannel,
    /// velocity, selection and mute state; §026: the active dense pass is
    /// outlined; §119: at any Selection Opacity (selected fills and borders
    /// come from the same tables on both paths).
    #[test]
    fn dense_notes_keep_their_exact_fill_and_border() {
        use crate::ui::theme::{NoteColorMode, NoteColorPrefs, NotePalette};
        use plumspirit_engine::view::tiles::{self, Scratch};
        // v0.9 §068: with any note palette — PlumSpirit's own, a seed, Manual.
        let bases = [
            NotePalette::default(),
            NotePalette::new(NoteColorPrefs {
                seed: 11,
                ..NoteColorPrefs::default()
            }),
            NotePalette::new(NoteColorPrefs {
                mode: NoteColorMode::Manual,
                seed: 0,
                manual: vec![[230, 30, 30], [30, 200, 90], [40, 70, 230]],
            }),
        ];
        let palettes: Vec<(NotePalette, u8)> = bases
            .iter()
            .flat_map(|p| [100u8, 50, 0].map(|o| (p.clone().with_selection(o), o)))
            .collect();
        let mut fills = Vec::new();
        for (palette, opacity, dark) in palettes
            .iter()
            .flat_map(|(p, o)| [true, false].map(|d| (p, *o, d)))
        {
            let pal = crate::ui::theme::palette(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            // One 30-tick note per row: (sub, velocity, selected, muted).
            let cases: Vec<(u32, u8, bool, bool)> = [0u32, 1, 5, 63, 64, 300]
                .iter()
                .flat_map(|&sub| {
                    [1u8, 64, 100, 127].iter().flat_map(move |&v| {
                        [(false, false), (true, false), (false, true), (true, true)]
                            .map(|(sel, m)| (sub, v, sel, m))
                    })
                })
                .take(128)
                .collect();
            let mut store = NoteStore::new();
            for (i, &(sub, v, _, muted)) in cases.iter().enumerate() {
                let attrs = NoteAttrs::new(i as u8, v, 0, 64, NoteOffKind::NoteOff)
                    .unwrap()
                    .with_muted(muted);
                store
                    .insert(
                        NoteRecord {
                            start_tick: 10,
                            duration_ticks: 30,
                            attrs,
                            sub: plumspirit_engine::model::SubchannelId(sub),
                            group: 0,
                        },
                        false,
                    )
                    .unwrap();
            }
            for (i, &(_, _, sel, _)) in cases.iter().enumerate() {
                if sel {
                    store.select_in_region(10, 10, i as u8, i as u8);
                }
            }
            let mut pass = track_pass(&store, pal, dark);
            pass.palette = palette;
            let colors = track_tile_colors(&pass);
            let opt = track_tile_options(KeyRange::Standard);
            let o = opt
                .outline
                .expect("the active dense pass is outlined (§026)");
            let mut sc = Scratch::default();
            let t = tiles::build_tile(&[&store], &colors, 0, 0, opt, &mut sc, &|| false).unwrap();
            let mut subs = crate::ui::theme::SubPalettes::default();
            let vp0 = crate::ui::theme::velocity_palette(palette, dark);
            for (i, &(sub, v, sel, muted)) in cases.iter().enumerate() {
                let n = store
                    .iter()
                    .find(|n| usize::from(n.attrs.pitch()) == i)
                    .expect("note");
                assert_eq!((n.selected, n.attrs.muted()), (sel, muted));
                let sp = if sub == 0 {
                    vp0.as_ref()
                } else {
                    subs.get(palette, sub, dark)
                };
                let (fill, border) = exact_paint(pal, sp, &n);
                let row = tiles::ROWS - 1 - i;
                let inner = t.pixels[row * tiles::TILE_COLS + 25];
                let edge = t.pixels[row * tiles::TILE_COLS + 10];
                let want = fill.to_srgba_unmultiplied();
                let case = format!(
                    "palette {} selection {opacity} % dark {dark} sub {sub} vel {v} selected {sel} muted {muted}",
                    palette.key()
                );
                if dark && !sel && !muted && v == 100 && sub == 5 && opacity == 100 {
                    fills.push(want);
                }
                assert_eq!(inner[..3], want[..3], "fill, {case}");
                assert_eq!(inner[3], want[3], "opacity, {case}");
                // Selected notes' borders are per note (no single colour).
                assert_eq!(o.selection, None);
                assert_eq!(
                    edge[..3],
                    [border.r(), border.g(), border.b()],
                    "border, {case}"
                );
                // B2-D02: the top / bottom border the roll draws from the
                // tile is the exact renderer's border too.
                let b = t.borders[row * tiles::TILE_COLS + 25];
                assert_eq!(
                    b[..3],
                    [border.r(), border.g(), border.b()],
                    "row border, {case}"
                );
            }
        }
        // The palettes really colour differently.
        assert_eq!(fills.len(), 3);
        assert!(
            fills[0] != fills[1] && fills[1] != fills[2] && fills[0] != fills[2],
            "{fills:?}"
        );
    }

    /// §119: the Onion tiles' identity ignores the view (scroll, zoom,
    /// playhead) and changes with notes, colours, order and options.
    #[test]
    fn onion_tiles_are_keyed_by_content_not_by_the_view() {
        let a = NoteStore::from_sorted_records(
            &[NoteRecord {
                start_tick: 0,
                duration_ticks: 10,
                attrs: NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: plumspirit_engine::model::SubchannelId(0),
                group: 0,
            }],
            false,
        )
        .unwrap();
        let b = a.clone();
        let src = |order: [usize; 2], color: Option<[u8; 3]>| -> Vec<OnionSource<'_>> {
            order
                .iter()
                .map(|&i| OnionSource {
                    id: TrackId(i as u32 + 1),
                    store: if i == 0 { &a } else { &b },
                    color: if i == 0 { color } else { None },
                    place: SourcePlace::IDENTITY,
                })
                .collect()
        };
        let base = src([0, 1], None);
        let pass = |s: &[OnionSource<'_>], alpha: u8, outlines: Option<u8>| {
            onion_family(
                &OnionPass {
                    palette: crate::ui::theme::default_note_palette(),
                    sources: s,
                    dark: true,
                    alpha,
                    outlines,
                },
                5.0,
                KeyRange::Standard,
            )
        };
        let f = pass(&base, 90, Some(75));
        // The only view input is whether pitch rows are tall enough for top /
        // bottom outlines: any scroll / time zoom / playhead gives `f`.
        // Content and options change it.
        assert_eq!(f, pass(&src([0, 1], None), 90, Some(75)));
        let o = OnionPass {
            palette: crate::ui::theme::default_note_palette(),
            sources: &base,
            dark: true,
            alpha: 90,
            outlines: Some(75),
        };
        assert_eq!(
            onion_family(&o, 5.0, KeyRange::Standard),
            onion_family(&o, 12.0, KeyRange::Standard)
        );
        assert_ne!(
            onion_family(&o, 5.0, KeyRange::Standard),
            onion_family(&o, 5.0, KeyRange::Extended256),
            "256-Key Mode builds 256-row tiles"
        );
        // B2-D02: row borders are drawn at paint time, so crossing the 3 px
        // border threshold no longer rebuilds the tiles.
        assert_eq!(
            onion_family(&o, 5.0, KeyRange::Standard),
            onion_family(&o, 2.0, KeyRange::Standard),
            "row height crosses 3 px"
        );
        assert_ne!(f, pass(&src([1, 0], None), 90, Some(75)), "layer order");
        assert_ne!(
            f,
            pass(&src([0, 1], Some([1, 2, 3])), 90, Some(75)),
            "colour"
        );
        assert_ne!(f, pass(&base, 91, Some(75)), "opacity");
        assert_ne!(f, pass(&base, 90, Some(76)), "outline darkness");
        assert_ne!(f, pass(&base, 90, None), "outlines off");
        let mut edited = a.clone();
        edited.select_in_region(0, 100, 0, 127);
        edited.remove_selected();
        let s2 = vec![
            OnionSource {
                id: TrackId(1),
                store: &edited,
                color: None,
                place: SourcePlace::IDENTITY,
            },
            OnionSource {
                id: TrackId(2),
                store: &b,
                color: None,
                place: SourcePlace::IDENTITY,
            },
        ];
        assert_ne!(f, pass(&s2, 90, Some(75)), "note revision");
    }

    /// Draws `sources` as Onion ghosts until complete (tiles built inline);
    /// returns the vertices of the exact path's meshes and the stats.
    fn onion_frames(
        sources: &[OnionSource<'_>],
        g: &Geometry,
    ) -> (Vec<(Pos2, Color32)>, RenderStats) {
        let ctx = egui::Context::default();
        let pass = OnionPass {
            palette: crate::ui::theme::default_note_palette(),
            sources,
            dark: true,
            alpha: 90,
            outlines: Some(75),
        };
        let mut cache = OnionCache::default();
        let mut dense = DenseRenderer::new(true);
        let mut out = (Vec::new(), RenderStats::default());
        for frame in 0..50 {
            let mut building = false;
            let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                let mut stats = RenderStats::default();
                dense.begin_frame();
                building = draw_onion(&p, ui.ctx(), g, &pass, &mut cache, &mut dense, &mut stats);
                out.1 = stats;
            });
            full.textures_delta.clear();
            out.0 = full
                .shapes
                .into_iter()
                .filter_map(|c| match c.shape {
                    Shape::Mesh(m) => Some(m),
                    _ => None,
                })
                .flat_map(|m| m.vertices.clone())
                .map(|v| (v.pos, v.color))
                .collect();
            // Inline builds land after the frame that asked for them.
            if !building && frame > 0 {
                break;
            }
        }
        out
    }

    /// L-G05: the Onion's notes are counted only until the view is surely
    /// dense (15,000 sources were each asked every frame). Many sources:
    /// the count stops early and says so — the same dense picture as one
    /// source holding every note; few notes in view: the exact count.
    #[test]
    fn the_onion_count_stops_once_the_view_is_surely_dense() {
        let rec = |t: u64, p: u8| NoteRecord {
            start_tick: t,
            duration_ticks: 100,
            attrs: NoteAttrs::new(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: plumspirit_engine::model::SubchannelId(0),
            group: 0,
        };
        let stores: Vec<NoteStore> = (0..400u64)
            .map(|k| {
                let v: Vec<NoteRecord> = (0..200u64)
                    .map(|j| rec(j * 500 + k, 20 + ((j + k) % 90) as u8))
                    .collect();
                NoteStore::from_sorted_records(&v, false).unwrap()
            })
            .collect();
        let sources: Vec<OnionSource<'_>> = stores
            .iter()
            .enumerate()
            .map(|(k, store)| OnionSource {
                id: TrackId(k as u32 + 1),
                store,
                color: None,
                place: SourcePlace::IDENTITY,
            })
            .collect();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = |origin: u64, ppt: f64| RollView {
            time: TimeAxis::new(origin, ppt),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 5.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        // The whole song (80,000 notes): dense, counted partly.
        let (_, st) = onion_frames(&sources, &Geometry::new(full, view(0, 0.01), 1.0));
        assert!(st.onion_dense.visible_tiles > 0, "dense");
        assert!(st.onion_estimate_partial);
        let limit = crate::piano_roll::dense::ENTER_DENSE;
        assert!(
            st.onion_estimate > limit && st.onion_estimate <= limit + 200,
            "{}",
            st.onion_estimate
        );
        // A few notes in view: exact, counted fully.
        let (v, st) = onion_frames(&sources, &Geometry::new(full, view(0, 2.0), 1.0));
        assert_eq!(st.onion_dense.visible_tiles, 0, "exact");
        assert!(!st.onion_estimate_partial && st.onion_estimate > 0);
        assert!(!v.is_empty());
    }

    /// L-S49: a new Onion family (opacity, outlines, colour, one source
    /// edited) shares the store clones of the sources whose notes did not
    /// change with the families still held instead of cloning every store
    /// again (1,431 sources: 35-59 ms on the owner's PC); an edited source
    /// gets a clone of its new notes, and the picture is the one a renderer
    /// that never saw the earlier families draws.
    #[test]
    fn a_new_onion_family_shares_the_clones_of_unchanged_sources() {
        let rec = |t: u64, p: u8| NoteRecord {
            start_tick: t,
            duration_ticks: 100,
            attrs: NoteAttrs::new(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: plumspirit_engine::model::SubchannelId(0),
            group: 0,
        };
        let mut stores: Vec<NoteStore> = (0..3u64)
            .map(|k| {
                let v: Vec<NoteRecord> = (0..6_000u64)
                    .map(|j| rec(j * 20 + k, 20 + ((j + k) % 90) as u8))
                    .collect();
                NoteStore::from_sorted_records(&v, false).unwrap()
            })
            .collect();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let g = Geometry::new(
            full,
            RollView {
                time: TimeAxis::new(0, 1200.0 / 120_000.0),
                pitch: PitchAxis {
                    scroll_rows: 0.0,
                    row_height: 5.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            },
            1.0,
        );
        let ctx = egui::Context::default();
        let mut cache = OnionCache::default();
        let mut dense = DenseRenderer::new(true);
        // Draws until the tiles are built; the family's sources and the
        // meshes' vertices.
        let draw = |dense: &mut DenseRenderer,
                    cache: &mut OnionCache,
                    stores: &[NoteStore],
                    alpha: u8,
                    outlines: Option<u8>| {
            let sources: Vec<OnionSource<'_>> = stores
                .iter()
                .enumerate()
                .map(|(k, store)| OnionSource {
                    id: TrackId(k as u32 + 1),
                    store,
                    color: None,
                    place: SourcePlace::IDENTITY,
                })
                .collect();
            let pass = OnionPass {
                palette: crate::ui::theme::default_note_palette(),
                sources: &sources,
                dark: true,
                alpha,
                outlines,
            };
            let mut vertices = Vec::new();
            for frame in 0..50 {
                let mut building = false;
                let mut stats = RenderStats::default();
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let p = ui.ctx().layer_painter(egui::LayerId::background());
                    dense.begin_frame();
                    building = draw_onion(&p, ui.ctx(), &g, &pass, cache, dense, &mut stats);
                });
                out.textures_delta.clear();
                assert!(stats.onion_dense.visible_tiles > 0, "dense");
                vertices = out
                    .shapes
                    .into_iter()
                    .filter_map(|c| match c.shape {
                        Shape::Mesh(m) => Some(m),
                        _ => None,
                    })
                    .flat_map(|m| m.vertices.clone())
                    .map(|v| (v.pos, v.color))
                    .collect::<Vec<_>>();
                if !building && frame > 0 {
                    break;
                }
            }
            (dense.current_stores(Role::Onion), vertices)
        };
        let (first, _) = draw(&mut dense, &mut cache, &stores, 90, Some(75));
        assert_eq!(first.len(), 3);
        // Opacity and outlines: every source shared.
        let (second, _) = draw(&mut dense, &mut cache, &stores, 91, Some(75));
        let (third, _) = draw(&mut dense, &mut cache, &stores, 91, None);
        for k in 0..3 {
            assert!(Arc::ptr_eq(&first[k], &second[k]), "source {k}");
            assert!(Arc::ptr_eq(&first[k], &third[k]), "source {k}");
        }
        // Source 1 edited: a clone of its new notes; 0 and 2 shared.
        stores[1].select_in_region(0, 40_000, 0, 127);
        stores[1].remove_selected();
        let (fourth, shown) = draw(&mut dense, &mut cache, &stores, 91, None);
        assert!(Arc::ptr_eq(&first[0], &fourth[0]));
        assert!(Arc::ptr_eq(&first[2], &fourth[2]));
        assert!(!Arc::ptr_eq(&first[1], &fourth[1]));
        assert_eq!(fourth[1].len(), stores[1].len());
        assert_eq!(fourth[1].content_revision(), stores[1].content_revision());
        assert!(fourth[1].len() < first[1].len());
        // The same picture as a renderer that never saw the earlier ones.
        let (_, fresh) = draw(
            &mut DenseRenderer::new(true),
            &mut OnionCache::default(),
            &stores,
            91,
            None,
        );
        assert!(!shown.is_empty());
        assert_eq!(shown, fresh);
    }

    /// v0.9 V09-S11 step 4 (§042): an Onion source drawn through a clip
    /// placement looks exactly like a copy of the notes the clip plays,
    /// moved to where it plays — on the exact path (the same rectangles and
    /// colours) and as dense tiles (the same picture) — with the window
    /// dropping the notes the clip does not play, and a placement before the
    /// view drawing nothing.
    #[test]
    fn placed_onion_sources_draw_where_their_clips_play() {
        let rec = |t: u64, p: u8| NoteRecord {
            start_tick: t,
            duration_ticks: 100,
            attrs: NoteAttrs::new(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: plumspirit_engine::model::SubchannelId((t / 480 % 3) as u32),
            group: 0,
        };
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = |ppt: f64| RollView {
            time: TimeAxis::new(1_000, ppt),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 5.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        // Clip: content ticks 480 ..< 1,440 shown from song tick 2,000.
        let place = SourcePlace {
            shift: 2_000 - 480,
            window: (480, 1_440),
        };
        for (n, ppt) in [(8u64, 0.25), (80_000, 0.01)] {
            let content: Vec<NoteRecord> = (0..n)
                .map(|k| rec(k * 480 / (n / 8), 50 + (k % 20) as u8))
                .collect();
            let store = NoteStore::from_sorted_records(&content, false).unwrap();
            let played: Vec<NoteRecord> = content
                .iter()
                .filter_map(|r| {
                    place.start(r.start_tick).map(|t| NoteRecord {
                        start_tick: t,
                        ..*r
                    })
                })
                .collect();
            assert!(!played.is_empty() && played.len() < content.len());
            let copy = NoteStore::from_sorted_records(&played, false).unwrap();
            let g = Geometry::new(full, view(ppt), 1.0);
            let src = |store: &NoteStore, place: SourcePlace| {
                onion_frames(
                    &[OnionSource {
                        id: TrackId(4),
                        store,
                        color: None,
                        place,
                    }],
                    &g,
                )
            };
            let (pv, ps) = src(&store, place);
            let (cv, cs) = src(&copy, SourcePlace::IDENTITY);
            if n == 8 {
                assert_eq!(ps.onion_dense.visible_tiles, 0, "exact path");
                assert!(!pv.is_empty());
                assert_eq!(pv, cv, "the same ghost rectangles");
                assert_eq!(ps.onion_primitives, cs.onion_primitives);
            } else {
                assert!(ps.onion_dense.visible_tiles > 0, "dense path");
                assert_eq!((ps.onion_dense.stand_ins, ps.onion_dense.coarse), (0, 0));
                assert_ne!(ps.onion_dense.picture, 0);
                assert_eq!(
                    ps.onion_dense.picture, cs.onion_dense.picture,
                    "the same tiles"
                );
            }
            // A placement wholly before the view: nothing of it is drawn.
            let gone = SourcePlace {
                shift: -1_000_000_000,
                window: (0, u64::MAX),
            };
            let (gv, gs) = src(&store, gone);
            assert!(gv.is_empty() && gs.onion_estimate == 0, "{n}");
        }
    }

    /// x extent (min, max) of a tessellated shape, in points.
    fn tessellated_x(shape: Shape, ppp: f32) -> (f32, f32) {
        let mut t = egui::epaint::Tessellator::new(
            ppp,
            egui::epaint::TessellationOptions::default(),
            [1, 1],
            Vec::new(),
        );
        let mut mesh = Mesh::default();
        t.tessellate_shape(shape, &mut mesh);
        assert!(!mesh.vertices.is_empty());
        mesh.vertices
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.pos.x), hi.max(v.pos.x))
            })
    }

    /// §249: at several DPI scales, zooms and scroll positions the ruler
    /// triangle's centre and the playhead / cursor line (as egui tessellates
    /// them) are at the same X — to within 1/100 of a physical pixel.
    #[test]
    fn playhead_triangle_and_line_share_one_x_at_every_dpi_zoom_and_scroll() {
        let mut checked = 0;
        for ppp in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            for ppt in [0.013, 0.37, 1.0, 4.2] {
                for origin in [0u64, 123, 99_999] {
                    for tick in [origin, origin + 77, origin + 1_001] {
                        let time = TimeAxis::new(origin, ppt);
                        let raw = 52.3 + time.tick_to_x(tick) as f32;
                        for pts in [PLAYHEAD_WIDTH, CURSOR_WIDTH] {
                            let w = marker_width(pts, ppp);
                            // Whole physical pixels.
                            assert!(((w * ppp) - (w * ppp).round()).abs() < 1e-4);
                            let x = marker_x(raw, w, ppp);
                            assert!(
                                (x - raw).abs() * ppp <= 0.5 + 1e-3,
                                "moved by at most half a pixel"
                            );
                            let (lo, hi) = tessellated_x(
                                Shape::vline(x, 0.0..=100.0, Stroke::new(w, Color32::WHITE)),
                                ppp,
                            );
                            let line = (lo + hi) / 2.0;
                            let (tlo, thi) = tessellated_x(
                                Shape::convex_polygon(
                                    vec![
                                        Pos2::new(x - 5.0, 12.0),
                                        Pos2::new(x + 5.0, 12.0),
                                        Pos2::new(x, 20.0),
                                    ],
                                    Color32::WHITE,
                                    Stroke::NONE,
                                ),
                                ppp,
                            );
                            let tri = (tlo + thi) / 2.0;
                            assert!(
                                (line - tri).abs() * ppp < 0.01,
                                "ppp {ppp} ppt {ppt} origin {origin} tick {tick}: line {line} triangle {tri}"
                            );
                            assert!((line - x).abs() * ppp < 0.01);
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 7 * 4 * 3 * 3 * 2);
    }

    /// `tracks` stores with `per_track` notes each spread over `span` ticks.
    fn stores(tracks: usize, per_track: usize, span: u64) -> Vec<NoteStore> {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        (0..tracks)
            .map(|_| {
                let mut recs: Vec<NoteRecord> = (0..per_track)
                    .map(|_| NoteRecord {
                        start_tick: next() % span,
                        duration_ticks: 60 + next() % 400,
                        attrs: NoteAttrs::new(
                            (next() % 128) as u8,
                            100,
                            0,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                        sub: plumspirit_engine::model::SubchannelId::MAIN,
                        group: 0,
                    })
                    .collect();
                recs.sort_by_key(|r| r.start_tick);
                NoteStore::from_sorted_records(&recs, false).unwrap()
            })
            .collect()
    }

    /// Runs `draw_onion` frame by frame until the ghost raster is complete.
    /// Returns (frames, slowest frame, total).
    fn render_until_done(stores: &[NoteStore], span: u64) -> (usize, Duration, Duration) {
        let ctx = egui::Context::default();
        let sources: Vec<OnionSource<'_>> = stores
            .iter()
            .enumerate()
            .map(|(i, s)| OnionSource {
                id: TrackId(i as u32 + 1),
                store: s,
                color: None,
                place: SourcePlace::IDENTITY,
            })
            .collect();
        let pass = OnionPass {
            palette: crate::ui::theme::default_note_palette(),
            sources: &sources,
            dark: true,
            alpha: 90,
            outlines: Some(75),
        };
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = RollView {
            // The whole span across the width, every pitch visible.
            time: TimeAxis::new(0, 1200.0 / span as f64),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 5.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let mut cache = OnionCache::default();
        let mut dense = DenseRenderer::default();
        let mut frames = 0;
        let mut slowest = Duration::ZERO;
        let mut total = Duration::ZERO;
        loop {
            let mut building = false;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                let mut stats = RenderStats::default();
                let t = Instant::now();
                dense.begin_frame();
                building = draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
                let e = t.elapsed();
                slowest = slowest.max(e);
                total += e;
            });
            frames += 1;
            if !building || frames > 10_000 {
                break;
            }
        }
        (frames, slowest, total)
    }

    /// B1-0059: what each Rendering Quality costs — the steady frame (draw +
    /// tessellation) of N visible notes drawn exactly vs as dense tiles.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit quality_frame_cost -- --ignored --nocapture --test-threads=1"]
    fn quality_frame_cost() {
        use super::super::dense::RenderQuality;
        let span = 960 * 4 * 64;
        for n in [6_000usize, 12_000, 24_000, 48_000] {
            let s = stores(1, n, span);
            for q in [RenderQuality::Fast, RenderQuality::Detailed] {
                let ctx = egui::Context::default();
                let sources = [OnionSource {
                    id: TrackId(1),
                    store: &s[0],
                    color: None,
                    place: SourcePlace::IDENTITY,
                }];
                let pass = OnionPass {
                    palette: crate::ui::theme::default_note_palette(),
                    sources: &sources,
                    dark: true,
                    alpha: 90,
                    outlines: Some(75),
                };
                let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
                let view = RollView {
                    time: TimeAxis::new(0, 1200.0 / span as f64),
                    pitch: PitchAxis {
                        scroll_rows: 0.0,
                        row_height: 5.0,
                        top: plumspirit_engine::model::note::MAX_PITCH,
                    },
                };
                let g = Geometry::new(full, view, 1.0);
                let mut cache = OnionCache::default();
                let mut dense = DenseRenderer::default();
                dense.set_quality(q);
                let mut times = Vec::new();
                let mut lod = false;
                for frame in 0..400 {
                    let t = Instant::now();
                    let mut building = false;
                    let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                        let p = ui.ctx().layer_painter(egui::LayerId::background());
                        let mut stats = RenderStats::default();
                        dense.begin_frame();
                        building =
                            draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
                        lod = stats.onion_dense.visible_tiles > 0;
                    });
                    let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
                    let e = t.elapsed();
                    if building {
                        std::thread::sleep(Duration::from_millis(2));
                    } else if frame > 2 {
                        times.push(e);
                        if times.len() == 30 {
                            break;
                        }
                    }
                }
                times.sort();
                let med = times.get(times.len() / 2).copied().unwrap_or_default();
                println!(
                    "{n:>6} visible notes, {q:?} ({}): {} , median frame {:>7.3} ms",
                    q.exact_limit(),
                    if lod {
                        "dense tiles"
                    } else {
                        "exact rectangles"
                    },
                    med.as_secs_f64() * 1e3
                );
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn onion_frame_cost_with_many_tracks() {
        for (tracks, per_track) in [(16, 100_000), (200, 10_000), (1_000, 1_000), (2_000, 50)] {
            let span = 960 * 4 * 64; // 64 bars at 960 PPQN
            let s = stores(tracks, per_track, span);
            let (frames, slowest, total) = render_until_done(&s, span);
            println!(
                "onion {tracks:>5} tracks x {per_track:>7} notes: {frames:>3} frame(s), slowest {:>7.2} ms, total {:>7.2} ms",
                slowest.as_secs_f64() * 1e3,
                total.as_secs_f64() * 1e3
            );
        }
    }

    /// Draws a dense preview (`n` results over 16 bars, whole span visible)
    /// as a computed run and as a lazy kernel: (drawn, cut, time) for each.
    fn tool_preview_frames(n: usize) -> Vec<(usize, bool, Duration)> {
        use plumspirit_engine::transform::tools::Pointwise;
        use plumspirit_engine::transform::{Boundary, Growth, Output, TNote};
        let span = 960 * 4 * 16;
        let mut out = Vec::new();
        let notes: Vec<TNote> = (0..n)
            .map(|i| TNote {
                start: (i as u64 * 7919) % span,
                dur: 240,
                pitch: (i % 120) as u8,
                vel: 100,
                ..TNote::default()
            })
            .collect();
        let mut sorted = notes.clone();
        sorted.sort_by_key(|q| q.start);
        let mut doc = plumspirit_engine::model::PlumDocument::new_project(
            "p".into(),
            plumspirit_engine::model::Bpm::DEFAULT,
            960,
        );
        let recs: Vec<NoteRecord> = sorted.iter().map(|q| q.to_record()).collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = RollView {
            time: TimeAxis::new(0, 1200.0 / span as f64),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 5.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let k = Pointwise {
            f: |n: &TNote, out: &mut Output| {
                out.push(*n);
            },
            ahead: 0,
            behind: 0,
            growth: Growth::Map,
        };
        let previews = [
            super::super::ToolPreview::Run {
                notes: &sorted,
                extra: &[],
                max_dur: 240,
                all: false,
                rev: 1,
            },
            super::super::ToolPreview::Lazy {
                kernel: &k,
                boundary: Boundary::default(),
                slot: 0,
                all: false,
                rev: 2,
            },
        ];
        for tp in &previews {
            let mut res = (0, false);
            let mut el = Duration::ZERO;
            let mut cache = None;
            for _ in 0..2 {
                let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let p = ui.ctx().layer_painter(egui::LayerId::background());
                    let mut subs = crate::ui::theme::SubPalettes::default();
                    let t = Instant::now();
                    res = draw_tool_preview(
                        &p,
                        &g,
                        doc.track(0).unwrap().into(),
                        tp,
                        pal,
                        true,
                        &mut subs,
                        &mut cache,
                    );
                    el = t.elapsed();
                });
                fo.textures_delta.clear();
                out.push((res.0, res.1, el));
            }
        }
        out
    }

    /// A dense preview: the first PREVIEW_DRAW_BUDGET results are notes, the
    /// rest aggregate into the coverage raster; Run and Lazy previews agree.
    #[test]
    fn dense_tool_previews_draw_a_bounded_number_of_notes_and_aggregate_the_rest() {
        for (drawn, cut, el) in tool_preview_frames(300_000) {
            assert_eq!(drawn, PREVIEW_DRAW_BUDGET, "notes drawn");
            assert!(!cut, "under the aggregation budget nothing is cut");
            // Debug builds are slow; the bound only guards against per-note
            // allocation or quadratic work.
            assert!(el < Duration::from_secs(5), "{el:?}");
        }
    }

    /// v0.9 §084 / §085: the Ctrl / Ctrl+Shift corner-gesture preview (the
    /// lazy `CornerKernel` over the selection, rev 0 = redrawn every frame as
    /// while dragging) next to the identity kernel, over the whole song and
    /// zoomed in to one bar: (label, drawn, cut, time).
    fn corner_preview_frames(n: usize) -> Vec<(String, usize, bool, Duration)> {
        use plumspirit_engine::transform::geom::{Corner, CornerWarp, GeomBox, WarpKind};
        use plumspirit_engine::transform::tools::Pointwise;
        use plumspirit_engine::transform::{Boundary, Growth, Output, TNote};
        let span = 960 * 4 * 16u64;
        let mut recs: Vec<NoteRecord> = (0..n)
            .map(|i| {
                TNote {
                    start: (i as u64 * 7919) % span,
                    dur: 240,
                    pitch: (i % 120) as u8,
                    vel: 100,
                    ..TNote::default()
                }
                .to_record()
            })
            .collect();
        recs.sort_by_key(|r| r.start_tick);
        let mut doc = plumspirit_engine::model::PlumDocument::new_project(
            "c".into(),
            plumspirit_engine::model::Bpm::DEFAULT,
            960,
        );
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        let track = doc.track(0).unwrap();
        let b = track.notes.selection_bounds().unwrap();
        let bx = GeomBox {
            t0: b.min_start,
            t1: b.max_end,
            p0: b.min_pitch,
            p1: b.max_pitch,
        };
        let keys = doc.project().keys;
        let skew = CornerWarp {
            kind: WarpKind::SkewX,
            corner: Corner::TopRight,
            dt: 3840,
            dr: 0,
        }
        .kernel(bx, None, keys);
        let persp = CornerWarp {
            kind: WarpKind::Perspective,
            corner: Corner::TopRight,
            dt: 7680,
            dr: 6,
        }
        .kernel(bx, None, keys);
        let ident = Pointwise {
            f: |n: &TNote, out: &mut Output| {
                out.push(*n);
            },
            ahead: 0,
            behind: 0,
            growth: Growth::Map,
        };
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let mut out = Vec::new();
        for (view_name, ppt) in [
            ("whole song", 1200.0 / span as f64),
            ("one bar", 1200.0 / 3840.0),
        ] {
            let view = RollView {
                time: TimeAxis::new(span / 2, ppt),
                pitch: PitchAxis {
                    scroll_rows: 0.0,
                    row_height: 5.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            };
            let g = Geometry::new(full, view, 1.0);
            let scale = plumspirit_engine::transform::geom::BoxScale {
                from: bx,
                to: GeomBox {
                    t1: bx.t1 + 3840,
                    p1: bx.p1 + 6,
                    ..bx
                },
                fill: None,
                prevent: false,
                cleanup: Default::default(),
            };
            let kernels: [(&str, &dyn plumspirit_engine::transform::PointKernel); 4] = [
                ("identity", &ident),
                ("scale handles", &scale),
                ("Ctrl skew", &skew),
                ("Ctrl+Shift perspective", &persp),
            ];
            for (name, k) in kernels {
                let tp = super::super::ToolPreview::Lazy {
                    kernel: k,
                    boundary: Boundary::default(),
                    slot: 0,
                    all: false,
                    rev: 0,
                };
                let mut res = (0, false);
                let mut el = Duration::MAX;
                for _ in 0..3 {
                    let mut cache = None;
                    let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                        let p = ui.ctx().layer_painter(egui::LayerId::background());
                        let mut subs = crate::ui::theme::SubPalettes::default();
                        let t = Instant::now();
                        res = draw_tool_preview(
                            &p,
                            &g,
                            track.into(),
                            &tp,
                            pal,
                            true,
                            &mut subs,
                            &mut cache,
                        );
                        el = el.min(t.elapsed());
                    });
                    fo.textures_delta.clear();
                }
                out.push((format!("{name}, {view_name}"), res.0, res.1, el));
            }
        }
        out
    }

    /// The corner-gesture previews draw like any lazy tool preview: bounded
    /// notes, the rest aggregated (debug build: only a gross bound).
    #[test]
    fn corner_gesture_previews_are_bounded_like_tool_previews() {
        for (label, drawn, _, el) in corner_preview_frames(200_000) {
            assert!(drawn <= PREVIEW_DRAW_BUDGET, "{label}: {drawn}");
            assert!(el < Duration::from_secs(5), "{label}: {el:?}");
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn corner_gesture_preview_frame_cost() {
        for (label, drawn, cut, el) in corner_preview_frames(1_000_000) {
            println!(
                "corner preview {label}: {drawn} drawn, cut {cut}, {:.2} ms",
                el.as_secs_f64() * 1e3
            );
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn tool_preview_frame_cost() {
        for n in [100_000usize, 1_000_000] {
            let kinds = ["run", "run (cached)", "lazy", "lazy (cached)"];
            for (kind, (drawn, cut, el)) in kinds.iter().zip(tool_preview_frames(n)) {
                println!(
                    "tool preview {kind:>13}, {n:>8} results visible: {drawn} drawn, cut {cut}, {:.2} ms",
                    el.as_secs_f64() * 1e3
                );
            }
        }
    }

    /// The colours of every mesh vertex a frame painted.
    fn painted_colors(fo: &egui::FullOutput) -> std::collections::HashSet<Color32> {
        let mut out = std::collections::HashSet::new();
        for c in &fo.shapes {
            if let Shape::Mesh(m) = &c.shape {
                out.extend(m.vertices.iter().map(|v| v.color));
            }
        }
        out
    }

    /// v1.0-beta1 B1-0092 (owner: "Selection Opacity … sometimes shows the
    /// new opacity on some tools while others maintain the default"): a tool
    /// Preview draws its notes with the user's Selection Opacity — the same
    /// selected fill and border as a selected note — and recolours when the
    /// opacity changes, although its cached meshes are otherwise reused.
    #[test]
    fn tool_previews_take_the_selection_opacity_and_recolour_when_it_changes() {
        use plumspirit_engine::transform::TNote;
        let notes: Vec<TNote> = (0..8u64)
            .map(|i| TNote {
                start: i * 480,
                dur: 240,
                pitch: 60 + i as u8,
                vel: 100,
                ..TNote::default()
            })
            .collect();
        let doc = plumspirit_engine::model::PlumDocument::new_project(
            "p".into(),
            plumspirit_engine::model::Bpm::DEFAULT,
            960,
        );
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = RollView {
            time: TimeAxis::new(0, 0.25),
            pitch: PitchAxis {
                scroll_rows: 40.0,
                row_height: 12.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let tp = super::super::ToolPreview::Run {
            notes: &notes,
            extra: &[],
            max_dur: 240,
            all: false,
            rev: 7,
        };
        let mut cache = None;
        let mut seen = Vec::new();
        for opacity in [100u8, 50, 20, 50] {
            let palette = NotePalette::default().with_selection(opacity);
            crate::ui::theme::set_note_palette(&ctx, std::sync::Arc::new(palette.clone()));
            let want = {
                let mut subs = crate::ui::theme::SubPalettes::default();
                NotePalette::selected_paint(subs.get(&palette, 0, true), 100)
            };
            let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                let mut subs = crate::ui::theme::SubPalettes::default();
                let (drawn, _) = draw_tool_preview(
                    &p,
                    &g,
                    doc.track(0).unwrap().into(),
                    &tp,
                    pal,
                    true,
                    &mut subs,
                    &mut cache,
                );
                assert_eq!(drawn, notes.len());
            });
            let colors = painted_colors(&fo);
            fo.textures_delta.clear();
            assert!(colors.contains(&want.0), "{opacity} %: selected fill");
            assert!(colors.contains(&want.1), "{opacity} %: selected border");
            seen.push(want);
        }
        assert_ne!(seen[0], seen[1], "100 % and 50 % differ");
        assert_ne!(seen[1], seen[2], "50 % and 20 % differ");
        assert_eq!(seen[1], seen[3], "back to 50 %: the same look");
    }

    /// The resolver without a subchannel colour: 0 % is the plain note look,
    /// 100 % the full selected look, and pasted / inserted notes use it.
    #[test]
    fn the_plain_selection_paint_follows_the_opacity_and_draws_added_notes() {
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let p0 = NotePalette::default()
            .with_selection(0)
            .selection_paint(pal);
        let p100 = NotePalette::default()
            .with_selection(100)
            .selection_paint(pal);
        assert_eq!(p0, (pal.note_fill, pal.note_border));
        assert_eq!(p100, (pal.note_selected_fill, pal.note_selected_border));
        let half = NotePalette::default().with_selection(50);
        let ctx = egui::Context::default();
        crate::ui::theme::set_note_palette(&ctx, std::sync::Arc::new(half.clone()));
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let view = RollView {
            time: TimeAxis::new(0, 0.25),
            pitch: PitchAxis {
                scroll_rows: 40.0,
                row_height: 12.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let rec = NoteRecord::new(
            0,
            480,
            NoteAttrs::new(70, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        );
        let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            draw_added(&p, &g, &[], &[rec], 480, pal);
        });
        let colors = painted_colors(&fo);
        fo.textures_delta.clear();
        let want = half.selection_paint(pal);
        assert!(
            colors.contains(&want.0) && colors.contains(&want.1),
            "{colors:?}"
        );
    }

    /// L-S46 (Windows, Import Into Track of 224M notes with Preview: 12.7 s
    /// a frame): the import preview stops reading once its draw budget is
    /// drawn, and after `PREVIEW_AGG_BUDGET` notes outside the visible rows;
    /// what it draws is the budget, never more.
    #[test]
    fn the_import_preview_stops_reading_once_its_budget_is_drawn() {
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let view = RollView {
            time: TimeAxis::new(0, 0.25),
            pitch: PitchAxis {
                scroll_rows: 40.0,
                row_height: 12.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let (lo, hi) = g.view.pitch.visible_pitches(f64::from(g.notes.height()));
        let store = |n: usize, pitch: u8| {
            let recs: Vec<NoteRecord> = (0..n as u64)
                .map(|k| {
                    NoteRecord::new(
                        k % 400,
                        60,
                        NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let mut recs = recs;
            recs.sort_by_key(|r| r.start_tick);
            let mut s = NoteStore::new();
            s.insert_sorted_batch(&recs, false).unwrap();
            s
        };
        let vertices = |stores: &[&NoteStore]| -> (usize, bool) {
            let mut cut = false;
            let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                cut = draw_added(&p, &g, stores, &[], 60, pal);
            });
            fo.textures_delta.clear();
            let v = fo
                .shapes
                .iter()
                .map(|c| match &c.shape {
                    egui::Shape::Mesh(m) => m.vertices.len(),
                    _ => 0,
                })
                .sum();
            (v, cut)
        };
        let one = store(1, lo);
        let (per_note, cut) = vertices(&[&one]);
        assert!(per_note > 0 && !cut);
        let seen = store(PREVIEW_DRAW_BUDGET, lo);
        let more = store(PREVIEW_DRAW_BUDGET, hi);
        let (v, cut) = vertices(&[&seen, &more, &seen]);
        assert!(cut, "simplified");
        assert_eq!(
            v,
            per_note * PREVIEW_DRAW_BUDGET,
            "exactly the budget is drawn"
        );
        // Notes outside the visible rows: reading stops after the read
        // budget, before the visible store after them.
        assert!(hi < 127, "a row above the view exists");
        let hidden = store(PREVIEW_AGG_BUDGET + 1, hi + 1);
        let (v, cut) = vertices(&[&hidden, &seen]);
        assert!(cut);
        assert_eq!(v, 0, "the read budget ends the walk");
    }

    /// B1-0092: the batch-tool Preview (`NoteKernel`: Flip, Scale, Quantize,
    /// Gate, Velocity, …) draws the same selected look as a tool Preview.
    #[test]
    fn batch_tool_previews_take_the_selection_opacity() {
        use plumspirit_engine::editor::preview::{NoteKernel, PreviewScope};
        let mut doc = plumspirit_engine::model::PlumDocument::new_project(
            "p".into(),
            plumspirit_engine::model::Bpm::DEFAULT,
            960,
        );
        let recs: Vec<NoteRecord> = (0..8u64)
            .map(|i| {
                NoteRecord::new(
                    i * 480,
                    240,
                    NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 700.0));
        let view = RollView {
            time: TimeAxis::new(0, 0.25),
            pitch: PitchAxis {
                scroll_rows: 40.0,
                row_height: 12.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        let g = Geometry::new(full, view, 1.0);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        for opacity in [20u8, 50, 100] {
            let palette = NotePalette::default().with_selection(opacity);
            crate::ui::theme::set_note_palette(&ctx, std::sync::Arc::new(palette.clone()));
            let want = {
                let mut subs = crate::ui::theme::SubPalettes::default();
                NotePalette::selected_paint(subs.get(&palette, 0, true), 90)
            };
            let mut fo = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                let mut subs = crate::ui::theme::SubPalettes::default();
                let (n, _) = draw_preview(
                    &p,
                    &g,
                    doc.track(0).unwrap().into(),
                    &NoteKernel::SetVelocity(90),
                    PreviewScope::Selected,
                    pal,
                    true,
                    &mut subs,
                );
                assert_eq!(n, 8);
            });
            let colors = painted_colors(&fo);
            fo.textures_delta.clear();
            assert!(
                colors.contains(&want.0) && colors.contains(&want.1),
                "{opacity} %"
            );
        }
    }
}

/// v1.0-beta1 B1-S18 diagnostics: the roll's dense layers as an image.
#[cfg(test)]
mod picture {
    use super::*;
    use plumspirit_engine::view::tiles::{self, Scratch};

    /// `PLUMSPIRIT_ROLL_MIDI=song.mid PLUMSPIRIT_PNG=out.png cargo test --release -p
    /// plumspirit --bin PlumSpirit dense_roll_picture -- --ignored --nocapture`:
    /// the densest two bars of the file as the dense roll composes them
    /// (dark theme, the densest track active, every other track as Onion at
    /// the default 35 % in project order, outlines at 75 %), one texel per
    /// tick column and 5 px per key row, over the roll background.
    #[test]
    #[ignore = "diagnostic picture of a real file (PLUMSPIRIT_ROLL_MIDI, PLUMSPIRIT_PNG)"]
    fn dense_roll_picture() {
        let (Ok(path), Ok(out)) = (
            std::env::var("PLUMSPIRIT_ROLL_MIDI"),
            std::env::var("PLUMSPIRIT_PNG"),
        ) else {
            println!("PLUMSPIRIT_ROLL_MIDI / PLUMSPIRIT_PNG not set");
            return;
        };
        let (doc, _) =
            plumspirit_engine::midi::import_file(std::path::Path::new(&path), Default::default())
                .unwrap();
        let bar = u64::from(doc.division().encoded()) * 4;
        let mut per_bar: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        for tr in doc.tracks() {
            for v in tr.notes.iter() {
                *per_bar.entry(v.start_tick / bar).or_default() += 1;
            }
        }
        let first = std::env::var("PLUMSPIRIT_BAR")
            .ok()
            .and_then(|b| b.parse::<u64>().ok())
            .unwrap_or_else(|| {
                *per_bar
                    .iter()
                    .max_by_key(|(b, n)| (**n, std::cmp::Reverse(**b)))
                    .unwrap()
                    .0
            });
        let (t0, t1) = (first * bar, first * bar + 2 * bar);
        let active = (0..doc.tracks().len())
            .max_by_key(|&i| doc.tracks()[i].notes.estimate_in_range(t0, t1))
            .unwrap();
        let palette = crate::ui::theme::default_note_palette();
        let others: Vec<usize> = (0..doc.tracks().len()).filter(|&t| t != active).collect();
        let sources: Vec<OnionSource<'_>> = others
            .iter()
            .map(|&t| OnionSource {
                id: doc.tracks()[t].id(),
                store: &doc.tracks()[t].notes,
                color: doc.tracks()[t].props().onion_color,
                place: SourcePlace::IDENTITY,
            })
            .collect();
        let pass = OnionPass {
            sources: &sources,
            dark: true,
            palette,
            alpha: (35 * 255 / 100) as u8,
            outlines: Some(75),
        };
        let level = tiles::level_for(1100.0 / (t1 - t0) as f64);
        let (i0, i1) = tiles::tile_range(level, t0, t1);
        let onion_stores: Vec<&NoteStore> = sources.iter().map(|s| s.store).collect();
        let track_colors = dense::Colors {
            sources: vec![dense::SourceColors::new(dense::Tint::Subchannels {
                dark: true,
                palette: Arc::new(palette.clone()),
            })],
            selection: true,
            muted_fill: Some(crate::ui::theme::DARK.note_ghost.to_srgba_unmultiplied()),
            muted: 255,
            ranked: false,
        };
        let track_opt = TileOptions {
            keys: KeyRange::Standard,
            fold: None,
            window: None,
            outline: Some(Outline {
                alpha: 255,
                darkness: 75,
                selection: None,
                vertical: true,
                borders: false,
            }),
        };
        let mut sc = Scratch::default();
        let cols = (i1 - i0 + 1) as usize * tiles::TILE_COLS;
        let bg = crate::ui::theme::DARK.roll_bg.to_array();
        let mut img = vec![[bg[0], bg[1], bg[2]]; cols * tiles::ROWS];
        let over = |dst: &mut [u8; 3], src: [u8; 4]| {
            let a = u32::from(src[3]);
            for k in 0..3 {
                dst[k] = ((u32::from(src[k]) * a + u32::from(dst[k]) * (255 - a)) / 255) as u8;
            }
        };
        for i in i0..=i1 {
            let o = tiles::build_tile(
                &onion_stores,
                &onion_tile_colors(&pass),
                level,
                i,
                onion_tile_options(&pass, 5.0, KeyRange::Standard),
                &mut sc,
                &|| false,
            )
            .unwrap();
            let tr = tiles::build_tile(
                &[&doc.tracks()[active].notes],
                &track_colors,
                level,
                i,
                track_opt,
                &mut sc,
                &|| false,
            )
            .unwrap();
            for r in 0..tiles::ROWS {
                for c in 0..tiles::TILE_COLS {
                    let d = &mut img[r * cols + (i - i0) as usize * tiles::TILE_COLS + c];
                    over(d, o.pixels[r * tiles::TILE_COLS + c]);
                    over(d, tr.pixels[r * tiles::TILE_COLS + c]);
                }
            }
        }
        let scale = 5;
        let mut buf = image::RgbImage::new(cols as u32, (tiles::ROWS * scale) as u32);
        for (y, row) in buf.rows_mut().enumerate() {
            for (x, p) in row.enumerate() {
                *p = image::Rgb(img[(y / scale) * cols + x]);
            }
        }
        buf.save(&out).unwrap();
        println!(
            "{path}: bars {first}-{} (ticks {t0}..{t1}), active track {active}, {} Onion tracks, level {level}, tiles {i0}..={i1} -> {out}",
            first + 2,
            others.len()
        );
    }
}
