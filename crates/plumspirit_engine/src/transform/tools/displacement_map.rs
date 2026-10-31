//! Displacement Map (V09-S47, contract §139 – §143): an original MIDI
//! adaptation of a video displacement map (the parameter accounting is in
//! `docs/references/v0_9/DISPLACEMENT_MAP.md`). A **map field** moves notes
//! in **time** (horizontal) and **pitch** (vertical):
//!
//! * **Sources** (§140): the track below / above, a track number (tracks and
//!   subtracks alike), **Vice Versa** (the selection is the map and the map
//!   track's notes move — replaced, or kept with the moved copies added), or
//!   a **picture file** (§141; decoded by the app into [`Ctx::picture`]).
//! * **Track maps** (§143): the map notes are rasterised into a field of
//!   cells (the Map cell ticks × one row) per channel — **density** (how
//!   much of a cell notes cover), **velocity**, **subchannel** (its place in
//!   the track's table), **occupancy** — with the coverage kept apart:
//!   where no note lies the field is at the reference level, so a track map
//!   moves notes only near its own notes. Pictures give Luma, Red, Green,
//!   Blue or Alpha per pixel (row 0 at the top = the highest pitch).
//!   White / Gray / Black are constant channels.
//! * **Preprocessing** (§142): blur (a separable box blur of the field, two
//!   passes), black / white levels and gamma on the channel value.
//! * **Displacement**: value v (coverage-blended towards the reference r) →
//!   d = clamp(2 (v − r), −1, 1) × Maximum × Master — half a unit above the
//!   reference moves the full maximum; time in ticks, pitch in semitones.
//!   Each note moves through the shared geometry engine ([`Geom`]): sampled
//!   at its start or middle, or split along the field (Split raster) so long
//!   notes bend with it; Fill, pivot / scale / position apply too.
//! * **Map behaviour**: Auto (a track map at its own ticks and pitches, a
//!   picture stretched over the notes), Stretch to fit, Centre or Tile (a
//!   pixel / map cell per Map cell × row); a map offset in time and pitch.
//! * **View Map**: the chosen channel drawn as notes over the selection's
//!   box (velocity 1 – 127 from the field) instead of the result.
//!
//! Fields are bounded ([`MAX_FIELD_CELLS`]: a long track map coarsens its
//! cell; a large picture is reduced first) and built once per run in
//! O(notes + cells); each note samples them bilinearly in O(1).

use std::sync::Arc;

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Geom, GeomBox, geom_specs};
use super::super::image_midi::{Rgba, box_resize};
use super::super::param::*;
use super::super::preflight::{Estimate, Growth};
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::sweep::note_places;
use super::super::{
    Ctx, Kernel, Limits, Output, Params, Source, SourceStats, SourceTrack, TNote, TrackScope,
    TransformError,
};
use super::{NOTE_VALUES, boxed, note_value};
use crate::model::PlumDocument;

pub const DISPLACEMENT_MAP: ToolDef = ToolDef {
    id: "displacement_map",
    spec: dmap_spec,
    build: dmap_build,
};

