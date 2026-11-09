//! Worker-prepared pixels with explicit job/project lifetimes. Only native code
//! owns texture handles; promotion shares approved data without extending files.
use super::*;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use mlua::AnyUserData;
use plumspirit_engine::transform::image_midi;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;

pub const MAX_IMAGES: usize = 16;
pub const MAX_SIDE: usize = 1024;
pub const MAX_JOB_PIXELS: usize = 4 * 1024 * 1024;
pub const MAX_UPLOAD_PIXELS: usize = 1024 * 1024;
const METADATA_BYTES: usize = 2048;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
struct ImageOwner {
    active: std::sync::atomic::AtomicBool,
    project: AtomicU64,
    release_epoch: Arc<AtomicU64>,
}
enum Lifetime {
    Job(Weak<JobLease>),
    Project {
        owner: Arc<ImageOwner>,
        project: u64,
        epoch: u64,
    },
}
impl Lifetime {
    fn job(&self) -> Option<Arc<JobLease>> {
        match self {
            Self::Job(lease) => lease.upgrade(),
            _ => None,
        }
    }
    fn active(&self) -> bool {
        match self {
            Self::Job(_) => self.job().is_some_and(|job| job.check().is_ok()),
            Self::Project {
                owner,
                project,
                epoch,
            } => {
                owner.active.load(Ordering::Acquire)
                    && *epoch != u64::MAX
                    && owner.project.load(Ordering::Acquire) == *project
                    && owner.release_epoch.load(Ordering::Acquire) == *epoch
            }
        }
    }
}
pub(crate) struct RetainedSlot {
    owner: Arc<ImageOwner>,
    project: Option<u64>,
    resources: Vec<Weak<Resource>>,
    created: Vec<Weak<Resource>>,
    attempts: usize,
    next_run_epoch: Option<u64>,
    run_epoch: u64,
}
impl Default for RetainedSlot {
    fn default() -> Self {
        Self::with_epoch(Arc::new(AtomicU64::new(0)))
    }
}
impl RetainedSlot {
    fn with_epoch(release_epoch: Arc<AtomicU64>) -> Self {
        Self {
            owner: Arc::new(ImageOwner {
                active: std::sync::atomic::AtomicBool::new(false),
                project: AtomicU64::new(0),
                release_epoch,
            }),
            project: None,
            resources: Vec::new(),
            created: Vec::new(),
            attempts: 0,
            next_run_epoch: None,
            run_epoch: 0,
        }
    }
}
pub(crate) fn initialize_release_epoch(lua: &Lua, epoch: Arc<AtomicU64>) {
    lua.set_app_data(RetainedSlot::with_epoch(epoch));
}
pub(crate) fn set_run_epoch(lua: &Lua, epoch: u64) {
    if let Some(mut s) = lua.app_data_mut::<RetainedSlot>() {
        s.next_run_epoch = Some(epoch);
    }
}
pub(crate) fn retained_run_epoch(lua: &Lua) -> Option<u64> {
    lua.app_data_ref::<RetainedSlot>()
        .filter(|slot| slot.run_active())
        .map(|slot| slot.run_epoch)
}
impl RetainedSlot {
    fn run_active(&self) -> bool {
        self.run_epoch != u64::MAX
            && self.run_epoch == self.owner.release_epoch.load(Ordering::Acquire)
    }
    fn release(&mut self) {
        self.owner.active.store(false, Ordering::Release);
        for resource in self.resources.drain(..).filter_map(|r| r.upgrade()) {
            resource.close();
        }
        self.created.clear();
        self.project = None;
        self.attempts = 0;
    }
    fn prune(&mut self) {
        self.resources.retain(|r| {
            r.upgrade()
                .is_some_and(|r| r.data.lock().is_ok_and(|d| d.is_some()))
        });
    }
}
impl Drop for RetainedSlot {
    fn drop(&mut self) {
        self.release();
    }
}
pub(crate) fn release_retained(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<RetainedSlot>() {
        slot.release();
    }
}
pub(crate) fn begin_retained_run(lua: &Lua) {
    let project = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.as_ref().and_then(|l| l.project.map(|p| p.serial)));
    if let Some(mut slot) = lua.app_data_mut::<RetainedSlot>() {
        slot.created.clear();
        slot.attempts = 0;
        slot.run_epoch = slot
            .next_run_epoch
            .take()
            .unwrap_or_else(|| slot.owner.release_epoch.load(Ordering::Acquire));
        if !slot.run_active() {
            return;
        }
        if let Some(project) = project
            && slot.project != Some(project)
        {
            slot.release();
            slot.project = Some(project);
            slot.owner.project.store(project, Ordering::Release);
            slot.owner.active.store(true, Ordering::Release);
        }
        slot.prune();
    }
}
pub(crate) fn end_retained_run(lua: &Lua, ok: bool) {
    let ok = ok
        && lua
            .app_data_ref::<Session>()
            .and_then(|s| s.lease.clone())
            .is_some_and(|lease| lease.check().is_ok());
    if let Some(mut slot) = lua.app_data_mut::<RetainedSlot>() {
        let ok = ok && slot.run_active();
        for resource in slot.created.drain(..).filter_map(|r| r.upgrade()) {
            if !ok {
                resource.close();
            }
        }
        slot.prune();
    }
}
fn require_image(lua: &Lua) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<super::super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    if granted.contains(&Capability::UiPanels) || granted.contains(&Capability::UiOverlay) {
        Ok(())
    } else {
        Host::require(&granted, Capability::UiPanels)
    }
}

