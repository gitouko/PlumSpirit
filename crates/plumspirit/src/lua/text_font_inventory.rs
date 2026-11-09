//! Explicit installed-font authority; private paths expire with the invoking job.
use super::*;
use crate::lua::{Capability, Host, handles::Granted};
use crate::ui::fonts::bounded as native;

const MAX_CATALOGS: usize = 4;
const MAX_LOADED: usize = 256 * 1024 * 1024;
const CATALOG_HANDLE: usize = 1024;
type CatalogData = Rc<RefCell<Option<CatalogState>>>;
#[derive(Default)]
struct Slot {
    states: Vec<Weak<RefCell<Option<CatalogState>>>>,
    loaded: usize,
    pages: usize,
}
struct CatalogState {
    inventory: native::Inventory,
    pager: Option<native::paged::Pager>,
    _allocation: Reservation,
}
struct Catalog {
    job: std::sync::Weak<JobLease>,
    data: CatalogData,
    _handle: Reservation,
}
const _: () = assert!(
    CATALOG_HANDLE
        >= std::mem::size_of::<JobLease>()
            + std::mem::size_of::<RefCell<Option<CatalogState>>>()
            + 64
);
pub(super) fn require(lua: &Lua) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, Capability::ListFonts)
}
pub(super) fn end_run(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        for data in slot.states.drain(..).filter_map(|w| w.upgrade()) {
            data.borrow_mut().take();
        }
        slot.loaded = 0;
        slot.pages = 0;
    }
}
fn reserve_page(lua: &Lua) -> mlua::Result<()> {
    let mut slot = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| bad("Font inventory scope unavailable."))?;
    if slot.pages >= native::paged::MAX_PAGES {
        return Err(stop(
            Category::Budget,
            "Installed font paging exceeds64page attempts per job.",
        ));
    }
    slot.pages += 1;
    Ok(())
}
fn options(t: Option<Table>) -> mlua::Result<(String, usize, bool)> {
    let mut query = String::new();
    let mut cap = native::MAX_FACES;
    let mut paged = false;
    if let Some(t) = t {
        if t.metatable().is_some() {
            return Err(bad("Font inventory options must be plain tables."));
        }
        for p in t.clone().pairs::<Value, Value>() {
            let (k, _) = p?;
            if !matches!(k,Value::String(ref s) if s.as_bytes().len()<=9 && ["query","max_faces","paged"].contains(&s.to_str()?.as_ref()))
            {
                return Err(bad("Unknown font inventory option."));
            }
        }
        match t.raw_get::<Value>("query")? {
            Value::Nil => {}
            Value::String(s) if s.as_bytes().len() <= 128 => query = s.to_str()?.to_string(),
            _ => return Err(bad("Font query must be UTF-8 text of at most128bytes.")),
        }
        match t.raw_get::<Value>("max_faces")? {
            Value::Nil => {}
            Value::Integer(n) if (1..=native::MAX_FACES as i64).contains(&n) => cap = n as usize,
            _ => {
                return Err(stop(
                    Category::Budget,
                    "Inventory max_faces must be an integer1–512.",
                ));
            }
        }
        match t.raw_get::<Value>("paged")? {
            Value::Nil => {}
            Value::Boolean(v) => paged = v,
            _ => return Err(bad("Font inventory paged must be a boolean.")),
        }
    }
    Ok((query, cap, paged))
}
fn page_error(error: native::paged::Error) -> mlua::Error {
    match error {
        native::paged::Error::Stopped => stop(Category::Canceled, "Font catalog paging canceled."),
        native::paged::Error::Budget => stop(
            Category::Budget,
            "Font catalog reached its page or per-face I/O/name-work budget.",
        ),
        native::paged::Error::Changed => stop(
            Category::Expired,
            "The font collection changed between pages; create a new catalog.",
        ),
        native::paged::Error::Memory => stop(
            Category::Memory,
            "Cannot allocate bounded font catalog data.",
        ),
    }
}
fn retained(
    inventory: &native::Inventory,
    pager: Option<&native::paged::Pager>,
) -> mlua::Result<usize> {
    let bytes = inventory
        .retained_bytes()
        .checked_add(pager.map_or(0, |p| p.retained_bytes()))
        .ok_or_else(|| page_error(native::paged::Error::Budget))?;
    if bytes > native::WORKSPACE {
        return Err(page_error(native::paged::Error::Budget));
    }
    Ok(bytes)
}
fn create_controlled(
    lua: &Lua,
    opts: Option<Table>,
    roots: Vec<std::path::PathBuf>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Catalog> {
    require(lua)?;
    check()?;
    let (query, cap, paged) = options(opts)?;
    let job = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| stop(Category::Expired, "No font inventory job is active."))?;
    font_scope(lua, &job)?;
    if lua
        .app_data_ref::<Slot>()
        .is_none_or(|s| s.states.len() >= MAX_CATALOGS)
    {
        return Err(stop(
            Category::Budget,
            "At most4font catalogs may be created per job, including closed catalogs.",
        ));
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Font accounting unavailable."))?
        .clone();
    let mut allocation = budget.reserve(native::WORKSPACE).map_err(|e| e.lua())?;
    if paged {
        reserve_page(lua)?;
    }
    let mut failure = None;
    let mut guarded = || match check() {
        Ok(()) => true,
        Err(e) => {
            failure = Some(e);
            false
        }
    };
    let result = if paged {
        native::paged::Pager::new(roots, &query, cap, &mut guarded).and_then(|mut pager| {
            let inventory = pager.next(&mut guarded)?;
            Ok((inventory, Some(pager)))
        })
    } else {
        native::collect(roots, &query, cap, &mut guarded)
            .map(|i| (i, None))
            .map_err(|_| native::paged::Error::Stopped)
    };
    if let Some(e) = failure {
        return Err(e);
    }
    let (inventory, pager) = result.map_err(page_error)?;
    check()?;
    require(lua)?;
    allocation.shrink_to(retained(&inventory, pager.as_ref())?);
    let handle = budget.reserve(CATALOG_HANDLE).map_err(|e| e.lua())?;
    let data = Rc::new(RefCell::new(Some(CatalogState {
        inventory,
        pager,
        _allocation: allocation,
    })));
    lua.app_data_mut::<Slot>()
        .ok_or_else(|| bad("Font inventory scope unavailable."))?
        .states
        .push(Rc::downgrade(&data));
    Ok(Catalog {
        job: Arc::downgrade(&job),
        data,
        _handle: handle,
    })
}
fn next_controlled(
    lua: &Lua,
    catalog: &Catalog,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<bool> {
    require(lua)?;
    check()?;
    let job = catalog.job.upgrade().ok_or_else(|| {
        stop(
            Category::Expired,
            "This font catalog belongs to an earlier job.",
        )
    })?;
    font_scope(lua, &job)?;
    {
        let data = catalog.data.borrow();
        let state = data
            .as_ref()
            .ok_or_else(|| stop(Category::Expired, "This font catalog is closed."))?;
        let pager = state.pager.as_ref().ok_or_else(|| {
            bad("Create installed_fonts{paged=true} before requesting another page.")
        })?;
        if !pager.has_more() {
            return Ok(false);
        }
        if pager.pages() >= native::paged::MAX_PAGES {
            return Err(page_error(native::paged::Error::Budget));
        }
    }
    // Preflight before advancing: denial preserves the current page and cursor.
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Font accounting unavailable."))?
        .clone();
    let mut allocation = budget.reserve(native::WORKSPACE).map_err(|e| e.lua())?;
    reserve_page(lua)?;
    let mut data = catalog.data.borrow_mut();
    let result = (|| {
        let state = data
            .as_mut()
            .ok_or_else(|| stop(Category::Expired, "This font catalog is closed."))?;
        let pager = state
            .pager
            .as_mut()
            .ok_or_else(|| bad("This font catalog is not paged."))?;
        let mut failure = None;
        let result = pager.next(&mut || match check() {
            Ok(()) => true,
            Err(e) => {
                failure = Some(e);
                false
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let inventory = result.map_err(page_error)?;
        check()?;
        font_scope(lua, &job)?;
        require(lua)?;
        if inventory.faces.is_empty() && !inventory.has_more {
            state.inventory.has_more = false;
            state.inventory.scan_truncated |= inventory.scan_truncated;
            state.inventory.truncated = state.inventory.scan_truncated;
            return Ok(false);
        }
        allocation.shrink_to(retained(&inventory, Some(pager))?);
        state.inventory = inventory;
        state._allocation = allocation;
        Ok(true)
    })();
    // Once native continuation started, a failure cannot expose a half-advanced page.
    if result.is_err() {
        data.take();
    }
    result
}
impl Catalog {
    fn with<T>(
        &self,
        lua: &Lua,
        f: impl FnOnce(&native::Inventory) -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        require(lua)?;
        let job = self.job.upgrade().ok_or_else(|| {
            stop(
                Category::Expired,
                "This font catalog belongs to an earlier job.",
            )
        })?;
        font_scope(lua, &job)?;
        let data = self.data.borrow();
        let state = data
            .as_ref()
            .ok_or_else(|| stop(Category::Expired, "This font catalog is closed."))?;
        let result = f(&state.inventory)?;
        font_scope(lua, &job)?;
        require(lua)?;
        Ok(result)
    }
}
fn face_index(value: Value, inventory: &native::Inventory) -> mlua::Result<usize> {
    match value {
        Value::Integer(n) if n >= 1 && n as usize <= inventory.faces.len() => Ok(n as usize - 1),
        _ => Err(bad(
            "Catalog face index must be an in-range1-based Lua integer.",
        )),
    }
}
fn load_controlled(
    lua: &Lua,
    catalog: &Catalog,
    value: Value,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Font> {
    check()?;
    catalog.with(lua, |inventory| {
        let face = &inventory.faces[face_index(value, inventory)?];
        let (job, budget) = context(lua)?;
        let mut file = native::open_checked(face, &inventory.roots).map_err(|_| {
            bad("Installed font is unavailable or changed; rescan and choose the face again.")
        })?;
        let length = usize::try_from(face.length)
            .map_err(|_| stop(Category::Budget, "Installed font exceeds64MiB."))?;
        if length == 0 || length > MAX_BYTES {
            return Err(stop(
                Category::Budget,
                "Installed font input must contain1byte–64MiB.",
            ));
        }
        {
            let mut slot = lua
                .app_data_mut::<Slot>()
                .ok_or_else(|| bad("Font inventory scope unavailable."))?;
            if slot.loaded.saturating_add(length) > MAX_LOADED {
                return Err(stop(
                    Category::Budget,
                    "Installed font loads exceed256MiB per job.",
                ));
            }
            slot.loaded += length;
        }
        let allocation = budget
            .reserve(length * 2 + 1024 * 1024)
            .map_err(|e| e.lua())?;
        let bytes = read_bytes(&mut file, length, &mut check)?;
        if !file
            .metadata()
            .ok()
            .is_some_and(|m| native::same_file(face, &m))
        {
            return Err(bad(
                "Installed font changed during the read; rescan and choose the face again.",
            ));
        }
        let mut failure = None;
        let matched = native::snapshot_matches(&bytes, face, &mut || match check() {
            Ok(()) => true,
            Err(e) => {
                failure = Some(e);
                false
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        if !matched {
            return Err(bad(
                "Installed font identity changed; rescan and choose the face again.",
            ));
        }
        check()?;
        store(
            lua,
            job,
            bytes,
            face.names.index,
            "installed",
            format!(
                "{} {}",
                face.names.effective_family(),
                face.names.effective_style()
            ),
            allocation,
        )
    })
}
impl UserData for Catalog {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("info", |lua, this, ()| {
            this.with(lua, |i| {
                let t = lua.create_table()?;
                t.set("count", i.faces.len())?;
                t.set("truncated", i.truncated)?;
                t.set("scanned_files", i.files)?;
                t.set("scanned_entries", i.entries)?;
                t.set("read_bytes", i.read_bytes)?;
                t.set("paged", i.paged)?;
                t.set("page", i.page)?;
                t.set("has_more", i.has_more)?;
                t.set("scan_truncated", i.scan_truncated)?;
                Ok(t)
            })
        });
        m.add_method("face", |lua, this, value: Value| {
            this.with(lua, |inventory| {
                let index = face_index(value, inventory)?;
                let face = &inventory.faces[index];
                let t = lua.create_table()?;
                t.set("index", index + 1)?;
                t.set("page", inventory.page)?;
                t.set("face_index", face.names.index)?;
                t.set("family", face.names.effective_family())?;
                t.set("style", face.names.effective_style())?;
                t.set("weight", face.weight)?;
                t.set("italic", face.italic)?;
                t.set("bytes", face.length)?;
                Ok(t)
            })
        });
        m.add_method("load", |lua, this, value: Value| {
            load_controlled(lua, this, value, || notes::check_budget(lua))
        });
        m.add_method("next_page", |lua, this, ()| {
            next_controlled(lua, this, || notes::check_budget(lua))
        });
        m.add_method("close", |_, this, ()| {
            Ok(this.data.borrow_mut().take().is_some())
        });
    }
}
pub(super) fn install(lua: &Lua, t: &Table) -> mlua::Result<()> {
    lua.set_app_data(Slot::default());
    t.set("max_font_catalogs", MAX_CATALOGS)?;
    t.set("max_installed_faces", native::MAX_FACES)?;
    t.set("max_font_pages", native::paged::MAX_PAGES)?;
    t.set("max_installed_font_bytes", MAX_LOADED)?;
    t.set(
        "installed_fonts",
        lua.create_function(|lua, opts: Option<Table>| {
            require(lua)?;
            create_controlled(lua, opts, native::host_roots(), || notes::check_budget(lua))
        })?,
    )
}
#[cfg(test)]
#[path = "text_font_inventory_tests.rs"]
mod tests;
