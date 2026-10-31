//! Dark / Light / System themes.
//!
//! Both palettes are flat desktop palettes: neutral grays, restrained separators and
//! a muted plum accent (in the logo's purple family) used only for selection,
//! the active track, the active tool, focus and the edit cursor. Selected notes differ
//! from normal notes in hue *and* lightness and carry a contrasting border, so they
//! stay distinguishable without relying on color alone.
//!
//! "System" is implemented with egui's `ThemePreference::System`, which follows the
//! operating system theme reported by the window system (winit), including live
//! changes on Windows 10/11.

use crate::settings::ThemeSetting;
use egui::{Color32, CornerRadius, Margin, Stroke, Theme, ThemePreference, Vec2, epaint::Shadow};

/// Colors used by the piano roll and custom widgets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub roll_bg: Color32,
    pub roll_black_row: Color32,
    pub grid_sub: Color32,
    pub grid_minor: Color32,
    pub grid_major: Color32,
    pub octave_line: Color32,
    pub note_fill: Color32,
    pub note_border: Color32,
    pub note_selected_fill: Color32,
    pub note_selected_border: Color32,
    pub note_hover_border: Color32,
    pub note_ghost: Color32,
    pub accent: Color32,
    pub accent_weak: Color32,
    pub cursor_line: Color32,
    pub ruler_bg: Color32,
    pub ruler_text: Color32,
    pub key_white: Color32,
    pub key_black: Color32,
    pub key_text: Color32,
    pub key_c_text: Color32,
    pub key_separator: Color32,
    pub panel_bg: Color32,
    pub panel_border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub box_select_fill: Color32,
    pub box_select_stroke: Color32,
    pub scrollbar_track: Color32,
    pub scrollbar_thumb: Color32,
    pub scrollbar_thumb_hover: Color32,
    pub overlay_bg: Color32,
    pub error: Color32,
    pub warning: Color32,
    /// Playback position line (distinct from the plum edit cursor).
    pub playhead: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const DARK: Palette = Palette {
    roll_bg: rgb(0x22, 0x23, 0x27),
    roll_black_row: rgb(0x1D, 0x1E, 0x21),
    grid_sub: rgb(0x2A, 0x2B, 0x30),
    grid_minor: rgb(0x31, 0x33, 0x39),
    grid_major: rgb(0x46, 0x48, 0x50),
    octave_line: rgb(0x39, 0x3B, 0x42),
    note_fill: rgb(0x5C, 0x9C, 0xB4),
    note_border: rgb(0x2E, 0x55, 0x66),
    note_selected_fill: rgb(0xC0, 0x8F, 0xD6),
    note_selected_border: rgb(0xF3, 0xE6, 0xF9),
    note_hover_border: rgb(0xE4, 0xE6, 0xEA),
    note_ghost: Color32::from_rgba_premultiplied(0x60, 0x48, 0x6C, 0xA0),
    accent: rgb(0xA3, 0x78, 0xBA),
    accent_weak: rgb(0x46, 0x3A, 0x50),
    cursor_line: rgb(0xC9, 0x9A, 0xDE),
    ruler_bg: rgb(0x2B, 0x2C, 0x31),
    ruler_text: rgb(0xB4, 0xB7, 0xBE),
    key_white: rgb(0xD2, 0xD4, 0xD8),
    key_black: rgb(0x33, 0x35, 0x3A),
    key_text: rgb(0x4B, 0x4E, 0x55),
    key_c_text: rgb(0x2B, 0x2C, 0x31),
    key_separator: rgb(0x9A, 0x9D, 0xA4),
    panel_bg: rgb(0x2B, 0x2C, 0x31),
    panel_border: rgb(0x3B, 0x3D, 0x44),
    text: rgb(0xDD, 0xDF, 0xE3),
    text_weak: rgb(0x93, 0x96, 0x9E),
    box_select_fill: Color32::from_rgba_premultiplied(0x2C, 0x22, 0x33, 0x40),
    box_select_stroke: rgb(0xB8, 0x8C, 0xCF),
    scrollbar_track: rgb(0x26, 0x27, 0x2B),
    scrollbar_thumb: rgb(0x4A, 0x4C, 0x54),
    scrollbar_thumb_hover: rgb(0x60, 0x63, 0x6C),
    overlay_bg: Color32::from_rgba_premultiplied(0x18, 0x18, 0x1B, 0xE6),
    error: rgb(0xE5, 0x73, 0x73),
    warning: rgb(0xE0, 0xB0, 0x5E),
    playhead: rgb(0xF0, 0xC0, 0x55),
};

pub const LIGHT: Palette = Palette {
    roll_bg: rgb(0xFF, 0xFF, 0xFF),
    roll_black_row: rgb(0xF1, 0xF2, 0xF4),
    grid_sub: rgb(0xEE, 0xEF, 0xF1),
    grid_minor: rgb(0xDD, 0xDF, 0xE3),
    grid_major: rgb(0xB5, 0xB8, 0xBF),
    octave_line: rgb(0xD0, 0xD2, 0xD7),
    note_fill: rgb(0x3E, 0x7F, 0x9C),
    note_border: rgb(0x23, 0x4E, 0x61),
    note_selected_fill: rgb(0x7E, 0x3F, 0x9A),
    note_selected_border: rgb(0x2E, 0x12, 0x3B),
    note_hover_border: rgb(0x1C, 0x1D, 0x21),
    note_ghost: Color32::from_rgba_premultiplied(0x3F, 0x20, 0x4D, 0x80),
    accent: rgb(0x7E, 0x4A, 0x96),
    accent_weak: rgb(0xEA, 0xDF, 0xF0),
    cursor_line: rgb(0x8A, 0x4F, 0xA6),
    ruler_bg: rgb(0xF0, 0xF1, 0xF3),
    ruler_text: rgb(0x4A, 0x4D, 0x54),
    key_white: rgb(0xFF, 0xFF, 0xFF),
    key_black: rgb(0x3A, 0x3C, 0x42),
    key_text: rgb(0x8A, 0x8D, 0x94),
    key_c_text: rgb(0x2B, 0x2C, 0x31),
    key_separator: rgb(0xC3, 0xC6, 0xCC),
    panel_bg: rgb(0xF3, 0xF3, 0xF5),
    panel_border: rgb(0xD5, 0xD7, 0xDC),
    text: rgb(0x1F, 0x20, 0x24),
    text_weak: rgb(0x6A, 0x6D, 0x74),
    box_select_fill: Color32::from_rgba_premultiplied(0x18, 0x0E, 0x1C, 0x30),
    box_select_stroke: rgb(0x7E, 0x4A, 0x96),
    scrollbar_track: rgb(0xEC, 0xED, 0xF0),
    scrollbar_thumb: rgb(0xC2, 0xC5, 0xCB),
    scrollbar_thumb_hover: rgb(0xA6, 0xAA, 0xB2),
    overlay_bg: Color32::from_rgba_premultiplied(0xE8, 0xE8, 0xE9, 0xF0),
    error: rgb(0xB8, 0x32, 0x32),
    warning: rgb(0x9A, 0x62, 0x00),
    playhead: rgb(0xC0, 0x6A, 0x00),
};

// ----------------------------------------------------------- track colours

/// The PlumSpirit Track palette (v1.0-beta1 B1-0056, contract §009 — replaces
/// the v0.8 pastel set the owner called "AI-picked"): twenty-four colours
/// after historical artists' pigments and dyes, not an evenly spread hue
/// wheel. Like the subchannel colours they carry real saturation (OKLCH
/// chroma 0.11–0.19), with a deliberate minority of pale (Mauveine, Celadon,
/// Rose Madder, Woad, Naples) and earth (Sienna, Ochre, Terre Verte,
/// Verdaccio, Mars Violet) tones so lightness also separates tracks; no
/// purple before track 6, so no early track reads as the plum selection
/// colour. Every colour has WCAG non-text contrast ≥ 3.2 : 1 against its
/// theme's roll background; closest pair ΔE_OK ≥ 0.063 (dark) / 0.055
/// (light); neighbours ≥ 0.13 (next track), 0.11 (two apart), 0.095 (three
/// apart), and direct neighbours stay ≥ 0.09 apart under simulated
/// protanopia / deuteranopia. Each entry: name,
/// dark-theme colour, light-theme colour (the same hue, deeper, for the white
/// roll).
pub const TRACK_PALETTE: [(&str, [u8; 3], [u8; 3]); 24] = [
    ("Vermilion", [0xF3, 0x5D, 0x40], [0xCF, 0x3B, 0x1D]),
    ("Turquoise", [0x46, 0xC6, 0xCF], [0x09, 0x9C, 0xA4]),
    ("Prussian", [0x43, 0x7E, 0xB9], [0x24, 0x60, 0x99]),
    ("Orpiment", [0xF3, 0xB0, 0x1D], [0xB6, 0x81, 0x03]),
    ("Sienna", [0xBC, 0x6F, 0x4C], [0x9C, 0x52, 0x2E]),
    ("Cobalt Violet", [0xB7, 0x6E, 0xC7], [0x97, 0x50, 0xA7]),
    ("Malachite", [0x4C, 0xB8, 0x6A], [0x25, 0x98, 0x4D]),
    ("Madder", [0xCC, 0x4F, 0x6E], [0xA9, 0x2E, 0x52]),
    ("Mauveine", [0xC9, 0xB3, 0xE2], [0x97, 0x81, 0xAE]),
    ("Ochre", [0xB5, 0x89, 0x47], [0x96, 0x6B, 0x26]),
    ("Celadon", [0xA9, 0xD7, 0xBD], [0x6B, 0x96, 0x7E]),
    ("Han Purple", [0x90, 0x72, 0xDF], [0x73, 0x53, 0xBE]),
    ("Lac", [0xD1, 0x53, 0xA2], [0xAE, 0x32, 0x83]),
    ("Rose Madder", [0xEC, 0xA2, 0xB6], [0xBA, 0x75, 0x88]),
    ("Terre Verte", [0x81, 0xA2, 0x5E], [0x64, 0x83, 0x41]),
    ("Ultramarine", [0x64, 0x7B, 0xEB], [0x49, 0x5C, 0xC9]),
    ("Woad", [0xA0, 0xC3, 0xDB], [0x70, 0x91, 0xA7]),
    ("Cadmium", [0xF0, 0x8A, 0x2A], [0xCA, 0x6E, 0x03]),
    ("Viridian", [0x39, 0xAE, 0x96], [0x02, 0x8F, 0x78]),
    ("Indigo", [0x6A, 0x6B, 0xAC], [0x4B, 0x4A, 0x88]),
    ("Verdaccio", [0x8B, 0x8A, 0x4F], [0x6D, 0x6C, 0x32]),
    ("Cerulean", [0x32, 0xA5, 0xD4], [0x08, 0x86, 0xB0]),
    ("Naples", [0xE4, 0xD4, 0x98], [0x9B, 0x8C, 0x52]),
    ("Mars Violet", [0x9A, 0x60, 0x77], [0x7B, 0x44, 0x5A]),
];

/// The smallest WCAG non-text contrast (1.4.11) an automatic track colour
/// keeps against its theme's roll background — the palette and every variant
/// beyond it (B1-0056).
pub const TRACK_MIN_CONTRAST: f64 = 3.0;

/// Variants used once the palette is exhausted (track ids 24, 25, …): per
/// round, a fixed change of OKLCH lightness, chroma (scale) and hue (degrees)
/// — lighter / deeper and slightly turned, never random.
const TRACK_VARIANTS: [(f64, f64, f64); 6] = [
    (0.09, 0.80, 10.0),
    (-0.09, 1.00, -10.0),
    (0.045, 0.70, 18.0),
    (-0.13, 0.90, -18.0),
    (0.13, 0.75, 5.0),
    (-0.045, 0.90, -5.0),
];

fn srgb_decode(v: u8) -> f64 {
    let x = f64::from(v) / 255.0;
    if x <= 0.040_45 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(x: f64) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let v = if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round() as u8
}

/// sRGB → OKLab (Björn Ottosson's reference matrices).
fn oklab([r, g, b]: [u8; 3]) -> (f64, f64, f64) {
    let (r, g, b) = (srgb_decode(r), srgb_decode(g), srgb_decode(b));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    )
}

/// OKLab → sRGB (out-of-gamut channels clamped).
fn from_oklab(l: f64, a: f64, b: f64) -> [u8; 3] {
    let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l - 0.089_484_177_5 * a - 1.291_485_548 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        srgb_encode(4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3),
        srgb_encode(-1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3),
        srgb_encode(-0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_614_701 * s3),
    ]
}

/// Perceptual distance of two colours (OKLab Euclidean).
pub fn color_distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (l1, a1, b1) = oklab(a);
    let (l2, a2, b2) = oklab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}

/// WCAG relative luminance of an sRGB colour.
pub fn wcag_luminance([r, g, b]: [u8; 3]) -> f64 {
    0.2126 * srgb_decode(r) + 0.7152 * srgb_decode(g) + 0.0722 * srgb_decode(b)
}

