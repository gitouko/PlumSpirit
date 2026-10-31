//! Embedded branding (B1-0175): the owner's official PlumSpirit logo (the plum)
//! and wordmark (the plum left of "Plum" + "Spirit"), derived from the owner's
//! artwork by `tools/make_icons.py` — transparent, un-matted (no white fringe)
//! PNGs in `assets/brand/`; provenance and hashes in
//! `docs/references/v1_0_beta1/LOGO_PROVENANCE.md` (B1-0176).
//!
//! Each image comes in a few pixel sizes; a draw takes the smallest one at
//! least as large as its on-screen pixels (UI scale and display density
//! included), decoded once on first use and kept as a texture. The light
//! theme's wordmark has the owner's colors; the dark theme's has its letters
//! lightened (hue kept) so they read on the dark background.

use std::collections::HashMap;

/// The logo (square), by side in pixels.
const LOGO: [(u32, &[u8]); 4] = [
    (32, include_bytes!("../../../assets/brand/logo_32.png")),
    (64, include_bytes!("../../../assets/brand/logo_64.png")),
    (128, include_bytes!("../../../assets/brand/logo_128.png")),
    (256, include_bytes!("../../../assets/brand/logo_256.png")),
];
/// The wordmark for light backgrounds, by height in pixels.
const WORDMARK_LIGHT: [(u32, &[u8]); 3] = [
    (
        72,
        include_bytes!("../../../assets/brand/wordmark_light_72.png"),
    ),
    (
        144,
        include_bytes!("../../../assets/brand/wordmark_light_144.png"),
    ),
    (
        288,
        include_bytes!("../../../assets/brand/wordmark_light_288.png"),
    ),
];
/// The wordmark for dark backgrounds, by height in pixels.
const WORDMARK_DARK: [(u32, &[u8]); 3] = [
    (
        72,
        include_bytes!("../../../assets/brand/wordmark_dark_72.png"),
    ),
    (
        144,
        include_bytes!("../../../assets/brand/wordmark_dark_144.png"),
    ),
    (
        288,
        include_bytes!("../../../assets/brand/wordmark_dark_288.png"),
    ),
];

/// One branding image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Brand {
    Logo,
    WordmarkLight,
    WordmarkDark,
}

impl Brand {
    /// The wordmark for the current theme.
    pub fn wordmark(dark: bool) -> Brand {
        if dark {
            Brand::WordmarkDark
        } else {
            Brand::WordmarkLight
        }
    }

    fn sizes(self) -> &'static [(u32, &'static [u8])] {
        match self {
            Brand::Logo => &LOGO,
            Brand::WordmarkLight => &WORDMARK_LIGHT,
            Brand::WordmarkDark => &WORDMARK_DARK,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Brand::Logo => "plumspirit-logo",
            Brand::WordmarkLight => "plumspirit-wordmark-light",
            Brand::WordmarkDark => "plumspirit-wordmark-dark",
        }
    }
}

/// The size (height) of `brand` to draw `px` pixels tall: the smallest at
/// least that tall, else the largest.
pub fn pick(brand: Brand, px: f32) -> (u32, &'static [u8]) {
    let sizes = brand.sizes();
    sizes
        .iter()
        .copied()
        .find(|(h, _)| *h as f32 >= px)
        .unwrap_or(sizes[sizes.len() - 1])
}

/// Decodes an embedded PNG (straight alpha).
fn decode(png: &[u8]) -> egui::ColorImage {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("embedded branding PNG decodes")
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())
}

/// The window and taskbar icon: the logo at 256 px (the system scales it).
pub fn window_icon() -> egui::IconData {
    let img = image::load_from_memory_with_format(LOGO[3].1, image::ImageFormat::Png)
        .expect("embedded logo decodes")
        .to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}

/// The branding textures, decoded on first use, one per (image, size).
#[derive(Default)]
pub struct BrandTextures {
    loaded: HashMap<(Brand, u32), egui::TextureHandle>,
}

