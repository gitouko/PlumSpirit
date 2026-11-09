//! Image to MIDI (v0.8 built-in plugin, contract §037–043): turns a picture
//! into notes, one colour group per palette entry.
//!
//! A native reimplementation of the behaviour of arduano's ImageToMidi (no
//! licence upstream: nothing copied; `docs/references/v0_8/IMAGE_TO_MIDI.md`):
//!
//! 1. the picture is resized with an area-weighted **box filter** so one pixel
//!    is one key (rows in the Piano Roll orientation, columns in the upstream
//!    one) and keeps its aspect ratio along time;
//! 2. a **palette** is chosen: automatic (deterministic median cut, optionally
//!    refined by the upstream's k-means "Clusterise" with a seeded generator),
//!    or edited by hand; palette entries can be **ignored** (background);
//! 3. every pixel takes the **nearest** palette colour (squared RGB distance,
//!    first minimum wins); pixels below the alpha threshold are transparent;
//! 4. along time, equal neighbouring pixels form **one note** (run length), cut
//!    by the maximum note length (from the image start or from the note start)
//!    — or one note per pixel when asked for.
//!
//! Rotation, flips and scaling are applied to the picture before the notes are
//! formed (nearest-pixel inverse mapping), so rotated artwork stays solid.
//! Decoding (PNG / JPEG / …) happens in the app; this module only sees RGBA.

use super::paste::{ColorInfo, ColorNote, Generated};
use super::rng::Rng;
#[path = "image_runs.rs"]
pub mod runs;

/// Largest decoded picture (pixels) the plugin accepts.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;
/// Largest side of a decoded picture.
pub const MAX_SIDE: u32 = 16_384;
/// Most columns along time after resizing (bounds the work and the notes).
pub const MAX_TIME_PIXELS: u32 = 1 << 18;
/// Largest palette.
pub const MAX_PALETTE: usize = 1024;

/// An RGBA picture, row-major from the top-left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<[u8; 4]>,
}

impl Rgba {
    pub fn new(w: u32, h: u32, px: Vec<[u8; 4]>) -> Result<Rgba, String> {
        check_size(w, h)?;
        if px.len() as u64 != u64::from(w) * u64::from(h) {
            return Err("The picture's pixel data does not match its size.".into());
        }
        Ok(Rgba { w, h, px })
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.px[(y as usize) * (self.w as usize) + x as usize]
    }
}

