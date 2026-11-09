//! Immutable, job-scoped RGBA resources. Decoders consume approved file
//! streams; native pixels never become a giant Lua table by default.
use mlua::{AnyUserData, Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::transform::image_midi::{self as im, Rgba};
use std::cell::{Cell, RefCell};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use super::bridge::{JobLease, NativeBudget, Reservation, Session};
use super::{Category, notes, stop};

pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_HANDLES: usize = 64;
pub const MAX_REGION_BYTES: usize = 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 16;
const HANDLE_BYTES: usize = 512;

/// Metadata may seek/read the same bytes repeatedly. Bound that work before
/// the decoder reports dimensions, independently of the whole-job file quota.
struct DecodeSource<'a> {
    source: &'a mut dyn super::files::ReadSource,
    header: Rc<Cell<bool>>,
    remaining: u64,
    exhausted: Rc<Cell<bool>>,
}
impl Read for DecodeSource<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let header = self.header.get();
        if header && self.remaining == 0 && !buffer.is_empty() {
            self.exhausted.set(true);
            return Err(std::io::Error::other("Image metadata read budget exceeded"));
        }
        let n = if header {
            buffer.len().min(self.remaining as usize)
        } else {
            buffer.len()
        };
        let read = self.source.read(&mut buffer[..n])?;
        if header {
            self.remaining -= read as u64;
        }
        Ok(read)
    }
}
impl Seek for DecodeSource<'_> {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        self.source.seek(from)
    }
}

#[derive(Default)]
pub(super) struct ImageSlot {
    images: Vec<Weak<RefCell<Option<Rc<Stored>>>>>,
    masks: Vec<Weak<RefCell<Option<masks::StoredMask>>>>,
    plans: Vec<Weak<RefCell<Option<plans::Stored>>>>,
    plan_attempts: usize,
    plan_pixels: u64,
    plan_work: u64,
    samplers: Vec<Weak<RefCell<Option<note_masks::Stored>>>>,
    sampler_attempts: usize,
    sampler_pixels: u64,
    cache: Vec<(CacheKey, Weak<Stored>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CacheKey {
    source: usize,
    operation: [u64; 12],
}

struct Stored {
    rgba: Rgba,
    format: &'static str,
    _allocation: Reservation,
}

#[derive(Clone)]
pub(super) struct LuaImage {
    job: std::sync::Weak<JobLease>,
    data: Rc<RefCell<Option<Rc<Stored>>>>,
    _handle: Arc<Reservation>,
}
const _: () = assert!(
    HANDLE_BYTES
        >= std::mem::size_of::<JobLease>()
            + std::mem::size_of::<RefCell<Option<Rc<Stored>>>>()
            + std::mem::size_of::<Stored>()
            + 64
);

/// Shared strict straight-RGBA descriptor for procedural images/font foregrounds.
pub(super) fn rgba(table: &Table) -> mlua::Result<[u8; 4]> {
    if table.metatable().is_some() || table.raw_len() != 4 {
        return Err(bad("RGBA must be a plain dense four-byte array."));
    }
    let mut count = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        count += 1;
        if count > 4 || !matches!(key, Value::Integer(1..=4)) {
            return Err(bad("RGBA must be a plain dense four-byte array."));
        }
    }
    if count != 4 {
        return Err(bad("RGBA must be a plain dense four-byte array."));
    }
    let mut color = [0; 4];
    for (i, c) in color.iter_mut().enumerate() {
        *c = integer(table.raw_get(i + 1)?, 255, "RGBA component")? as u8;
    }
    Ok(color)
}

pub(super) fn end_run(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<ImageSlot>() {
        slot.cache.clear();
        for image in slot.images.drain(..).filter_map(|s| s.upgrade()) {
            image.borrow_mut().take();
        }
        for mask in slot.masks.drain(..).filter_map(|s| s.upgrade()) {
            mask.borrow_mut().take();
        }
        for plan in slot.plans.drain(..).filter_map(|s| s.upgrade()) {
            plan.borrow_mut().take();
        }
        for sampler in slot.samplers.drain(..).filter_map(|s| s.upgrade()) {
            sampler.borrow_mut().take();
        }
        slot.sampler_attempts = 0;
        slot.sampler_pixels = 0;
        slot.plan_attempts = 0;
        slot.plan_pixels = 0;
        slot.plan_work = 0;
    }
}

fn bad(message: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, message)
}