/// The map sources (§140).
pub const SOURCES: &[&str] = &[
    "The track below",
    "The track above",
    "Track number… (a track or subtrack)",
    "Picture file",
];
/// Channels of a track map (§143) and the constant ones.
pub const TRACK_CHANNELS: &[&str] = &[
    "Density (how much of a cell notes cover)",
    "Velocity",
    "Subchannel (its place in the track's table)",
    "Occupancy (any note)",
    "White",
    "Gray",
    "Black",
];
/// Channels of a picture map and the constant ones.
pub const PICTURE_CHANNELS: &[&str] = &[
    "Luma", "Red", "Green", "Blue", "Alpha", "White", "Gray", "Black",
];
/// Map behaviours.
pub const BEHAVIOURS: &[&str] = &[
    "Auto (a track map at its own ticks and pitches, a picture stretched)",
    "Stretch to fit the notes",
    "Center (a pixel or map cell a cell)",
    "Tile (a pixel or map cell a cell)",
];
const VIEWS: &[&str] = &[
    "The result",
    "View Map: the time channel",
    "View Map: the pitch channel",
];
/// Most cells of one map field (2 M: 32 MB of channels).
pub const MAX_FIELD_CELLS: usize = 2 << 20;
/// Most cells View Map draws.
pub const MAX_VIEW_CELLS: u64 = 4 << 20;
/// What Vice Versa may add before the map track is read (its count is
/// known only then; `estimate_source` counts it).
const VICE_UNKNOWN: u64 = 50_000_000;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const DISPLACEMENT_MAP_HELP: &[(&str, &str)] = &[
    (
        "source",
        "Where the map comes from: the track below the selected one, the track above, a track by its number, or a picture file.",
    ),
    (
        "source.n",
        "The number of the track (or subtrack) in the track list whose notes make the map.",
    ),
    (
        "picture",
        "The picture file used as the map, when the map source is Picture File.",
    ),
    (
        "vice",
        "Turns it around: the selected notes make the map and the map track's notes are moved instead.",
    ),
    (
        "vice.keep",
        "With Vice Versa, keeps the map track's notes where they are and adds the moved copies.",
    ),
    (
        "behaviour",
        "How the map lies over the notes: Auto (a track at its own place, a picture stretched), stretched to fit, centered, or tiled.",
    ),
    (
        "res",
        "The size of one map cell in time, for track maps that are centered or tiled.",
    ),
    (
        "res.ticks",
        "The map cell's length in ticks, used when Map Cell is set to Custom Ticks.",
    ),
    ("map.dx", "Moves the map in time, in ticks."),
    ("map.dy", "Moves the map in pitch, in rows."),
    (
        "blur",
        "Softens the map by this many cells (or pixels), so the movement changes more smoothly.",
    ),
    (
        "black",
        "Map values at or below this level count as black (the darkest).",
    ),
    (
        "white",
        "Map values at or above this level count as white (the brightest).",
    ),
    (
        "gamma",
        "Bends the map's middle values: above 1 brightens them, below 1 darkens them.",
    ),
    (
        "h.track",
        "Which part of a track map moves the notes in time: how much notes cover, their velocity, their subchannel, whether any note is there, or a constant white, gray or black.",
    ),
    (
        "h.picture",
        "Which channel of a picture moves the notes in time: brightness, red, green, blue, transparency, or a constant white, gray or black.",
    ),
    (
        "h.max",
        "How far a fully white or black map moves a note in time, in ticks.",
    ),
    (
        "h.ref",
        "The map level that moves notes in time not at all; brighter moves them later, darker earlier.",
    ),
    (
        "v.track",
        "Which part of a track map moves the notes in pitch: how much notes cover, their velocity, their subchannel, whether any note is there, or a constant white, gray or black.",
    ),
    (
        "v.picture",
        "Which channel of a picture moves the notes in pitch: brightness, red, green, blue, transparency, or a constant white, gray or black.",
    ),
    (
        "v.max",
        "How far a fully white or black map moves a note in pitch, in semitones.",
    ),
    (
        "v.ref",
        "The map level that moves notes in pitch not at all; brighter moves them up, darker down.",
    ),
    (
        "master",
        "Scales the whole movement (negative moves the other way).",
    ),
    (
        "view",
        "Shows the moved notes, or draws the map itself (its time or its pitch channel) as notes to check it.",
    ),
];