struct Pixels {
    image: Arc<ColorImage>,
    _allocation: Reservation,
}
struct Data {
    pixels: Arc<Pixels>,
    texture: Option<TextureHandle>,
}
pub(crate) struct Resource {
    pub id: u64,
    lifetime: Lifetime,
    fault: Arc<AtomicU8>,
    width: usize,
    height: usize,
    source: [u32; 2],
    nearest: bool,
    data: Mutex<Option<Data>>,
    _allocation: Reservation,
    #[cfg(test)]
    uploads: AtomicUsize,
}
#[derive(Clone)]
pub(crate) struct Prepared {
    pub texture: TextureHandle,
    // Keep native accounting alive until every frontend using this texture drops it.
    _pixels: Arc<Pixels>,
}
impl Resource {
    pub(crate) fn retained(&self) -> bool {
        matches!(self.lifetime, Lifetime::Project { .. })
    }
    pub fn alive(&self) -> bool {
        self.lifetime.active() && self.data.try_lock().ok().is_some_and(|s| s.is_some())
    }
    fn close(&self) -> bool {
        self.data.lock().ok().and_then(|mut s| s.take()).is_some()
    }
    /// No conversion/callback happens here. One native texture is shared by views.
    pub(crate) fn prepare(&self, ctx: &egui::Context, remaining: &mut usize) -> Option<Prepared> {
        if !self.lifetime.active() {
            return None;
        }
        let mut data = self.data.try_lock().ok()?;
        let data = data.as_mut()?;
        if data.texture.is_none() {
            let count = self.width * self.height;
            if count > *remaining {
                ctx.request_repaint();
                return None;
            }
            let limit = ctx.input(|i| i.max_texture_side);
            if self.width > limit || self.height > limit {
                let _ = self.fault.compare_exchange(
                    0,
                    IMAGE_LIMIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if let Some(lease) = self.lifetime.job() {
                    lease.cancel_from_surface();
                }
                return None;
            }
            *remaining -= count;
            data.texture = Some(ctx.load_texture(
                format!("plum-lua-image-{}", self.id),
                Arc::clone(&data.pixels.image),
                if self.nearest {
                    TextureOptions::NEAREST
                } else {
                    TextureOptions::LINEAR
                },
            ));
            #[cfg(test)]
            self.uploads.fetch_add(1, Ordering::Relaxed);
        }
        Some(Prepared {
            texture: data.texture.as_ref()?.clone(),
            _pixels: Arc::clone(&data.pixels),
        })
    }
    fn check(&self, lua: &Lua) -> mlua::Result<Arc<JobLease>> {
        require_image(lua)?;
        notes::check_budget(lua)?;
        let lease = lua
            .app_data_ref::<Session>()
            .and_then(|s| s.lease.clone())
            .ok_or_else(|| error(EXPIRED))?;
        match &self.lifetime {
            Lifetime::Job(job) => {
                if !job.upgrade().is_some_and(|job| Arc::ptr_eq(&job, &lease)) {
                    return Err(error(EXPIRED));
                }
            }
            Lifetime::Project { owner, project, .. } => {
                let granted = lua
                    .app_data_ref::<super::super::handles::Granted>()
                    .map(|g| Arc::clone(&g.0))
                    .unwrap_or_default();
                Host::require(&granted, Capability::UiOverlay)?;
                if !self.lifetime.active()
                    || lease.project.is_none_or(|p| p.serial != *project)
                    || !lua
                        .app_data_ref::<RetainedSlot>()
                        .is_some_and(|slot| Arc::ptr_eq(&slot.owner, owner))
                {
                    return Err(error(EXPIRED));
                }
            }
        }
        lease.check().map_err(|e| e.lua())?;
        let fault = self.fault.load(Ordering::Acquire);
        if fault != 0 {
            return Err(error(fault));
        }
        if self.data.lock().map_err(|_| error(EXPIRED))?.is_none() {
            return Err(error(EXPIRED));
        }
        Ok(lease)
    }
}
struct UiImage(Arc<Resource>);
impl UserData for UiImage {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("info", |lua, this, ()| {
            this.0.check(lua)?;
            let out = lua.create_table()?;
            out.set("width", this.0.width)?;
            out.set("height", this.0.height)?;
            out.set("source_width", this.0.source[0])?;
            out.set("source_height", this.0.source[1])?;
            out.set("bytes", this.0.width * this.0.height * 4)?;
            out.set("filter", if this.0.nearest { "nearest" } else { "linear" })?;
            out.set("origin", "top_left")?;
            if matches!(this.0.lifetime, Lifetime::Project { .. }) {
                out.set("lifetime", "project")?;
            }
            Ok(out)
        });
        m.add_method("close", |_, this, ()| Ok(this.0.close()));
    }
}
pub(super) fn reference(lua: &Lua, value: Value) -> mlua::Result<Arc<Resource>> {
    let Value::UserData(u) = value else {
        return Err(stop(
            Category::Runtime,
            "Canvas image needs an opaque UI image resource.",
        ));
    };
    let image = u.borrow::<UiImage>().map_err(|_| {
        stop(
            Category::Runtime,
            "Canvas image needs a UI image resource made by plum.ui.image.",
        )
    })?;
    image.0.check(lua)?;
    Ok(Arc::clone(&image.0))
}
pub(super) fn release(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        for image in slot.images.drain(..).filter_map(|s| s.upgrade()) {
            image.close();
        }
        slot.image_pixels = 0;
    }
}
#[cfg(test)]
struct TestCheckpoint {
    hits: Arc<AtomicUsize>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    at: usize,
}
fn checkpoint(lua: &Lua) -> mlua::Result<()> {
    #[cfg(test)]
    if let Some(h) = lua.app_data_ref::<TestCheckpoint>()
        && h.hits.fetch_add(1, Ordering::Relaxed) + 1 == h.at
    {
        h.cancel.store(true, Ordering::SeqCst);
    }
    notes::check_budget(lua)
}
#[cfg(test)]
impl Resource {
    pub(crate) fn test_close(&self) {
        self.close();
    }
    pub(crate) fn test_uploads(&self) -> usize {
        self.uploads.load(Ordering::Relaxed)
    }
}
fn create(lua: &Lua, (source, options): (AnyUserData, Option<Table>)) -> mlua::Result<UiImage> {
    require_image(lua)?;
    notes::check_budget(lua)?;
    let lease = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| error(EXPIRED))?;
    lease.check().map_err(|e| e.lua())?;
    if !lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "UI images require an explicitly invoked plugin command or tool.",
        ));
    }
    let (side, nearest) = if let Some(t) = options {
        schema::fields(&t, &["max_side", "filter"])?;
        let side = match t.raw_get::<Value>("max_side")? {
            Value::Nil => 512,
            Value::Integer(n) if (1..=MAX_SIDE as i64).contains(&n) => n as usize,
            _ => {
                return Err(stop(
                    Category::Runtime,
                    "UI image max_side must be an integer1–1024.",
                ));
            }
        };
        let nearest = match t.raw_get::<Value>("filter")? {
            Value::Nil => false,
            Value::String(s) if s.as_bytes().as_ref() == b"linear" => false,
            Value::String(s) if s.as_bytes().as_ref() == b"nearest" => true,
            _ => {
                return Err(stop(
                    Category::Runtime,
                    "UI image filter must be 'nearest' or 'linear'.",
                ));
            }
        };
        (side, nearest)
    } else {
        (512, false)
    };
    let source = source
        .borrow::<super::super::images::LuaImage>()
        .map_err(|_| {
            stop(
                Category::Runtime,
                "UI image source must be an existing Image resource.",
            )
        })?;
    source.with_pixels(lua, |source| {
        let largest = (source.w as usize).max(source.h as usize);
        let dimensions = if largest <= side {
            [source.w as usize, source.h as usize]
        } else {
            [
                (source.w as usize * side / largest).max(1),
                (source.h as usize * side / largest).max(1),
            ]
        };
        let count = dimensions[0] * dimensions[1];
        {
            let mut slot = lua.app_data_mut::<Slot>().ok_or_else(|| error(EXPIRED))?;
            if slot.images.len() >= MAX_IMAGES
                || slot.image_pixels.saturating_add(count) > MAX_JOB_PIXELS
            {
                return Err(stop(
                    Category::Budget,
                    "UI image job exceeds16resources or4,194,304converted pixels.",
                ));
            }
            // Charge attempts before native work, including failed/canceled conversions.
            slot.image_pixels += count;
        }
        let budget = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| error(EXPIRED))?
            .clone();
        let metadata = budget.reserve(METADATA_BYTES).map_err(|e| e.lua())?;
        let mut allocation = None;
        let mut fault = None;
        let rgba = image_midi::box_resize_controlled(
            source,
            dimensions[0] as u32,
            dimensions[1] as u32,
            |_, _, scratch| match budget.reserve(count * 20 + scratch) {
                Ok(a) => {
                    allocation = Some(a);
                    Ok(())
                }
                Err(e) => {
                    fault = Some(e);
                    Err("UI image allocation preflight failed.".into())
                }
            },
            || checkpoint(lua).map_err(|e| super::super::error_text(&e)),
        );
        notes::check_budget(lua)?;
        if let Some(e) = fault {
            return Err(e.lua());
        }
        let rgba = rgba.map_err(|e| {
            stop(
                if e.starts_with("memory:") {
                    Category::Memory
                } else {
                    Category::Runtime
                },
                e,
            )
        })?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|_| stop(Category::Memory, "Cannot allocate UI image pixels."))?;
        for row in rgba.px.chunks(dimensions[0]) {
            notes::check_budget(lua)?;
            pixels.extend(
                row.iter()
                    .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])),
            );
        }
        drop(rgba);
        let mut allocation = allocation.ok_or_else(|| error(OVERFLOW))?;
        // Conservative CPU/texture/in-flight copy allowance; driver overhead is bounded
        // by texture/side/upload caps rather than claimed as an exact GPU byte count.
        allocation.shrink_to(count * 16);
        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| error(OVERFLOW))?;
        lease.check().map_err(|e| e.lua())?;
        let resource = Arc::new(Resource {
            id,
            lifetime: Lifetime::Job(Arc::downgrade(&lease)),
            fault: Arc::clone(&lua.app_data_ref::<Slot>().unwrap().fault),
            width: dimensions[0],
            height: dimensions[1],
            source: [source.w, source.h],
            nearest,
            data: Mutex::new(Some(Data {
                pixels: Arc::new(Pixels {
                    image: Arc::new(ColorImage::new(dimensions, pixels)),
                    _allocation: allocation,
                }),
                texture: None,
            })),
            _allocation: metadata,
            #[cfg(test)]
            uploads: AtomicUsize::new(0),
        });
        lua.app_data_mut::<Slot>()
            .unwrap()
            .images
            .push(Arc::downgrade(&resource));
        Ok(UiImage(resource))
    })
}
pub(super) fn install(lua: &Lua, t: &Table) -> mlua::Result<()> {
    t.set("image", lua.create_function(create)?)?;
    t.set("retain_image", lua.create_function(retain)?)?;
    for (name, n) in [
        ("max_ui_images", MAX_IMAGES),
        ("max_ui_image_side", MAX_SIDE),
        ("max_ui_image_pixels", MAX_JOB_PIXELS),
        ("max_ui_upload_pixels", MAX_UPLOAD_PIXELS),
        ("max_retained_ui_images", MAX_IMAGES),
        ("max_retained_ui_pixels", MAX_JOB_PIXELS),
    ] {
        t.set(name, n)?;
    }
    Ok(())
}