/// WCAG contrast ratio of two colours (1 – 21).
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (x, y) = (wcag_luminance(a), wcag_luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `rgb` with its OKLab lightness moved away from `bg` in 0.01 steps until
/// its contrast against `bg` reaches `min` (unchanged when it already does).
fn with_contrast(rgb: [u8; 3], bg: [u8; 3], min: f64) -> [u8; 3] {
    if contrast_ratio(rgb, bg) >= min {
        return rgb;
    }
    let (l, a, b) = oklab(rgb);
    let step = if wcag_luminance(bg) < 0.18 {
        0.01
    } else {
        -0.01
    };
    let mut lt = l;
    for _ in 0..100 {
        lt = (lt + step).clamp(0.0, 1.0);
        let c = from_oklab(lt, a, b);
        if contrast_ratio(c, bg) >= min {
            return c;
        }
    }
    rgb
}

/// The automatic colour of track `id` (its stable id, so a track keeps its
/// colour when tracks are moved): [`TRACK_PALETTE`] entry `id % 24`, and from
/// id 24 on a deterministic variant of it ([`TRACK_VARIANTS`]), kept at
/// [`TRACK_MIN_CONTRAST`] against the theme's roll background.
pub fn auto_track_rgb(id: u32, dark: bool) -> [u8; 3] {
    // L-G05: the colour is a pure function of (id, dark) but costs OKLab
    // round trips and a contrast search; with 15,000 Onion tracks it was
    // recomputed for every ghost every frame (65 % of the frame). Track ids
    // are small and dense, so a lazily grown table per theme holds them.
    const MAX_CACHED: usize = 1 << 18;
    static CACHE: std::sync::RwLock<[Vec<[u8; 4]>; 2]> =
        std::sync::RwLock::new([Vec::new(), Vec::new()]);
    let (i, d) = (id as usize, usize::from(dark));
    if let Ok(c) = CACHE.read()
        && let Some(&[r, g, b, 1]) = c[d].get(i)
    {
        return [r, g, b];
    }
    let v = compute_auto_track_rgb(id, dark);
    if i < MAX_CACHED
        && let Ok(mut c) = CACHE.write()
    {
        let t = &mut c[d];
        if t.len() <= i {
            t.resize(i + 1, [0; 4]);
        }
        t[i] = [v[0], v[1], v[2], 1];
    }
    v
}

fn compute_auto_track_rgb(id: u32, dark: bool) -> [u8; 3] {
    let n = TRACK_PALETTE.len() as u32;
    let (_, d, l) = TRACK_PALETTE[(id % n) as usize];
    let base = if dark { d } else { l };
    let range = if dark { (0.50, 0.90) } else { (0.36, 0.72) };
    let bg = if dark { DARK.roll_bg } else { LIGHT.roll_bg };
    let bg = [bg.r(), bg.g(), bg.b()];
    let round = id / n;
    let v = with_contrast(color_variant(base, round, range), bg, TRACK_MIN_CONTRAST);
    if round == 0 || color_distance(v, base) >= 0.04 {
        return v;
    }
    // The contrast floor undid the variant's lightness step: step away from
    // the background instead (always more contrast).
    let (_, a, b) = oklab(v);
    let (lb, ..) = oklab(base);
    let l = if dark { lb + 0.10 } else { lb - 0.10 };
    with_contrast(from_oklab(l.clamp(0.0, 1.0), a, b), bg, TRACK_MIN_CONTRAST)
}

/// Variant `round` of `base` (round 0: `base` itself): per round a fixed
/// change of OKLCH lightness, chroma and hue ([`TRACK_VARIANTS`], then 3° more
/// per full cycle), lightness kept within `range` — deterministic, never
/// random. Shared by the track colours and the Manual note palette's
/// expansion beyond its chosen colours (v0.9 §066).
fn color_variant(base: [u8; 3], round: u32, (lo, hi): (f64, f64)) -> [u8; 3] {
    if round == 0 {
        return base;
    }
    let k = (round - 1) as usize;
    let (dl, cs, dh) = TRACK_VARIANTS[k % TRACK_VARIANTS.len()];
    let extra = 3.0 * (k / TRACK_VARIANTS.len()) as f64;
    let (l, a, b) = oklab(base);
    let c = (a.hypot(b) * cs).min(0.17);
    let h = b.atan2(a) + (dh + extra).to_radians();
    from_oklab((l + dl).clamp(lo, hi), c * h.cos(), c * h.sin())
}

/// The automatic colour of track `id` with opacity `alpha` (0–255).
pub fn auto_track_color(id: u32, dark: bool, alpha: u8) -> Color32 {
    let [r, g, b] = auto_track_rgb(id, dark);
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

/// The canonical colour of a track (§096): its Track Properties colour when
/// set, else [`auto_track_color`]. Used by the track list, the Onion chooser,
/// Track Properties, ghost notes, graph ghosts and the keyboard.
pub fn track_color(id: u32, custom: Option<[u8; 3]>, dark: bool, alpha: u8) -> Color32 {
    match custom {
        Some([r, g, b]) => Color32::from_rgba_unmultiplied(r, g, b, alpha),
        None => auto_track_color(id, dark, alpha),
    }
}

/// How a source's notes are coloured — the one note-colour resolver (§096,
/// §125; v0.9 §068 with the [`NotePalette`]) shared by the Piano Roll (exact
/// and dense), Onion Skin and the live keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteTint {
    /// The active track as the Piano Roll draws it: subchannel colours.
    Subchannels,
    /// Any other track: its Track colour turned to each subchannel's hue
    /// ([`ghost_tints`]).
    Track([u8; 3]),
    /// v0.9 (V09-S17): the MIDI Player's identity colour of the track whose
    /// subchannel 0 is palette id `base` and which has `count` subchannel
    /// ids — palette id `base + sub mod count` at full saturation, never
    /// shaded by velocity (`docs/PLAYER.md` P7, `dense::PlayedColors`).
    Played { base: u32, count: u32 },
}

impl NoteTint {
    /// The tint of a track (stable `id`, Track Properties colour) — the active
    /// track is drawn in subchannel colours, every other in its Track colour.
    pub fn of(id: u32, custom: Option<[u8; 3]>, active: bool, dark: bool) -> NoteTint {
        if active {
            NoteTint::Subchannels
        } else {
            let c = track_color(id, custom, dark, 255);
            NoteTint::Track([c.r(), c.g(), c.b()])
        }
    }
}

/// Cached canonical note colours (opaque): one table lookup per note.
#[derive(Default)]
pub struct NoteColors {
    subs: SubPalettes,
    ghosts: GhostTints,
}

impl NoteColors {
    /// Colour of a note of `tint`, subchannel `sub`, `velocity`.
    pub fn get(
        &mut self,
        palette: &NotePalette,
        tint: NoteTint,
        sub: u32,
        velocity: u8,
        dark: bool,
    ) -> Color32 {
        match tint {
            NoteTint::Subchannels => {
                self.subs.get(palette, sub, dark).fill[usize::from(velocity.min(127))]
            }
            NoteTint::Track([r, g, b]) => {
                self.ghosts
                    .get_with_alpha(palette, Color32::from_rgb(r, g, b), sub, 255)
                    [usize::from(velocity.min(127) >> 3)]
            }
            NoteTint::Played { base, count } => {
                let f = palette
                    .entry(base.saturating_add(sub % count.max(1)), 127, dark)
                    .fill;
                Color32::from_rgb(f.r(), f.g(), f.b())
            }
        }
    }
}

/// Outline colour at `darkness` percent (§121): 0 % total white, 50 % the
/// source colour itself, 100 % total black (the engine's one implementation,
/// shared with the dense tiles). Not an opacity.
pub fn outline_shade(rgb: [u8; 3], darkness: u8) -> [u8; 3] {
    plumspirit_engine::view::tiles::outline_shade(rgb, darkness)
}

// ----------------------------------------------------------- velocity colours

/// Saturation of velocity 1 and 127 (HSV). v0.9 §181 (Kiva-level clarity:
/// high saturation): Black MIDI is often all velocity 1, so velocity only
/// shades the colour gently and velocity 1 stays strongly coloured (v0.4 –
/// v0.8 used 0.30, a pastel).
pub const MIN_NOTE_SATURATION: f32 = 0.62;
pub const MAX_NOTE_SATURATION: f32 = 0.90;
/// Selected notes keep more colour at low velocity so their hue stays obvious.
const MIN_SELECTED_SATURATION: f32 = 0.68;
/// The same floor for Onion ghosts, relative to their track colour (v0.8: 0.55).
const MIN_GHOST_SATURATION: f32 = 0.80;

/// Note colours by velocity, computed once per theme (the renderer only looks
/// them up). Index = velocity 0–127; 0 (not a note-on velocity) looks like 1.
#[derive(Debug, PartialEq)]
pub struct VelocityPalette {
    pub fill: [Color32; 128],
    pub border: [Color32; 128],
    pub selected_fill: [Color32; 128],
    /// v0.9 §119: selected notes' borders and muted fills at the Selection
    /// Opacity (looked up, never blended per note).
    pub selected_border: [Color32; 128],
    pub selected_muted_fill: [Color32; 128],
}

fn to_hsv(c: Color32) -> (f32, f32, f32) {
    let [r, g, b, _] = c.to_array().map(|v| f32::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

fn from_hsv(h: f32, s: f32, v: f32) -> Color32 {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let enc = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(enc(r), enc(g), enc(b))
}

/// Saturation for `velocity`: linear from `lo` (velocity 1) to the maximum (127).
pub fn velocity_saturation(velocity: u8, lo: f32) -> f32 {
    let n = f32::from(velocity.clamp(1, 127) - 1) / 126.0;
    lo + n * (MAX_NOTE_SATURATION - lo)
}

/// One entry of a subchannel's velocity palette (fill, border, selected fill):
/// the exact renderer's colours for a note — and, through the dense tiles,
/// the dense renderer's (v0.9 §029). [`build_sub_palette`] is made of these ([`NotePalette::entry`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEntry {
    pub fill: Color32,
    pub border: Color32,
    /// Fill of a selected note: the selection overlay at the palette's
    /// Selection Opacity (v0.9 §119) over `fill`.
    pub selected_fill: Color32,
    /// Border of a selected note (the selection border at that opacity over
    /// `border`).
    pub selected_border: Color32,
    /// Fill of a selected muted note (premultiplied, translucent): the
    /// overlay over the hollow ghost fill of muted notes.
    pub selected_muted_fill: Color32,
}

/// Global automatic Track color preference. Property overrides always win.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TrackColorMode {
    #[default]
    Curated,
    Rainbow16,
    Manual,
}
impl TrackColorMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::Curated => "curated",
            Self::Rainbow16 => "rainbow16",
            Self::Manual => "manual",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "curated" => Some(Self::Curated),
            "rainbow16" => Some(Self::Rainbow16),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// Rebuilt on document structure / preference changes, never per note. The
/// order is every musical Track in document order, including nested Subtracks;
/// Conductor and Folder headers are not musical Tracks and consume no slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackPalette {
    mode: TrackColorMode,
    indices: std::collections::BTreeMap<u32, u32>,
    manual: Vec<[u8; 3]>,
    key: u64,
}
impl TrackPalette {
    pub fn new(mode: TrackColorMode, ids: impl IntoIterator<Item = u32>) -> Self {
        if mode == TrackColorMode::Curated {
            return Self::default();
        }
        let indices = ids
            .into_iter()
            .enumerate()
            .map(|(i, id)| (id, i as u32))
            .collect();
        Self {
            mode,
            indices,
            manual: Vec::new(),
            key: 0,
        }
        .with_manual(&[])
    }
    pub fn with_manual(mut self, colors: &[[u8; 3]]) -> Self {
        if self.mode == TrackColorMode::Curated {
            return self;
        }
        self.manual = if self.mode == TrackColorMode::Manual {
            if colors.is_empty() {
                TRACK_PALETTE.iter().take(16).map(|(_, d, _)| *d).collect()
            } else {
                colors.iter().take(16).copied().collect()
            }
        } else {
            Vec::new()
        };
        let mut h = mix64(0xB216_7A6C ^ self.mode as u64);
        for (&id, &index) in &self.indices {
            h = mix64(h ^ (u64::from(id) << 32) ^ u64::from(index));
        }
        for c in &self.manual {
            h = mix64(h ^ ((c[0] as u64) << 16 | (c[1] as u64) << 8 | c[2] as u64));
        }
        self.key = h;
        self
    }
    pub fn color(&self, id: u32, custom: Option<[u8; 3]>, dark: bool, alpha: u8) -> Color32 {
        if custom.is_some() || self.mode == TrackColorMode::Curated {
            return track_color(id, custom, dark, alpha);
        }
        let index = self.indices.get(&id).copied().unwrap_or(id);
        let [r, g, b] = if self.mode == TrackColorMode::Manual {
            let n = self.manual.len() as u32;
            manual_variant(self.manual[(index % n) as usize], index / n)
        } else {
            RAINBOW_16[(index % 16) as usize]
        };
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }
}

// ------------------------------------------------ note colour preferences

/// How many colours the Manual note palette holds (v0.9 §066): subchannels
/// 1–16; later subchannels get deterministic variants of them.
pub const MANUAL_NOTE_COLORS: usize = 16;

