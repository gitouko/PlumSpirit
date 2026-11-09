//! Shared raster decoder for the built-in converter and approved Lua files.
//! Check dimensions and native resource preflight before allocating pixels.
use image::{ImageDecoder, ImageFormat};
use plumspirit_engine::transform::image_midi::{self as im, Rgba};
use std::io::{BufRead, Seek};

#[derive(Clone, Copy)]
pub struct Bounds {
    pub side: u32,
    pub pixels: u64,
    pub allocation: u64,
    /// Separate header allowance for untrusted plugin files. The built-in
    /// caller retains its historical limits with None.
    pub header_allocation: Option<u64>,
}

pub const BUILTIN: Bounds = Bounds {
    side: im::MAX_SIDE,
    pixels: im::MAX_PIXELS,
    allocation: im::MAX_PIXELS * 8,
    header_allocation: None,
};

pub struct Info {
    pub width: u32,
    pub height: u32,
    pub decoded_bytes: u64,
}

pub struct Decoded {
    pub rgba: Rgba,
    pub format: &'static str,
}

pub fn decode<R: BufRead + Seek>(
    source: R,
    hint: Option<ImageFormat>,
    bounds: Bounds,
    before_pixels: impl FnOnce(&Info) -> Result<(), String>,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<Decoded, String> {
    let result = (|| {
        check()?;
        // Preserve ImageReader::open's basename fallback when magic guessing
        // does not cover a codec variant (e.g. BigTIFF). Content still wins.
        let mut reader = image::ImageReader::new(source);
        if let Some(format) = hint {
            reader.set_format(format);
        }
        let mut reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
        let format = match reader.format() {
            Some(ImageFormat::Png) => "png",
            Some(ImageFormat::Jpeg) => "jpeg",
            Some(ImageFormat::Bmp) => "bmp",
            Some(ImageFormat::Gif) => "gif",
            Some(ImageFormat::WebP) => "webp",
            Some(ImageFormat::Tiff) => "tiff",
            _ => return Err("unsupported raster format".to_owned()),
        };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(bounds.side);
        limits.max_image_height = Some(bounds.side);
        limits.max_alloc = Some(bounds.header_allocation.unwrap_or(bounds.allocation));
        reader.limits(limits);
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (width, height) = decoder.dimensions();
        im::check_size(width, height)?;
        if u64::from(width) * u64::from(height) > bounds.pixels {
            return Err(format!(
                "the picture exceeds the {}-pixel limit",
                bounds.pixels
            ));
        }
        before_pixels(&Info {
            width,
            height,
            decoded_bytes: decoder.total_bytes(),
        })?;
        if bounds.header_allocation.is_some() {
            // Codec-owned raw/intermediate buffers plus external DynamicImage
            // storage are separate allocations (notably TIFF). Keep the codec
            // limit within the preflighted allowance, with bounded scratch.
            let payload = decoder
                .total_bytes()
                .saturating_add(u64::from(width) * u64::from(height) * 4)
                .saturating_add(1024 * 1024)
                .min(bounds.allocation);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(bounds.side);
            limits.max_image_height = Some(bounds.side);
            limits.max_alloc = Some(payload);
            decoder.set_limits(limits).map_err(|e| e.to_string())?;
        }
        check()?;
        // DynamicImage keeps the existing first-frame/first-image policy and
        // color conversion. No new ICC or EXIF orientation processing is added.
        let rgba = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        check()?;
        let count = u64::from(width) * u64::from(height);
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count as usize)
            .map_err(|_| "not enough memory for the decoded picture".to_owned())?;
        for (i, pixel) in rgba.pixels().enumerate() {
            if i % 65_536 == 0 {
                check()?;
            }
            pixels.push(pixel.0);
        }
        Ok(Decoded {
            rgba: Rgba::new(width, height, pixels)?,
            format,
        })
    })();
    result.map_err(|e: String| format!(
        "The picture could not be decoded ({e}). PNG, JPEG, BMP, GIF, WebP and TIFF pictures up to {} × {} pixels are supported.", bounds.side, bounds.side
    ))
}
