//! Bounded color services and deterministic sampled regional histograms.
use super::*;

pub const MAX_HISTOGRAM_SAMPLES: u32 = 262_144;
pub const MAX_HISTOGRAM_BINS: u32 = 1024;

pub(super) fn rgb(r: Value, g: Value, b: Value) -> mlua::Result<[u8; 3]> {
    Ok([
        integer(r, 255, "Red")? as u8,
        integer(g, 255, "Green")? as u8,
        integer(b, 255, "Blue")? as u8,
    ])
}

pub(super) fn color_table(table: &Table) -> mlua::Result<[u8; 3]> {
    for pair in table.clone().pairs::<Value, Value>() {
        if !matches!(pair?.0, Value::Integer(1..=3)) {
            return Err(bad(
                "A color must contain exactly three integer RGB entries.",
            ));
        }
    }
    rgb(table.raw_get(1)?, table.raw_get(2)?, table.raw_get(3)?)
}

pub(super) fn unit(options: &Table, key: &str, default: f64) -> mlua::Result<f64> {
    let value = ops::number(options, key, default)?;
    if !(0.0..=1.0).contains(&value) {
        return Err(bad(format!("Image {key} must be between 0 and 1.")));
    }
    Ok(value)
}

fn numeric(value: Value) -> mlua::Result<f64> {
    match value {
        Value::Integer(v) => Ok(v as f64),
        Value::Number(v) if v.is_finite() => Ok(v),
        _ => Err(bad("Color coordinates must be finite numbers.")),
    }
}

pub(super) fn distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let sum: u32 = (0..3)
        .map(|c| {
            let d = i32::from(a[c]) - i32::from(b[c]);
            (d * d) as u32
        })
        .sum();
    (f64::from(sum) / (3.0 * 255.0 * 255.0)).sqrt()
}

pub(super) fn brightness(rgb: [u8; 3]) -> f64 {
    let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
    (r + g + b) / 3.0
}