/// Refuses absurd dimensions before anything is allocated.
pub fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("The picture is empty.".into());
    }
    if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!(
            "The picture is {w} × {h} pixels; Image to MIDI accepts up to {MAX_SIDE} pixels a side and {} million pixels.",
            MAX_PIXELS / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Area-weighted box-filter resize (premultiplied by coverage, RGBA averaged).
pub fn box_resize(src: &Rgba, nw: u32, nh: u32) -> Rgba {
    box_resize_controlled(src, nw, nh, |_, _, _| Ok(()), || Ok(()))
        .expect("Image resize allocation failed")
}

/// The same box filter, with allocation preflight and cooperative checkpoints.
/// `before` receives output dimensions and an upper bound on temporary span bytes
/// (excluding output pixels). No output or span storage is allocated before it.
/// The caller enforces its own dimensional/workspace quotas; native conversion
/// deliberately retains its historical wider time-axis limits.
pub fn box_resize_controlled(
    src: &Rgba,
    nw: u32,
    nh: u32,
    mut before: impl FnMut(u32, u32, usize) -> Result<(), String>,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<Rgba, String> {
    let (nw, nh) = (nw.max(1), nh.max(1));
    let identity = nw == src.w && nh == src.h;
    let scratch = if identity {
        0
    } else {
        let axes = u64::from(nw) + u64::from(nh);
        // Each axis partitions its source range. At most two partial source
        // cells per output span are added to the source cells' total count.
        let entries = u64::from(src.w) + u64::from(src.h) + 2 * axes;
        usize::try_from(
            axes * std::mem::size_of::<Vec<(u32, f64)>>() as u64
                + entries * std::mem::size_of::<(u32, f64)>() as u64,
        )
        .map_err(|_| "Image resize workspace overflows this platform.".to_string())?
    };
    check()?;
    before(nw, nh, scratch)?;
    let mut px = pixel_buffer(nw, nh)?;
    if nw == src.w && nh == src.h {
        for row in src.px.chunks(src.w as usize) {
            check()?;
            px.extend_from_slice(row);
        }
        return Ok(Rgba { w: nw, h: nh, px });
    }
    let sx = f64::from(src.w) / f64::from(nw);
    let sy = f64::from(src.h) / f64::from(nh);
    // Per output column: the source columns it covers with their weights.
    let mut spans = |n: u32, scale: f64, len: u32| -> Result<Vec<Vec<(u32, f64)>>, String> {
        let mut out = Vec::new();
        out.try_reserve_exact(n as usize)
            .map_err(|_| "memory: image resize spans could not be allocated".to_string())?;
        for i in 0..n {
            check()?;
            let a = f64::from(i) * scale;
            let b = (f64::from(i) + 1.0) * scale;
            let mut v = Vec::new();
            let mut s = a.floor() as u32;
            let end = (b.ceil() as u32).min(len);
            v.try_reserve_exact(end.saturating_sub(s) as usize)
                .map_err(|_| "memory: image resize span could not be allocated".to_string())?;
            while f64::from(s) < b && s < len {
                let lo = a.max(f64::from(s));
                let hi = b.min(f64::from(s) + 1.0);
                if hi > lo {
                    v.push((s, hi - lo));
                }
                s += 1;
            }
            out.push(v);
        }
        Ok(out)
    };
    let xs = spans(nw, sx, src.w)?;
    let ys = spans(nh, sy, src.h)?;
    let mut work = 0usize;
    for yspan in &ys {
        check()?;
        for xspan in &xs {
            let mut acc = [0.0f64; 4];
            let mut wsum = 0.0;
            for &(y, wy) in yspan {
                for &(x, wx) in xspan {
                    if work.is_multiple_of(65_536) {
                        check()?;
                    }
                    work = work.wrapping_add(1);
                    let p = src.at(x, y);
                    let w = wx * wy;
                    for c in 0..4 {
                        acc[c] += f64::from(p[c]) * w;
                    }
                    wsum += w;
                }
            }
            let mut o = [0u8; 4];
            if wsum > 0.0 {
                for c in 0..4 {
                    o[c] = (acc[c] / wsum).round().clamp(0.0, 255.0) as u8;
                }
            }
            px.push(o);
        }
    }
    Ok(Rgba { w: nw, h: nh, px })
}

fn pixel_buffer(w: u32, h: u32) -> Result<Vec<[u8; 4]>, String> {
    let count = usize::try_from(u64::from(w) * u64::from(h))
        .map_err(|_| "Image pixel count overflows this platform.".to_string())?;
    let mut px = Vec::new();
    px.try_reserve_exact(count)
        .map_err(|_| "memory: image pixels could not be allocated".to_string())?;
    Ok(px)
}

/// Geometric changes applied to the picture before conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageTransform {
    /// Clockwise degrees.
    pub rotate: f64,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Scale factors (negative = mirrored); 1 = unchanged.
    pub scale_x: f64,
    pub scale_y: f64,
}

impl Default for ImageTransform {
    fn default() -> Self {
        ImageTransform {
            rotate: 0.0,
            flip_x: false,
            flip_y: false,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

impl ImageTransform {
    pub fn is_identity(&self) -> bool {
        *self == ImageTransform::default()
    }
}

/// Applies `t` (scale, flips, rotation about the centre) by inverse mapping
/// to the nearest source pixel; the canvas grows to hold the rotated picture
/// and uncovered pixels are transparent.
pub fn transform(src: &Rgba, t: &ImageTransform) -> Result<Rgba, String> {
    transform_controlled(src, t, |_, _, _| Ok(()), || Ok(()))
}

/// Existing inverse mapping with caller allocation preflight and cancellation.
/// The temporary byte count is zero: this operation only allocates output pixels.
pub fn transform_controlled(
    src: &Rgba,
    t: &ImageTransform,
    mut before: impl FnMut(u32, u32, usize) -> Result<(), String>,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<Rgba, String> {
    check()?;
    if t.is_identity() {
        before(src.w, src.h, 0)?;
        let mut px = pixel_buffer(src.w, src.h)?;
        for row in src.px.chunks(src.w as usize) {
            check()?;
            px.extend_from_slice(row);
        }
        return Ok(Rgba {
            w: src.w,
            h: src.h,
            px,
        });
    }
    let (sx, sy) = (t.scale_x, t.scale_y);
    if !(sx.is_finite() && sy.is_finite()) || sx.abs() < 1e-3 || sy.abs() < 1e-3 {
        return Err("The scale must not be zero.".into());
    }
    if !t.rotate.is_finite() {
        return Err("The rotation must be finite.".into());
    }
    let fx = if (sx < 0.0) != t.flip_x { -1.0 } else { 1.0 };
    let fy = if (sy < 0.0) != t.flip_y { -1.0 } else { 1.0 };
    let (sw, sh) = (f64::from(src.w) * sx.abs(), f64::from(src.h) * sy.abs());
    let a = t.rotate.to_radians();
    let (c, s) = (a.cos(), a.sin());
    let nw = (sw * c.abs() + sh * s.abs()).round().max(1.0);
    let nh = (sw * s.abs() + sh * c.abs()).round().max(1.0);
    if nw > f64::from(MAX_SIDE) || nh > f64::from(MAX_SIDE) || nw * nh > MAX_PIXELS as f64 {
        return Err(format!(
            "The transformed picture would be {nw} × {nh} pixels; reduce the scale."
        ));
    }
    let (nw, nh) = (nw as u32, nh as u32);
    before(nw, nh, 0)?;
    let (cx, cy) = (f64::from(nw) / 2.0, f64::from(nh) / 2.0);
    let (scx, scy) = (sw / 2.0, sh / 2.0);
    let mut px = pixel_buffer(nw, nh)?;
    for y in 0..nh {
        check()?;
        for x in 0..nw {
            // Destination pixel centre relative to the centre, rotated back.
            let dx = f64::from(x) + 0.5 - cx;
            let dy = f64::from(y) + 0.5 - cy;
            let rx = dx * c + dy * s;
            let ry = -dx * s + dy * c;
            // Into the scaled (and mirrored) picture.
            let ux = fx * rx + scx;
            let uy = fy * ry + scy;
            if ux < 0.0 || uy < 0.0 || ux >= sw || uy >= sh {
                px.push([0, 0, 0, 0]);
                continue;
            }
            let ox = ((ux / sx.abs()) as u32).min(src.w - 1);
            let oy = ((uy / sy.abs()) as u32).min(src.h - 1);
            px.push(src.at(ox, oy));
        }
    }
    Ok(Rgba { w: nw, h: nh, px })
}

#[inline]
fn dist2(a: [u8; 3], b: [u8; 3]) -> u32 {
    let d = |i: usize| (i32::from(a[i]) - i32::from(b[i])).unsigned_abs();
    d(0) * d(0) + d(1) * d(1) + d(2) * d(2)
}

/// Index of the nearest palette colour (first minimum wins).
#[inline]
pub fn nearest(palette: &[[u8; 3]], c: [u8; 3]) -> usize {
    let mut best = 0;
    let mut bd = u32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let d = dist2(*p, c);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

/// Opaque pixels of `img` (sampled evenly down to at most `limit`).
fn opaque_sample(img: &Rgba, alpha_min: u8, limit: usize) -> Vec<[u8; 3]> {
    let n = img.px.len();
    let step = n.div_ceil(limit.max(1)).max(1);
    img.px
        .iter()
        .step_by(step)
        .filter(|p| p[3] >= alpha_min)
        .map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// A deterministic median-cut palette of up to `n` colours, most used first.
pub fn median_cut(img: &Rgba, n: usize, alpha_min: u8) -> Vec<[u8; 3]> {
    let mut pts = opaque_sample(img, alpha_min, 1 << 18);
    if pts.is_empty() || n == 0 {
        return Vec::new();
    }
    // Boxes as ranges of `pts`.
    let mut boxes: Vec<(usize, usize)> = vec![(0, pts.len())];
    let range = |v: &[[u8; 3]]| -> (usize, u8) {
        let mut best = (0, 0u8);
        for c in 0..3 {
            let (lo, hi) = v
                .iter()
                .fold((255u8, 0u8), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])));
            let r = hi.saturating_sub(lo);
            if r > best.1 {
                best = (c, r);
            }
        }
        best
    };
    while boxes.len() < n.min(MAX_PALETTE) {
        // Split the box with the widest channel range (ties: the larger box,
        // then the earlier one).
        let mut pick: Option<(usize, usize, u8, usize)> = None; // (box, channel, range, size)
        for (i, &(a, b)) in boxes.iter().enumerate() {
            if b - a < 2 {
                continue;
            }
            let (c, r) = range(&pts[a..b]);
            if r == 0 {
                continue;
            }
            let better = match pick {
                None => true,
                Some((_, _, pr, ps)) => r > pr || (r == pr && b - a > ps),
            };
            if better {
                pick = Some((i, c, r, b - a));
            }
        }
        let Some((i, c, _, _)) = pick else {
            break;
        };
        let (a, b) = boxes[i];
        pts[a..b].sort_unstable_by_key(|p| (p[c], p[(c + 1) % 3], p[(c + 2) % 3]));
        // Split at the median, moved to the nearest change of value so equal
        // colours stay together.
        let mid = a + (b - a) / 2;
        let v = pts[mid][c];
        let lo = a + pts[a..b].partition_point(|p| p[c] < v);
        let hi = a + pts[a..b].partition_point(|p| p[c] <= v);
        let m = if lo == a {
            hi
        } else if hi == b || mid - lo <= hi - mid {
            lo
        } else {
            hi
        };
        boxes[i] = (a, m);
        boxes.push((m, b));
    }
    let mut out: Vec<([u8; 3], usize)> = boxes
        .iter()
        .map(|&(a, b)| {
            let mut s = [0u64; 3];
            for p in &pts[a..b] {
                for c in 0..3 {
                    s[c] += u64::from(p[c]);
                }
            }
            let k = (b - a).max(1) as u64;
            (
                [
                    ((s[0] + k / 2) / k) as u8,
                    ((s[1] + k / 2) / k) as u8,
                    ((s[2] + k / 2) / k) as u8,
                ],
                b - a,
            )
        })
        .collect();
    out.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
    out.dedup_by(|x, y| x.0 == y.0);
    out.into_iter().map(|x| x.0).collect()
}

/// The upstream's "Clusterise": k-means refinement of `palette` (10 rounds on
/// a copy at most 128 pixels wide, each centre moving 30 % of the way to its
/// cluster's mean; an empty cluster restarts at a random opaque pixel). Seeded
/// (upstream used an unseeded generator); transparent pixels take no part
/// (upstream put them into cluster 0). Sorted by use, most used first.
pub fn clusterise(img: &Rgba, palette: &[[u8; 3]], alpha_min: u8, seed: u64) -> Vec<[u8; 3]> {
    if palette.is_empty() {
        return Vec::new();
    }
    let small = if img.w > 128 {
        let nh = ((u64::from(img.h) * 128) / u64::from(img.w)).max(1) as u32;
        box_resize(img, 128, nh)
    } else {
        img.clone()
    };
    let pts = opaque_sample(&small, alpha_min, usize::MAX);
    if pts.is_empty() {
        return palette.to_vec();
    }
    let mut centres: Vec<[f64; 3]> = palette
        .iter()
        .map(|c| [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])
        .collect();
    let mut rng = Rng::new(seed ^ 0x1A6E_70D1);
    let mut counts = vec![0usize; centres.len()];
    for _ in 0..10 {
        let rounded: Vec<[u8; 3]> = centres
            .iter()
            .map(|c| c.map(|v| v.round().clamp(0.0, 255.0) as u8))
            .collect();
        let mut sums = vec![[0.0f64; 3]; centres.len()];
        counts.iter_mut().for_each(|c| *c = 0);
        for p in &pts {
            let k = nearest(&rounded, *p);
            counts[k] += 1;
            for c in 0..3 {
                sums[k][c] += f64::from(p[c]);
            }
        }
        for (k, centre) in centres.iter_mut().enumerate() {
            if counts[k] == 0 {
                let p = pts[rng.range_i64(0, pts.len() as i64 - 1) as usize];
                *centre = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
                continue;
            }
            for c in 0..3 {
                let mean = sums[k][c] / counts[k] as f64;
                centre[c] += (mean - centre[c]) * 0.3;
            }
        }
    }
    let mut out: Vec<([u8; 3], usize)> = centres
        .iter()
        .zip(&counts)
        .map(|(c, &n)| (c.map(|v| v.round().clamp(0.0, 255.0) as u8), n))
        .collect();
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out.into_iter().map(|x| x.0).collect()
}

/// How the picture lies on the Piano Roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// x → time, y → pitch (top row = highest key): the picture appears upright
    /// in PlumSpirit's horizontal Piano Roll.
    PianoRoll,
    /// ImageToMidi's own layout: x → key, bottom row first in time (upright in
    /// falling-note players, rotated in a horizontal Piano Roll).
    Upstream,
}

pub const ORIENTATIONS: &[&str] = &[
    "Upright in the Piano Roll (x = time, y = pitch)",
    "Original Image to MIDI (x = key, bottom row first)",
];

/// Where a long note is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitFrom {
    /// Every `len` pixels from the start of the picture (upstream option).
    ImageStart,
    /// When a note reaches `len` pixels.
    NoteStart,
}

pub const SPLITS: &[&str] = &["From the start of the image", "From the start of each note"];

/// Conversion settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The project's keyboard (v0.9 256-Key Mode): the keys may reach its
    /// highest key (127, or 255).
    pub keys: crate::model::project::KeyRange,
    pub first_key: u8,
    pub last_key: u8,
    pub orientation: Orientation,
    /// Pixels along time after resizing (`None`: keep the aspect ratio).
    pub time_pixels: Option<u32>,
    pub ticks_per_pixel: u64,
    /// Pixels with alpha below this are transparent.
    pub alpha_min: u8,
    pub palette: Vec<[u8; 3]>,
    /// Palette entries that make no notes (background).
    pub ignored: Vec<bool>,
    /// Maximum note length in pixels (0 = none).
    pub max_len: u32,
    pub split: SplitFrom,
    /// One note per pixel (no run-length joining).
    pub pixel_notes: bool,
    pub velocity: u8,
    /// Colour shown for each palette entry: the palette itself, or seeded
    /// random colours (upstream "random colours").
    pub random_colors: Option<u64>,
    pub transform: ImageTransform,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            keys: Default::default(),
            first_key: 0,
            // The standard keyboard's top key.
            last_key: crate::model::note::MAX_PITCH,
            orientation: Orientation::PianoRoll,
            time_pixels: None,
            ticks_per_pixel: 5,
            alpha_min: 128,
            palette: Vec::new(),
            ignored: Vec::new(),
            max_len: 0,
            split: SplitFrom::NoteStart,
            pixel_notes: false,
            velocity: 1,
            random_colors: None,
            transform: ImageTransform::default(),
        }
    }
}

