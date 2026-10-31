//! Warp Transform (V09-S44, contract §124 – §131): an original MIDI
//! adaptation of a video warp-transform (the parameter accounting is in
//! `docs/references/v0_9/WARP_TRANSFORM.md`). The selection's unit box (u =
//! time, v = pitch rows, `transform/geom.rs`) goes through one plane
//! transform: scale about an anchor (centre, a held edge — Anchor Left moves
//! the right edge, Anchor Right the left one, likewise bottom / top — or the
//! pivot), shear, rotation about the pivot, shift, then the plane swivelled
//! (about the pitch axis) and tilted (about the time axis) in front of a
//! camera at the Z Distance with a lens of the Perspective Amount, and
//! projected back. It is a homography H of the unit box; the result is
//! rasterised by the shared geometry engine.
//!
//! Density is explicit (§129): **Transform Existing** maps N notes to N
//! notes (each sampled once; Wrap folds a note that leaves the source box
//! back into it whole — its start wrapped or mirrored, its length cut at the
//! box's far edge); **Fill / Duplicate** keeps the warped note art covered:
//! notes split along their paths at the sampling resolution, stretched rows
//! filled, and with Wrap the warped selection repeats (Tile) or mirrors
//! (Reflect) over the source box as an image would — a one-direction squeeze
//! becomes a repeated pattern (wrapped duration, §131), every copy cut at
//! the box. **Quantized** snaps the final starts and ends to the Piano
//! Roll's snap; **Unquantized** keeps the continuous map until the final
//! tick rounding (§130). A Source Region (Crop Input) limits the notes the
//! warp takes; the others stay as they are.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Fill, Geom, GeomBox, Raster};
use super::super::param::*;
use super::super::preflight::Estimate;
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::sweep::note_places;
use super::super::{
    Ctx, Growth, Kernel, Output, Source, SourceStats, SourceTrack, TNote, TransformError,
};
use super::boxed;
use crate::midi::group_digits;

pub const WARP_TRANSFORM: ToolDef = ToolDef {
    id: "warp_transform",
    spec: warp_spec,
    build: warp_build,
};