fn dmap_spec() -> ToolSpec {
    let mut p = vec![
        choice("source", "Map source", SOURCES, 0),
        int("source.n", "Map track number", 1, 65_535, 1),
        text(
            "picture",
            "Picture file (PNG, JPEG, BMP, GIF, WebP, TIFF)",
            4096,
            "",
        ),
        flag(
            "vice",
            "Vice Versa (the selection is the map; the map track's notes move)",
            false,
        ),
        flag(
            "vice.keep",
            "Vice Versa: keep the map track's notes (add the moved copies)",
            false,
        ),
        choice("behaviour", "Map behavior", BEHAVIOURS, 0),
        choice(
            "res",
            "Map cell (track maps; Center / Tile)",
            NOTE_VALUES,
            10,
        ),
        ticks("res.ticks", "Custom cell (ticks)", 1, 1 << 24, 120),
        int(
            "map.dx",
            "Map offset in time (ticks)",
            -(1 << 40),
            1 << 40,
            0,
        ),
        int("map.dy", "Map offset in pitch (rows)", -255, 255, 0),
        int("blur", "Map blur (cells or pixels)", 0, 32, 0),
        float("black", "Map black level", 0.0, 100.0, 0.0, "%"),
        float("white", "Map white level", 0.0, 100.0, 100.0, "%"),
        float("gamma", "Map gamma", 0.1, 10.0, 1.0, ""),
        choice("h.track", "Time channel (track map)", TRACK_CHANNELS, 0),
        choice("h.picture", "Time channel (picture)", PICTURE_CHANNELS, 0),
        ticks(
            "h.max",
            "Maximum time displacement (ticks)",
            0,
            1 << 32,
            480,
        ),
        float("h.ref", "Time reference level", 0.0, 100.0, 50.0, "%"),
        choice("v.track", "Pitch channel (track map)", TRACK_CHANNELS, 1),
        choice("v.picture", "Pitch channel (picture)", PICTURE_CHANNELS, 0),
        int(
            "v.max",
            "Maximum pitch displacement (semitones)",
            0,
            255,
            12,
        ),
        float("v.ref", "Pitch reference level", 0.0, 100.0, 50.0, "%"),
        float("master", "Master displacement", -400.0, 400.0, 100.0, "%"),
        choice("view", "Show", VIEWS, 0),
    ];
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, DISPLACEMENT_MAP_HELP);
    ToolSpec {
        id: "displacement_map",
        name: "Displacement Map",
        family: Family::Geometry,
        summary: "Moves the selected notes in time and pitch by a map — another track's notes (density, velocity, subchannel or occupancy) or a picture — with blur, levels, gamma, reference levels and a master amount.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// Reads the map track's notes as auxiliary layer 0 (track sources), its
/// slot as the auxiliary destination (Vice Versa writes there). B2-F16: the
/// map track as the song shows it (its clips, at their places) on the edited
/// notes' ticks, counted from the track the edited clip sits on; each map
/// note keeps the slot of its own clip, so Vice Versa moves and erases it
/// there.
pub fn prepare(
    doc: &PlumDocument,
    scopes: &[TrackScope],
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    let source = p.choice("source");
    if source >= 3 {
        return Ok(());
    }
    let Some((active, shift)) = super::super::source::edit_place(scopes, ctx) else {
        return Ok(());
    };
    let base = match source {
        0 => active + 1,
        1 => active.wrapping_sub(1),
        _ => (p.int("source.n").max(1) - 1) as usize,
    };
    if base == active || doc.track(base).is_none() {
        return Err(TransformError::Invalid(
            "Choose a map track: another existing track than the one being edited.".into(),
        ));
    }
    let dest = super::super::source::placed_destination(doc, base, shift)
        .ok_or_else(|| TransformError::Invalid("The map track does not exist.".into()))?;
    let layer = src.placed_layer_slots(doc, base, limit, shift)?;
    let slot = match src
        .tracks
        .iter()
        .position(|t| t.host() == dest.host() && t.frame == dest.frame)
    {
        Some(s) => s,
        None => {
            src.tracks.push(dest);
            src.tracks.len() - 1
        }
    } as u16;
    src.aux = Arc::new(vec![layer]);
    src.aux_slot = Some(slot);
    Ok(())
}

/// A channel: a track channel, a picture channel, or a constant value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Chan {
    Density,
    Velocity,
    Subchannel,
    Occupancy,
    /// Luma, red, green, blue, alpha.
    Pixel(usize),
    Const(f64),
}

impl Chan {
    fn track(i: usize) -> Chan {
        match i {
            0 => Chan::Density,
            1 => Chan::Velocity,
            2 => Chan::Subchannel,
            3 => Chan::Occupancy,
            4 => Chan::Const(1.0),
            5 => Chan::Const(0.5),
            _ => Chan::Const(0.0),
        }
    }

    fn picture(i: usize) -> Chan {
        match i {
            0..=4 => Chan::Pixel(i),
            5 => Chan::Const(1.0),
            6 => Chan::Const(0.5),
            _ => Chan::Const(0.0),
        }
    }

    fn of_pixel(self, px: [u8; 4]) -> f32 {
        let [r, g, b, a] = px.map(|c| f32::from(c) / 255.0);
        match self {
            Chan::Pixel(0) => 0.2126 * r + 0.7152 * g + 0.0722 * b,
            Chan::Pixel(1) => r,
            Chan::Pixel(2) => g,
            Chan::Pixel(3) => b,
            _ => a,
        }
    }
}

/// A map field on a grid: per cell each channel's value premultiplied by
/// its coverage, and the coverage (0 – 1); row 0 is the lowest.
#[derive(Clone, Debug)]
struct Field {
    cols: usize,
    rows: usize,
    hp: Vec<f32>,
    hc: Vec<f32>,
    vp: Vec<f32>,
    vc: Vec<f32>,
    /// A track map's own placement: the tick and row of cell (0, 0)'s
    /// corner and the cell's ticks (pictures: none).
    native: Option<(f64, f64, f64)>,
    /// Empty cells / rows around a track map's notes (for the blur); the
    /// core in between is what Stretch fits and Tile repeats.
    pad: usize,
}

/// Where a field lies: tick / row of cell (0, 0)'s corner, a cell's size
/// (ticks, rows), tiling.
#[derive(Clone, Copy, Debug)]
struct Lay {
    t0: f64,
    p0: f64,
    cw: f64,
    rh: f64,
    tile: bool,
    /// Outside the field: its edge cells (a stretched picture covers the
    /// whole box) rather than no coverage.
    clamp: bool,
}