/// Intentional red-to-orange-to-yellow-to-green-to-blue-to-violet-to-rose cycle.
/// Logical Subchannel 1 (Main, internal id 0) starts at red; id 16 repeats it.
pub const RAINBOW_16: [[u8; 3]; 16] = [
    [230, 75, 60],
    [237, 113, 53],
    [242, 169, 59],
    [224, 203, 55],
    [156, 203, 66],
    [71, 175, 89],
    [36, 158, 121],
    [36, 159, 156],
    [43, 171, 183],
    [43, 140, 196],
    [65, 105, 194],
    [102, 89, 188],
    [143, 80, 178],
    [180, 76, 171],
    [209, 79, 135],
    [222, 79, 99],
];

/// v0.9 §064: how subchannel colours are chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NoteColorMode {
    /// Hues from a stored seed (§065): the same seed always gives the same
    /// palette; seed 0 is PlumSpirit's own palette (the v0.4–v0.9 colours).
    #[default]
    RandomBySeed,
    /// Up to 16 chosen colours (§066), automatic variants after them.
    Manual,
    Rainbow16,
}

impl NoteColorMode {
    pub const ALL: [NoteColorMode; 3] = [
        NoteColorMode::RandomBySeed,
        NoteColorMode::Manual,
        NoteColorMode::Rainbow16,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NoteColorMode::RandomBySeed => "Random by Seed",
            NoteColorMode::Manual => "Manual",
            NoteColorMode::Rainbow16 => "Rainbow Through 16 Subchannels",
        }
    }

    /// The settings-file value.
    pub fn key(self) -> &'static str {
        match self {
            NoteColorMode::RandomBySeed => "random",
            NoteColorMode::Manual => "manual",
            NoteColorMode::Rainbow16 => "rainbow16",
        }
    }

    pub fn from_key(s: &str) -> Option<NoteColorMode> {
        NoteColorMode::ALL.into_iter().find(|m| m.key() == s)
    }
}

/// The note colour preferences (v0.9 §064–§067), kept in the application
/// settings: they survive restarts and apply to every project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoteColorPrefs {
    pub mode: NoteColorMode,
    /// Random by Seed: 0 is PlumSpirit's own palette.
    pub seed: u64,
    /// Manual: the colours of subchannels 1–16 (at most
    /// [`MANUAL_NOTE_COLORS`]; empty: [`NoteColorPrefs::default_manual`]).
    pub manual: Vec<[u8; 3]>,
}

impl NoteColorPrefs {
    /// The Manual colours a reset gives: subchannels 1–16 of PlumSpirit's own
    /// palette as dark-theme velocity-127 notes, so switching to Manual
    /// changes nothing until a colour is picked.
    pub fn default_manual() -> Vec<[u8; 3]> {
        let p = NotePalette::default();
        (0..MANUAL_NOTE_COLORS as u32)
            .map(|id| {
                let c = p.entry(id, 127, true).fill;
                [c.r(), c.g(), c.b()]
            })
            .collect()
    }

    /// The colours Manual mode uses: the chosen ones (at most 16), or the
    /// defaults when none are stored.
    pub fn manual_colors(&self) -> Vec<[u8; 3]> {
        if self.manual.is_empty() {
            Self::default_manual()
        } else {
            self.manual
                .iter()
                .take(MANUAL_NOTE_COLORS)
                .copied()
                .collect()
        }
    }
}

/// The resolved note palette — the one canonical note-colour resolver (v0.9
/// §068): every note colour (Piano Roll exact and dense, Onion, keyboard,
/// swatches; the Track List, Player and clip previews use it too) is
/// [`NotePalette::entry`] or a ghost tint turned by
/// [`NotePalette::hue_offset`]. The app keeps the current one in the egui
/// context ([`set_note_palette`] / [`note_palette`]); caches key on
/// [`NotePalette::key`].
#[derive(Clone, Debug)]
pub struct NotePalette {
    prefs: NoteColorPrefs,
    tracks: std::sync::Arc<TrackPalette>,
    /// Manual colours in use (empty in Random by Seed mode).
    manual: Vec<[u8; 3]>,
    /// v0.9 §119: Selection Opacity in percent (0 = selected notes look like
    /// the others, 100 = the full selected-note treatment).
    selection: u8,
    /// What the colours depend on, without the selection opacity.
    colors_key: u64,
    key: u64,
}

/// v0.9 §119: the Selection Opacity of a new palette (percent) — the full
/// selected-note treatment, PlumSpirit's look before the setting existed.
pub const DEFAULT_SELECTION_OPACITY: u8 = 100;

impl Default for NotePalette {
    fn default() -> Self {
        NotePalette::new(NoteColorPrefs::default())
    }
}

/// Equal palettes give every note the same colour (a stored empty Manual
/// list equals the default colours; the seed of Manual mode is unused).
impl PartialEq for NotePalette {
    fn eq(&self, other: &Self) -> bool {
        let seed = |p: &NotePalette| match p.prefs.mode {
            NoteColorMode::RandomBySeed => p.prefs.seed,
            NoteColorMode::Manual | NoteColorMode::Rainbow16 => 0,
        };
        self.prefs.mode == other.prefs.mode
            && seed(self) == seed(other)
            && self.manual == other.manual
            && self.tracks == other.tracks
            && self.selection == other.selection
    }
}

impl Eq for NotePalette {}

/// How one subchannel's colour is derived.
#[derive(Clone, Copy, Debug)]
enum SubBase {
    /// The theme's note colours turned by `hue` (fraction of a turn) with
    /// brightness step `step`; `main`: the v0.4 note colours exactly.
    Theme { hue: f32, step: f32, main: bool },
    /// A chosen colour (or a variant of one).
    Rgb([u8; 3]),
}

/// SplitMix64 finalizer (seeded palettes: stable across runs and platforms).
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A hash in [0, 1).
fn unit(x: u64) -> f32 {
    (x >> 40) as f32 / (1u64 << 24) as f32
}

impl NotePalette {
    pub fn new(prefs: NoteColorPrefs) -> NotePalette {
        let mut prefs = prefs;
        prefs.manual.truncate(MANUAL_NOTE_COLORS);
        let manual = match prefs.mode {
            NoteColorMode::Manual => prefs.manual_colors(),
            NoteColorMode::RandomBySeed | NoteColorMode::Rainbow16 => Vec::new(),
        };
        // What the colours depend on (the seed of an unused mode does not).
        let key = match prefs.mode {
            NoteColorMode::RandomBySeed => mix64(prefs.seed ^ 0x5EED),
            NoteColorMode::Rainbow16 => mix64(0xB216_C1C1),
            NoteColorMode::Manual => manual.iter().fold(mix64(0x3A11), |h, c| {
                mix64(h ^ (u64::from(c[0]) << 16 | u64::from(c[1]) << 8 | u64::from(c[2])))
            }),
        };
        NotePalette {
            prefs,
            tracks: std::sync::Arc::new(TrackPalette::default()),
            manual,
            selection: DEFAULT_SELECTION_OPACITY,
            colors_key: key,
            key,
        }
    }

    pub fn with_tracks(mut self, tracks: std::sync::Arc<TrackPalette>) -> Self {
        self.tracks = tracks;
        let selection = self.selection;
        self.with_selection(selection)
    }
    pub fn tracks_match(&self, other: &std::sync::Arc<TrackPalette>) -> bool {
        std::sync::Arc::ptr_eq(&self.tracks, other) || self.tracks == *other
    }
    pub fn track_color(&self, id: u32, custom: Option<[u8; 3]>, dark: bool, alpha: u8) -> Color32 {
        self.tracks.color(id, custom, dark, alpha)
    }
    pub fn track_tint(
        &self,
        id: u32,
        custom: Option<[u8; 3]>,
        active: bool,
        dark: bool,
    ) -> NoteTint {
        if self.tracks.mode == TrackColorMode::Curated {
            return NoteTint::of(id, custom, active, dark);
        }
        if active {
            NoteTint::Subchannels
        } else {
            let c = self.track_color(id, custom, dark, 255);
            NoteTint::Track([c.r(), c.g(), c.b()])
        }
    }

    /// The same colours with Selection Opacity `percent` (0–100; v0.9 §119):
    /// how strongly selected notes take the selected-note treatment.
    /// Independent of the Onion ghost opacity (§187).
    #[must_use]
    pub fn with_selection(mut self, percent: u8) -> NotePalette {
        self.selection = percent.min(100);
        self.key = if self.selection == DEFAULT_SELECTION_OPACITY {
            self.colors_key
        } else {
            mix64(self.colors_key ^ (u64::from(self.selection) << 56) ^ 0x005E_1EC7)
        };
        if self.tracks.key != 0 {
            self.key = mix64(self.key ^ self.tracks.key);
        }
        self
    }

    /// `full` (a selected-note colour at 100 %) over `base` (the same thing
    /// unselected) at this palette's Selection Opacity — for selected
    /// colours that are not per subchannel (velocity lane columns).
    pub fn selection_mix(&self, base: Color32, full: Color32) -> Color32 {
        mix(base, full, f32::from(self.selection) / 100.0)
    }

    /// v1.0-beta1 B1-0092 — the one final-stage selected / previewed look
    /// of a note of subchannel palette `sp` at `velocity`: its selected fill
    /// and border at this palette's Selection Opacity. Every selection, tool
    /// Preview, drag and gesture draws through it (or through
    /// [`Self::selection_paint`] where no subchannel colour applies), so the
    /// user's opacity never reverts to a default for some tools.
    pub fn selected_paint(sp: &VelocityPalette, velocity: u8) -> (Color32, Color32) {
        let v = usize::from(velocity.min(127));
        (sp.selected_fill[v], sp.selected_border[v])
    }

    /// The same treatment without a subchannel colour (a note being drawn,
    /// notes about to be pasted / inserted, aggregated preview coverage):
    /// the plain note look of `p` with the selected look over it at the
    /// Selection Opacity.
    pub fn selection_paint(&self, p: &Palette) -> (Color32, Color32) {
        let o = f32::from(self.selection) / 100.0;
        (
            mix(p.note_fill, p.note_selected_fill, o),
            mix(p.note_border, p.note_selected_border, o),
        )
    }

    /// Changes whenever the colours change (cache key).
    pub fn key(&self) -> u64 {
        self.key
    }

    /// PlumSpirit's own palette (Random by Seed, seed 0).
    pub fn is_default(&self) -> bool {
        self.prefs.mode == NoteColorMode::RandomBySeed && self.prefs.seed == 0
    }

    /// Hue offset (fraction of a turn) of subchannel `id` in Random by Seed
    /// mode; seed 0: [`subchannel_hue_offset`] exactly.
    fn seeded_hue(&self, id: u32) -> f32 {
        let seed = self.prefs.seed;
        if seed == 0 {
            return subchannel_hue_offset(id);
        }
        let phase = unit(mix64(seed));
        let jitter = (unit(mix64(
            seed ^ u64::from(id)
                .wrapping_add(1)
                .wrapping_mul(0xA24B_AED4_963E_E407),
        )) - 0.5)
            * 0.07;
        (phase + id as f32 * GOLDEN_TURN + jitter).rem_euclid(1.0)
    }

    /// Which of the three brightness steps subchannel `id` takes (seed 0:
    /// the v0.5 pattern, steps change every five subchannels).
    fn seeded_step(&self, id: u32) -> usize {
        let rot = if self.prefs.seed == 0 {
            0
        } else {
            (mix64(self.prefs.seed.rotate_left(17)) % 3) as u32
        };
        ((id / 5 + rot) % 3) as usize
    }

    fn base(&self, id: u32) -> SubBase {
        if self.prefs.mode == NoteColorMode::Rainbow16 {
            return SubBase::Rgb(RAINBOW_16[(id % 16) as usize]);
        }
        if !self.manual.is_empty() {
            let n = self.manual.len() as u32;
            let c = self.manual[(id % n) as usize];
            return SubBase::Rgb(manual_variant(c, id / n));
        }
        if self.is_default() && id == 0 {
            return SubBase::Theme {
                hue: 0.0,
                step: 0.0,
                main: true,
            };
        }
        SubBase::Theme {
            hue: self.seeded_hue(id),
            step: [0.0f32, -0.08, 0.06][self.seeded_step(id)],
            main: false,
        }
    }

    /// The colour of subchannel `id` as an opaque RGB swatch (a velocity-127
    /// note; in Manual mode exactly the chosen colour).
    #[cfg(test)]
    pub fn base_rgb(&self, id: u32, dark: bool) -> [u8; 3] {
        let c = self.entry(id, 127, dark).fill;
        [c.r(), c.g(), c.b()]
    }

    /// Fill, border and selected fill of a note of subchannel `id` at
    /// `velocity` (the exact renderer's colours; the dense tiles use the
    /// same entries). Velocity shades the saturation; velocity 1 stays
    /// clearly coloured.
    pub fn entry(&self, id: u32, velocity: u8, dark: bool) -> NoteEntry {
        let p = if dark { &DARK } else { &LIGHT };
        let (fill, border, full) = self.full_entry(id, velocity, p);
        // v0.9 §119: the selection treatment is an overlay at the Selection
        // Opacity over the note's own look (unselected, or hollow when muted).
        let o = f32::from(self.selection) / 100.0;
        NoteEntry {
            fill,
            border,
            selected_fill: mix(fill, full, o),
            selected_border: mix(border, p.note_selected_border, o),
            selected_muted_fill: lerp_premultiplied(p.note_ghost, full.gamma_multiply(0.35), o),
        }
    }