/// The processed picture: resized and transformed, before the palette.
pub fn prepare(src: &Rgba, s: &Settings) -> Result<Rgba, String> {
    let top = s.keys.max_pitch();
    if s.last_key < s.first_key || s.last_key > top {
        return Err(format!(
            "The last key must be at or above the first key (0 to {top})."
        ));
    }
    let img = transform(src, &s.transform)?;
    let keys = u32::from(s.last_key - s.first_key) + 1;
    let (w, h) = match s.orientation {
        Orientation::PianoRoll => {
            let t = s.time_pixels.unwrap_or_else(|| {
                ((u64::from(img.w) * u64::from(keys)) / u64::from(img.h)).max(1) as u32
            });
            (t, keys)
        }
        Orientation::Upstream => {
            let t = s.time_pixels.unwrap_or_else(|| {
                ((u64::from(img.h) * u64::from(keys)) / u64::from(img.w)).max(1) as u32
            });
            (keys, t)
        }
    };
    let time = if s.orientation == Orientation::PianoRoll {
        w
    } else {
        h
    };
    if time > MAX_TIME_PIXELS {
        return Err(format!(
            "The picture would be {time} pixels long in time; at most {MAX_TIME_PIXELS} are allowed. Set a time resolution."
        ));
    }
    Ok(box_resize(&img, w, h))
}

