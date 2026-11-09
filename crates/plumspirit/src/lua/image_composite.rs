//! Ordered byte-space source-over on existing immutable native RGBA resources.
use super::*;
const MAX_LAYERS: usize = 32;
const MAX_WORK: usize = 64 * 1024 * 1024;
struct Layer {
    source: Rc<Stored>,
    x: i64,
    y: i64,
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}
fn offset(t: &Table, key: &str) -> mlua::Result<i64> {
    match t.raw_get::<Value>(key)? {
        Value::Nil => Ok(0),
        Value::Integer(n) if i32::try_from(n).is_ok() => Ok(n),
        _ => Err(bad(
            "Composite offsets must be signed32-bit integer pixel coordinates.",
        )),
    }
}
fn over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = u32::from(src[3]);
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return src;
    }
    let da = u32::from(dst[3]);
    let rest = 255 - sa;
    let alpha = sa * 255 + da * rest;
    let mut out = [0; 4];
    for i in 0..3 {
        let numerator = u32::from(src[i]) * sa * 255 + u32::from(dst[i]) * da * rest;
        out[i] = ((numerator + alpha / 2) / alpha) as u8;
    }
    out[3] = ((alpha + 127) / 255) as u8;
    out
}
fn pixels(
    width: usize,
    count: usize,
    background: [u8; 4],
    layers: &[Layer],
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<[u8; 4]>> {
    check()?;
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| stop(Category::Memory, "Cannot allocate composite RGBA pixels."))?;
    while out.len() < count {
        check()?;
        out.resize((out.len() + 4096).min(count), background);
    }
    for layer in layers {
        check()?;
        let source = &layer.source.rgba;
        for y in layer.top..layer.bottom {
            check()?;
            let sy = (y as i64 - layer.y) as usize;
            for (i, x) in (layer.left..layer.right).enumerate() {
                if i.is_multiple_of(4096) {
                    check()?;
                }
                let sx = (x as i64 - layer.x) as usize;
                let target = y * width + x;
                out[target] = over(out[target], source.px[sy * source.w as usize + sx]);
            }
        }
    }
    check()?;
    Ok(out)
}
fn controlled(
    lua: &Lua,
    input: Table,
    options: Table,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<LuaImage> {
    check()?;
    let (job, budget) = context(lua)?;
    if options.metatable().is_some() || input.metatable().is_some() {
        return Err(bad(
            "Composite descriptors and layers must be plain tables.",
        ));
    }
    ops::keys(&options, &[b"width", b"height", b"background"], "Composite")?;
    let width = integer(options.raw_get("width")?, im::MAX_SIDE, "Composite width")?;
    let height = integer(options.raw_get("height")?, im::MAX_SIDE, "Composite height")?;
    let count = size(width, height)?;
    let background = match options.raw_get::<Value>("background")? {
        Value::Nil => [0; 4],
        Value::Table(t) => rgba(&t)?,
        _ => return Err(bad("Composite background must be a plain RGBA array.")),
    };
    let n = input.raw_len();
    if n > MAX_LAYERS {
        return Err(stop(
            Category::Budget,
            "Composite supports at most32source layers.",
        ));
    }
    let mut keys = 0;
    for pair in input.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        keys += 1;
        if keys > n || !matches!(k,Value::Integer(i)if i>=1 && i as usize<=n) {
            return Err(bad("Composite layers must be a plain dense array."));
        }
    }
    if keys != n {
        return Err(bad("Composite layers cannot contain holes."));
    }
    let _scratch = budget
        .reserve(n * std::mem::size_of::<Layer>() + 1024)
        .map_err(|e| e.lua())?;
    let mut layers = Vec::new();
    layers.try_reserve_exact(n).map_err(|_| {
        stop(
            Category::Memory,
            "Cannot allocate bounded composite descriptors.",
        )
    })?;
    let mut work = count;
    for i in 1..=n {
        check()?;
        let desc = input.raw_get::<Table>(i)?;
        if desc.metatable().is_some() {
            return Err(bad("Composite layer descriptors must be plain tables."));
        }
        ops::keys(&desc, &[b"image", b"x", b"y"], "Composite layer")?;
        let x = offset(&desc, "x")?;
        let y = offset(&desc, "y")?;
        let image = desc.raw_get::<AnyUserData>("image")?;
        let image = image.borrow::<LuaImage>()?;
        let _lease = image.lease(lua)?;
        let source = Rc::clone(
            image
                .data
                .borrow()
                .as_ref()
                .ok_or_else(|| stop(Category::Expired, "Composite source is closed."))?,
        );
        let left = x.clamp(0, i64::from(width)) as usize;
        let top = y.clamp(0, i64::from(height)) as usize;
        let right = (x + i64::from(source.rgba.w)).clamp(0, i64::from(width)) as usize;
        let bottom = (y + i64::from(source.rgba.h)).clamp(0, i64::from(height)) as usize;
        work = work
            .checked_add(right.saturating_sub(left) * bottom.saturating_sub(top))
            .ok_or_else(|| bad("Composite work overflow."))?;
        if work > MAX_WORK {
            return Err(stop(
                Category::Budget,
                "Composite exceeds67,108,864clipped pixel operations; crop or split layers.",
            ));
        }
        layers.push(Layer {
            source,
            x,
            y,
            left,
            top,
            right,
            bottom,
        });
    }
    let handle = reserve_handle(lua)?;
    let allocation = budget.reserve(count * 4).map_err(|e| e.lua())?;
    let output = pixels(width as usize, count, background, &layers, &mut check)?;
    store_charged(
        lua,
        job,
        Rgba::new(width, height, output).map_err(bad)?,
        "rgba",
        allocation,
        handle,
    )
}
pub(super) fn install(lua: &Lua, images: &Table) -> mlua::Result<()> {
    images.set("max_composite_layers", MAX_LAYERS)?;
    images.set("max_composite_work", MAX_WORK)?;
    images.set(
        "composite",
        lua.create_function(|lua, (input, options): (Table, Table)| {
            controlled(lua, input, options, || notes::check_budget(lua))
        })?,
    )
}
#[cfg(test)]
#[path = "image_composite_tests.rs"]
mod tests;