impl Field {
    /// A field covering nothing (constant channels only).
    fn empty() -> Field {
        Field {
            cols: 1,
            rows: 1,
            hp: vec![0.0],
            hc: vec![0.0],
            vp: vec![0.0],
            vc: vec![0.0],
            native: None,
            pad: 0,
        }
    }

    /// A track map: the notes' rectangles rasterised per row with
    /// difference arrays (O(notes + cells)), the cell coarsened so the field
    /// stays under [`MAX_FIELD_CELLS`]. `None` without notes.
    fn of_notes(
        notes: &[TNote],
        tracks: &[SourceTrack],
        cell: u64,
        h: Chan,
        v: Chan,
        pad: usize,
    ) -> Option<Field> {
        let a = notes.iter().map(|n| n.start).min()?;
        let b = notes.iter().map(|n| n.end()).max()?.max(a + 1);
        let pmin = notes.iter().map(|n| n.pitch).min()?;
        let pmax = notes.iter().map(|n| n.pitch).max()?;
        // `pad` empty cells / rows around the notes (where a blur spreads).
        let rows = usize::from(pmax - pmin) + 1 + 2 * pad;
        let span = b - a;
        let mut cell = cell.max(1);
        let max_cols = (MAX_FIELD_CELLS / rows).max(1 + 2 * pad) as u64 - 2 * pad as u64;
        if span.div_ceil(cell) > max_cols.max(1) {
            cell = span.div_ceil(max_cols.max(1)).max(1);
        }
        let cols = span.div_ceil(cell) as usize + 2 * pad;
        let n = cols * rows;
        let value = |c: Chan, t: &TNote| -> f64 {
            match c {
                Chan::Velocity => f64::from(t.vel) / 127.0,
                Chan::Subchannel => tracks
                    .get(usize::from(t.track))
                    .and_then(|tr| {
                        let i = tr.sub_index(t.sub)?;
                        Some(if tr.subs.len() > 1 {
                            i as f64 / (tr.subs.len() - 1) as f64
                        } else {
                            0.0
                        })
                    })
                    .unwrap_or(0.0),
                _ => 1.0,
            }
        };
        let mut f = Field {
            cols,
            rows,
            hp: vec![0.0; n],
            hc: vec![0.0; n],
            vp: vec![0.0; n],
            vc: vec![0.0; n],
            native: Some((
                a as f64 - (pad as u64 * cell) as f64,
                f64::from(pmin) - pad as f64,
                cell as f64,
            )),
            pad,
        };
        let mut by_row: Vec<Vec<usize>> = vec![Vec::new(); rows];
        for (i, t) in notes.iter().enumerate() {
            by_row[usize::from(t.pitch - pmin) + pad].push(i);
        }
        let cf = cell as f64;
        // Per row: coverage and the two weighted sums (difference arrays for
        // the cells a note covers whole, direct adds for its end cells).
        let mut cov = vec![0.0f64; cols + 1];
        let mut wh = vec![0.0f64; cols + 1];
        let mut wv = vec![0.0f64; cols + 1];
        let mut dc = vec![0.0f64; cols + 1];
        let mut dh = vec![0.0f64; cols + 1];
        let mut dv = vec![0.0f64; cols + 1];
        for (r, ids) in by_row.iter().enumerate() {
            if ids.is_empty() {
                continue;
            }
            for v in [&mut cov, &mut wh, &mut wv, &mut dc, &mut dh, &mut dv] {
                v.iter_mut().for_each(|x| *x = 0.0);
            }
            for &i in ids {
                let t = &notes[i];
                let off = pad as u64 * cell;
                let (s, e) = (t.start - a + off, t.end() - a + off);
                let (xh, xv) = (value(h, t), value(v, t));
                let (c0, c1) = ((s / cell) as usize, ((e - 1) / cell) as usize);
                let mut part = |c: usize, len: f64| {
                    let k = len / cf;
                    cov[c] += k;
                    wh[c] += k * xh;
                    wv[c] += k * xv;
                };
                if c0 == c1 {
                    part(c0, (e - s) as f64);
                } else {
                    part(c0, ((c0 as u64 + 1) * cell - s) as f64);
                    part(c1, (e - c1 as u64 * cell) as f64);
                    if c1 > c0 + 1 {
                        dc[c0 + 1] += 1.0;
                        dc[c1] -= 1.0;
                        dh[c0 + 1] += xh;
                        dh[c1] -= xh;
                        dv[c0 + 1] += xv;
                        dv[c1] -= xv;
                    }
                }
            }
            let (mut rc, mut rh, mut rv) = (0.0, 0.0, 0.0);
            for c in 0..cols {
                rc += dc[c];
                rh += dh[c];
                rv += dv[c];
                let (sc, sh, sv) = (cov[c] + rc, wh[c] + rh, wv[c] + rv);
                if sc <= 1e-12 {
                    continue;
                }
                let k = r * cols + c;
                let cover = sc.min(1.0);
                let fill = |ch: Chan, sum: f64| -> (f32, f32) {
                    match ch {
                        Chan::Occupancy => (1.0, 1.0),
                        _ => ((sum / sc * cover) as f32, cover as f32),
                    }
                };
                (f.hp[k], f.hc[k]) = fill(h, sh);
                (f.vp[k], f.vc[k]) = fill(v, sv);
            }
        }
        Some(f)
    }