impl BrandTextures {
    /// `brand` drawn `height` points tall: its texture and on-screen size.
    pub fn sized(
        &mut self,
        ctx: &egui::Context,
        brand: Brand,
        height: f32,
    ) -> egui::load::SizedTexture {
        let px = height * ctx.pixels_per_point();
        let (h, png) = pick(brand, px);
        let tex = self.loaded.entry((brand, h)).or_insert_with(|| {
            ctx.load_texture(
                format!("{}-{h}", brand.name()),
                decode(png),
                egui::TextureOptions::LINEAR,
            )
        });
        let [w, hh] = tex.size();
        let width = height * w as f32 / hh.max(1) as f32;
        egui::load::SizedTexture::new(tex.id(), egui::vec2(width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8()
    }

    #[test]
    fn every_branding_png_has_its_declared_size() {
        for (s, png) in LOGO {
            let img = rgba(png);
            assert_eq!((img.width(), img.height()), (s, s));
        }
        for set in [WORDMARK_LIGHT, WORDMARK_DARK] {
            let aspect = {
                let img = rgba(set[2].1);
                img.width() as f32 / img.height() as f32
            };
            for (h, png) in set {
                let img = rgba(png);
                assert_eq!(img.height(), h);
                let a = img.width() as f32 / h as f32;
                assert!((a - aspect).abs() < 0.02, "{h}: aspect {a} vs {aspect}");
            }
        }
    }

    /// B1-0175: transparent, dark-safe assets — clear corners, and no opaque
    /// neutral near-white pixel on the outline (the old white background's
    /// fringe; the dark wordmark's pale lavender letters are tinted, not
    /// white).
    #[test]
    fn branding_is_transparent_without_a_white_fringe() {
        let all = LOGO
            .iter()
            .chain(WORDMARK_LIGHT.iter())
            .chain(WORDMARK_DARK.iter());
        for (s, png) in all {
            let img = rgba(png);
            let (w, h) = (img.width(), img.height());
            for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
                assert_eq!(
                    img.get_pixel(x, y)[3],
                    0,
                    "{s}: corner ({x}, {y}) not clear"
                );
            }
            let clear = |x: i64, y: i64| {
                x < 0
                    || y < 0
                    || x >= w as i64
                    || y >= h as i64
                    || img.get_pixel(x as u32, y as u32)[3] == 0
            };
            let mut fringe = 0;
            for y in 0..h {
                for x in 0..w {
                    let p = img.get_pixel(x, y);
                    let (xi, yi) = (x as i64, y as i64);
                    let edge = p[3] > 0
                        && (clear(xi - 1, yi)
                            || clear(xi + 1, yi)
                            || clear(xi, yi - 1)
                            || clear(xi, yi + 1));
                    if edge
                        && p[3] >= 96
                        && p[0].min(p[1]).min(p[2]) >= 220
                        && p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) <= 12
                    {
                        fringe += 1;
                    }
                }
            }
            assert_eq!(fringe, 0, "{s}: {fringe} white fringe pixel(s)");
        }
    }

    #[test]
    fn the_size_drawn_is_the_smallest_at_least_as_large() {
        assert_eq!(pick(Brand::Logo, 16.0).0, 32);
        assert_eq!(pick(Brand::Logo, 64.0).0, 64);
        assert_eq!(pick(Brand::Logo, 65.0).0, 128);
        assert_eq!(pick(Brand::Logo, 4000.0).0, 256);
        assert_eq!(pick(Brand::WordmarkDark, 96.0).0, 144);
    }

    #[test]
    fn the_window_icon_is_the_logo_at_256_px() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    /// The Windows icon resource holds every size the shell asks for.
    #[test]
    fn the_windows_icon_holds_16_to_256_px() {
        let ico = include_bytes!("../../../assets/icon/plumspirit.ico");
        assert_eq!(&ico[0..4], &[0, 0, 1, 0], "an ICO header");
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        let mut sizes: Vec<u32> = (0..count)
            .map(|i| {
                let w = ico[6 + 16 * i];
                if w == 0 { 256 } else { u32::from(w) }
            })
            .collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![16, 24, 32, 48, 64, 128, 256]);
    }
}