/// Palette index of every pixel of `img` (`u32::MAX` = transparent / ignored).
pub fn index_pixels(img: &Rgba, s: &Settings) -> Vec<u32> {
    runs::index_controlled(img, s, |_| Ok(()), || Ok(()))
        .expect("Image palette index allocation failed")
}

/// The display colour of palette entry `k`.
pub fn display_color(s: &Settings, k: usize) -> [u8; 3] {
    match s.random_colors {
        Some(seed) => {
            let mut r = Rng::new(seed.wrapping_mul(0x9E37_79B9).wrapping_add(k as u64 + 256));
            hsv([r.unit() * 360.0, 1.0, 0.5])
        }
        None => s.palette[k],
    }
}

fn hsv(c: [f64; 3]) -> [u8; 3] {
    let (h, s, v) = (c[0].rem_euclid(360.0), c[1], c[2]);
    let ch = v * s;
    let x = ch * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - ch;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (ch, x, 0.0),
        1 => (x, ch, 0.0),
        2 => (0.0, ch, x),
        3 => (0.0, x, ch),
        4 => (x, 0.0, ch),
        _ => (ch, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// Converts the processed picture into notes (ticks from 0). Refuses with a
/// sentence when more than `limit` notes would be made (counted while
/// forming them, before they are stored).
pub fn convert(img: &Rgba, s: &Settings, limit: usize) -> Result<Generated, String> {
    let idx = runs::index_controlled(img, s, |_| Ok(()), || Ok(()))?;
    let tpp = s.ticks_per_pixel.max(1);
    let vel = s.velocity.clamp(1, 127);
    let mut notes: Vec<ColorNote> = Vec::new();
    let mut cursor = runs::Cursor::new(&idx, img.w, img.h, s)?;
    let (lines, _) = cursor.dimensions();
    if lines > 256
        || (s.orientation == Orientation::PianoRoll && lines > u32::from(s.last_key) + 1)
        || (s.orientation == Orientation::Upstream && u32::from(s.first_key) + lines > 256)
    {
        return Err("The processed picture extends beyond its native key range.".into());
    }
    loop {
        let count = cursor.visit_controlled(
            &idx,
            runs::MAX_BATCH,
            |run| {
                if notes.len() >= limit {
                    return Err(format!(
                        "The picture would make more than {} notes; lower the resolution or the palette size.",
                        crate::midi::group_digits(limit as u64)
                    ));
                }
                notes.push(ColorNote {
                    start: u64::from(run.start) * tpp,
                    dur: u64::from(run.length) * tpp,
                    pitch: match s.orientation {
                        Orientation::PianoRoll => s.last_key - run.line as u8,
                        Orientation::Upstream => s.first_key + run.line as u8,
                    },
                    vel,
                    color: run.color,
                });
                Ok(())
            },
            || Ok(()),
        )?;
        if count == 0 {
            break;
        }
    }
    let colors = (0..s.palette.len())
        .map(|k| {
            let mut ci = ColorInfo::new(display_color(s, k));
            ci.name = format!(
                "#{:02X}{:02X}{:02X}",
                s.palette[k][0], s.palette[k][1], s.palette[k][2]
            );
            ci
        })
        .collect();
    let mut g = Generated { notes, colors };
    g.sort();
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Rgba {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.push(f(x, y));
            }
        }
        Rgba::new(w, h, px).unwrap()
    }

    const R: [u8; 4] = [255, 0, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const T: [u8; 4] = [0, 0, 0, 0];

    #[test]
    fn box_resize_averages_by_area_and_keeps_exact_sizes() {
        let a = img(4, 2, |x, _| if x < 2 { R } else { B });
        let b = box_resize(&a, 2, 1);
        assert_eq!(b.px, vec![R, B]);
        let c = box_resize(&a, 1, 1);
        assert_eq!(c.px, vec![[128, 0, 128, 255]]);
        assert_eq!(box_resize(&a, 4, 2), a);
    }

    #[test]
    fn controlled_image_operations_preflight_and_cancel_inside_work() {
        use std::cell::Cell;
        let a = img(1024, 1024, |_, _| R);
        let checks = Cell::new(0);
        let called = Cell::new(false);
        let error = box_resize_controlled(
            &a,
            1,
            1,
            |w, h, scratch| {
                assert_eq!((w, h), (1, 1));
                assert!(scratch >= 2048 * std::mem::size_of::<(u32, f64)>());
                called.set(true);
                Ok(())
            },
            || {
                checks.set(checks.get() + 1);
                // Initial, X/Y span, row and first source-pixel checkpoints
                // precede the next check inside this million-pixel output cell.
                if checks.get() == 6 {
                    Err("cancel inside sample accumulation".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert!(called.get());
        assert_eq!(checks.get(), 6);
        assert_eq!(error, "cancel inside sample accumulation");
        assert_eq!(a.at(0, 0), R);

        let error = transform_controlled(
            &a,
            &ImageTransform::default(),
            |w, h, scratch| {
                assert_eq!((w, h, scratch), (1024, 1024, 0));
                Err("quota before allocation".into())
            },
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error, "quota before allocation");
        let checks = Cell::new(0);
        let error = transform_controlled(
            &a,
            &ImageTransform {
                flip_x: true,
                ..Default::default()
            },
            |_, _, _| Ok(()),
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    Err("cancel inside mapping".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error, "cancel inside mapping");
    }

    #[test]
    fn absurd_sizes_are_refused_before_allocation() {
        assert!(check_size(0, 5).is_err());
        assert!(check_size(MAX_SIDE + 1, 1).is_err());
        assert!(check_size(16_000, 16_000).is_ok() == (16_000u64 * 16_000 <= MAX_PIXELS));
        assert!(Rgba::new(2, 2, vec![R; 3]).is_err());
    }

    #[test]
    fn median_cut_is_deterministic_and_finds_distinct_colours() {
        let a = img(8, 8, |x, y| match (x < 4, y < 4) {
            (true, true) => R,
            (false, true) => B,
            (true, false) => [0, 255, 0, 255],
            (false, false) => T,
        });
        let p = median_cut(&a, 3, 128);
        assert_eq!(p.len(), 3);
        for c in [[255, 0, 0], [0, 0, 255], [0, 255, 0]] {
            assert!(p.contains(&c), "{p:?}");
        }
        assert_eq!(p, median_cut(&a, 3, 128));
        let k = clusterise(&a, &p, 128, 7);
        assert_eq!(k, clusterise(&a, &p, 128, 7));
        assert_eq!(k.len(), 3);
    }

    #[test]
    fn a_stripe_becomes_one_note_per_row_with_runs_in_time() {
        // 6 × 2: top row R R B B R R, bottom row transparent then R.
        let a = img(6, 2, |x, y| {
            if y == 0 {
                if (2..4).contains(&x) { B } else { R }
            } else if x < 3 {
                T
            } else {
                R
            }
        });
        let s = Settings {
            first_key: 60,
            last_key: 61,
            time_pixels: Some(6),
            ticks_per_pixel: 10,
            palette: vec![[255, 0, 0], [0, 0, 255]],
            ..Default::default()
        };
        let p = prepare(&a, &s).unwrap();
        let g = convert(&p, &s, 1000).unwrap();
        let v: Vec<(u64, u64, u8, u32)> = g
            .notes
            .iter()
            .map(|n| (n.start, n.dur, n.pitch, n.color))
            .collect();
        assert_eq!(
            v,
            vec![
                (0, 20, 61, 0),
                (20, 20, 61, 1),
                (30, 30, 60, 0),
                (40, 20, 61, 0)
            ]
        );
        assert!(g.notes.iter().all(|n| n.vel == 1));
    }

    #[test]
    fn max_length_splits_from_the_image_or_the_note_start_and_pixel_mode_splits_all() {
        let a = img(10, 1, |x, _| if x == 0 { T } else { R });
        let base = Settings {
            first_key: 60,
            last_key: 60,
            time_pixels: Some(10),
            ticks_per_pixel: 1,
            palette: vec![[255, 0, 0]],
            max_len: 4,
            ..Default::default()
        };
        let lens = |s: &Settings| -> Vec<(u64, u64)> {
            convert(&prepare(&a, s).unwrap(), s, 100)
                .unwrap()
                .notes
                .iter()
                .map(|n| (n.start, n.dur))
                .collect()
        };
        assert_eq!(lens(&base), vec![(1, 4), (5, 4), (9, 1)]);
        let s2 = Settings {
            split: SplitFrom::ImageStart,
            ..base.clone()
        };
        assert_eq!(lens(&s2), vec![(1, 3), (4, 4), (8, 2)]);
        let s3 = Settings {
            pixel_notes: true,
            max_len: 0,
            ..base.clone()
        };
        assert_eq!(lens(&s3).len(), 9);
        // The note cap refuses before storing more.
        assert!(convert(&prepare(&a, &s3).unwrap(), &s3, 5).is_err());
    }

    #[test]
    fn upstream_orientation_runs_from_the_bottom_row_and_columns_are_keys() {
        // 2 × 3: column 0 red at the bottom two rows; column 1 blue at the top.
        let a = img(2, 3, |x, y| match (x, y) {
            (0, 1) | (0, 2) => R,
            (1, 0) => B,
            _ => T,
        });
        let s = Settings {
            first_key: 40,
            last_key: 41,
            orientation: Orientation::Upstream,
            time_pixels: Some(3),
            ticks_per_pixel: 5,
            palette: vec![[255, 0, 0], [0, 0, 255]],
            ..Default::default()
        };
        let g = convert(&prepare(&a, &s).unwrap(), &s, 100).unwrap();
        let v: Vec<(u64, u64, u8, u32)> = g
            .notes
            .iter()
            .map(|n| (n.start, n.dur, n.pitch, n.color))
            .collect();
        assert_eq!(v, vec![(0, 10, 40, 0), (10, 5, 41, 1)]);
    }

    #[test]
    fn transparency_threshold_and_ignored_colours_make_no_notes() {
        let a = img(4, 1, |x, _| match x {
            0 => [255, 0, 0, 100],
            1 => [255, 0, 0, 200],
            2 => B,
            _ => [250, 250, 250, 255],
        });
        let s = Settings {
            first_key: 60,
            last_key: 60,
            time_pixels: Some(4),
            ticks_per_pixel: 1,
            palette: vec![[255, 0, 0], [0, 0, 255], [255, 255, 255]],
            ignored: vec![false, false, true],
            ..Default::default()
        };
        let g = convert(&prepare(&a, &s).unwrap(), &s, 100).unwrap();
        let v: Vec<(u64, u32)> = g.notes.iter().map(|n| (n.start, n.color)).collect();
        assert_eq!(v, vec![(1, 0), (2, 1)]);
    }

    #[test]
    fn flips_rotation_and_scale_move_pixels_exactly() {
        let a = img(3, 2, |x, y| if (x, y) == (0, 0) { R } else { B });
        let fx = transform(
            &a,
            &ImageTransform {
                flip_x: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fx.at(2, 0), R);
        let fy = transform(
            &a,
            &ImageTransform {
                scale_y: -1.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fy.at(0, 1), R);
        let r = transform(
            &a,
            &ImageTransform {
                rotate: 90.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((r.w, r.h), (2, 3));
        // Clockwise: the top-left pixel goes to the top-right.
        assert_eq!(r.at(1, 0), R);
        let s = transform(
            &a,
            &ImageTransform {
                scale_x: 2.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((s.w, s.h), (6, 2));
        assert_eq!((s.at(0, 0), s.at(1, 0), s.at(2, 0)), (R, R, B));
        let r45 = transform(
            &a,
            &ImageTransform {
                rotate: 45.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r45.px.contains(&T) && r45.px.contains(&B));
        assert!(
            transform(
                &a,
                &ImageTransform {
                    scale_x: 0.0,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn random_colours_are_seeded_and_names_keep_the_palette_colour() {
        let a = img(2, 1, |x, _| if x == 0 { R } else { B });
        let s = Settings {
            first_key: 60,
            last_key: 60,
            time_pixels: Some(2),
            palette: vec![[255, 0, 0], [0, 0, 255]],
            random_colors: Some(3),
            ..Default::default()
        };
        let g = convert(&prepare(&a, &s).unwrap(), &s, 100).unwrap();
        let g2 = convert(&prepare(&a, &s).unwrap(), &s, 100).unwrap();
        assert_eq!(g, g2);
        assert_ne!(g.colors[0].rgb, [255, 0, 0]);
        assert_eq!(g.colors[0].name, "#FF0000");
    }
}