    /// Fill, border and the full-strength selected fill of a note of
    /// subchannel `id` at `velocity` (Selection Opacity 100 %).
    fn full_entry(&self, id: u32, velocity: u8, p: &Palette) -> (Color32, Color32, Color32) {
        let (h, _, v) = to_hsv(p.note_fill);
        let (hb, _, vb) = to_hsv(p.note_border);
        let (hs, _, vs) = to_hsv(p.note_selected_fill);
        let sat = velocity_saturation(velocity, MIN_NOTE_SATURATION);
        let sel = velocity_saturation(velocity, MIN_SELECTED_SATURATION);
        match self.base(id) {
            // The main subchannel keeps the v0.4 note colours exactly.
            SubBase::Theme { main: true, .. } => (
                from_hsv(h, sat, v),
                from_hsv(hb, sat, vb),
                from_hsv(hs, sel, vs),
            ),
            SubBase::Theme { hue, step, .. } => {
                let hue = h + hue;
                // Selection must stay obvious whatever the hue (Section 032):
                // the subchannel colour pulled strongly towards the theme's
                // selection highlight (near white on dark, deep plum on
                // light), on top of the selected-note border layer.
                let base = from_hsv(hue, sel, (vs + step).clamp(0.2, 1.0));
                (
                    from_hsv(hue, sat, (v + step).clamp(0.2, 1.0)),
                    from_hsv(hue, sat, (vb + step).clamp(0.1, 1.0)),
                    mix(base, p.note_selected_border, 0.42),
                )
            }
            SubBase::Rgb(rgb) => {
                let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let (hc, sc, vc) = to_hsv(c);
                let k = sat / MAX_NOTE_SATURATION;
                let ks = sel / MAX_NOTE_SATURATION;
                // At full velocity the note is exactly the chosen colour.
                let fill = if velocity >= 127 {
                    c
                } else {
                    from_hsv(hc, sc * k, vc)
                };
                // The theme's border / fill brightness ratio keeps the frame.
                let border = from_hsv(hc, sc * k, (vc * vb / v.max(0.01)).clamp(0.1, 1.0));
                let base = from_hsv(hc, sc * ks, vc);
                (fill, border, mix(base, p.note_selected_border, 0.42))
            }
        }
    }

    /// Hue offset (fraction of a turn) of subchannel `id` from subchannel 1:
    /// how a ghost track's colour turns per subchannel (Onion, other
    /// tracks), so ghosts follow the same palette.
    pub fn hue_offset(&self, id: u32) -> f32 {
        match (self.base(id), self.base(0)) {
            (SubBase::Rgb(a), SubBase::Rgb(b)) => {
                let ha = to_hsv(Color32::from_rgb(a[0], a[1], a[2])).0;
                let hb = to_hsv(Color32::from_rgb(b[0], b[1], b[2])).0;
                (ha - hb).rem_euclid(1.0)
            }
            _ if self.is_default() => subchannel_hue_offset(id),
            _ => self.seeded_hue(id) - self.seeded_hue(0),
        }
    }

    /// Brightness step of a ghost track's subchannel `id`.
    fn ghost_step(&self, id: u32) -> f32 {
        match (self.base(id), self.base(0)) {
            (SubBase::Rgb(a), SubBase::Rgb(b)) => {
                let va = to_hsv(Color32::from_rgb(a[0], a[1], a[2])).2;
                let vb = to_hsv(Color32::from_rgb(b[0], b[1], b[2])).2;
                ((va - vb) * 0.5).clamp(-0.1, 0.1)
            }
            _ if self.is_default() && id == 0 => 0.0,
            _ => [0.0f32, -0.07, 0.05][self.seeded_step(id)],
        }
    }
}

/// The colour of Manual subchannel `id` = chosen colour `c` in round
/// `round` (0: `c` itself). Later rounds are [`color_variant`]s, kept apart
/// from `c` even for greys at the lightness limits — subchannel 17 never
/// repeats colour 1 (§066).
fn manual_variant(c: [u8; 3], round: u32) -> [u8; 3] {
    const RANGE: (f64, f64) = (0.30, 0.92);
    let v = color_variant(c, round, RANGE);
    if round == 0 || color_distance(v, c) >= 0.03 {
        return v;
    }
    // The lightness step was clamped away: step the other way.
    let (l, a, b) = oklab(v);
    let (lc, _, _) = oklab(c);
    let l2 = if lc > 0.61 { l - 0.12 } else { l + 0.12 };
    from_oklab(l2.clamp(0.05, 0.98), a, b)
}

/// PlumSpirit's own palette (Random by Seed, seed 0), shared (tests).
#[cfg(test)]
pub fn default_note_palette() -> &'static NotePalette {
    static P: std::sync::OnceLock<NotePalette> = std::sync::OnceLock::new();
    P.get_or_init(NotePalette::default)
}

/// Stores the note palette in the context (the app does this when the
/// preferences change and at startup).
pub fn set_note_palette(ctx: &egui::Context, palette: std::sync::Arc<NotePalette>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("plumspirit-note-palette"), palette));
}

/// The current note palette (PlumSpirit's own when none was set).
pub fn note_palette(ctx: &egui::Context) -> std::sync::Arc<NotePalette> {
    ctx.data(|d| {
        d.get_temp::<std::sync::Arc<NotePalette>>(egui::Id::new("plumspirit-note-palette"))
    })
    .unwrap_or_default()
}

/// Golden angle as a fraction of a turn: consecutive subchannel numbers get hues
/// that never fall into a short repeating cycle.
const GOLDEN_TURN: f32 = 0.381_966_02;

/// Hue offset (fraction of a turn) of PlumSpirit subchannel `id` from the theme's
/// note hue in PlumSpirit's own palette. The main subchannel (0) keeps the v0.4
/// note colour exactly.
pub fn subchannel_hue_offset(id: u32) -> f32 {
    if id == 0 {
        0.0
    } else {
        (id as f32 * GOLDEN_TURN).fract()
    }
}

/// Velocity palette of subchannel `id`: the palette's colour of the
/// subchannel shaded by velocity (saturation; velocity 1 stays clearly
/// coloured).
pub fn build_sub_palette(palette: &NotePalette, id: u32, dark: bool) -> VelocityPalette {
    let mut out = VelocityPalette {
        fill: [Color32::BLACK; 128],
        border: [Color32::BLACK; 128],
        selected_fill: [Color32::BLACK; 128],
        selected_border: [Color32::BLACK; 128],
        selected_muted_fill: [Color32::BLACK; 128],
    };
    for vel in 0..128u8 {
        let e = palette.entry(id, vel, dark);
        let i = usize::from(vel);
        out.fill[i] = e.fill;
        out.border[i] = e.border;
        out.selected_fill[i] = e.selected_fill;
        out.selected_border[i] = e.selected_border;
        out.selected_muted_fill[i] = e.selected_muted_fill;
    }
    out
}