fn context(lua: &Lua) -> mlua::Result<(Arc<JobLease>, NativeBudget)> {
    notes::check_budget(lua)?;
    let job = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| stop(Category::Expired, "No image job is active."))?;
    job.check().map_err(|e| e.lua())?;
    if lua
        .app_data_ref::<ImageSlot>()
        .is_none_or(|s| s.images.len() + s.masks.len() >= MAX_HANDLES)
    {
        return Err(stop(
            Category::Budget,
            "At most 64 image resources (images and masks together) may be created in one job; reuse native batches.",
        ));
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .ok_or_else(|| bad("Native image accounting is unavailable."))?;
    Ok((job, budget))
}

fn size(width: u32, height: u32) -> mlua::Result<usize> {
    im::check_size(width, height).map_err(bad)?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(stop(
            Category::Budget,
            "The image exceeds the plugin limit of 16,777,216 pixels.",
        ));
    }
    Ok(pixels as usize)
}

fn integer(value: Value, max: u32, label: &str) -> mlua::Result<u32> {
    match value {
        Value::Integer(n) if n >= 0 && n as u64 <= u64::from(max) => Ok(n as u32),
        _ => Err(bad(format!("{label} must be an integer from 0 to {max}."))),
    }
}

fn store(
    lua: &Lua,
    job: Arc<JobLease>,
    rgba: Rgba,
    format: &'static str,
    allocation: Reservation,
) -> mlua::Result<LuaImage> {
    store_charged(lua, job, rgba, format, allocation, reserve_handle(lua)?)
}

fn reserve_handle(lua: &Lua) -> mlua::Result<Reservation> {
    lua.app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Image accounting unavailable."))?
        .reserve(HANDLE_BYTES)
        .map_err(|e| e.lua())
}

fn store_charged(
    lua: &Lua,
    job: Arc<JobLease>,
    rgba: Rgba,
    format: &'static str,
    mut allocation: Reservation,
    handle: Reservation,
) -> mlua::Result<LuaImage> {
    job.check().map_err(|e| e.lua())?;
    allocation.shrink_to(rgba.px.len() * 4);
    shared_charged(
        lua,
        job,
        Rc::new(Stored {
            rgba,
            format,
            _allocation: allocation,
        }),
        handle,
    )
}

fn shared(lua: &Lua, job: Arc<JobLease>, image: Rc<Stored>) -> mlua::Result<LuaImage> {
    shared_charged(lua, job, image, reserve_handle(lua)?)
}

fn shared_charged(
    lua: &Lua,
    job: Arc<JobLease>,
    image: Rc<Stored>,
    handle: Reservation,
) -> mlua::Result<LuaImage> {
    job.check().map_err(|e| e.lua())?;
    let data = Rc::new(RefCell::new(Some(image)));
    lua.app_data_mut::<ImageSlot>()
        .ok_or_else(|| bad("No image scope is active."))?
        .images
        .push(Rc::downgrade(&data));
    Ok(LuaImage {
        job: Arc::downgrade(&job),
        data,
        _handle: Arc::new(handle),
    })
}

impl LuaImage {
    fn lease(&self, lua: &Lua) -> mlua::Result<Arc<JobLease>> {
        let job = self
            .job
            .upgrade()
            .ok_or_else(|| stop(Category::Expired, "This image belongs to a finished job."))?;
        check_scope(lua, &job, "image")?;
        Ok(job)
    }
    pub(super) fn with_pixels<T>(
        &self,
        lua: &Lua,
        f: impl FnOnce(&Rgba) -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        self.with(lua, |image| f(&image.rgba))
    }
    fn with<T>(
        &self,
        lua: &Lua,
        operation: impl FnOnce(&Stored) -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        let _job = self.lease(lua)?;
        let data = self.data.borrow();
        operation(
            data.as_deref()
                .ok_or_else(|| stop(Category::Expired, "This image is closed."))?,
        )
    }

