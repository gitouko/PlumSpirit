//! Image-space affine descriptors sharing native generator geometry. Matrices
//! are plain bounded data, so author presets need not retain resource tokens.
use super::*;
use plumspirit_engine::transform::generator::draw::Aff;

const LIMIT: f64 = 1e12;

fn fields(m: Aff) -> [f64; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f]
}
fn coordinate(value: Value) -> mlua::Result<f64> {
    let value = match value {
        Value::Integer(v) => v as f64,
        Value::Number(v) => v,
        _ => return Err(bad("Image coordinates must be numbers.")),
    };
    if !value.is_finite() || value.abs() > LIMIT {
        return Err(bad(
            "Image mapping values must be finite, with magnitude at most 1e12.",
        ));
    }
    Ok(value)
}
pub(in crate::lua) fn checked_point((x, y): (f64, f64)) -> mlua::Result<(f64, f64)> {
    if !x.is_finite() || !y.is_finite() || x.abs() > LIMIT || y.abs() > LIMIT {
        return Err(bad(
            "Mapped image coordinates exceed the finite +/-1e12 range.",
        ));
    }
    Ok((x, y))
}
fn inverse(m: Aff) -> mlua::Result<Aff> {
    if fields(m).iter().any(|v| !v.is_finite() || v.abs() > LIMIT) {
        return Err(bad(
            "Image matrix coefficients exceed the finite +/-1e12 range.",
        ));
    }
    let scale = m.a.abs().max(m.b.abs()).max(m.c.abs()).max(m.d.abs());
    if scale == 0.0 {
        return Err(bad("The image matrix is singular."));
    }
    let (a, b, c, d) = (m.a / scale, m.b / scale, m.c / scale, m.d / scale);
    let det = a * d - b * c;
    if det.abs() <= 1e-12 {
        return Err(bad(
            "The image matrix is singular or too ill-conditioned to invert safely.",
        ));
    }
    let inv = Aff {
        a: d / (det * scale),
        b: -b / (det * scale),
        c: -c / (det * scale),
        d: a / (det * scale),
        e: 0.0,
        f: 0.0,
    };
    let inv = Aff {
        e: -(inv.a * m.e + inv.b * m.f),
        f: -(inv.c * m.e + inv.d * m.f),
        ..inv
    };
    if fields(inv)
        .iter()
        .any(|v| !v.is_finite() || v.abs() > LIMIT)
    {
        return Err(bad(
            "The inverse image matrix exceeds the finite +/-1e12 range.",
        ));
    }
    Ok(inv)
}
fn valid(m: Aff) -> mlua::Result<Aff> {
    inverse(m)?;
    Ok(m)
}
pub(in crate::lua) fn read(t: &Table) -> mlua::Result<Aff> {
    ops::keys(t, &[b"a", b"b", b"c", b"d", b"e", b"f"], "Image matrix")?;
    valid(Aff {
        a: coordinate(t.raw_get("a")?)?,
        b: coordinate(t.raw_get("b")?)?,
        c: coordinate(t.raw_get("c")?)?,
        d: coordinate(t.raw_get("d")?)?,
        e: coordinate(t.raw_get("e")?)?,
        f: coordinate(t.raw_get("f")?)?,
    })
}
fn table(lua: &Lua, m: Aff) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (key, value) in ["a", "b", "c", "d", "e", "f"].into_iter().zip(fields(m)) {
        out.raw_set(key, value)?;
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum Edge {
    Transparent,
    Clamp,
    Wrap,
    Mirror,
}
impl Edge {
    fn parse(name: &str) -> mlua::Result<Self> {
        match name {
            "transparent" => Ok(Self::Transparent),
            "clamp" => Ok(Self::Clamp),
            "wrap" => Ok(Self::Wrap),
            "mirror" => Ok(Self::Mirror),
            _ => Err(bad(
                "Image edge must be transparent, clamp, wrap or mirror.",
            )),
        }
    }
    fn sample(self, src: &Rgba, (x, y): (f64, f64)) -> [u8; 4] {
        if matches!(self, Self::Transparent)
            && (x < 0.0 || y < 0.0 || x >= f64::from(src.w) || y >= f64::from(src.h))
        {
            return [0, 0, 0, 0];
        }
        let axis = |v: f64, len: u32| -> u32 {
            let value = v.floor();
            let n = f64::from(len);
            match self {
                Self::Transparent | Self::Clamp => value.clamp(0.0, n - 1.0) as u32,
                Self::Wrap => value.rem_euclid(n) as u32,
                Self::Mirror => {
                    let k = value.rem_euclid(2.0 * n);
                    if k >= n {
                        (2.0 * n - 1.0 - k) as u32
                    } else {
                        k as u32
                    }
                }
            }
        };
        src.at(axis(x, src.w), axis(y, src.h))
    }
}

fn warp(lua: &Lua, image: &LuaImage, m: Aff, w: u32, h: u32, edge: Edge) -> mlua::Result<LuaImage> {
    let inv = inverse(m)?;
    // An affine function's extrema are at corners. Refuse extreme inverse
    // coordinates before any output allocation or float->index conversion.
    for corner in [
        (0.0, 0.0),
        (f64::from(w), 0.0),
        (0.0, f64::from(h)),
        (f64::from(w), f64::from(h)),
    ] {
        checked_point(inv.xy(corner))?;
    }
    let mut key = [0u64; 12];
    key[..4].copy_from_slice(&[4, u64::from(w), u64::from(h), edge as u64]);
    for (slot, value) in key[4..10].iter_mut().zip(fields(m)) {
        *slot = value.to_bits();
    }
    image.derived(lua, key, |src, before, check| {
        before(w, h, 0)?;
        let mut px = Vec::new();
        px.try_reserve_exact(w as usize * h as usize)
            .map_err(|_| "memory: image warp pixels could not be allocated".to_string())?;
        for y in 0..h {
            check()?;
            for x in 0..w {
                px.push(edge.sample(src, inv.xy((f64::from(x) + 0.5, f64::from(y) + 0.5))));
            }
        }
        Ok(Rgba { w, h, px })
    })
}

pub(super) fn add_methods<M: UserDataMethods<LuaImage>>(methods: &mut M) {
    methods.add_method("warp", |lua, this, options: Table| {
        ops::keys(
            &options,
            &[b"matrix", b"width", b"height", b"edge"],
            "Image warp",
        )?;
        let w = integer(options.raw_get("width")?, im::MAX_SIDE, "Warp width")?;
        let h = integer(options.raw_get("height")?, im::MAX_SIDE, "Warp height")?;
        size(w, h)?;
        let m = read(&options.raw_get::<Table>("matrix")?)?;
        let edge = Edge::parse(
            &options
                .raw_get::<Option<String>>("edge")?
                .unwrap_or_else(|| "transparent".into()),
        )?;
        warp(lua, this, m, w, h, edge)
    });
    methods.add_method(
        "fit",
        |lua, this, (width, height, policy): (Value, Value, Option<String>)| {
            let w = integer(width, im::MAX_SIDE, "Fit width")?;
            let h = integer(height, im::MAX_SIDE, "Fit height")?;
            size(w, h)?;
            let policy = policy.as_deref().unwrap_or("contain");
            if !matches!(policy, "contain" | "cover" | "stretch") {
                return Err(bad("Image fit policy must be contain, cover or stretch."));
            }
            let m = this.with(lua, |image| {
                let sx = f64::from(w) / f64::from(image.rgba.w);
                let sy = f64::from(h) / f64::from(image.rgba.h);
                let (sx, sy) = match policy {
                    "contain" => (sx.min(sy), sx.min(sy)),
                    "cover" => (sx.max(sy), sx.max(sy)),
                    _ => (sx, sy),
                };
                valid(
                    Aff::translate(
                        -f64::from(image.rgba.w) / 2.0,
                        -f64::from(image.rgba.h) / 2.0,
                    )
                    .then(Aff {
                        a: sx,
                        d: sy,
                        ..Aff::ID
                    })
                    .then(Aff::translate(f64::from(w) / 2.0, f64::from(h) / 2.0)),
                )
            })?;
            let result = warp(lua, this, m, w, h, Edge::Transparent)?;
            Ok((result, table(lua, m)?))
        },
    );
}

pub(in crate::lua) fn install(lua: &Lua, images: &Table) -> mlua::Result<()> {
    images.set(
        "mapping",
        lua.create_function(|lua, options: Table| {
            ops::keys(
                &options,
                &[
                    b"translate_x",
                    b"translate_y",
                    b"scale_x",
                    b"scale_y",
                    b"rotate",
                    b"pivot_x",
                    b"pivot_y",
                ],
                "Image mapping",
            )?;
            let value =
                |key, default| coordinate(Value::Number(ops::number(&options, key, default)?));
            let (px, py) = (value("pivot_x", 0.0)?, value("pivot_y", 0.0)?);
            let (sx, sy) = (value("scale_x", 1.0)?, value("scale_y", 1.0)?);
            let angle = value("rotate", 0.0)?.to_radians();
            let (dx, dy) = (value("translate_x", 0.0)?, value("translate_y", 0.0)?);
            let m = Aff::translate(-px, -py)
                .then(Aff {
                    a: sx,
                    d: sy,
                    ..Aff::ID
                })
                .then(Aff::rotate(angle, 0.0, 0.0, 1.0))
                .then(Aff::translate(px + dx, py + dy));
            table(lua, valid(m)?)
        })?,
    )?;
    images.set(
        "compose",
        lua.create_function(|lua, (first, second): (Table, Table)| {
            table(lua, valid(read(&first)?.then(read(&second)?))?)
        })?,
    )?;
    images.set(
        "inverse",
        lua.create_function(|lua, m: Table| table(lua, inverse(read(&m)?)?))?,
    )?;
    images.set(
        "map_point",
        lua.create_function(|_, (m, x, y): (Table, Value, Value)| {
            checked_point(read(&m)?.xy((coordinate(x)?, coordinate(y)?)))
        })?,
    )?;
    Ok(())
}