const ANCHORS_X: &[&str] = &[
    "Center",
    "Anchor Left (the right edge moves)",
    "Anchor Right (the left edge moves)",
    "Pivot",
];
const ANCHORS_Y: &[&str] = &[
    "Center",
    "Anchor Bottom (the top edge moves)",
    "Anchor Top (the bottom edge moves)",
    "Pivot",
];
/// Wrap X / Y (§126).
pub const WRAPS: &[&str] = &["No", "Tile", "Reflect", "Y-Flip Tile", "180°-Tile"];
/// The density modes (§129).
pub const DENSITY: &[&str] = &["Transform Existing (N notes → N notes)", "Fill / Duplicate"];
const QUANT: &[&str] = &["Unquantized (continuous)", "Quantized (to the snap)"];
const RASTER: &[&str] = &[
    "Sample the start",
    "Sample the middle",
    "Split along the path",
];
/// Most copies Fill / Duplicate's Tile / Reflect lays over the box (each
/// axis at most [`AXIS_CAP`]); beyond, a sentence refuses the request.
pub const COPY_CAP: u64 = 4096;
const AXIS_CAP: i64 = 256;
/// The projection's divisor never falls below this share of the focal
/// length (the plane's far side stays in front of the camera).
const NEAR_CLIP: f64 = 0.05;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const WARP_TRANSFORM_HELP: &[(&str, &str)] = &[
    (
        "scale_x",
        "How long the selection becomes in time, as a share of its length (negative also mirrors it).",
    ),
    (
        "scale_y",
        "How tall the selection becomes in pitch, as a share of its range (negative also flips it).",
    ),
    (
        "anchor_x",
        "What stays in place when scaling in time: the center, the left edge, the right edge, or the pivot.",
    ),
    (
        "anchor_y",
        "What stays in place when scaling in pitch: the center, the bottom edge, the top edge, or the pivot.",
    ),
    ("shift_x", "Moves the selection in time, in ticks."),
    ("shift_y", "Moves the selection in pitch, in semitones."),
    (
        "z_dist",
        "How far the camera is from the plane; nearer makes swivel and tilt look stronger.",
    ),
    (
        "rotate",
        "Turns the selection around the pivot, in degrees (counterclockwise).",
    ),
    (
        "swivel",
        "Turns the plane around the pitch axis, so one side moves away in depth, in degrees.",
    ),
    (
        "tilt",
        "Tips the plane around the time axis, so the top or bottom moves away in depth, in degrees.",
    ),
    (
        "persp",
        "How strong the perspective lens is: higher values make the far side shrink more.",
    ),
    (
        "shear_x",
        "Slants the selection in time by pitch: higher notes move later (negative: earlier).",
    ),
    (
        "shear_y",
        "Slants the selection in pitch by time: later notes move up (negative: down).",
    ),
    (
        "wrap_x",
        "What happens to notes that leave the box in time: nothing, they come back in at the other side (Tile), or they bounce back (Reflect). Odd tiles can instead mirror pitch (Y-Flip Tile) or rotate time and pitch (180°-Tile).",
    ),
    (
        "wrap_y",
        "What happens to notes that leave the box in pitch: nothing, they come back in at the other side (Tile), or they bounce back (Reflect). Odd tiles can instead mirror pitch (Y-Flip Tile) or rotate time and pitch (180°-Tile).",
    ),
    (
        "pivot_x",
        "The point the selection turns around: its place in time, as a share of the box.",
    ),
    (
        "pivot_y",
        "The point the selection turns around: its place in pitch, as a share of the box.",
    ),
    (
        "density",
        "Transform Existing keeps one note per note; Fill / Duplicate fills stretched areas and repeats the warped selection with Wrap.",
    ),
    (
        "quant",
        "Unquantized keeps the exact warped times; Quantized snaps every start and end to the Piano Roll's snap.",
    ),
    (
        "raster",
        "How each note is placed: by where its start lands, by where its middle lands, or split into pieces along its warped path.",
    ),
    (
        "resolution",
        "How many pieces each note is split into along its path, with Split Along the Path.",
    ),
    (
        "fill_res",
        "Fills only every n-th row of stretched areas (1 = every row).",
    ),
    (
        "fill_fade",
        "Makes the filled copies quieter the farther they are from the original notes.",
    ),
    (
        "keep_sub",
        "Filled rows keep the subchannel of the note they come from.",
    ),
    (
        "crop_left",
        "Leaves out notes in this share of the selection's left side, so they are not warped.",
    ),
    (
        "crop_right",
        "Leaves out notes in this share of the selection's right side, so they are not warped.",
    ),
    (
        "crop_bottom",
        "Leaves out notes in this share of the selection's bottom, so they are not warped.",
    ),
    (
        "crop_top",
        "Leaves out notes in this share of the selection's top, so they are not warped.",
    ),
];

