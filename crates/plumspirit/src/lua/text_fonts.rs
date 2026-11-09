//! Job-owned font data/metrics on the existing skrifa stack; no OS path API.
use super::*;
use mlua::{AnyUserData, UserData, UserDataMethods};
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::types::GlyphId;
use skrifa::{FontRef, MetadataProvider};
#[path = "text_font_color.rs"]
mod color;
#[path = "text_font_coverage.rs"]
mod coverage;
#[path = "text_font_inventory.rs"]
mod inventory;
#[path = "text_font_outlines.rs"]
mod outlines;
#[path = "text_font_shaping.rs"]
mod shaping;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FACES: u32 = 256;
const MAX_HANDLES: usize = 32;
const PARSER_WORK: usize = 64 * 1024;
const HANDLE_BYTES: usize = 512;
type Data = Rc<RefCell<Option<Stored>>>;
#[derive(Default)]
struct FontSlot(Vec<Weak<RefCell<Option<Stored>>>>, usize);
struct Stored {
    bytes: Vec<u8>,
    index: u32,
    kind: &'static str,
    label: String,
    _allocation: Reservation,
}
struct Font {
    job: std::sync::Weak<JobLease>,
    data: Data,
    // Account the small native cell/job reference even from expired userdata.
    _handle: Reservation,
}
const _: () = assert!(
    HANDLE_BYTES
        >= std::mem::size_of::<JobLease>() + std::mem::size_of::<RefCell<Option<Stored>>>() + 64
);
fn bad(message: &str) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn font_scope(lua: &Lua, job: &Arc<JobLease>) -> mlua::Result<()> {
    job.check().map_err(|e| e.lua())?;
    if !lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .is_some_and(|s| Arc::ptr_eq(&s, job))
    {
        return Err(stop(
            Category::Expired,
            "This font belongs to an earlier job; load it again.",
        ));
    }
    notes::check_budget(lua)
}
pub(super) fn end_run(lua: &Lua) {
    inventory::end_run(lua);
    if let Some(mut slot) = lua.app_data_mut::<FontSlot>() {
        slot.1 = 0;
        for data in slot.0.drain(..).filter_map(|w| w.upgrade()) {
            data.borrow_mut().take();
        }
    }
}
fn context(lua: &Lua) -> mlua::Result<(Arc<JobLease>, NativeBudget)> {
    notes::check_budget(lua)?;
    let job = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| stop(Category::Expired, "No font job is active."))?;
    font_scope(lua, &job)?;
    if lua
        .app_data_ref::<FontSlot>()
        .is_none_or(|s| s.0.len() >= MAX_HANDLES)
    {
        return Err(stop(
            Category::Budget,
            "At most32font handles may be created per job, including closed handles.",
        ));
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Font accounting unavailable."))?
        .clone();
    Ok((job, budget))
}
fn index(v: Value, max: u32) -> mlua::Result<u32> {
    match v {
        Value::Integer(n) if n >= 0 && n <= i64::from(max) => Ok(n as u32),
        _ => Err(bad(
            "Font face/glyph index must be an in-range Lua integer.",
        )),
    }
}
fn scalar(v: Value) -> mlua::Result<char> {
    match v {
        Value::Integer(n) if (0..=0x10ffff).contains(&n) => {
            char::from_u32(n as u32).ok_or_else(|| bad("Surrogates are not Unicode scalar values."))
        }
        Value::String(s) if s.as_bytes().len() <= 4 => {
            let s = s.to_str()?;
            let mut chars = s.chars();
            let ch = chars
                .next()
                .ok_or_else(|| bad("Expected one Unicode scalar, not an empty string."))?;
            if chars.next().is_some() {
                return Err(bad(
                    "Coverage accepts one Unicode scalar; grapheme clusters require shaping.",
                ));
            }
            Ok(ch)
        }
        _ => Err(bad(
            "Coverage accepts one UTF-8 scalar or integer codepoint, not a shaped grapheme.",
        )),
    }
}
fn parsed(data: &Stored) -> mlua::Result<FontRef<'_>> {
    FontRef::from_index(&data.bytes, data.index).map_err(|_| {
        bad("Unsupported, malformed or unavailable font face; expected raw TTF/OTF/TTC.")
    })
}
fn read_bytes<R: std::io::Read + ?Sized>(
    reader: &mut R,
    length: usize,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<u8>> {
    if length == 0 || length > MAX_BYTES {
        return Err(stop(
            Category::Budget,
            "Font input must contain1byte–64MiB.",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| stop(Category::Memory, "Cannot allocate selected font."))?;
    let mut chunk = [0u8; 64 * 1024];
    while bytes.len() < length {
        check()?;
        let n = chunk.len().min(length - bytes.len());
        reader
            .read_exact(&mut chunk[..n])
            .map_err(|_| bad("Selected font could not be read completely."))?;
        bytes.extend_from_slice(&chunk[..n]);
    }
    check()?;
    Ok(bytes)
}
fn store(
    lua: &Lua,
    job: Arc<JobLease>,
    bytes: Vec<u8>,
    index: u32,
    kind: &'static str,
    label: String,
    allocation: Reservation,
) -> mlua::Result<Font> {
    font_scope(lua, &job)?;
    if bytes.starts_with(b"ttcf") {
        let count = bytes
            .get(8..12)
            .and_then(|s| s.try_into().ok())
            .map(u32::from_be_bytes)
            .unwrap_or(0);
        if count == 0 || count > MAX_FACES {
            return Err(stop(Category::Budget, "Font collections allow1–256faces."));
        }
    }
    let mut stored = Stored {
        bytes,
        index,
        kind,
        label,
        _allocation: allocation,
    };
    let font = parsed(&stored)?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    if metrics.units_per_em == 0 || metrics.glyph_count == 0 {
        return Err(bad("Font has no usable units-per-em or glyph metrics."));
    }
    // Do not call native glyph-bound fallback while loading metadata: CFF/variable
    // bounds may draw outlines. Explicit bounded outlines arrive separately.
    notes::check_budget(lua)?;
    stored._allocation.shrink_to(stored.bytes.len() + 2048);
    let handle = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Font accounting unavailable."))?
        .reserve(HANDLE_BYTES)
        .map_err(|e| e.lua())?;
    let data = Rc::new(RefCell::new(Some(stored)));
    lua.app_data_mut::<FontSlot>()
        .ok_or_else(|| bad("No font resource scope."))?
        .0
        .push(Rc::downgrade(&data));
    Ok(Font {
        job: Arc::downgrade(&job),
        data,
        _handle: handle,
    })
}
impl Font {
    fn with<T>(
        &self,
        lua: &Lua,
        f: impl FnOnce(&Stored, FontRef<'_>) -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        let job = self.job.upgrade().ok_or_else(|| {
            stop(
                Category::Expired,
                "This font belongs to an earlier job; load it again.",
            )
        })?;
        font_scope(lua, &job)?;
        let budget = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| bad("Font accounting unavailable."))?
            .clone();
        let _scratch = budget.reserve(PARSER_WORK).map_err(|e| e.lua())?;
        let data = self.data.borrow();
        let stored = data
            .as_ref()
            .ok_or_else(|| stop(Category::Expired, "This font is closed."))?;
        if stored.kind == "installed" {
            inventory::require(lua)?;
        }
        let out = f(stored, parsed(stored)?)?;
        notes::check_budget(lua)?;
        Ok(out)
    }
}
fn advance(font: &FontRef<'_>, id: u32) -> mlua::Result<f64> {
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let advance = font
        .glyph_metrics(Size::unscaled(), LocationRef::default())
        .advance_width(GlyphId::new(id))
        .ok_or_else(|| bad("Glyph advance unavailable in this font."))?;
    let advance = f64::from(advance) / f64::from(metrics.units_per_em);
    if !advance.is_finite() || advance.abs() > 1e6 {
        return Err(bad("Font glyph advance is invalid."));
    }
    Ok(advance)
}
impl UserData for Font {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        outlines::add_methods(m);
        shaping::add_methods(m);
        color::add_methods(m);
        m.add_method("info", |lua, this, ()| {
            this.with(lua, |data, font| {
                let metrics = font.metrics(Size::unscaled(), LocationRef::default());
                let em = f64::from(metrics.units_per_em);
                let t = lua.create_table()?;
                t.set("source", data.kind)?;
                t.set("shaping_supported", data.kind == "builtin" || cfg!(windows))?;
                t.set(
                    "shape_backend",
                    if data.kind == "builtin" {
                        "bundled_native"
                    } else if cfg!(windows) {
                        "isolated_native"
                    } else {
                        "unsupported"
                    },
                )?;
                t.set(
                    "outline_format",
                    crate::app::spiderweb::fonts::outline_format(&font),
                )?;
                t.set("source_name", data.label.as_str())?;
                t.set("face_index", data.index)?;
                t.set("bytes", data.bytes.len())?;
                t.set("units_per_em", metrics.units_per_em)?;
                t.set("glyph_count", metrics.glyph_count)?;
                for (key, value) in [
                    ("ascent", metrics.ascent),
                    ("descent", metrics.descent),
                    ("line_gap", metrics.leading),
                    (
                        "line_height",
                        metrics.ascent - metrics.descent + metrics.leading,
                    ),
                ] {
                    let value = f64::from(value) / em;
                    if !value.is_finite() {
                        return Err(bad("Font contains nonfinite metrics."));
                    }
                    t.set(key, value)?;
                }
                Ok(t)
            })
        });
        m.add_method("glyph_id", |lua, this, ch: Value| {
            let ch = scalar(ch)?;
            this.with(lua, |_, font| {
                let id = font
                    .charmap()
                    .map(ch)
                    .map(|g| g.to_u32())
                    .filter(|g| *g != 0);
                if id.is_some_and(|id| {
                    id >= u32::from(
                        font.metrics(Size::unscaled(), LocationRef::default())
                            .glyph_count,
                    )
                }) {
                    return Err(bad("Font maps this scalar to an invalid glyph index."));
                }
                Ok(id)
            })
        });
        m.add_method("glyph_advance", |lua, this, v: Value| {
            this.with(lua, |_, font| {
                let metrics = font.metrics(Size::unscaled(), LocationRef::default());
                let id = index(v, u32::from(metrics.glyph_count) - 1)?;
                advance(&font, id)
            })
        });
        m.add_method("close", |_, this, ()| {
            Ok(this.data.borrow_mut().take().is_some())
        });
    }
}
pub(super) fn install(lua: &Lua, text: &Table) -> mlua::Result<()> {
    lua.set_app_data(FontSlot::default());
    text.set("max_font_bytes", MAX_BYTES)?;
    text.set("max_font_handles", MAX_HANDLES)?;
    text.set("max_font_faces", MAX_FACES)?;
    text.set("max_color_layers", color::MAX_LAYERS)?;
    outlines::install(text)?;
    shaping::install(text)?;
    inventory::install(lua, text)?;
    coverage::install(lua, text)?;
    text.set(
        "builtin_font",
        lua.create_function(|lua, kind: Value| {
            let family = match kind {
                Value::Nil => Some(egui::FontFamily::Proportional),
                Value::String(s) if s.as_bytes().len() <= 16 => match s.to_str()?.as_ref() {
                    "proportional" => Some(egui::FontFamily::Proportional),
                    "monospace" => Some(egui::FontFamily::Monospace),
                    "emoji" => None,
                    _ => {
                        return Err(bad(
                            "Builtin font must be proportional, monospace or emoji.",
                        ));
                    }
                },
                _ => {
                    return Err(bad(
                        "Builtin font must be proportional, monospace or emoji.",
                    ));
                }
            };
            let (job, budget) = context(lua)?;
            let _defs = budget.reserve(PARSER_WORK).map_err(|e| e.lua())?;
            let defs = egui::FontDefinitions::default();
            let (name, source) = if let Some(family) = family {
                let names = defs
                    .families
                    .get(&family)
                    .ok_or_else(|| bad("Bundled font unavailable."))?;
                names
                    .iter()
                    .find_map(|n| defs.font_data.get(n).map(|f| (n, f)))
                    .ok_or_else(|| bad("Bundled font unavailable."))?
            } else {
                defs.font_data
                    .iter()
                    .find(|(name, _)| name.contains("Emoji"))
                    .ok_or_else(|| bad("Bundled emoji font unavailable."))?
            };
            if source.font.len() > MAX_BYTES {
                return Err(stop(Category::Budget, "Bundled font exceeds64MiB."));
            }
            let allocation = budget
                .reserve(source.font.len() * 2 + 1024 * 1024)
                .map_err(|e| e.lua())?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(source.font.len())
                .map_err(|_| stop(Category::Memory, "Cannot allocate bundled font."))?;
            for chunk in source.font.chunks(64 * 1024) {
                notes::check_budget(lua)?;
                bytes.extend_from_slice(chunk);
            }
            store(
                lua,
                job,
                bytes,
                source.index,
                "builtin",
                name.clone(),
                allocation,
            )
        })?,
    )?;
    text.set(
        "load_font",
        lua.create_function(|lua, (file, face): (AnyUserData, Value)| {
            let (job, budget) = context(lua)?;
            let face = if matches!(face, Value::Nil) {
                0
            } else {
                index(face, MAX_FACES - 1)?
            };
            let kind = super::super::files::source_kind(lua, &file)?;
            super::super::files::with_reader(lua, &file, |reader, length, name| {
                if length == 0 || length > MAX_BYTES as u64 {
                    return Err(stop(
                        Category::Budget,
                        "Font input must contain1byte–64MiB.",
                    ));
                }
                let allocation = budget
                    .reserve(length as usize * 2 + 1024 * 1024)
                    .map_err(|e| e.lua())?;
                let bytes = read_bytes(reader, length as usize, || notes::check_budget(lua))?;
                store(
                    lua,
                    Arc::clone(&job),
                    bytes,
                    face,
                    kind,
                    name.chars().take(256).collect(),
                    allocation,
                )
            })
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "text_font_tests.rs"]
mod tests;