pub(super) fn luminance(rgb: [u8; 3]) -> f64 {
    let [r, g, b] = rgb.map(|v| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn cylindrical(rgb: [u8; 3], hsl: bool) -> [f64; 3] {
    let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let delta = high - low;
    let light = (high + low) / 2.0;
    let hue = if delta == 0.0 {
        0.0
    } else if high == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if high == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if delta == 0.0 {
        0.0
    } else if hsl {
        delta / (1.0 - (2.0 * light - 1.0).abs())
    } else {
        delta / high
    };
    // Integer RGB is mathematically in range; HSL division near an endpoint
    // can overshoot one by a few ulps. Keep forward outputs valid for inverse
    // conversion without relaxing validation of caller-supplied coordinates.
    [
        hue.rem_euclid(360.0),
        saturation.clamp(0.0, 1.0),
        if hsl { light } else { high },
    ]
}

fn from_cylindrical(h: f64, s: f64, v: f64, hsl: bool) -> [u8; 3] {
    let chroma = if hsl {
        (1.0 - (2.0 * v - 1.0).abs()) * s
    } else {
        v * s
    };
    let hue = if h == 360.0 { 0.0 } else { h / 60.0 };
    let x = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let offset = if hsl { v - chroma / 2.0 } else { v - chroma };
    let channels = match hue as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    channels.map(|v| ((v + offset) * 255.0).round().clamp(0.0, 255.0) as u8)
}

#[derive(Clone, Copy)]
enum Channel {
    Red,
    Green,
    Blue,
    Alpha,
    Luminance,
    Brightness,
    Hue,
    Saturation,
    Value,
    Lightness,
}
impl Channel {
    fn parse(name: &str) -> mlua::Result<Self> {
        Ok(match name {
            "red" => Self::Red,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "alpha" => Self::Alpha,
            "luminance" => Self::Luminance,
            "brightness" => Self::Brightness,
            "hue" => Self::Hue,
            "saturation" => Self::Saturation,
            "value" => Self::Value,
            "lightness" => Self::Lightness,
            _ => return Err(bad("Unknown image histogram channel.")),
        })
    }
    fn sample(self, p: [u8; 4]) -> f64 {
        let rgb = [p[0], p[1], p[2]];
        match self {
            Self::Red => f64::from(p[0]) / 255.0,
            Self::Green => f64::from(p[1]) / 255.0,
            Self::Blue => f64::from(p[2]) / 255.0,
            Self::Alpha => f64::from(p[3]) / 255.0,
            Self::Luminance => luminance(rgb),
            Self::Brightness => brightness(rgb),
            Self::Hue => cylindrical(rgb, false)[0] / 360.0,
            Self::Saturation => cylindrical(rgb, false)[1],
            Self::Value => cylindrical(rgb, false)[2],
            Self::Lightness => cylindrical(rgb, true)[2],
        }
    }
}

/// Strict integer pixel rectangle, defaulting to the remainder of the source.
pub(super) fn rectangle(options: &Table, w: u32, h: u32) -> mlua::Result<(u32, u32, u32, u32)> {
    let get = |key, default, max| {
        integer(
            options
                .raw_get::<Option<Value>>(key)?
                .unwrap_or(Value::Integer(i64::from(default))),
            max,
            key,
        )
    };
    let x = get("x", 0, w - 1)?;
    let y = get("y", 0, h - 1)?;
    let width = get("width", w - x, w - x)?;
    let height = get("height", h - y, h - y)?;
    if width == 0 || height == 0 {
        return Err(bad("The image rectangle must be nonempty."));
    }
    Ok((x, y, width, height))
}

pub(super) fn add_methods<M: UserDataMethods<LuaImage>>(methods: &mut M) {
    methods.add_method("histogram", |lua, this, options: Option<Table>| {
        let options = options.unwrap_or(lua.create_table()?);
        ops::keys(
            &options,
            &[
                b"channel",
                b"bins",
                b"max_samples",
                b"alpha_min",
                b"x",
                b"y",
                b"width",
                b"height",
            ],
            "Image histogram",
        )?;
        let name = options
            .raw_get::<Option<String>>("channel")?
            .unwrap_or_else(|| "luminance".into());
        let channel = Channel::parse(&name)?;
        let bins = integer(
            options
                .raw_get::<Option<Value>>("bins")?
                .unwrap_or(Value::Integer(256)),
            MAX_HISTOGRAM_BINS,
            "Histogram bins",
        )?;
        let max = integer(
            options
                .raw_get::<Option<Value>>("max_samples")?
                .unwrap_or(Value::Integer(i64::from(MAX_HISTOGRAM_SAMPLES))),
            MAX_HISTOGRAM_SAMPLES,
            "Histogram samples",
        )?;
        if bins == 0 || max == 0 {
            return Err(bad("Histogram bins and samples must be positive."));
        }
        let alpha = integer(
            options
                .raw_get::<Option<Value>>("alpha_min")?
                .unwrap_or(Value::Integer(if matches!(channel, Channel::Alpha) {
                    0
                } else {
                    1
                })),
            255,
            "Histogram alpha minimum",
        )? as u8;
        this.with(lua, |image| {
            let (x, y, w, h) = rectangle(&options, image.rgba.w, image.rgba.h)?;
            let budget = lua.app_data_ref::<NativeBudget>().unwrap().clone();
            let _allocation = budget.reserve(bins as usize * 8).map_err(|e| e.lua())?;
            let mut counts = Vec::<u64>::new();
            counts
                .try_reserve_exact(bins as usize)
                .map_err(|_| stop(Category::Memory, "Not enough memory for the histogram."))?;
            counts.resize(bins as usize, 0);
            let total = u64::from(w) * u64::from(h);
            let examined = total.min(u64::from(max));
            let mut samples = 0u64;
            for k in 0..examined {
                if k.is_multiple_of(4096) {
                    notes::check_budget(lua)?;
                }
                let index = k * total / examined;
                let p = image.rgba.at(
                    x + (index % u64::from(w)) as u32,
                    y + (index / u64::from(w)) as u32,
                );
                if p[3] < alpha {
                    continue;
                }
                let bin =
                    ((channel.sample(p) * f64::from(bins)).floor() as usize).min(bins as usize - 1);
                counts[bin] += 1;
                samples += 1;
            }
            notes::check_budget(lua)?;
            let out = lua.create_table()?;
            out.set("counts", lua.create_sequence_from(counts)?)?;
            out.set("samples", samples)?;
            out.set("examined", examined)?;
            out.set("pixels", total)?;
            out.set("estimated", examined < total)?;
            out.set("channel", name)?;
            out.set("x", x)?;
            out.set("y", y)?;
            out.set("width", w)?;
            out.set("height", h)?;
            Ok(out)
        })
    });
}

pub(super) fn install(lua: &Lua, images: &Table) -> mlua::Result<()> {
    for (name, linear) in [("brightness", false), ("luminance", true)] {
        images.set(
            name,
            lua.create_function(move |_, (r, g, b): (Value, Value, Value)| {
                let color = rgb(r, g, b)?;
                Ok(if linear {
                    luminance(color)
                } else {
                    brightness(color)
                })
            })?,
        )?;
    }
    for (name, hsl) in [("rgb_to_hsv", false), ("rgb_to_hsl", true)] {
        images.set(
            name,
            lua.create_function(move |_, (r, g, b): (Value, Value, Value)| {
                let [h, s, v] = cylindrical(rgb(r, g, b)?, hsl);
                Ok((h, s, v))
            })?,
        )?;
    }
    for (name, hsl) in [("hsv_to_rgb", false), ("hsl_to_rgb", true)] {
        images.set(name,lua.create_function(move |_,(h,s,v):(Value,Value,Value)| {
            let (h,s,v)=(numeric(h)?,numeric(s)?,numeric(v)?);
            if !h.is_finite() || !(0.0..=360.0).contains(&h) || !s.is_finite() || !(0.0..=1.0).contains(&s) || !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(bad("Hue must be 0..360 degrees; saturation/value/lightness must be 0..1, all finite."));
            }
            let [r,g,b]=from_cylindrical(h,s,v,hsl);Ok((r,g,b))
        })?)?;
    }
    images.set(
        "color_distance",
        lua.create_function(
            |_, (r, g, b, r2, g2, b2): (Value, Value, Value, Value, Value, Value)| {
                Ok(distance(rgb(r, g, b)?, rgb(r2, g2, b2)?))
            },
        )?,
    )?;
    images.set(
        "nearest_color",
        lua.create_function(|lua, (palette, r, g, b): (Table, Value, Value, Value)| {
            let color = rgb(r, g, b)?;
            let len = palette.raw_len();
            if len == 0 || len > im::MAX_PALETTE {
                return Err(bad("Palette needs 1..1024 RGB colors."));
            }
            let mut entries = 0;
            for pair in palette.clone().pairs::<Value, Value>() {
                entries += 1;
                if entries > len
                    || !matches!(pair?.0,Value::Integer(i) if i>=1 && i as u64<=len as u64)
                {
                    return Err(bad("Palette must be a dense sequence of RGB colors."));
                }
            }
            if entries != len {
                return Err(bad("Palette must be a dense sequence of RGB colors."));
            }
            let budget = lua
                .app_data_ref::<NativeBudget>()
                .ok_or_else(|| bad("Native image accounting is unavailable."))?
                .clone();
            let _allocation = budget.reserve(len * 3).map_err(|e| e.lua())?;
            let mut colors = Vec::new();
            colors
                .try_reserve_exact(len)
                .map_err(|_| stop(Category::Memory, "Not enough memory for this palette."))?;
            for i in 1..=len {
                notes::check_budget(lua)?;
                colors.push(color_table(&palette.raw_get::<Table>(i)?)?);
            }
            let index = im::nearest(&colors, color);
            Ok((index + 1, distance(colors[index], color)))
        })?,
    )?;
    images.set("max_histogram_samples", MAX_HISTOGRAM_SAMPLES)?;
    images.set("max_histogram_bins", MAX_HISTOGRAM_BINS)?;
    images.set("max_palette", im::MAX_PALETTE)?;
    Ok(())
}