fn warp_spec() -> ToolSpec {
    let mut p = vec![
        float("scale_x", "Scale X", -800.0, 800.0, 100.0, "%"),
        float("scale_y", "Scale Y", -800.0, 800.0, 100.0, "%"),
        choice("anchor_x", "Scale X from", ANCHORS_X, 0),
        choice("anchor_y", "Scale Y from", ANCHORS_Y, 0),
        int(
            "shift_x",
            "Shift X (ticks)",
            -1_000_000_000,
            1_000_000_000,
            0,
        ),
        int("shift_y", "Shift Y (semitones)", -255, 255, 0),
        float("z_dist", "Z Distance", 0.001, 100.0, 1.0, "×"),
        float(
            "rotate",
            "Rotate (counter-clockwise)",
            -3600.0,
            3600.0,
            0.0,
            "°",
        ),
        float(
            "swivel",
            "Swivel (about the pitch axis)",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        float(
            "tilt",
            "Tilt (about the time axis)",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        float("persp", "Perspective Amount", 0.25, 4.0, 1.0, "×"),
        float(
            "shear_x",
            "Shear X (time by pitch)",
            -400.0,
            400.0,
            0.0,
            "%",
        ),
        float(
            "shear_y",
            "Shear Y (pitch by time)",
            -400.0,
            400.0,
            0.0,
            "%",
        ),
        choice("wrap_x", "Wrap X", WRAPS, 0),
        choice("wrap_y", "Wrap Y", WRAPS, 0),
        float("pivot_x", "Pivot X (of the box)", -100.0, 200.0, 50.0, "%"),
        float("pivot_y", "Pivot Y (of the box)", -100.0, 200.0, 50.0, "%"),
        choice("density", "Density", DENSITY, 0),
        choice("quant", "Quantization", QUANT, 0),
        choice("raster", "Rasterization quality", RASTER, 1),
        int(
            "resolution",
            "Sampling resolution (segments per note)",
            1,
            256,
            8,
        ),
        int("fill_res", "Fill every n-th row", 1, 12, 1),
        flag("fill_fade", "Filled copies fade with distance", false),
        flag("keep_sub", "Filled rows keep the subchannel", true),
        float(
            "crop_left",
            "Source Region: left inset",
            0.0,
            100.0,
            0.0,
            "%",
        ),
        float(
            "crop_right",
            "Source Region: right inset",
            0.0,
            100.0,
            0.0,
            "%",
        ),
        float(
            "crop_bottom",
            "Source Region: bottom inset",
            0.0,
            100.0,
            0.0,
            "%",
        ),
        float("crop_top", "Source Region: top inset", 0.0, 100.0, 0.0, "%"),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, WARP_TRANSFORM_HELP);
    ToolSpec {
        id: "warp_transform",
        name: "Warp Transform",
        family: Family::Geometry,
        summary: "Scales (from the center or a held edge), shears, rotates, shifts, swivels, tilts and projects the selection like a warped image; Transform Existing keeps one note per note, Fill / Duplicate keeps the warped art covered and tiles or mirrors it over the box.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// A 3 × 3 matrix, row major.
pub type M3 = [f64; 9];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    m
}

fn translate(x: f64, y: f64) -> M3 {
    [1.0, 0.0, x, 0.0, 1.0, y, 0.0, 0.0, 1.0]
}

/// The inverse of `m` (None when singular).
pub fn invert(m: &M3) -> Option<M3> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let co = [
        e * i - f * h,
        -(d * i - f * g),
        d * h - e * g,
        -(b * i - c * h),
        a * i - c * g,
        -(a * h - b * g),
        b * f - c * e,
        -(a * f - c * d),
        a * e - b * d,
    ];
    let det = a * co[0] + b * co[1] + c * co[2];
    if det.abs() < 1e-15 || !det.is_finite() {
        return None;
    }
    Some([
        co[0] / det,
        co[3] / det,
        co[6] / det,
        co[1] / det,
        co[4] / det,
        co[7] / det,
        co[2] / det,
        co[5] / det,
        co[8] / det,
    ])
}

/// One note's warp settings (swept per note).
#[derive(Clone, Copy, Debug)]
pub struct Warp {
    pub scale: (f64, f64),
    /// Shift in box units.
    pub shift: (f64, f64),
    pub z_dist: f64,
    /// Radians.
    pub rotate: f64,
    pub swivel: f64,
    pub tilt: f64,
    pub persp: f64,
    pub shear: (f64, f64),
    /// The scale anchor and the rotation pivot (box units).
    pub anchor: (f64, f64),
    pub pivot: (f64, f64),
}

impl Warp {
    /// The homography of the unit box: scale about the anchor, shear about
    /// the centre, rotate about the pivot, shift; then the plane (centred
    /// on the box) swivelled and tilted at Z Distance in front of a camera
    /// of focal length 2 × Perspective Amount (at a Z Distance of 1 and no
    /// swivel or tilt the plane keeps its size).
    pub fn matrix(&self) -> M3 {
        let (ax, ay) = self.anchor;
        let scale = mul(
            &translate(ax, ay),
            &mul(
                &[
                    self.scale.0,
                    0.0,
                    0.0,
                    0.0,
                    self.scale.1,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                ],
                &translate(-ax, -ay),
            ),
        );
        let shear = mul(
            &translate(0.5, 0.5),
            &mul(
                &[
                    1.0,
                    self.shear.0,
                    0.0,
                    self.shear.1,
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                ],
                &translate(-0.5, -0.5),
            ),
        );
        let (s, c) = self.rotate.sin_cos();
        let (px, py) = self.pivot;
        let rot = mul(
            &translate(px, py),
            &mul(
                &[c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0],
                &translate(-px, -py),
            ),
        );
        let affine = mul(
            &translate(self.shift.0, self.shift.1),
            &mul(&rot, &mul(&shear, &scale)),
        );
        // The plane (X, Y, 0) about the box's centre: swivel (about the
        // pitch axis: x toward depth), then tilt (about the time axis: y
        // toward depth); columns 0 and 1 of the rotation.
        let (ss, cs) = self.swivel.sin_cos();
        let (st, ct) = self.tilt.sin_cos();
        // R = Rtilt · Rswivel applied to (X, Y, 0).
        let r = [[cs, 0.0], [st * ss, ct], [-ct * ss, st]];
        let f = 2.0 * self.persp;
        let d = f * self.z_dist;
        let proj = [
            f * r[0][0],
            f * r[0][1],
            0.0,
            f * r[1][0],
            f * r[1][1],
            0.0,
            r[2][0],
            r[2][1],
            d,
        ];
        mul(
            &translate(0.5, 0.5),
            &mul(&proj, &mul(&translate(-0.5, -0.5), &affine)),
        )
    }

    /// The focal length (the projection's near clip is a share of it).
    fn focal(&self) -> f64 {
        2.0 * self.persp
    }
}

/// `m` applied to (u, v), its divisor kept in front of the camera.
#[inline]
pub fn apply(m: &M3, near: f64, u: f64, v: f64) -> (f64, f64) {
    let x = m[0] * u + m[1] * v + m[2];
    let y = m[3] * u + m[4] * v + m[5];
    let w = (m[6] * u + m[7] * v + m[8]).max(near);
    (x / w, y / w)
}

/// `x` folded into [0, 1): tiled (`reflect` false) or mirrored, and the
/// copy index it came from.
#[inline]
fn fold(x: f64, reflect: bool) -> (f64, i64) {
    let k = x.floor();
    let f = x - k;
    let k = k as i64;
    if reflect && k.rem_euclid(2) == 1 {
        (1.0 - f, k)
    } else {
        (f, k)
    }
}

#[derive(Clone)]
struct WarpK {
    bbox: GeomBox,
    tracks: Vec<SourceTrack>,
    ctx: Ctx,
    base: Warp,
    /// 0 No, 1 Tile, 2 Reflect, 3 Y-Flip Tile, 4 180°-Tile per axis.
    wrap: (usize, usize),
    fill: bool,
    quant: Option<u64>,
    raster: Raster,
    segments: u32,
    fill_opts: Fill,
    /// The source region (unit box): u0, u1, v0, v1.
    region: (f64, f64, f64, f64),
    top: i64,
    /// The loose bound before the notes are known.
    growth: Growth,
}

impl WarpK {
    /// Every note's warp (the settings' without sweeps).
    fn warps(&self, src: &Source) -> Vec<Warp> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let places = note_places(src);
        let b = self.base;
        let w = self.bbox.width();
        let rows = self.bbox.rows();
        src.notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let deg = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v.to_degrees(), n, pl).to_radians()
                    } else {
                        v
                    }
                };
                let mut x = b;
                x.scale = (pct("scale_x", b.scale.0), pct("scale_y", b.scale.1));
                if sw.drives("shift_x") {
                    x.shift.0 = sw.int("shift_x", (b.shift.0 * w).round() as i64, n, pl) as f64 / w;
                }
                if sw.drives("shift_y") {
                    x.shift.1 =
                        sw.int("shift_y", (b.shift.1 * rows).round() as i64, n, pl) as f64 / rows;
                }
                x.z_dist = if sw.drives("z_dist") {
                    sw.float("z_dist", b.z_dist, n, pl).max(0.001)
                } else {
                    b.z_dist
                };
                x.rotate = deg("rotate", b.rotate);
                x.swivel = deg("swivel", b.swivel);
                x.tilt = deg("tilt", b.tilt);
                x.persp = if sw.drives("persp") {
                    sw.float("persp", b.persp, n, pl).clamp(0.25, 4.0)
                } else {
                    b.persp
                };
                x.shear = (pct("shear_x", b.shear.0), pct("shear_y", b.shear.1));
                x
            })
            .collect()
    }

    fn inside(&self, n: &TNote) -> bool {
        let (u, v) = self.bbox.to_unit(
            (n.start as f64 + n.end() as f64) / 2.0,
            f64::from(n.pitch) + 0.5,
        );
        let (u0, u1, v0, v1) = self.region;
        u >= u0 && u <= u1 && v >= v0 && v <= v1
    }

    /// The copies (x, y) Fill / Duplicate's Tile / Reflect lays over the
    /// box for homography `m`: the source-space cells the box's inverse
    /// image meets (one copy without wrap).
    fn copies(&self, m: &M3) -> Result<(Vec<i64>, Vec<i64>), TransformError> {
        let one = (vec![0], vec![0]);
        if !self.fill || (self.wrap.0 == 0 && self.wrap.1 == 0) {
            return Ok(one);
        }
        let Some(inv) = invert(m) else {
            return Ok(one);
        };
        // The box's inverse image (sampled along its edges; a horizon
        // inside the box makes it unbounded — capped per axis).
        let (mut lo, mut hi) = (
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
        let k = 16;
        for i in 0..=k {
            let t = f64::from(i) / f64::from(k);
            for (u, v) in [(t, 0.0), (t, 1.0), (0.0, t), (1.0, t)] {
                let x = inv[0] * u + inv[1] * v + inv[2];
                let y = inv[3] * u + inv[4] * v + inv[5];
                let w = inv[6] * u + inv[7] * v + inv[8];
                let (sx, sy) = if w.abs() > 1e-12 {
                    (x / w, y / w)
                } else {
                    (f64::INFINITY.copysign(x), f64::INFINITY.copysign(y))
                };
                lo = (lo.0.min(sx), lo.1.min(sy));
                hi = (hi.0.max(sx), hi.1.max(sy));
            }
        }
        let range = |a: f64, b: f64, wrap: usize| -> Result<Vec<i64>, TransformError> {
            if wrap == 0 {
                return Ok(vec![0]);
            }
            let a = if a.is_finite() {
                a.floor().max(-1e9) as i64
            } else {
                -AXIS_CAP
            };
            let b = if b.is_finite() {
                b.floor().min(1e9) as i64
            } else {
                AXIS_CAP
            };
            if b - a + 1 > AXIS_CAP {
                return Err(TransformError::TooLarge(format!(
                    "Tiling the warped selection over its box would take {} copies across; at most {} are allowed — squeeze less or turn Wrap off.",
                    group_digits((b - a + 1).max(0) as u64),
                    AXIS_CAP
                )));
            }
            Ok((a..=b).collect())
        };
        let xs = range(lo.0, hi.0, self.wrap.0)?;
        let ys = range(lo.1, hi.1, self.wrap.1)?;
        if (xs.len() * ys.len()) as u64 > COPY_CAP {
            return Err(TransformError::TooLarge(format!(
                "Tiling the warped selection over its box would take {} copies; at most {} are allowed — squeeze less or turn Wrap off.",
                group_digits((xs.len() * ys.len()) as u64),
                group_digits(COPY_CAP)
            )));
        }
        Ok((xs, ys))
    }

    /// Odd cells of the selected axis determine each tile's orientation.
    /// Y-Flip always mirrors global pitch; two flips of an axis cancel.
    fn tile_flips(&self, i: i64, j: i64) -> (bool, bool) {
        let odd_x = i.rem_euclid(2) == 1;
        let odd_y = j.rem_euclid(2) == 1;
        let mx = (odd_x && matches!(self.wrap.0, 2 | 4)) ^ (odd_y && self.wrap.1 == 4);
        let my = (odd_x && matches!(self.wrap.0, 3 | 4)) ^ (odd_y && matches!(self.wrap.1, 2..=4));
        (mx, my)
    }

    /// The source point of copy (i, j) for unit point (u, v): the copy's
    /// cell, mirrored in odd cells under Reflect.
    #[inline]
    fn cell(&self, i: i64, j: i64, u: f64, v: f64) -> (f64, f64) {
        let (mx, my) = self.tile_flips(i, j);
        (
            i as f64 + if mx { 1.0 - u } else { u },
            j as f64 + if my { 1.0 - v } else { v },
        )
    }

    fn geom(&self) -> Geom {
        Geom {
            pivot: (0.5, 0.5),
            scale: (1.0, 1.0),
            pos: (0.0, 0.0),
            raster: self.raster,
            segments: self.segments,
            fill: self.fill.then_some(self.fill_opts),
            bbox: self.bbox,
            undefined: 0,
            top: self.top,
        }
    }

    /// Cuts the notes of `out` from `from` to the box in the wrapped axes
    /// (a note leaving the box keeps its part inside; none left: removed).
    fn cut(&self, out: &mut Output, from: usize) {
        let (t0, t1) = (self.bbox.t0, self.bbox.t1);
        let (p0, p1) = (self.bbox.p0, self.bbox.p1);
        let (wx, wy) = (self.wrap.0 != 0, self.wrap.1 != 0);
        if !wx && !wy {
            return;
        }
        // In place, the notes' driver dimensions kept with them.
        out.retain_notes_from(from, |n| {
            if wy && (n.pitch < p0 || n.pitch > p1) {
                return false;
            }
            if wx {
                let s = n.start.max(t0);
                let e = n.end().min(t1);
                if e <= s {
                    return false;
                }
                n.start = s;
                n.dur = e - s;
            }
            true
        });
    }

    /// Snaps the notes of `out` from `from` to the snap (starts and ends;
    /// a note never vanishes: at least one step).
    fn snap(&self, out: &mut Output, from: usize) {
        let Some(q) = self.quant else {
            return;
        };
        for n in &mut out.notes[from..] {
            let r = |x: u64| (x + q / 2) / q * q;
            let s = r(n.start);
            let e = r(n.end()).max(s + q);
            n.start = s;
            n.dur = e - s;
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let per = self.warps(src);
        let g = self.geom();
        let one = (self.base.matrix(), self.copies(&self.base.matrix())?);
        for (i, n) in src.notes.iter().enumerate() {
            out.set_dim(0);
            if !self.inside(n) {
                // Outside the Source Region: unchanged.
                if !out.emit(
                    n.start as i128,
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                ) {
                    return out.check();
                }
                continue;
            }
            let swept;
            let (m, (xs, ys)) = match per.get(i) {
                Some(w) => {
                    let m = w.matrix();
                    swept = (m, self.copies(&m)?);
                    (&swept.0, (&swept.1.0, &swept.1.1))
                }
                None => (&one.0, (&one.1.0, &one.1.1)),
            };
            let near = NEAR_CLIP * per.get(i).unwrap_or(&self.base).focal();
            let from = out.notes.len();
            if self.fill {
                for &cx in xs {
                    for &cy in ys {
                        // V09-0143: the tile's ring around the original (0
                        // = the original tile, 1 = the tiles next to it …).
                        let ring = cx.unsigned_abs().max(cy.unsigned_abs());
                        out.set_dim(u32::try_from(ring).unwrap_or(u32::MAX));
                        let map = |u: f64, v: f64| {
                            let (su, sv) = self.cell(cx, cy, u, v);
                            apply(m, near, su, sv)
                        };
                        if !g.rasterize(n, &map, &self.tracks, out) {
                            self.cut(out, from);
                            self.snap(out, from);
                            return out.check();
                        }
                    }
                }
            } else {
                // Transform Existing: one note; Wrap folds the whole note
                // back into the box by the cell of its mapped start (and
                // row), its length cut at the box's far edge.
                let (us, v) = self.bbox.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
                let (um, _) = self
                    .bbox
                    .to_unit((n.start as f64 + n.end() as f64) / 2.0, 0.0);
                let (xs0, _) = apply(m, near, us, v);
                let (_, ym) = apply(m, near, um, v);
                let (_, kx) = fold(xs0, self.wrap.0 == 2);
                let (_, ky) = fold(ym, self.wrap.1 == 2);
                let (kx, ky) = (
                    if self.wrap.0 == 0 { 0 } else { kx },
                    if self.wrap.1 == 0 { 0 } else { ky },
                );
                let (mx, my) = self.tile_flips(kx, ky);
                let map = |u: f64, v: f64| {
                    let (x, y) = apply(m, near, u, v);
                    let x = x - kx as f64;
                    let y = y - ky as f64;
                    (if mx { 1.0 - x } else { x }, if my { 1.0 - y } else { y })
                };
                if !g.rasterize(n, &map, &self.tracks, out) {
                    self.cut(out, from);
                    self.snap(out, from);
                    return out.check();
                }
            }
            self.cut(out, from);
            self.snap(out, from);
            out.check()?;
        }
        Ok(())
    }
}

impl WarpK {
    /// An upper bound of the notes the run makes with these source notes:
    /// one a note in Transform Existing; in Fill / Duplicate, per note and
    /// copy, the raster's segments × the rows its filled row covers (the
    /// shared Fill's rows at the note's middle, plus its own row) — counted
    /// like the rasterisation, so a squeeze is not refused by the shared
    /// 129-rows-a-note bound.
    fn bound(&self, src: &Source) -> u64 {
        if !self.fill {
            return src.notes.len() as u64;
        }
        let per = self.warps(src);
        let segs = if self.raster == Raster::Split {
            u64::from(self.segments.max(1))
        } else {
            1
        };
        let base_m = self.base.matrix();
        let Ok(base_c) = self.copies(&base_m) else {
            return u64::MAX;
        };
        let rows = self.bbox.rows();
        let p0 = f64::from(self.bbox.p0);
        let mut total = 0u64;
        for (i, n) in src.notes.iter().enumerate() {
            if !self.inside(n) {
                total = total.saturating_add(1);
                continue;
            }
            let own;
            let (m, xs, ys, focal) = match per.get(i) {
                Some(w) => {
                    let m = w.matrix();
                    let Ok(c) = self.copies(&m) else {
                        return u64::MAX;
                    };
                    own = (m, c);
                    (&own.0, &own.1.0, &own.1.1, w.focal())
                }
                None => (&base_m, &base_c.0, &base_c.1, self.base.focal()),
            };
            let near = NEAR_CLIP * focal;
            let xm = (n.start as f64 + n.end() as f64) / 2.0;
            let (u, v0) = self.bbox.to_unit(xm, f64::from(n.pitch));
            let (_, v1) = self.bbox.to_unit(xm, f64::from(n.pitch) + 1.0);
            for &cx in xs {
                for &cy in ys {
                    let y = |v: f64| {
                        let (su, sv) = self.cell(cx, cy, u, v);
                        p0 + apply(m, near, su, sv).1 * rows
                    };
                    let (a, b) = (y(v0), y(v1));
                    let (lo, hi) = (a.min(b), a.max(b));
                    let first = ((lo - 0.5).ceil() as i64).max(0);
                    let last = ((hi - 0.5).ceil() as i64 - 1).max(first).min(self.top);
                    let filled = (last - first + 1).max(0) as u64 + 1;
                    total = total.saturating_add(segs.saturating_mul(filled));
                }
            }
        }
        total
    }
}

impl Kernel for WarpK {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        Estimate::from_growth(self.growth, stats)
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        Estimate::for_notes(src.notes.len() as u64, self.bound(src))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        WarpK::run(self, src, out)?;
        out.check()
    }
}