    /// A picture map (row 0 = the picture's bottom row), reduced first when
    /// it has more than [`MAX_FIELD_CELLS`] pixels.
    fn of_picture(pic: &Rgba, h: Chan, v: Chan) -> Field {
        let px = u64::from(pic.w) * u64::from(pic.h);
        let small;
        let pic = if px > MAX_FIELD_CELLS as u64 {
            let k = (MAX_FIELD_CELLS as f64 / px as f64).sqrt();
            let nw = ((f64::from(pic.w) * k).floor() as u32).max(1);
            let nh = ((f64::from(pic.h) * k).floor() as u32).max(1);
            small = box_resize(pic, nw, nh);
            &small
        } else {
            pic
        };
        let (cols, rows) = (pic.w as usize, pic.h as usize);
        let n = cols * rows;
        let mut f = Field {
            cols,
            rows,
            hp: vec![0.0; n],
            hc: vec![1.0; n],
            vp: vec![0.0; n],
            vc: vec![1.0; n],
            native: None,
            pad: 0,
        };
        for r in 0..rows {
            let y = (rows - 1 - r) as u32;
            for c in 0..cols {
                let p = pic.at(c as u32, y);
                let k = r * cols + c;
                f.hp[k] = h.of_pixel(p);
                f.vp[k] = v.of_pixel(p);
            }
        }
        f
    }

    /// Separable box blur of radius `k` cells, two passes (edges average
    /// what lies inside).
    fn blur(&mut self, k: usize) {
        if k == 0 {
            return;
        }
        let (w, h) = (self.cols, self.rows);
        let mut tmp = Vec::new();
        for v in [&mut self.hp, &mut self.hc, &mut self.vp, &mut self.vc] {
            for _ in 0..2 {
                box_pass(v, &mut tmp, w, h, k, true);
                box_pass(v, &mut tmp, w, h, k, false);
            }
        }
    }

    /// (hp, hc, vp, vc) at a point in field cells (bilinear between cell
    /// centres; outside: no coverage unless tiled).
    fn sample(&self, fx: f64, fy: f64, tile: bool, clamp: bool) -> [f32; 4] {
        let (x, y) = (fx - 0.5, fy - 0.5);
        let (c0, r0) = (x.floor(), y.floor());
        let (tx, ty) = ((x - c0) as f32, (y - r0) as f32);
        let get = |c: f64, r: f64| -> [f32; 4] {
            let (c, r) = if tile {
                // The core repeats (without the blur's padding).
                let p = self.pad as i64;
                let (cc, cr) = (
                    (self.cols as i64 - 2 * p).max(1),
                    (self.rows as i64 - 2 * p).max(1),
                );
                (
                    (c as i64 - p).rem_euclid(cc) + p,
                    (r as i64 - p).rem_euclid(cr) + p,
                )
            } else if clamp {
                (
                    (c as i64).clamp(0, self.cols as i64 - 1),
                    (r as i64).clamp(0, self.rows as i64 - 1),
                )
            } else {
                if c < 0.0 || r < 0.0 || c >= self.cols as f64 || r >= self.rows as f64 {
                    return [0.0; 4];
                }
                (c as i64, r as i64)
            };
            let k = r as usize * self.cols + c as usize;
            [self.hp[k], self.hc[k], self.vp[k], self.vc[k]]
        };
        let (a, b, c, d) = (
            get(c0, r0),
            get(c0 + 1.0, r0),
            get(c0, r0 + 1.0),
            get(c0 + 1.0, r0 + 1.0),
        );
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let lo = a[i] + (b[i] - a[i]) * tx;
            let hi = c[i] + (d[i] - c[i]) * tx;
            out[i] = lo + (hi - lo) * ty;
        }
        out
    }
}

