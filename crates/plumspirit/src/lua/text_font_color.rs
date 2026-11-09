//! Bounded original COLRv0 layers and CPAL colors; no paint interpreter.
use super::*;
use skrifa::raw::{
    TableProvider,
    tables::{colr::Colr, cpal::Cpal},
    types::Tag,
};
pub(super) const MAX_LAYERS: usize = 256;
fn malformed(_: impl std::fmt::Display) -> mlua::Error {
    bad("Invalid COLR/CPAL color-font table.")
}
fn tables<'a>(face: &FontRef<'a>) -> mlua::Result<Option<(Colr<'a>, Cpal<'a>)>> {
    if face.table_data(Tag::new(b"COLR")).is_none() {
        return Ok(None);
    }
    let colr = face.colr().map_err(malformed)?;
    let cpal = face.cpal().map_err(malformed)?;
    if colr.version() > 1 || cpal.version() > 1 {
        return Err(bad("Unsupported COLR/CPAL table version."));
    }
    if cpal.num_palettes() == 0 || cpal.color_record_indices().len() != cpal.num_palettes() as usize
    {
        return Err(bad("Invalid CPAL palette index table."));
    }
    Ok(Some((colr, cpal)))
}
fn info(lua: &Lua, font: &Font) -> mlua::Result<Option<Table>> {
    font.with(lua, |_, face| {
        let Some((colr, cpal)) = tables(&face)? else {
            return Ok(None);
        };
        let t = lua.create_table()?;
        t.raw_set("format", "colr_v0")?;
        t.raw_set("table_version", colr.version())?;
        t.raw_set("palette_count", cpal.num_palettes())?;
        t.raw_set("palette_entries", cpal.num_palette_entries())?;
        t.raw_set("base_glyph_count", colr.num_base_glyph_records())?;
        t.raw_set("layer_count", colr.num_layer_records())?;
        t.raw_set("v0_layers_available", colr.num_base_glyph_records() != 0)?;
        Ok(Some(t))
    })
}
fn foreground(t: &Table) -> mlua::Result<[u8; 4]> {
    match t.raw_get::<Value>("foreground")? {
        Value::Nil => Ok([0, 0, 0, 255]),
        Value::Table(t) => crate::lua::images::rgba(&t),
        _ => Err(bad(
            "Color foreground must be a plain four-byte RGBA array.",
        )),
    }
}

struct Layer {
    glyph: u32,
    palette: u16,
    rgba: [u8; 4],
    foreground: bool,
}
fn layers_controlled(
    lua: &Lua,
    font: &Font,
    id: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Option<Table>> {
    check()?;
    let opts = match opts {
        Some(opts) => opts,
        None => lua.create_table()?,
    };
    super::super::layout::keys(&opts, &["palette", "max_layers", "foreground"])?;
    let cap = super::super::layout::limit(opts.raw_get("max_layers")?, 64, MAX_LAYERS)?;
    let color = foreground(&opts)?;
    let palette = match opts.raw_get::<Value>("palette")? {
        Value::Nil => 0,
        Value::Integer(n) if (0..=65535).contains(&n) => n as usize,
        _ => return Err(bad("Color palette must be an integer index0–65535.")),
    };
    font.with(lua, |_, face| {
        let count = u32::from(
            face.metrics(Size::unscaled(), LocationRef::default())
                .glyph_count,
        );
        let glyph = index(id, count - 1)?;
        let Some((colr, cpal)) = tables(&face)? else {
            return Ok(None);
        };
        if palette >= cpal.num_palettes() as usize {
            return Err(bad("Color palette is outside this font's palette list."));
        }
        // This binary search is bounded by the u16 base-record count. It does
        // not enter recursive COLRv1 paints or copy the font's layer/palette arrays.
        check()?;
        let range = colr.v0_base_glyph(GlyphId::new(glyph)).map_err(malformed)?;
        let Some(range) = range else {
            return Ok(None);
        };
        if range.len() > cap {
            return Err(stop(
                Category::Budget,
                "Color glyph exceeds max_layers; no partial layers returned.",
            ));
        }
        if range.end > colr.num_layer_records() as usize {
            return Err(bad("Color glyph layer range is outside COLR."));
        }
        let _native = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| bad("Color glyph accounting unavailable."))?
            .reserve(cap * 64 + 1024)
            .map_err(|e| e.lua())?;
        let colors = cpal
            .color_records_array()
            .ok_or_else(|| bad("Missing CPAL color array."))?
            .map_err(malformed)?;
        let start = cpal.color_record_indices()[palette].get() as usize;
        if start + cpal.num_palette_entries() as usize > colors.len() {
            return Err(bad("Color palette range is outside CPAL."));
        }
        let mut layers = Vec::new();
        layers
            .try_reserve_exact(range.len())
            .map_err(|_| stop(Category::Memory, "Cannot allocate bounded color layers."))?;
        for (i, layer) in range.enumerate() {
            if i.is_multiple_of(16) {
                check()?;
            }
            let (id, pal) = colr.v0_layer(layer).map_err(malformed)?;
            let glyph = id.to_u32();
            if glyph >= count {
                return Err(bad("Color layer maps to an invalid glyph index."));
            }
            let foreground = pal == 65535;
            let rgba = if foreground {
                color
            } else {
                if pal >= cpal.num_palette_entries() {
                    return Err(bad("Color layer palette entry is outside CPAL."));
                }
                let c = &colors[start + pal as usize];
                [c.red(), c.green(), c.blue(), c.alpha()]
            };
            layers.push(Layer {
                glyph,
                palette: pal,
                rgba,
                foreground,
            });
        }
        let out = lua.create_table()?;
        let names = [
            "glyph_id",
            "palette_index",
            "foreground",
            "red",
            "green",
            "blue",
            "alpha",
        ];
        let cols = names
            .iter()
            .map(|_| lua.create_table_with_capacity(layers.len(), 0))
            .collect::<mlua::Result<Vec<_>>>()?;
        for (i, layer) in layers.iter().enumerate() {
            if i.is_multiple_of(16) {
                check()?;
            }
            cols[0].raw_set(i + 1, layer.glyph)?;
            cols[1].raw_set(i + 1, layer.palette)?;
            cols[2].raw_set(i + 1, layer.foreground)?;
            for (col, value) in cols[3..].iter().zip(layer.rgba) {
                col.raw_set(i + 1, value)?;
            }
        }
        for (name, col) in names.iter().zip(cols) {
            out.raw_set(*name, col)?;
        }
        out.raw_set("count", layers.len())?;
        out.raw_set("format", "colr_v0")?;
        out.raw_set("table_version", colr.version())?;
        out.raw_set("palette", palette)?;
        out.raw_set("palette_count", cpal.num_palettes())?;
        check()?;
        Ok(Some(out))
    })
}
pub(super) fn add_methods<M: UserDataMethods<Font>>(m: &mut M) {
    m.add_method("color_info", |lua, font, ()| info(lua, font));
    m.add_method(
        "color_layers",
        |lua, font, (id, opts): (Value, Option<Table>)| {
            layers_controlled(lua, font, id, opts, || notes::check_budget(lua))
        },
    );
}
#[cfg(test)]
#[path = "text_font_color_tests.rs"]
pub(super) mod tests;