fn warp_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let bbox = GeomBox::of(b.stats);
    let pct = |k: &str| p.float(k) / 100.0;
    let pivot = (pct("pivot_x"), pct("pivot_y"));
    let anchor_choice = (p.choice("anchor_x"), p.choice("anchor_y"));
    let anchor = (
        match anchor_choice.0 {
            1 => 0.0,
            2 => 1.0,
            3 => pivot.0,
            _ => 0.5,
        },
        match anchor_choice.1 {
            1 => 0.0,
            2 => 1.0,
            3 => pivot.1,
            _ => 0.5,
        },
    );
    let base = Warp {
        scale: (pct("scale_x"), pct("scale_y")),
        shift: (
            p.int("shift_x") as f64 / bbox.width(),
            p.int("shift_y") as f64 / bbox.rows(),
        ),
        z_dist: p.float("z_dist").max(0.001),
        rotate: p.float("rotate").to_radians(),
        swivel: p.float("swivel").to_radians(),
        tilt: p.float("tilt").to_radians(),
        persp: p.float("persp").clamp(0.25, 4.0),
        shear: (pct("shear_x"), pct("shear_y")),
        anchor,
        pivot,
    };
    let fill = p.choice("density") == 1;
    let raster = if fill {
        match p.choice("raster") {
            0 => Raster::SampleStart,
            1 => Raster::SampleMid,
            _ => Raster::Split,
        }
    } else {
        // Transform Existing: one note per note.
        match p.choice("raster") {
            0 => Raster::SampleStart,
            _ => Raster::SampleMid,
        }
    };
    let k = WarpK {
        bbox,
        tracks: b.tracks.to_vec(),
        ctx: b.ctx.clone(),
        base,
        wrap: (
            p.choice("wrap_x").min(WRAPS.len() - 1),
            p.choice("wrap_y").min(WRAPS.len() - 1),
        ),
        fill,
        quant: (p.choice("quant") == 1).then(|| b.ctx.snap.max(1)),
        raster,
        segments: p.int("resolution").clamp(1, 256) as u32,
        fill_opts: Fill {
            res: p.int("fill_res").clamp(1, 12) as u8,
            fade: p.flag("fill_fade"),
            keep_sub: p.flag("keep_sub"),
            merge: true,
            mode: super::super::geom::FillMode::Rows,
        },
        region: (
            pct("crop_left"),
            1.0 - pct("crop_right"),
            pct("crop_bottom"),
            1.0 - pct("crop_top"),
        ),
        top: i64::from(p.keys.max_pitch()),
        growth: Growth::Map,
    };
    // Refuse a tiling over the cap before anything runs. With Control
    // Sweeps a note's own warp sets its copies: the bound is the cap's.
    let (xs, ys) = k.copies(&k.base.matrix())?;
    let per_copy = k.geom().per_note();
    let swept = b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty());
    let copies = if swept && k.fill && (k.wrap.0 != 0 || k.wrap.1 != 0) {
        COPY_CAP
    } else {
        (xs.len() * ys.len()) as u64
    };
    let per = per_copy.saturating_mul(copies);
    let mut k = k;
    k.growth = if per > 1 {
        Growth::Expand(per)
    } else {
        Growth::Map
    };
    boxed(k)
}