/// One box-filter pass along rows (`horiz`) or columns.
fn box_pass(v: &mut [f32], tmp: &mut Vec<f32>, w: usize, h: usize, k: usize, horiz: bool) {
    let (lines, len) = if horiz { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| if horiz { line * w + i } else { i * w + line };
    tmp.resize(len + 1, 0.0);
    for line in 0..lines {
        tmp[0] = 0.0;
        for i in 0..len {
            tmp[i + 1] = tmp[i] + v[at(line, i)];
        }
        for i in 0..len {
            let lo = i.saturating_sub(k);
            let hi = (i + k + 1).min(len);
            v[at(line, i)] = (tmp[hi] - tmp[lo]) / (hi - lo) as f32;
        }
    }
}

/// One note's displacement settings (Control Sweeps vary them per note).
#[derive(Clone, Copy, Debug)]
struct Cfg {
    master: f64,
    hmax: f64,
    vmax: f64,
    href: f64,
    vref: f64,
    black: f64,
    white: f64,
    gamma: f64,
    dx: f64,
    dy: f64,
}

impl Cfg {
    fn levels(&self, raw: f64) -> f64 {
        let span = (self.white - self.black).max(1e-9);
        ((raw - self.black) / span)
            .clamp(0.0, 1.0)
            .powf(1.0 / self.gamma.max(0.01))
    }

    /// A channel's value (premultiplied, coverage) blended towards the
    /// reference where the map is not covered.
    fn value(&self, ch: Chan, p: f32, c: f32, reference: f64) -> f64 {
        let (raw, cover) = match ch {
            Chan::Const(k) => (k, 1.0),
            _ if c <= 1e-6 => return reference,
            _ => (f64::from(p / c), f64::from(c).min(1.0)),
        };
        reference + cover * (self.levels(raw) - reference)
    }
}

#[derive(Debug)]
struct Dmap {
    g: Geom,
    tracks: Vec<SourceTrack>,
    picture: Option<Arc<Rgba>>,
    vice: bool,
    keep: bool,
    behaviour: usize,
    cell: u64,
    blur: usize,
    h: Chan,
    v: Chan,
    base: Cfg,
    view: usize,
    ctx: Ctx,
    growth: Growth,
}

impl Dmap {
    /// Each note's settings (the base without sweeps).
    fn cfgs(&self, notes: &[TNote], src: &Source, map_notes: bool) -> Vec<Cfg> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return vec![self.base; notes.len()];
        };
        // Sweeps are measured on the selection's notes; the map track's notes
        // (Vice Versa) take the base settings.
        if map_notes {
            return vec![self.base; notes.len()];
        }
        let places = note_places(src);
        let b = self.base;
        notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let f = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v, n, pl)
                    } else {
                        v
                    }
                };
                let pct = |k: &str, v: f64| f(k, v * 100.0) / 100.0;
                let i = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.int(k, v as i64, n, pl) as f64
                    } else {
                        v
                    }
                };
                Cfg {
                    master: pct("master", b.master),
                    hmax: i("h.max", b.hmax).max(0.0),
                    vmax: i("v.max", b.vmax).max(0.0),
                    href: pct("h.ref", b.href).clamp(0.0, 1.0),
                    vref: pct("v.ref", b.vref).clamp(0.0, 1.0),
                    black: pct("black", b.black).clamp(0.0, 1.0),
                    white: pct("white", b.white).clamp(0.0, 1.0),
                    gamma: f("gamma", b.gamma).clamp(0.1, 10.0),
                    dx: i("map.dx", b.dx),
                    dy: i("map.dy", b.dy),
                }
            })
            .collect()
    }

    /// The field and where it lies over the box `t` of the notes it moves.
    fn field(&self, map: &[TNote]) -> Option<Field> {
        let mut f = match &self.picture {
            Some(p) => Field::of_picture(p, self.h, self.v),
            None => Field::of_notes(map, &self.tracks, self.cell, self.h, self.v, 2 * self.blur)?,
        };
        f.blur(self.blur);
        Some(f)
    }

    fn lay(&self, f: &Field, t: &GeomBox, c: &Cfg) -> Lay {
        let (w, rows) = (t.width(), t.rows());
        let (cols, frows) = (f.cols as f64, f.rows as f64);
        let mid = (t.t0 as f64 + w / 2.0, f64::from(t.p0) + rows / 2.0);
        let (cw, nt0, np0) = match f.native {
            Some((t0, p0, cw)) => (cw, t0, p0),
            None => (self.cell as f64, t.t0 as f64, f64::from(t.p0)),
        };
        let picture = f.native.is_none();
        let mut l = match (self.behaviour, picture) {
            // Auto: a track map where its notes are; a picture stretched.
            (0, false) => Lay {
                t0: nt0,
                p0: np0,
                cw,
                rh: 1.0,
                tile: false,
                clamp: false,
            },
            // Stretched: a picture covers the box to its edges; a track map
            // (its own box, padded for the blur) keeps its coverage.
            (0 | 1, _) => {
                let pad = f.pad as f64;
                let (core_c, core_r) = ((cols - 2.0 * pad).max(1.0), (frows - 2.0 * pad).max(1.0));
                let (cell_w, sy) = (w / core_c, rows / core_r);
                Lay {
                    t0: t.t0 as f64 - pad * cell_w,
                    p0: f64::from(t.p0) - pad * sy,
                    cw: cell_w,
                    rh: sy,
                    tile: false,
                    clamp: picture,
                }
            }
            (2, _) => Lay {
                t0: mid.0 - cols * cw / 2.0,
                p0: (mid.1 - frows / 2.0).round(),
                cw,
                rh: 1.0,
                tile: false,
                clamp: false,
            },
            _ => Lay {
                t0: t.t0 as f64 - f.pad as f64 * cw,
                p0: f64::from(t.p0) - f.pad as f64,
                cw,
                rh: 1.0,
                tile: true,
                clamp: false,
            },
        };
        l.t0 += c.dx;
        l.p0 += c.dy;
        l
    }

    /// The displacement (ticks, semitones) at an absolute point.
    fn at(&self, f: &Field, l: &Lay, c: &Cfg, x: f64, y: f64) -> (f64, f64) {
        let s = f.sample((x - l.t0) / l.cw, (y - l.p0) / l.rh, l.tile, l.clamp);
        let dh = (2.0 * (c.value(self.h, s[0], s[1], c.href) - c.href)).clamp(-1.0, 1.0);
        let dv = (2.0 * (c.value(self.v, s[2], s[3], c.vref) - c.vref)).clamp(-1.0, 1.0);
        (dh * c.hmax * c.master, dv * c.vmax * c.master)
    }

    /// Moves `notes` (with `geom` on their box) through the field.
    fn displace(
        &self,
        geom: &Geom,
        notes: &[TNote],
        cfgs: &[Cfg],
        f: Option<&Field>,
        out: &mut Output,
    ) -> bool {
        let t = &geom.bbox;
        let (w, rows) = (t.width(), t.rows());
        let empty = Field::empty();
        for (n, c) in notes.iter().zip(cfgs) {
            let ok = match f {
                None if !self.constant() => geom.rasterize(n, &|u, v| (u, v), &self.tracks, out),
                _ => {
                    let f = f.unwrap_or(&empty);
                    let l = self.lay(f, t, c);
                    geom.rasterize(
                        n,
                        &|u, v| {
                            let (x, y) = (t.t0 as f64 + u * w, f64::from(t.p0) + v * rows);
                            let (dx, dy) = self.at(f, &l, c, x, y);
                            (u + dx / w, v + dy / rows)
                        },
                        &self.tracks,
                        out,
                    )
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Whether both channels are constant (the field is not needed).
    fn constant(&self) -> bool {
        matches!(self.h, Chan::Const(_)) && matches!(self.v, Chan::Const(_))
    }

    /// View Map: the chosen channel over the box as notes.
    fn view_map(
        &self,
        src: &Source,
        f: Option<&Field>,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let Some(tpl) = src.notes.first() else {
            return Ok(());
        };
        let t = &self.g.bbox;
        let cell = self.cell.max(1);
        let cols = (t.t1 - t.t0).div_ceil(cell);
        let c = self.base;
        let empty = Field::empty();
        let f = f.unwrap_or(&empty);
        let l = self.lay(f, t, &c);
        for p in t.p0..=t.p1 {
            let mut run: Option<(u64, u64, u8)> = None;
            for k in 0..cols {
                let a = t.t0 + k * cell;
                let b = (a + cell).min(t.t1);
                let x = (a as f64 + b as f64) / 2.0;
                let s = f.sample(
                    (x - l.t0) / l.cw,
                    (f64::from(p) + 0.5 - l.p0) / l.rh,
                    l.tile,
                    l.clamp,
                );
                let v = if self.view == 1 {
                    c.value(self.h, s[0], s[1], c.href)
                } else {
                    c.value(self.v, s[2], s[3], c.vref)
                };
                let vel = (1.0 + (126.0 * v).round()).clamp(1.0, 127.0) as u8;
                match &mut run {
                    Some(r) if r.2 == vel => r.1 = b,
                    _ => {
                        if let Some((s0, e0, vl)) = run.take()
                            && !out.emit(
                                i128::from(s0),
                                i128::from(e0 - s0),
                                i64::from(p),
                                i64::from(vl),
                                tpl,
                            )
                        {
                            return out.check();
                        }
                        run = Some((a, b, vel));
                    }
                }
            }
            if let Some((s0, e0, vl)) = run
                && !out.emit(
                    i128::from(s0),
                    i128::from(e0 - s0),
                    i64::from(p),
                    i64::from(vl),
                    tpl,
                )
            {
                return out.check();
            }
        }
        out.check()
    }
}

impl Kernel for Dmap {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        let n = src.notes.len() as u64;
        if self.view != 0 || !self.vice {
            return Estimate::from_growth(self.growth, &src.stats);
        }
        let aux = src.aux.first().map_or(0, Vec::len) as u64;
        Estimate::for_notes(n, n.saturating_add(aux.saturating_mul(self.g.per_note())))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let aux: &[TNote] = src.aux.first().map_or(&[], Vec::as_slice);
        if self.view != 0 {
            let map = if self.vice { &src.notes[..] } else { aux };
            let f = self.field(map);
            return self.view_map(src, f.as_ref(), out);
        }
        if !self.vice {
            let f = self.field(aux);
            let cfgs = self.cfgs(&src.notes, src, false);
            self.displace(&self.g, &src.notes, &cfgs, f.as_ref(), out);
            return out.check();
        }
        // Vice Versa: the selection stays and maps; the map track's notes
        // move (replaced, or kept with the moved copies added).
        for n in &src.notes {
            if !out.push(*n) {
                return out.check();
            }
        }
        if aux.is_empty() {
            return out.check();
        }
        let f = self.field(&src.notes);
        let mut st = SourceStats::default();
        for n in aux {
            st.add(n);
        }
        let mut geom = self.g;
        geom.bbox = GeomBox::of(&st);
        let cfgs = self.cfgs(aux, src, true);
        if !self.displace(&geom, aux, &cfgs, f.as_ref(), out) {
            return out.check();
        }
        if !self.keep {
            out.erase.extend_from_slice(aux);
        }
        out.check()
    }
}

fn dmap_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let source = p.choice("source").min(SOURCES.len() - 1);
    let vice = p.flag("vice");
    let picture = if source == 3 {
        if vice {
            return Err(TransformError::Invalid(
                "Vice Versa needs a map track: a picture cannot be moved. Choose a track as the map source.".into(),
            ));
        }
        match &b.ctx.picture {
            Some(Ok(pic)) => Some(pic.clone()),
            Some(Err(e)) => return Err(TransformError::Invalid(e.clone())),
            None => {
                return Err(TransformError::Invalid(
                    "Choose a picture file for the map (PNG, JPEG, BMP, GIF, WebP or TIFF).".into(),
                ));
            }
        }
    } else {
        None
    };
    let (h, v) = if picture.is_some() {
        (
            Chan::picture(p.choice("h.picture")),
            Chan::picture(p.choice("v.picture")),
        )
    } else {
        (
            Chan::track(p.choice("h.track")),
            Chan::track(p.choice("v.track")),
        )
    };
    let pct = |k: &str| p.float(k) / 100.0;
    let g = Geom::from_params(p, b.stats);
    let view = p.choice("view").min(VIEWS.len() - 1);
    let cell = note_value(p.choice("res"), p.ticks("res.ticks"), b.ctx).max(1);
    let growth = if view != 0 {
        let s = b.stats;
        let cols = s.max_end.saturating_sub(s.min_start).div_ceil(cell).max(1);
        let rows = u64::from(s.max_pitch.saturating_sub(s.min_pitch)) + 1;
        let cells = cols.saturating_mul(rows);
        if cells > MAX_VIEW_CELLS {
            return Err(TransformError::Invalid(format!(
                "View Map would draw {} cells (at most {}); choose a larger map cell.",
                crate::midi::group_digits(cells),
                crate::midi::group_digits(MAX_VIEW_CELLS)
            )));
        }
        Growth::Generate(cells)
    } else if vice {
        Growth::Add(VICE_UNKNOWN.min(Limits::default().max_notes))
    } else {
        match g.per_note() {
            0 | 1 => Growth::Map,
            k => Growth::Expand(k),
        }
    };
    boxed(Dmap {
        g,
        tracks: b.tracks.to_vec(),
        picture,
        vice,
        keep: p.flag("vice.keep"),
        behaviour: p.choice("behaviour").min(BEHAVIOURS.len() - 1),
        cell,
        blur: p.int("blur").clamp(0, 32) as usize,
        h,
        v,
        base: Cfg {
            master: pct("master"),
            hmax: p.ticks("h.max") as f64,
            vmax: p.int("v.max").clamp(0, 255) as f64,
            href: pct("h.ref").clamp(0.0, 1.0),
            vref: pct("v.ref").clamp(0.0, 1.0),
            black: pct("black").clamp(0.0, 1.0),
            white: pct("white").clamp(0.0, 1.0),
            gamma: p.float("gamma").clamp(0.1, 10.0),
            dx: p.int("map.dx") as f64,
            dy: p.int("map.dy") as f64,
        },
        view,
        ctx: b.ctx.clone(),
        growth,
    })
}