    fn region(
        &self,
        lua: &Lua,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> mlua::Result<mlua::LuaString> {
        self.with(lua, |image| {
            let rgba = &image.rgba;
            if width == 0
                || height == 0
                || u64::from(x) + u64::from(width) > u64::from(rgba.w)
                || u64::from(y) + u64::from(height) > u64::from(rgba.h)
            {
                return Err(bad(
                    "The image rectangle must be nonempty and inside the image.",
                ));
            }
            let bytes = u64::from(width) * u64::from(height) * 4;
            if bytes > MAX_REGION_BYTES as u64 {
                return Err(stop(
                    Category::Budget,
                    "Image reads are limited to 1 MiB; request smaller rows or tiles.",
                ));
            }
            let budget = lua.app_data_ref::<NativeBudget>().unwrap().clone();
            let _allocation = budget.reserve(bytes as usize).map_err(|e| e.lua())?;
            let mut out = Vec::new();
            out.try_reserve_exact(bytes as usize)
                .map_err(|_| stop(Category::Memory, "Not enough memory for this image region."))?;
            for row in y..y + height {
                notes::check_budget(lua)?;
                let start = (row as usize) * rgba.w as usize + x as usize;
                for pixel in &rgba.px[start..start + width as usize] {
                    out.extend_from_slice(pixel);
                }
            }
            lua.create_string(out)
        })
    }
}

impl UserData for LuaImage {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        ops::add_methods(methods);
        color::add_methods(methods);
        masks::add_image_methods(methods);
        mapping::add_methods(methods);
        plans::add_methods(methods);
        methods.add_method("info", |lua, this, ()| {
            this.with(lua, |image| {
                let out = lua.create_table()?;
                out.set("width", image.rgba.w)?;
                out.set("height", image.rgba.h)?;
                out.set("format", image.format)?;
                out.set("bytes", (image.rgba.px.len() * 4) as i64)?;
                out.set("origin", "top_left")?;
                out.set("alpha", "straight")?;
                Ok(out)
            })
        });
        methods.add_method("pixel", |lua, this, (x, y): (Value, Value)| {
            this.with(lua, |image| {
                let x = integer(x, image.rgba.w - 1, "Pixel X")?;
                let y = integer(y, image.rgba.h - 1, "Pixel Y")?;
                let [r, g, b, a] = image.rgba.at(x, y);
                Ok((r, g, b, a))
            })
        });
        methods.add_method("read", |lua,this,options:Table| {
            for pair in options.clone().pairs::<Value,Value>() {
                let (key,_)=pair?;
                if !matches!(key,Value::String(s) if matches!(s.as_bytes().as_ref(),b"x"|b"y"|b"width"|b"height")) {
                    return Err(bad("Image read accepts only x, y, width and height."));
                }
            }
            let x=integer(options.get::<Option<Value>>("x")?.unwrap_or(Value::Integer(0)),im::MAX_SIDE,"Rectangle X")?;
            let y=integer(options.get::<Option<Value>>("y")?.unwrap_or(Value::Integer(0)),im::MAX_SIDE,"Rectangle Y")?;
            let width=integer(options.get("width")?,im::MAX_SIDE,"Rectangle width")?;
            let height=integer(options.get("height")?,im::MAX_SIDE,"Rectangle height")?;
            this.region(lua,x,y,width,height)
        });
        methods.add_method(
            "row",
            |lua, this, (y, x, count): (Value, Option<Value>, Option<Value>)| {
                let (x, y, count) = this.with(lua, |image| {
                    let x = integer(x.unwrap_or(Value::Integer(0)), image.rgba.w - 1, "Row X")?;
                    let y = integer(y, image.rgba.h - 1, "Row Y")?;
                    let count = integer(
                        count.unwrap_or(Value::Integer(i64::from(image.rgba.w - x))),
                        image.rgba.w - x,
                        "Row pixel count",
                    )?;
                    Ok((x, y, count))
                })?;
                this.region(lua, x, y, count, 1)
            },
        );
        methods.add_method("close", |_, this, ()| {
            this.data.borrow_mut().take();
            Ok(())
        });
    }
}

#[path = "image_composite.rs"]
mod composite;
#[path = "image_ops.rs"]
mod ops;

#[path = "image_color.rs"]
mod color;

#[path = "image_masks.rs"]
mod masks;
#[path = "image_notes.rs"]
pub(super) mod plans;

#[path = "image_note_masks.rs"]
pub(super) mod note_masks;

#[path = "image_mapping.rs"]
pub(super) mod mapping;

fn check_scope(lua: &Lua, job: &Arc<JobLease>, kind: &str) -> mlua::Result<()> {
    job.check().map_err(|e| e.lua())?;
    let current = lua.app_data_ref::<Session>().and_then(|s| s.lease.clone());
    if !current.is_some_and(|current| Arc::ptr_eq(&current, job)) {
        return Err(stop(
            Category::Expired,
            format!("This {kind} belongs to an earlier job; load or create it again."),
        ));
    }
    notes::check_budget(lua)
}

pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let images = lua.create_table()?;
    images.set("load",lua.create_function(|lua,file:AnyUserData| {
        let (job,budget)=context(lua)?;
        super::files::with_reader(lua,&file,|reader,length,name| {
            if length>MAX_FILE_BYTES {return Err(stop(Category::Budget,"The compressed image exceeds the 64 MiB plugin input limit."));}
            // Cover encoded storage, metadata copies and fixed parser scratch
            // before constructing the decoder, not only its final pixels.
            let header_bytes=length*2+2*1024*1024;
            let _input=budget.reserve(header_bytes as usize).map_err(|e|e.lua())?;
            let header=Rc::new(Cell::new(true));
            let exhausted=Rc::new(Cell::new(false));
            let source=DecodeSource{source:reader,header:Rc::clone(&header),remaining:header_bytes,exhausted:Rc::clone(&exhausted)};
            let mut allocation=None;
            let mut quota_error=None;
            let decoded=crate::image_decode::decode(
                BufReader::new(source),
                image::ImageFormat::from_path(name).ok(),
                crate::image_decode::Bounds {side:im::MAX_SIDE,pixels:MAX_PIXELS,allocation:super::bridge::NATIVE_PLUGIN_BYTES as u64,header_allocation:Some(header_bytes)},
                |info| {
                    if exhausted.get() {return Err("image metadata read budget exceeded".into());}
                    let pixels=u64::from(info.width)*u64::from(info.height);
                    let peak=info.decoded_bytes.checked_mul(2)
                        .and_then(|n|n.checked_add(pixels*8+2*1024*1024))
                        .and_then(|n|usize::try_from(n).ok());
                    match peak.and_then(|n|budget.reserve(n).ok()) {
                        Some(guard)=>{allocation=Some(guard);header.set(false);Ok(())},
                        None=>{quota_error=Some(stop(Category::Budget,"Image decoding exceeds the plugin/global native memory quota; choose a smaller image or release other resources."));Err("native image allocation budget exceeded".into())}
                    }
                },
                || notes::check_budget(lua).map_err(|e|super::error_text(&e)),
            );
            notes::check_budget(lua)?;
            if exhausted.get() {return Err(stop(Category::Budget,"Image metadata exceeded its bounded read allowance; choose a simpler image."));}
            if let Some(error)=quota_error {return Err(error);}
            let decoded=decoded.map_err(|e|bad(format!("decode: {e}")))?;
            store(lua,job,decoded.rgba,decoded.format,allocation.ok_or_else(||bad("The decoder did not complete image preflight."))?)
        })
    })?)?;
    images.set(
        "from_rgba",
        lua.create_function(
            |lua, (width, height, bytes): (Value, Value, mlua::LuaString)| {
                let (job, budget) = context(lua)?;
                let width = integer(width, im::MAX_SIDE, "Image width")?;
                let height = integer(height, im::MAX_SIDE, "Image height")?;
                let count = size(width, height)?;
                if bytes.as_bytes().len() != count * 4 {
                    return Err(bad("RGBA byte length must equal width × height × 4."));
                }
                let allocation = budget.reserve(count * 4).map_err(|e| e.lua())?;
                let mut pixels = Vec::new();
                pixels.try_reserve_exact(count).map_err(|_| {
                    stop(Category::Memory, "Not enough memory for this RGBA image.")
                })?;
                for (i, pixel) in bytes.as_bytes().chunks_exact(4).enumerate() {
                    if i % 65_536 == 0 {
                        notes::check_budget(lua)?;
                    }
                    pixels.push([pixel[0], pixel[1], pixel[2], pixel[3]]);
                }
                store(
                    lua,
                    job,
                    Rgba::new(width, height, pixels).map_err(bad)?,
                    "rgba",
                    allocation,
                )
            },
        )?,
    )?;
    color::install(lua, &images)?;
    composite::install(lua, &images)?;
    masks::install(lua, &images)?;
    mapping::install(lua, &images)?;
    images.set("max_pixels", MAX_PIXELS as i64)?;
    images.set("max_file_bytes", MAX_FILE_BYTES as i64)?;
    images.set("max_region_bytes", MAX_REGION_BYTES as i64)?;
    images.set("max_handles", MAX_HANDLES)?;
    images.set("max_cache_entries", MAX_CACHE_ENTRIES)?;
    images.set("max_regions", masks::MAX_REGIONS)?;
    images.set("max_note_plans", plans::MAX_PLANS)?;
    images.set("max_note_samplers", note_masks::MAX_SAMPLERS)?;
    images.set("max_note_sample_batch", note_masks::MAX_BATCH)?;
    images.set("max_note_plan_batch", plans::MAX_BATCH)?;
    images.set("max_note_plan_work", plans::MAX_WORK)?;
    plum.set("images", images)
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