/// Linear blend of two opaque colours (`t` = share of `b`; exactly `a` at 0
/// and `b` at 1).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Linear blend of two premultiplied (possibly translucent) colours — the
/// compositing of an overlay of opacity `t` (exactly `a` at 0, `b` at 1).
fn lerp_premultiplied(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// Per-frame cache of subchannel palettes (built only for subchannels that are
/// actually drawn; bounded), for one note palette at a time.
#[derive(Default)]
pub struct SubPalettes {
    dark: bool,
    key: u64,
    map: std::collections::HashMap<u32, Box<VelocityPalette>>,
}

impl SubPalettes {
    pub fn get(&mut self, palette: &NotePalette, id: u32, dark: bool) -> &VelocityPalette {
        if dark != self.dark || palette.key() != self.key || self.map.len() > 4096 {
            self.map.clear();
            self.dark = dark;
            self.key = palette.key();
        }
        self.map
            .entry(id)
            .or_insert_with(|| Box::new(build_sub_palette(palette, id, dark)))
    }
}

/// Swatch colour of subchannel `id` (as a velocity-100 note).
pub fn subchannel_color(palette: &NotePalette, id: u32, dark: bool) -> Color32 {
    palette.entry(id, 100, dark).fill
}

/// Ghost (Onion Skin) colours of one source track and subchannel, by velocity
/// bucket (`velocity >> 3`): the track's ghost tint turned by the palette's
/// hue offset of the subchannel (the main subchannel keeps the tint exactly),
/// with velocity controlling saturation more gently than on active notes so
/// faint ghosts stay readable. Opaque; the caller applies the Onion opacity.
pub fn ghost_tints(palette: &NotePalette, track_tint: Color32, sub: u32) -> [Color32; 16] {
    let hsv = to_hsv(track_tint);
    let mut out = [Color32::BLACK; 16];
    for (b, c) in out.iter_mut().enumerate() {
        *c = ghost_tint_hsv(palette, hsv, sub, b);
    }
    out
}

/// Entry `band` (velocity band 0–15) of [`ghost_tints`] alone — the same
/// colour, without computing the other fifteen (v0.8 §256: building the
/// colour tables of 2,000 Onion sources computed 16× too many colours).
pub fn ghost_tint(palette: &NotePalette, track_tint: Color32, sub: u32, band: usize) -> Color32 {
    ghost_tint_hsv(palette, to_hsv(track_tint), sub, band)
}

fn ghost_tint_hsv(
    palette: &NotePalette,
    (h, s, v): (f32, f32, f32),
    sub: u32,
    band: usize,
) -> Color32 {
    let hue = h + palette.hue_offset(sub);
    let step = palette.ghost_step(sub);
    let vel = (band.min(15) * 8 + 7).min(127) as u8;
    let f = velocity_saturation(vel, MIN_GHOST_SATURATION) / MAX_NOTE_SATURATION;
    let full = note_tone(from_hsv(
        hue,
        s.clamp(0.0, 1.0),
        (v + step).clamp(0.15, 1.0),
    ));
    ghost_velocity(full, f)
}

/// A toned full-velocity ghost fill shaded for a lower velocity: its OKLab
/// chroma scaled by `f` (same lightness and hue, always inside the gamut).
/// Shading before [`note_tone`] was lost for saturated track colours
/// (B1-0056), whose toned fill clips at the gamut edge at every velocity.
fn ghost_velocity(full: Color32, f: f32) -> Color32 {
    if f >= 1.0 {
        return full;
    }
    let (l, a, b) = oklab([full.r(), full.g(), full.b()]);
    let f = f64::from(f);
    let [r, g, bb] = from_oklab(l, a * f, b * f);
    Color32::from_rgb(r, g, bb)
}

/// Highest OKLCH lightness of a ghost note's fill (v1.0-beta1 B1-0153).
pub const NOTE_TONE_MAX_L: f64 = 0.66;
/// Chroma gain of a ghost note's fill (at most [`NOTE_TONE_MAX_C`]).
const NOTE_TONE_GAIN: f64 = 1.25;
const NOTE_TONE_MAX_C: f64 = 0.16;

/// A track colour as a Piano Roll note fill (v1.0-beta1 B1-0153, contract
/// §022-A, image13 against Domino): the same hue, lightness at most
/// [`NOTE_TONE_MAX_L`], chroma a little stronger but never neon — the dark
/// theme's pastel swatches (OKLCH L 0.5–0.9) stacked into opaque ghosts
/// were the washed-out, overbright dense picture. Greys stay grey.
pub fn note_tone(c: Color32) -> Color32 {
    let (l, a, b) = oklab([c.r(), c.g(), c.b()]);
    let ch = a.hypot(b);
    let k = if ch > 1e-9 {
        (ch * NOTE_TONE_GAIN).min(NOTE_TONE_MAX_C.max(ch)) / ch
    } else {
        1.0
    };
    let lt = l.min(NOTE_TONE_MAX_L);
    // Out of the sRGB gamut the channels clip and the colour turns lighter:
    // then keep the chroma as it was.
    let boosted = from_oklab(lt, a * k, b * k);
    let [r, g, bb] = if oklab(boosted).0 <= lt + 0.005 {
        boosted
    } else {
        from_oklab(lt, a, b)
    };
    Color32::from_rgb(r, g, bb)
}

/// Per-view cache of [`ghost_tints`] with the Onion opacity already applied,
/// keyed by (track tint, opacity, subchannel) for one note palette; bounded.
/// Drawing a ghost note is then one table lookup (no colour maths per note).
#[derive(Default)]
pub struct GhostTints {
    key: u64,
    map: std::collections::HashMap<(u32, u32), [Color32; 16]>,
}

impl GhostTints {
    /// The same tints with opacity `alpha` (0–255) applied.
    pub fn get_with_alpha(
        &mut self,
        palette: &NotePalette,
        track_tint: Color32,
        sub: u32,
        alpha: u8,
    ) -> &[Color32; 16] {
        if self.map.len() > 4096 || palette.key() != self.key {
            self.map.clear();
            self.key = palette.key();
        }
        let [r, g, b, _] = track_tint.to_array();
        let key = (
            u32::from(r) << 24 | u32::from(g) << 16 | u32::from(b) << 8 | u32::from(alpha),
            sub,
        );
        self.map.entry(key).or_insert_with(|| {
            let mut row = ghost_tints(palette, Color32::from_rgb(r, g, b), sub);
            if alpha != 255 {
                for c in &mut row {
                    *c = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
                }
            }
            row
        })
    }
}

/// The velocity palette of the main subchannel (built on first use per note
/// palette and theme, then shared).
pub fn velocity_palette(palette: &NotePalette, dark: bool) -> std::sync::Arc<VelocityPalette> {
    use std::sync::{Arc, Mutex, OnceLock};
    type Cache = std::collections::HashMap<(u64, bool), Arc<VelocityPalette>>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut map = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() > 64 {
        map.clear();
    }
    map.entry((palette.key(), dark))
        .or_insert_with(|| Arc::new(build_sub_palette(palette, 0, dark)))
        .clone()
}

pub fn palette(theme: Theme) -> &'static Palette {
    match theme {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

/// The saved default represents the existing purple in both themes.
pub const DEFAULT_UI_ACCENT: [u8; 3] = [0xA3, 0x78, 0xBA];

#[derive(Clone, Copy)]
struct UiPalettes([Palette; 2]);

/// Only interaction colors vary. Note paint, Track colors and their cache
/// identities stay independent of this application preference.
fn accent_palette(theme: Theme, accent: Option<[u8; 3]>) -> Palette {
    let mut p = *palette(theme);
    if let Some(c) = accent.filter(|c| *c != DEFAULT_UI_ACCENT) {
        let bg = if theme == Theme::Dark {
            p.panel_bg
        } else {
            p.ruler_bg
        };
        let c = with_contrast(c, [bg.r(), bg.g(), bg.b()], 4.5);
        p.accent = Color32::from_rgb(c[0], c[1], c[2]);
        p.accent_weak = mix(p.panel_bg, p.accent, 0.16);
        p.cursor_line = p.accent;
        p.box_select_stroke = p.accent;
        p.box_select_fill = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 48);
    }
    p
}

/// The active theme's interaction palette, cached when preferences change.
/// The immutable base palettes remain available to note-only consumers.
pub fn ui_palette(ctx: &egui::Context) -> Palette {
    let theme = ctx.theme();
    ctx.data(|d| d.get_temp::<UiPalettes>(egui::Id::new("plumspirit-ui-palettes")))
        .map_or_else(
            || *palette(theme),
            |p| p.0[usize::from(theme == Theme::Light)],
        )
}

pub fn preference(setting: ThemeSetting) -> ThemePreference {
    match setting {
        ThemeSetting::Dark => ThemePreference::Dark,
        ThemeSetting::Light => ThemePreference::Light,
        ThemeSetting::System => ThemePreference::System,
    }
}

/// Font sizes (points). Compact desktop scale.
pub mod size {
    pub const BODY: f32 = 13.5;
    pub const SMALL: f32 = 11.5;
    pub const EMPHASIS: f32 = 14.5;
    pub const TITLE: f32 = 22.0;
    /// B1-0069: the smallest hit target of a control, in points (scaled with
    /// the UI scale). Smaller painted marks get a hit rectangle at least this
    /// big, or an equivalent full-size control elsewhere (WCAG 2.5.8).
    pub const MIN_TARGET: f32 = 18.0;
    /// B1-0069: width of the keyboard-focus ring (accent colour).
    pub const FOCUS_STROKE: f32 = 2.0;
}

/// Spacing scale: 4 px micro gap, 8 px control gap, 12 px section separation.
pub mod space {
    pub const MICRO: f32 = 4.0;
    pub const NORMAL: f32 = 8.0;
    pub const SECTION: f32 = 12.0;
}

/// B2-F04: the corner radius of every container, control, popup and canvas
/// frame PlumSpirit draws — square. (Semantically round shapes — a slider's
/// knob, a radio button, circular generator art — are circles, not rounded
/// rectangles, and keep their shape.)
pub const CORNER: CornerRadius = CornerRadius::ZERO;

fn visuals_for(theme: Theme, high_contrast: bool) -> egui::Visuals {
    let p = palette(theme);
    let mut v = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    v.panel_fill = p.panel_bg;
    v.window_fill = p.panel_bg;
    v.window_stroke = Stroke::new(1.0, p.panel_border);
    // B2-F04: square corners everywhere in the owned UI — windows, menus,
    // popups, tooltips (egui's popup frames read these), every widget state.
    v.window_corner_radius = CORNER;
    v.menu_corner_radius = CORNER;
    // B1-0055: flat surfaces — a short, faint drop only to lift floating
    // windows off the canvas, no soft glow.
    v.window_shadow = Shadow {
        offset: [0, 1],
        blur: 3,
        spread: 0,
        color: Color32::from_black_alpha(if theme == Theme::Dark { 70 } else { 26 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 1],
        blur: 2,
        spread: 0,
        color: Color32::from_black_alpha(if theme == Theme::Dark { 60 } else { 22 }),
    };
    v.extreme_bg_color = p.roll_bg;
    v.faint_bg_color = p.roll_black_row;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent_weak;
    // B1-0069: a focused text field's frame is the focus ring.
    v.selection.stroke = Stroke::new(size::FOCUS_STROKE, p.accent);
    v.override_text_color = Some(p.text);
    v.error_fg_color = p.error;
    v.warn_fg_color = p.warning;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CORNER;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.panel_border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    // Hover: a 1 px accent outline. Keyboard focus (egui draws focused
    // widgets with the `active` look) and presses: the 2 px focus ring
    // (B1-0069).
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.bg_stroke = Stroke::new(size::FOCUS_STROKE, p.accent);
    if high_contrast {
        high_contrast_visuals(&mut v, theme);
    }
    v
}

/// B1-0059 (Appearance → High Contrast): text at full black / white, weak
/// text and borders strong enough for WCAG AA (≥ 4.5 : 1) against the
/// panels, thicker control outlines and focus strokes. The Piano Roll's note
/// colours are not changed.
pub fn high_contrast_colors(theme: Theme) -> (Color32, Color32, Color32) {
    match theme {
        // (text, weak text, borders)
        Theme::Dark => (Color32::WHITE, rgb(0xC8, 0xC8, 0xC8), rgb(0xA0, 0xA0, 0xA8)),
        Theme::Light => (Color32::BLACK, rgb(0x30, 0x30, 0x30), rgb(0x50, 0x50, 0x58)),
    }
}

fn high_contrast_visuals(v: &mut egui::Visuals, theme: Theme) {
    let (text, weak, border) = high_contrast_colors(theme);
    v.override_text_color = Some(text);
    v.weak_text_color = Some(weak);
    v.window_stroke = Stroke::new(1.5, border);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.5, text);
    for w in [
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_stroke = Stroke::new(2.0, w.bg_stroke.color);
        w.fg_stroke = Stroke::new(1.5, text);
    }
    v.selection.stroke = Stroke::new(2.0, text);
}

fn style_for(theme: Theme, high_contrast: bool) -> egui::Style {
    let mut s = egui::Style {
        visuals: visuals_for(theme, high_contrast),
        ..egui::Style::default()
    };
    use egui::{FontFamily, FontId, TextStyle};
    s.text_styles = [
        (
            TextStyle::Small,
            FontId::new(size::SMALL, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(size::BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(size::BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(size::EMPHASIS, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(size::BODY, FontFamily::Monospace),
        ),
    ]
    .into();
    s.spacing.item_spacing = Vec2::new(space::NORMAL, space::MICRO);
    s.spacing.button_padding = Vec2::new(8.0, 3.0);
    s.spacing.menu_margin = Margin::same(4);
    s.spacing.window_margin = Margin::same(12);
    s.spacing.interact_size = Vec2::new(24.0, 22.0);
    // No decorative animations.
    s.animation_time = 0.0;
    s
}

/// Installs both theme styles and the preference. egui picks the dark or light
/// style according to the preference (and the OS theme for `System`).
/// B1-0059: `high_contrast` strengthens text, borders and outlines.
pub fn apply(
    ctx: &egui::Context,
    setting: ThemeSetting,
    high_contrast: bool,
    accent: Option<[u8; 3]>,
) {
    let palettes = UiPalettes([
        accent_palette(Theme::Dark, accent),
        accent_palette(Theme::Light, accent),
    ]);
    for (theme, p) in [Theme::Dark, Theme::Light].into_iter().zip(palettes.0) {
        let mut style = style_for(theme, high_contrast);
        if accent.is_some_and(|c| c != DEFAULT_UI_ACCENT) {
            let v = &mut style.visuals;
            v.hyperlink_color = p.accent;
            v.selection.bg_fill = p.accent_weak;
            v.selection.stroke.color = p.accent;
            for w in [
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.bg_stroke.color = p.accent;
                w.bg_fill = p.accent_weak;
                w.weak_bg_fill = p.accent_weak;
            }
        }
        ctx.set_style_of(theme, style);
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("plumspirit-ui-palettes"), palettes));
    ctx.set_theme(preference(setting));
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-G05: the remembered automatic track colours are exactly the
    /// computed ones, in both themes, for every id (also past the table).
    #[test]
    fn remembered_track_colours_are_exactly_the_computed_ones() {
        for dark in [true, false] {
            for id in (0..3_000u32).chain([65_535, 4_000_000, u32::MAX]) {
                let want = compute_auto_track_rgb(id, dark);
                assert_eq!(auto_track_rgb(id, dark), want, "{id} {dark}");
                assert_eq!(auto_track_rgb(id, dark), want, "{id} {dark} again");
            }
        }
    }

    #[test]
    fn ui_accents_keep_contrast_and_leave_note_and_track_colors_unchanged() {
        let ctx = egui::Context::default();
        let notes = note_palette(&ctx);
        for theme in [Theme::Dark, Theme::Light] {
            let setting = if theme == Theme::Dark {
                ThemeSetting::Dark
            } else {
                ThemeSetting::Light
            };
            let base = *palette(theme);
            let before = build_sub_palette(&notes, 3, theme == Theme::Dark);
            let tracks: Vec<_> = (0..32)
                .map(|id| auto_track_rgb(id, theme == Theme::Dark))
                .collect();
            for hc in [false, true] {
                for r in [0, 64, 128, 192, 255] {
                    for g in [0, 64, 128, 192, 255] {
                        for b in [0, 64, 128, 192, 255] {
                            apply(&ctx, setting, hc, Some([r, g, b]));
                            let p = ui_palette(&ctx);
                            for bg in [p.panel_bg, p.roll_bg, p.roll_black_row, p.ruler_bg] {
                                assert!(
                                    contrast_ratio(
                                        p.accent.to_array()[..3].try_into().unwrap(),
                                        bg.to_array()[..3].try_into().unwrap()
                                    ) >= 4.5,
                                    "{theme:?} {r} {g} {b} on {bg:?}"
                                );
                            }
                            assert!(contrast(p.text, p.accent_weak) >= 4.5);
                            assert_eq!(
                                [
                                    p.note_fill,
                                    p.note_border,
                                    p.note_selected_fill,
                                    p.note_selected_border,
                                    p.note_hover_border,
                                    p.note_ghost,
                                    p.error,
                                    p.warning,
                                    p.playhead
                                ],
                                [
                                    base.note_fill,
                                    base.note_border,
                                    base.note_selected_fill,
                                    base.note_selected_border,
                                    base.note_hover_border,
                                    base.note_ghost,
                                    base.error,
                                    base.warning,
                                    base.playhead
                                ]
                            );
                            assert_eq!(
                                ctx.style_of(theme).visuals.selection.stroke.color,
                                p.accent
                            );
                            assert_eq!(
                                ctx.style_of(theme).visuals.widgets.active.bg_stroke.color,
                                p.accent
                            );
                            assert_eq!(
                                ctx.style_of(theme).visuals.widgets.active.corner_radius,
                                CORNER
                            );
                        }
                    }
                }
            }
            assert_eq!(
                build_sub_palette(&note_palette(&ctx), 3, theme == Theme::Dark),
                before
            );
            assert_eq!(
                (0..32)
                    .map(|id| auto_track_rgb(id, theme == Theme::Dark))
                    .collect::<Vec<_>>(),
                tracks
            );
            apply(&ctx, setting, false, None);
            assert_eq!(ui_palette(&ctx), base);
            assert_eq!(ctx.style_of(theme).visuals, visuals_for(theme, false));
        }
        assert_eq!(note_palette(&ctx), notes);
        assert_eq!(ui_palette(&egui::Context::default()), DARK);
    }

    #[test]
    fn ui_accents_follow_the_active_system_theme_without_changing_the_saved_color() {
        let ctx = egui::Context::default();
        apply(&ctx, ThemeSetting::System, false, Some([0, 200, 150]));
        ctx.set_theme(ThemePreference::Dark);
        assert_eq!(
            ui_palette(&ctx),
            accent_palette(Theme::Dark, Some([0, 200, 150]))
        );
        ctx.set_theme(ThemePreference::Light);
        assert_eq!(
            ui_palette(&ctx),
            accent_palette(Theme::Light, Some([0, 200, 150]))
        );
        assert_eq!(
            ui_palette(&egui::Context::default()),
            DARK,
            "contexts are independent"
        );
        apply(&ctx, ThemeSetting::Light, false, Some(DEFAULT_UI_ACCENT));
        assert_eq!(ui_palette(&ctx), LIGHT);
    }

    #[test]
    fn rainbow_notes_cycle_after_sixteen_and_keep_velocity_and_ghost_shading() {
        let p = NotePalette::new(NoteColorPrefs {
            mode: NoteColorMode::Rainbow16,
            ..Default::default()
        });
        assert_ne!(p.key(), NotePalette::default().key());
        for dark in [true, false] {
            for id in [0, 1, 15, 16, 17, 31, 32, 33, u32::MAX] {
                assert_eq!(p.base_rgb(id, dark), RAINBOW_16[(id % 16) as usize]);
                for vel in [1, 64, 127] {
                    assert_eq!(p.entry(id, vel, dark), p.entry(id % 16, vel, dark));
                }
                assert_eq!(p.hue_offset(id), p.hue_offset(id % 16));
                assert_eq!(p.ghost_step(id), p.ghost_step(id % 16));
            }
            assert_ne!(p.entry(0, 1, dark).fill, p.entry(0, 127, dark).fill);
        }
        let mut prefs = p.prefs.clone();
        prefs.seed = 42;
        prefs.manual = vec![[1, 2, 3]; 16];
        let other = NotePalette::new(prefs);
        assert_eq!(p, other);
        assert_eq!(p.key(), other.key());
    }

    #[test]
    fn rainbow_tracks_follow_musical_order_and_preserve_explicit_overrides() {
        let ids: Vec<_> = (100..133).collect();
        let p = TrackPalette::new(TrackColorMode::Rainbow16, ids.iter().copied());
        for dark in [true, false] {
            for (i, &id) in ids.iter().enumerate() {
                let c = p.color(id, None, dark, 255);
                assert_eq!([c.r(), c.g(), c.b()], RAINBOW_16[i % 16]);
                assert_eq!(
                    p.color(id, Some([3, 7, 11]), dark, 77),
                    Color32::from_rgba_unmultiplied(3, 7, 11, 77)
                );
            }
        }
        let mut reordered = ids.clone();
        reordered.swap(0, 16);
        let moved = TrackPalette::new(TrackColorMode::Rainbow16, reordered.iter().copied());
        assert_ne!(p.key, moved.key);
        reordered.insert(0, 999);
        reordered.remove(4);
        let edited = TrackPalette::new(TrackColorMode::Rainbow16, reordered.iter().copied());
        assert_eq!(
            edited.color(999, None, true, 255),
            p.color(100, None, true, 255)
        );
        for (i, id) in reordered.into_iter().enumerate() {
            let c = edited.color(id, None, true, 255);
            assert_eq!([c.r(), c.g(), c.b()], RAINBOW_16[i % 16]);
        }
        let base = NotePalette::default();
        let with = base.clone().with_tracks(std::sync::Arc::new(p));
        assert_ne!(base.key(), with.key());
        assert_eq!(
            base.entry(3, 90, true),
            with.entry(3, 90, true),
            "independent note settings"
        );
        assert_ne!(
            base.track_tint(100, None, false, true),
            with.track_tint(100, None, false, true)
        );
    }

    fn dp() -> NotePalette {
        NotePalette::default()
    }

    /// WCAG relative luminance.
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let x = f32::from(v) / 255.0;
            if x <= 0.039_28 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// B1-0059: High Contrast meets WCAG AAA for text (≥ 7 : 1) and AA for
    /// weak text and borders (≥ 4.5 : 1 / ≥ 3 : 1) on the panels of both
    /// themes, and the normal look is unchanged when it is off.
    #[test]
    fn high_contrast_meets_wcag_on_both_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = palette(theme);
            let (text, weak, border) = high_contrast_colors(theme);
            assert!(contrast(text, p.panel_bg) >= 7.0, "{theme:?} text");
            assert!(contrast(weak, p.panel_bg) >= 4.5, "{theme:?} weak text");
            assert!(contrast(border, p.panel_bg) >= 3.0, "{theme:?} borders");
            assert!(contrast(text, p.panel_bg) > contrast(p.text, p.panel_bg));
            let hc = visuals_for(theme, true);
            assert_eq!(hc.override_text_color, Some(text));
            assert_eq!(hc.weak_text_color, Some(weak));
            assert!(hc.widgets.inactive.fg_stroke.width > 1.0);
            let normal = visuals_for(theme, false);
            assert_eq!(normal.override_text_color, Some(p.text));
            assert_eq!(
                normal.weak_text_color,
                egui::Visuals::dark().weak_text_color
            );
            assert_eq!(normal.window_stroke, Stroke::new(1.0, p.panel_border));
        }
    }

    #[test]
    fn velocity_saturation_is_monotonic_and_never_gray() {
        for dark in [true, false] {
            let v = velocity_palette(&dp(), dark);
            let pal = if dark { &DARK } else { &LIGHT };
            let mut prev = -1.0f32;
            for vel in 1..=127u8 {
                let (h, s, _) = to_hsv(v.fill[usize::from(vel)]);
                assert!(s >= prev - 0.01, "monotonic at {vel}");
                prev = s;
                // Same hue as the theme's note colour (identity kept).
                let (h0, ..) = to_hsv(pal.note_fill);
                assert!(
                    (h - h0).abs() < 0.02 || (h - h0).abs() > 0.98,
                    "hue at {vel}"
                );
            }
            let (_, s1, _) = to_hsv(v.fill[1]);
            let (_, s127, _) = to_hsv(v.fill[127]);
            // v0.9 §181: velocity 1 is strongly coloured too; velocity
            // still shades the colour visibly.
            assert!(s1 >= 0.6, "velocity 1 stays saturated ({s1})");
            assert!(
                s127 >= 0.85 && s127 > s1 + 0.2,
                "velocity 127 is saturated ({s127})"
            );
            assert_eq!(v.fill[0], v.fill[1]);
            // Readable on the roll background at every velocity.
            for vel in [1usize, 64, 127] {
                assert!(
                    contrast(v.fill[vel], pal.roll_bg) >= 2.5,
                    "contrast at {vel} ({dark})"
                );
            }
            // Selected notes differ from unselected ones at every velocity: in hue,
            // and by their border.
            for vel in 1..=127usize {
                let (hn, ..) = to_hsv(v.fill[vel]);
                let (hs, ..) = to_hsv(v.selected_fill[vel]);
                let dh = (hn - hs).abs().min(1.0 - (hn - hs).abs());
                assert!(dh > 0.15, "selected hue differs at {vel}");
                assert!(contrast(pal.note_selected_border, v.fill[vel]) >= 1.5);
            }
        }
    }

    /// Machado et al. (2009) full-severity simulation (linear RGB):
    /// protanopia, deuteranopia.
    fn simulate_cvd(c: [u8; 3], deutan: bool) -> [u8; 3] {
        const P: [[f64; 3]; 3] = [
            [0.152_286, 1.052_583, -0.204_868],
            [0.114_503, 0.786_281, 0.099_216],
            [-0.003_882, -0.048_116, 1.051_998],
        ];
        const D: [[f64; 3]; 3] = [
            [0.367_322, 0.860_646, -0.227_968],
            [0.280_085, 0.672_501, 0.047_413],
            [-0.011_820, 0.042_940, 0.968_881],
        ];
        let m = if deutan { D } else { P };
        let lin = c.map(srgb_decode);
        let row = |r: [f64; 3]| srgb_encode(r[0] * lin[0] + r[1] * lin[1] + r[2] * lin[2]);
        [row(m[0]), row(m[1]), row(m[2])]
    }

    /// B2-F04: no rounded corner comes back into the owned UI — every
    /// painter rectangle (`rect_filled` / `rect_stroke`), frame
    /// (`.corner_radius(…)`) and `CornerRadius::same(…)` in the app's source
    /// is square: a literal radius other than 0 fails, naming the file and
    /// line. (Circles are drawn with `circle_*`, never as rounded
    /// rectangles.)
    #[test]
    fn no_rounded_rectangles_in_the_owned_ui() {
        fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        /// The second top-level argument of the call whose `(` is at `open`.
        fn second_arg(s: &str, open: usize) -> Option<&str> {
            let b = s.as_bytes();
            let (mut depth, mut start) = (0i32, None::<usize>);
            for (k, &c) in b.iter().enumerate().skip(open) {
                match c {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return start.map(|a| &s[a..k]);
                        }
                    }
                    b',' if depth == 1 => match start {
                        None => start = Some(k + 1),
                        Some(a) => return Some(&s[a..k]),
                    },
                    _ => {}
                }
            }
            None
        }
        /// The first argument of the call whose `(` is at `open`.
        fn first_arg(s: &str, open: usize) -> &str {
            let rest = &s[open + 1..];
            let end = rest.find([')', ',']).unwrap_or(rest.len());
            &rest[..end]
        }
        let rounded = |a: &str| {
            let a = a.trim().trim_end_matches("_f32").trim_end_matches("f32");
            a.parse::<f64>().is_ok_and(|v| v != 0.0)
        };
        let mut all = Vec::new();
        files(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut all,
        );
        let mut bad = Vec::new();
        for f in &all {
            let s = std::fs::read_to_string(f).unwrap();
            let line = |at: usize| s[..at].matches('\n').count() + 1;
            for pat in ["rect_filled(", "rect_stroke("] {
                for (at, _) in s.match_indices(pat) {
                    let open = at + pat.len() - 1;
                    if second_arg(&s, open).is_some_and(rounded) {
                        bad.push(format!("{}:{}", f.display(), line(at)));
                    }
                }
            }
            for pat in [".corner_radius(", "CornerRadius::same("] {
                for (at, _) in s.match_indices(pat) {
                    if rounded(first_arg(&s, at + pat.len() - 1)) {
                        bad.push(format!("{}:{}", f.display(), line(at)));
                    }
                }
            }
            // ProgressBar defaults to a pill, independently of the theme.
            for (at, _) in s.match_indices(concat!("ProgressBar::", "new(")) {
                let widget = s[at..].split(';').next().unwrap();
                if !widget.contains(".corner_radius(") {
                    bad.push(format!("{}:{} (progress bar)", f.display(), line(at)));
                }
            }
        }
        assert!(all.len() > 50, "the source was found");
        assert!(bad.is_empty(), "rounded corners at {bad:?}");
    }

    /// B1-0055 / B1-0069: one control geometry (B2-F04: every radius
    /// square — widgets, windows, menus), flat surfaces (no soft shadows),
    /// usable targets and a visible 2 px keyboard-focus ring, with and
    /// without High Contrast, in both themes.
    #[test]
    fn design_tokens_keep_one_geometry_flat_surfaces_and_a_visible_focus_ring() {
        for theme in [Theme::Dark, Theme::Light] {
            for hc in [false, true] {
                let s = style_for(theme, hc);
                let v = &s.visuals;
                let r = v.widgets.inactive.corner_radius;
                for w in [
                    &v.widgets.noninteractive,
                    &v.widgets.inactive,
                    &v.widgets.hovered,
                    &v.widgets.active,
                    &v.widgets.open,
                ] {
                    assert_eq!(w.corner_radius, r);
                    assert_eq!(w.expansion, 0.0, "no growing on hover");
                }
                assert_eq!(r, CornerRadius::ZERO, "square widgets");
                assert_eq!(v.window_corner_radius, CornerRadius::ZERO, "square windows");
                assert_eq!(v.menu_corner_radius, CornerRadius::ZERO, "square menus");
                assert!(v.window_shadow.blur <= 3 && v.popup_shadow.blur <= 3);
                assert!(v.window_shadow.spread == 0 && v.popup_shadow.spread == 0);
                assert!(v.widgets.active.bg_stroke.width >= size::FOCUS_STROKE);
                assert!(v.selection.stroke.width >= size::FOCUS_STROKE);
                assert_ne!(
                    v.widgets.active.bg_stroke.color,
                    v.widgets.inactive.bg_stroke.color
                );
                assert!(s.spacing.interact_size.y >= size::MIN_TARGET);
                assert!(s.spacing.interact_size.x >= size::MIN_TARGET);
                assert_eq!(s.animation_time, 0.0);
                assert_eq!(
                    s.spacing.item_spacing,
                    Vec2::new(space::NORMAL, space::MICRO)
                );
            }
        }
    }

    #[test]
    fn track_palette_is_curated_distinct_and_deterministic_beyond_it() {
        let n = TRACK_PALETTE.len();
        assert!((16..=24).contains(&n));
        for dark in [true, false] {
            let bg = if dark { DARK.roll_bg } else { LIGHT.roll_bg };
            let bg3 = [bg.r(), bg.g(), bg.b()];
            let base: Vec<[u8; 3]> = TRACK_PALETTE
                .iter()
                .map(|&(_, d, l)| if dark { d } else { l })
                .collect();
            // The first tracks get the curated colours as they are.
            for (i, c) in base.iter().enumerate() {
                assert_eq!(auto_track_rgb(i as u32, dark), *c);
                let col = Color32::from_rgb(c[0], c[1], c[2]);
                // WCAG 1.4.11 non-text contrast against the roll.
                assert!(
                    contrast_ratio(*c, bg3) >= 3.2,
                    "{} contrast {} ({dark})",
                    TRACK_PALETTE[i].0,
                    contrast_ratio(*c, bg3)
                );
                assert!((f64::from(contrast(col, bg)) - contrast_ratio(*c, bg3)).abs() < 0.01);
                // No neon: no fully saturated, fully bright colour.
                let (_, s, v) = to_hsv(col);
                assert!(!(s > 0.9 && v > 0.95), "{} is neon", TRACK_PALETTE[i].0);
            }
            // Not the plum selection colour among the first five tracks.
            for c in &base[..5] {
                let (h, ..) = to_hsv(Color32::from_rgb(c[0], c[1], c[2]));
                assert!(!(0.72..0.86).contains(&h), "early purple {c:?}");
            }
            // All differ clearly; neighbours (up to three apart) much more,
            // and direct neighbours stay apart for colour-blind users.
            for i in 0..n {
                for j in i + 1..n {
                    let d = color_distance(base[i], base[j]);
                    assert!(
                        d >= 0.055,
                        "{} / {}: {d}",
                        TRACK_PALETTE[i].0,
                        TRACK_PALETTE[j].0
                    );
                    let gap = (j - i).min(n - (j - i));
                    // Measured minima: 0.134 / 0.112 / 0.099 at gaps 1 / 2 / 3.
                    let min = [0.0, 0.13, 0.11, 0.095];
                    if gap <= 3 {
                        assert!(d >= min[gap], "neighbors {i} / {j}: {d}");
                    }
                    if gap == 1 {
                        for deutan in [false, true] {
                            let dc = color_distance(
                                simulate_cvd(base[i], deutan),
                                simulate_cvd(base[j], deutan),
                            );
                            assert!(dc >= 0.09, "cvd neighbors {i} / {j}: {dc}");
                        }
                    }
                }
            }
            // Beyond the palette: deterministic variants that are neither
            // the base colour nor a near copy of a neighbouring track, and
            // still readable on the roll.
            let ids: Vec<u32> = (0..400).collect();
            let colours: Vec<[u8; 3]> = ids.iter().map(|&i| auto_track_rgb(i, dark)).collect();
            assert_eq!(
                colours,
                ids.iter()
                    .map(|&i| auto_track_rgb(i, dark))
                    .collect::<Vec<_>>()
            );
            for (i, c) in colours.iter().enumerate() {
                assert!(
                    contrast_ratio(*c, bg3) >= TRACK_MIN_CONTRAST,
                    "track {i} contrast ({dark})"
                );
            }
            for i in n..400usize {
                assert!(
                    color_distance(colours[i], colours[i % n]) >= 0.03,
                    "variant {i} differs from its base"
                );
                assert!(
                    color_distance(colours[i], colours[i - 1]) >= 0.05,
                    "{i} vs {}",
                    i - 1
                );
            }
            assert_eq!(auto_track_color(3, dark, 77).a(), 77);
        }
        // Track Properties colours win.
        assert_eq!(
            track_color(4, Some([1, 2, 3]), true, 255),
            Color32::from_rgb(1, 2, 3)
        );
    }

    #[test]
    fn note_tints_resolve_like_the_piano_roll() {
        let mut nc = NoteColors::default();
        for dark in [true, false] {
            // Active track: subchannel palettes (the roll's active notes).
            let t = NoteTint::of(7, None, true, dark);
            assert_eq!(t, NoteTint::Subchannels);
            assert_eq!(
                nc.get(&dp(), t, 3, 90, dark),
                build_sub_palette(&dp(), 3, dark).fill[90]
            );
            // Other tracks: their Track colour per subchannel (the ghosts).
            let a = NoteTint::of(0, None, false, dark);
            let b = NoteTint::of(1, None, false, dark);
            assert_ne!(
                nc.get(&dp(), a, 0, 100, dark),
                nc.get(&dp(), b, 0, 100, dark)
            );
            let NoteTint::Track(base) = a else { panic!() };
            assert_eq!(
                nc.get(&dp(), a, 2, 100, dark),
                ghost_tints(&dp(), Color32::from_rgb(base[0], base[1], base[2]), 2)[100 >> 3]
            );
            assert_eq!(
                dp().entry(5, 100, dark).fill,
                subchannel_color(&dp(), 5, dark)
            );
        }
    }

    #[test]
    fn outline_darkness_runs_from_white_through_the_colour_to_black() {
        let c = [200, 100, 50];
        assert_eq!(outline_shade(c, 0), [255, 255, 255]);
        assert_eq!(outline_shade(c, 50), c);
        assert_eq!(outline_shade(c, 100), [0, 0, 0]);
        assert_eq!(outline_shade(c, 200), [0, 0, 0]);
        // Monotonic darkening.
        let mut prev = u32::MAX;
        for d in 0..=100u8 {
            let s = outline_shade(c, d)
                .iter()
                .map(|&v| u32::from(v))
                .sum::<u32>();
            assert!(s <= prev, "{d}");
            prev = s;
        }
    }

    fn distance(a: Color32, b: Color32) -> i32 {
        let [r1, g1, b1, _] = a.to_array().map(i32::from);
        let [r2, g2, b2, _] = b.to_array().map(i32::from);
        (r1 - r2).abs() + (g1 - g2).abs() + (b1 - b2).abs()
    }

    #[test]
    fn subchannel_colors_are_deterministic_distinct_and_keep_velocity_saturation() {
        for dark in [true, false] {
            // The main subchannel keeps the v0.4 note colours exactly.
            let main = build_sub_palette(&dp(), 0, dark);
            let v04 = velocity_palette(&dp(), dark);
            assert_eq!(main.fill, v04.fill);
            assert_eq!(subchannel_color(&dp(), 0, dark), v04.fill[100]);
            // Deterministic: the same id always gives the same palette.
            for id in [1, 2, 17, 255, 1023, 100_000] {
                assert_eq!(
                    build_sub_palette(&dp(), id, dark).fill,
                    build_sub_palette(&dp(), id, dark).fill
                );
            }
            // Neighbouring subchannels (the ones users see side by side) differ
            // clearly; over 256 subchannels no two consecutive ones look alike.
            for id in 0..256u32 {
                let a = subchannel_color(&dp(), id, dark);
                let b = subchannel_color(&dp(), id + 1, dark);
                assert!(distance(a, b) >= 40, "{id}: {a:?} vs {b:?}");
            }
            // Velocity controls saturation monotonically and velocity 1 is still coloured.
            for id in [0, 1, 5, 64] {
                let p = build_sub_palette(&dp(), id, dark);
                let mut last = -1.0f32;
                for v in 1..128 {
                    let (_, sat, _) = to_hsv(p.fill[v]);
                    assert!(sat + 0.02 >= last, "id {id} vel {v}");
                    last = sat;
                }
                let (_, s1, _) = to_hsv(p.fill[1]);
                assert!(s1 >= 0.2, "velocity 1 of subchannel {id} is gray ({s1})");
                // Selection stays visible: the selected fill differs from the fill.
                for v in [1, 64, 127] {
                    assert!(distance(p.fill[v], p.selected_fill[v]) >= 20, "{id}/{v}");
                }
            }
        }
    }

    #[test]
    fn ghost_tints_keep_subchannel_hue_and_the_track_tint() {
        let tint = auto_track_color(3, true, 255);
        let main = ghost_tints(&dp(), tint, 0);
        // The main subchannel at full velocity is the track colour as a
        // note fill (v1.0-beta1 B1-0153: `note_tone`; v0.4 – v0.9 used the
        // swatch itself).
        let toned = note_tone(tint);
        assert!(
            distance(main[15], toned) <= 6,
            "{:?} vs {toned:?}",
            main[15]
        );
        // Subchannels are not one colour: consecutive ones differ clearly and
        // none is gray.
        for sub in 0..64 {
            let t = ghost_tints(&dp(), tint, sub);
            let next = ghost_tints(&dp(), tint, sub + 1);
            assert!(distance(t[15], next[15]) >= 30, "sub {sub}");
            let (_, s, _) = to_hsv(t[0]);
            assert!(s > 0.05, "sub {sub} low velocity is gray");
        }
        // Velocity: lower velocity is less saturated.
        let (_, s_lo, _) = to_hsv(main[0]);
        let (_, s_hi, _) = to_hsv(main[15]);
        assert!(s_lo < s_hi);
        let mut cache = GhostTints::default();
        assert_eq!(
            *cache.get_with_alpha(&dp(), tint, 7, 255),
            ghost_tints(&dp(), tint, 7)
        );
    }

    /// v1.0-beta1 B1-0153 (contract §022-A, image13): a ghost's fill keeps
    /// its track colour's hue but is never lighter than OKLCH
    /// `NOTE_TONE_MAX_L`, a little more chromatic but never neon, and greys
    /// stay grey — every palette track colour, dark and light theme.
    #[test]
    fn ghost_fills_are_mid_tone_with_the_track_hue_and_greys_stay_grey() {
        for id in 0..60u32 {
            for dark in [true, false] {
                let c = auto_track_color(id, dark, 255);
                let t = note_tone(c);
                let (l0, a0, b0) = oklab([c.r(), c.g(), c.b()]);
                let (l, a, b) = oklab([t.r(), t.g(), t.b()]);
                assert!(l <= NOTE_TONE_MAX_L + 0.01, "{id} {dark}: L {l}");
                assert!(l <= l0 + 0.01, "{id} {dark}: never lighter");
                let (c0, c1) = (a0.hypot(b0), a.hypot(b));
                assert!(c1 <= NOTE_TONE_MAX_C.max(c0) + 0.01, "{id}: chroma {c1}");
                if c0 > 0.03 {
                    let dh = (b.atan2(a) - b0.atan2(a0)).abs();
                    let dh = dh.min(std::f64::consts::TAU - dh);
                    assert!(dh < 0.2, "{id} {dark}: hue moved {dh}");
                }
            }
        }
        let grey = Color32::from_rgb(200, 200, 200);
        let g = note_tone(grey);
        assert!(to_hsv(g).1 < 0.03, "{g:?}");
        assert!(oklab([g.r(), g.g(), g.b()]).0 <= NOTE_TONE_MAX_L + 0.01);
    }

    #[test]
    fn one_ghost_tint_equals_its_entry_of_the_table() {
        for c in [
            Color32::from_rgb(200, 40, 90),
            Color32::from_rgb(10, 220, 180),
            Color32::from_rgb(128, 128, 128),
        ] {
            for sub in [0, 1, 4, 5, 13, 63, 1000] {
                let t = ghost_tints(&dp(), c, sub);
                for (b, e) in t.iter().enumerate() {
                    assert_eq!(ghost_tint(&dp(), c, sub, b), *e);
                }
            }
        }
    }

    // ------------------------------------------------ v0.9 §064–§068

    /// The v0.5–v0.9 subchannel colours, as they were before the note palette
    /// existed (reference for "the default palette changes nothing").
    fn legacy_entry(id: u32, velocity: u8, dark: bool) -> NoteEntry {
        let p = if dark { &DARK } else { &LIGHT };
        let (h, _, v) = to_hsv(p.note_fill);
        let (hb, _, vb) = to_hsv(p.note_border);
        let (hs, _, vs) = to_hsv(p.note_selected_fill);
        let sat = velocity_saturation(velocity, MIN_NOTE_SATURATION);
        let sel = velocity_saturation(velocity, MIN_SELECTED_SATURATION);
        // v0.8 selected notes: the selection border, muted ones at 35 %.
        let entry = |fill, border, selected_fill: Color32| NoteEntry {
            fill,
            border,
            selected_fill,
            selected_border: p.note_selected_border,
            selected_muted_fill: selected_fill.gamma_multiply(0.35),
        };
        if id == 0 {
            return entry(
                from_hsv(h, sat, v),
                from_hsv(hb, sat, vb),
                from_hsv(hs, sel, vs),
            );
        }
        let step = [0.0f32, -0.08, 0.06][(id / 5 % 3) as usize];
        let hue = h + subchannel_hue_offset(id);
        let base = from_hsv(hue, sel, (vs + step).clamp(0.2, 1.0));
        entry(
            from_hsv(hue, sat, (v + step).clamp(0.2, 1.0)),
            from_hsv(hue, sat, (vb + step).clamp(0.1, 1.0)),
            mix(base, p.note_selected_border, 0.42),
        )
    }

    fn legacy_ghost(track_tint: Color32, sub: u32, band: usize) -> Color32 {
        let (h, s, v) = to_hsv(track_tint);
        let hue = h + subchannel_hue_offset(sub);
        let step = if sub == 0 {
            0.0
        } else {
            [0.0f32, -0.07, 0.05][(sub / 5 % 3) as usize]
        };
        let vel = (band.min(15) * 8 + 7).min(127) as u8;
        let f = velocity_saturation(vel, MIN_GHOST_SATURATION) / MAX_NOTE_SATURATION;
        from_hsv(hue, (s * f).clamp(0.0, 1.0), (v + step).clamp(0.15, 1.0))
    }

    fn rgb_of(c: Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    /// v0.9 §119: Selection Opacity composites the selected-note treatment
    /// over the note's own look — 100 % is the v0.8 treatment exactly
    /// (default), 0 % looks like an unselected note (muted: the hollow ghost
    /// fill), in between the strength grows with the slider; unselected
    /// colours and ghost tints never change (§187: not the ghost opacity).
    #[test]
    fn selection_opacity_composites_the_selected_treatment() {
        let bases = [
            NotePalette::default(),
            NotePalette::new(NoteColorPrefs {
                seed: 99,
                ..NoteColorPrefs::default()
            }),
            NotePalette::new(NoteColorPrefs {
                mode: NoteColorMode::Manual,
                seed: 0,
                manual: vec![[250, 20, 20], [20, 20, 250]],
            }),
        ];
        let dist = |a: Color32, b: Color32| {
            let [r, g, b2, a2] = a.to_array();
            let [r1, g1, b1, a1] = b.to_array();
            [(r, r1), (g, g1), (b2, b1), (a2, a1)]
                .iter()
                .map(|&(x, y)| (i32::from(x) - i32::from(y)).abs())
                .sum::<i32>()
        };
        for base in &bases {
            assert_eq!(base.selection, DEFAULT_SELECTION_OPACITY);
            assert_eq!(base.clone().with_selection(100), *base);
            assert_eq!(base.clone().with_selection(100).key(), base.key());
            assert_eq!(base.clone().with_selection(250).selection, 100);
            let keys: std::collections::HashSet<u64> = (0..=100u8)
                .map(|o| base.clone().with_selection(o).key())
                .collect();
            assert_eq!(keys.len(), 101, "every opacity has its own cache key");
            for dark in [true, false] {
                let p = if dark { &DARK } else { &LIGHT };
                for id in [0u32, 1, 7, 16, 300] {
                    for vel in [1u8, 64, 127] {
                        let full = base.entry(id, vel, dark);
                        let none = base.clone().with_selection(0).entry(id, vel, dark);
                        // 0 %: no selection overlay at all.
                        assert_eq!(none.selected_fill, none.fill);
                        assert_eq!(none.selected_border, none.border);
                        assert_eq!(none.selected_muted_fill, p.note_ghost);
                        // 100 %: the full treatment (v0.8 border, muted 35 %).
                        assert_eq!(full.selected_border, p.note_selected_border);
                        assert_eq!(
                            full.selected_muted_fill,
                            full.selected_fill.gamma_multiply(0.35)
                        );
                        let mut last = (0, 0, 0);
                        for o in [0u8, 25, 50, 75, 100] {
                            let e = base.clone().with_selection(o).entry(id, vel, dark);
                            assert_eq!((e.fill, e.border), (full.fill, full.border));
                            let d = (
                                dist(e.selected_fill, e.fill),
                                dist(e.selected_border, e.border),
                                dist(e.selected_muted_fill, p.note_ghost),
                            );
                            assert!(
                                d.0 >= last.0 && d.1 >= last.1 && d.2 >= last.2,
                                "{id} {vel} {o}: {d:?} after {last:?}"
                            );
                            last = d;
                        }
                        assert!(last.0 > 0, "the full treatment differs from the fill");
                    }
                }
                for t in 0..4u32 {
                    let tint = track_color(t, None, t % 2 == 0, 255);
                    for sub in [0u32, 3, 40] {
                        for band in [0usize, 7, 15] {
                            assert_eq!(
                                ghost_tint(&base.clone().with_selection(10), tint, sub, band),
                                ghost_tint(base, tint, sub, band)
                            );
                        }
                    }
                }
            }
        }
        // Velocity palettes are tables: built once per palette, one lookup a note.
        let half = NotePalette::default().with_selection(50);
        let vp = build_sub_palette(&half, 3, true);
        for vel in 0..128u8 {
            let e = half.entry(3, vel, true);
            let i = usize::from(vel);
            assert_eq!(
                (
                    vp.selected_fill[i],
                    vp.selected_border[i],
                    vp.selected_muted_fill[i]
                ),
                (e.selected_fill, e.selected_border, e.selected_muted_fill)
            );
        }
        let mut subs = SubPalettes::default();
        let a = subs.get(&NotePalette::default(), 3, true).selected_fill[90];
        let b = subs.get(&half, 3, true).selected_fill[90];
        assert_ne!(a, b, "a new opacity rebuilds the cached palettes");
    }

    #[test]
    fn the_default_palette_changes_no_colour() {
        let p = NotePalette::default();
        assert!(p.is_default());
        assert_eq!(p, NotePalette::new(NoteColorPrefs::default()));
        for dark in [true, false] {
            for id in 0..300u32 {
                for vel in 0..=127u8 {
                    assert_eq!(
                        p.entry(id, vel, dark),
                        legacy_entry(id, vel, dark),
                        "{id} {vel} {dark}"
                    );
                }
            }
        }
        for t in 0..8u32 {
            let tint = track_color(t, None, t % 2 == 0, 255);
            for sub in 0..300u32 {
                for band in 0..16 {
                    let vel = (band * 8 + 7).min(127) as u8;
                    let f = velocity_saturation(vel, MIN_GHOST_SATURATION) / MAX_NOTE_SATURATION;
                    assert_eq!(
                        ghost_tint(&p, tint, sub, band),
                        ghost_velocity(note_tone(legacy_ghost(tint, sub, 15)), f)
                    );
                }
            }
        }
    }

    fn seeded(seed: u64) -> NotePalette {
        NotePalette::new(NoteColorPrefs {
            seed,
            ..NoteColorPrefs::default()
        })
    }

    #[test]
    fn a_seed_always_gives_the_same_palette_and_seeds_differ() {
        for seed in [1u64, 42, 0xDEAD_BEEF, u64::MAX] {
            let (a, b) = (seeded(seed), seeded(seed));
            assert_eq!(a.key(), b.key());
            for dark in [true, false] {
                for id in 0..64 {
                    assert_eq!(a.entry(id, 90, dark), b.entry(id, 90, dark));
                }
                // Subchannels 1–16 stay apart from each other.
                for i in 0..16u32 {
                    for j in 0..i {
                        let d = color_distance(a.base_rgb(i, dark), a.base_rgb(j, dark));
                        assert!(d > 0.02, "seed {seed}: subchannels {i} / {j} ({d})");
                    }
                }
            }
            assert_ne!(a.key(), NotePalette::default().key());
        }
        // Different seeds, different palettes (subchannel 1 included).
        let (a, b) = (seeded(1), seeded(2));
        assert_ne!(a.key(), b.key());
        let differ = (0..16)
            .filter(|&id| a.base_rgb(id, true) != b.base_rgb(id, true))
            .count();
        assert!(differ >= 14, "{differ}");
        assert_ne!(
            seeded(7).base_rgb(0, true),
            NotePalette::default().base_rgb(0, true)
        );
        // Velocity still shades a seeded colour; velocity 1 stays coloured.
        let p = seeded(9);
        for id in 0..8 {
            let (_, s1, _) = to_hsv(p.entry(id, 1, true).fill);
            let (_, s127, _) = to_hsv(p.entry(id, 127, true).fill);
            assert!(s1 >= 0.6 && s127 > s1 + 0.2, "{id}: {s1} {s127}");
        }
    }

    fn manual(colors: &[[u8; 3]]) -> NotePalette {
        NotePalette::new(NoteColorPrefs {
            mode: NoteColorMode::Manual,
            seed: 0,
            manual: colors.to_vec(),
        })
    }

    #[test]
    fn manual_colours_are_exact_and_expand_without_repeating_colour_1() {
        let chosen: Vec<[u8; 3]> = TRACK_PALETTE.iter().take(16).map(|t| t.1).collect();
        let p = manual(&chosen);
        for dark in [true, false] {
            for (k, c) in chosen.iter().enumerate() {
                assert_eq!(rgb_of(p.entry(k as u32, 127, dark).fill), *c, "color {k}");
                assert_eq!(p.base_rgb(k as u32, dark), *c);
            }
        }
        // Subchannel 17 (id 16) is not colour 1; the expansion is
        // deterministic and keeps 48 more subchannels apart.
        let c17 = p.base_rgb(16, true);
        assert!(color_distance(c17, chosen[0]) >= 0.03, "{c17:?}");
        assert_eq!(c17, manual(&chosen).base_rgb(16, false));
        let all: Vec<[u8; 3]> = (0..64).map(|id| p.base_rgb(id, true)).collect();
        for i in 0..64 {
            for j in 0..i {
                assert_ne!(all[i], all[j], "subchannels {i} / {j}");
            }
        }
        // Fewer colours: the expansion starts after them.
        let three = manual(&chosen[..3]);
        assert_eq!(three.base_rgb(2, true), chosen[2]);
        assert!(color_distance(three.base_rgb(3, true), chosen[0]) >= 0.03);
        // Greys at the lightness limits still expand to another colour.
        for c in [
            [255u8, 255, 255],
            [0, 0, 0],
            [128, 128, 128],
            [235, 235, 235],
        ] {
            let g = manual(&[c]);
            for id in 1..8 {
                assert!(
                    color_distance(g.base_rgb(id, true), c) >= 0.03,
                    "{c:?} {id}"
                );
            }
        }
        // More than 16 stored: only 16 are used.
        let many: Vec<[u8; 3]> = (0..20).map(|i| [i * 10, 50, 200]).collect();
        let m = manual(&many);
        assert_eq!(m.prefs.manual.len(), MANUAL_NOTE_COLORS);
        assert_ne!(m.base_rgb(16, true), many[16]);
        // Velocity shades the chosen colour's saturation, never its hue.
        let red = manual(&[[220, 40, 40]]);
        let mut prev = -1.0f32;
        for vel in 1..=127u8 {
            let (h, s, _) = to_hsv(red.entry(0, vel, true).fill);
            assert!(s >= prev - 0.01, "{vel}");
            assert!(!(0.02..=0.98).contains(&h), "{vel}: {h}");
            prev = s;
        }
        // Selected notes stay distinguishable.
        for k in 0..16 {
            let e = p.entry(k, 100, true);
            assert_ne!(e.fill, e.selected_fill);
        }
    }

    #[test]
    fn switching_to_manual_keeps_the_colours_until_one_is_picked() {
        let d = NotePalette::default();
        let defaults = NoteColorPrefs::default_manual();
        assert_eq!(defaults.len(), MANUAL_NOTE_COLORS);
        // Empty stored list = the defaults.
        let m = manual(&[]);
        assert_eq!(m, manual(&defaults));
        for id in 0..16 {
            assert_eq!(m.base_rgb(id, true), d.base_rgb(id, true), "{id}");
        }
        // The seed does not matter in Manual mode.
        let other = NotePalette::new(NoteColorPrefs {
            mode: NoteColorMode::Manual,
            seed: 99,
            manual: Vec::new(),
        });
        assert_eq!(other.key(), m.key());
        assert_eq!(
            NoteColorMode::from_key("manual"),
            Some(NoteColorMode::Manual)
        );
        assert_eq!(
            NoteColorMode::from_key("random"),
            Some(NoteColorMode::RandomBySeed)
        );
        assert_eq!(NoteColorMode::from_key("rainbow"), None);
    }

    #[test]
    fn ghosts_turn_by_the_palettes_subchannel_hues() {
        let tint = track_color(2, None, true, 255);
        let (h0, ..) = to_hsv(tint);
        let chosen = [[220u8, 40, 40], [40, 200, 60], [40, 80, 220]];
        let p = manual(&chosen);
        // The main subchannel keeps the track's hue; the others turn as far
        // as their chosen colour is from colour 1.
        let (g0, ..) = to_hsv(ghost_tint(&p, tint, 0, 12));
        assert!((g0 - h0).abs() < 0.01 || (g0 - h0).abs() > 0.99);
        for k in 1..3u32 {
            let (gk, ..) = to_hsv(ghost_tint(&p, tint, k, 12));
            let want = (h0 + p.hue_offset(k)).rem_euclid(1.0);
            let d = (gk - want).abs();
            assert!(!(0.01..=0.99).contains(&d), "{k}: {gk} vs {want}");
        }
        // Caches follow a palette change.
        let mut cache = GhostTints::default();
        let a = *cache.get_with_alpha(&dp(), tint, 1, 255);
        let b = *cache.get_with_alpha(&p, tint, 1, 255);
        assert_eq!(a, ghost_tints(&dp(), tint, 1));
        assert_eq!(b, ghost_tints(&p, tint, 1));
        assert_ne!(a, b);
        let mut subs = SubPalettes::default();
        let x = subs.get(&dp(), 1, true).fill[64];
        let y = subs.get(&p, 1, true).fill[64];
        assert_ne!(x, y);
        assert_eq!(*velocity_palette(&p, true), build_sub_palette(&p, 0, true));
    }

    #[test]
    fn the_context_holds_the_note_palette() {
        let ctx = egui::Context::default();
        assert!(note_palette(&ctx).is_default());
        let p = std::sync::Arc::new(seeded(5));
        set_note_palette(&ctx, p.clone());
        assert_eq!(note_palette(&ctx), p);
        // Another context is unaffected (tests and windows stay isolated).
        assert!(note_palette(&egui::Context::default()).is_default());
    }
}