fn retain(lua: &Lua, source: AnyUserData) -> mlua::Result<UiImage> {
    super::super::reactive::check_read(lua)?;
    let granted = lua
        .app_data_ref::<super::super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, Capability::UiOverlay)?;
    let source = source.borrow::<UiImage>().map_err(|_| {
        stop(
            Category::Runtime,
            "Retain an opaque UI image made by plum.ui.image.",
        )
    })?;
    let lease = source.0.check(lua)?;
    if !lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "Retaining UI images requires an explicitly invoked plugin command or tool.",
        ));
    }
    let project = lease.project.map(|p| p.serial).ok_or_else(|| {
        stop(
            Category::Runtime,
            "Retained UI images require an open project.",
        )
    })?;
    let (pixels, texture) = source
        .0
        .data
        .lock()
        .map_err(|_| error(EXPIRED))?
        .as_ref()
        .map(|d| (Arc::clone(&d.pixels), d.texture.clone()))
        .ok_or_else(|| error(EXPIRED))?;
    let mut slot = lua
        .app_data_mut::<RetainedSlot>()
        .ok_or_else(|| error(EXPIRED))?;
    if slot.project != Some(project) {
        return Err(error(EXPIRED));
    }
    if !slot.run_active() {
        return Err(error(EXPIRED));
    }
    if slot.attempts >= MAX_IMAGES {
        return Err(stop(
            Category::Budget,
            "At most16UI image retention attempts are allowed per job.",
        ));
    }
    slot.attempts += 1;
    slot.prune();
    let mut live = 0usize;
    let mut total = 0usize;
    for resource in slot.resources.iter().filter_map(|r| r.upgrade()) {
        let data = resource.data.lock().map_err(|_| error(EXPIRED))?;
        if let Some(data) = data.as_ref() {
            if Arc::ptr_eq(&data.pixels, &pixels) && resource.nearest == source.0.nearest {
                return Ok(UiImage(Arc::clone(&resource)));
            }
            live += 1;
            total = total.saturating_add(resource.width * resource.height);
        }
    }
    let count = source.0.width * source.0.height;
    if live >= MAX_IMAGES || total.saturating_add(count) > MAX_JOB_PIXELS {
        return Err(stop(
            Category::Budget,
            "Retained UI images exceed16live resources or4,194,304pixels in this project.",
        ));
    }
    let metadata = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| error(EXPIRED))?
        .reserve(METADATA_BYTES)
        .map_err(|e| e.lua())?;
    let id = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| error(OVERFLOW))?;
    lease.check().map_err(|e| e.lua())?;
    let resource = Arc::new(Resource {
        id,
        lifetime: Lifetime::Project {
            owner: Arc::clone(&slot.owner),
            project,
            epoch: slot.run_epoch,
        },
        fault: Arc::new(AtomicU8::new(0)),
        width: source.0.width,
        height: source.0.height,
        source: source.0.source,
        nearest: source.0.nearest,
        data: Mutex::new(Some(Data { pixels, texture })),
        _allocation: metadata,
        #[cfg(test)]
        uploads: AtomicUsize::new(0),
    });
    slot.resources.push(Arc::downgrade(&resource));
    slot.created.push(Arc::downgrade(&resource));
    Ok(UiImage(resource))
}

#[cfg(test)]
#[path = "ui_retained_image_tests.rs"]
mod retained_tests;
#[cfg(test)]
#[path = "ui_image_tests.rs"]
mod tests;
