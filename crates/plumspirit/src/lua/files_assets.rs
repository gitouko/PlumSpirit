//! Read-only package authority reusing the selected-file implementation and fences.
use super::*;
use crate::lua::bridge::Reservation;

const MAX_ATTEMPTS: usize = 32;
const MAX_PATH_BYTES: usize = 1024;
// Covers owned root/identity strings, reference cells and at most66pinned handles.
const METADATA_BYTES: usize = 256 * 1024;

pub(super) struct Scope {
    plugin: String,
    root: PathBuf,
    _native: Reservation,
}
impl Scope {
    pub(super) fn check(&self, lua: &Lua) -> mlua::Result<()> {
        let same = lua
            .app_data_ref::<crate::lua::plugin_api::PluginSlot>()
            .is_some_and(|s| s.meta.id == self.plugin && s.meta.root == self.root);
        if !same {
            return Err(stop(
                Category::Expired,
                "This asset belongs to another plugin package; open it again.",
            ));
        }
        Ok(())
    }
}
fn single_link(file: &File) -> mlua::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: own live borrowed file handle and correctly sized output storage.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io_error(
                "Identify package asset",
                std::io::Error::last_os_error(),
            ));
        }
        if unsafe { info.assume_init() }.nNumberOfLinks != 1 {
            return Err(bad("Package assets must not be hard links."));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file
            .metadata()
            .map_err(|e| io_error("Identify package asset", e))?
            .nlink()
            != 1
        {
            return Err(bad("Package assets must not be hard links."));
        }
    }
    #[cfg(not(any(windows, unix)))]
    return Err(bad(
        "Package asset identity checks are unavailable on this platform.",
    ));
    Ok(())
}
fn open(lua: &Lua, path: Value) -> mlua::Result<LuaFile> {
    let job = lua
        .app_data_ref::<FileSlot>()
        .and_then(|s| s.0.clone())
        .ok_or_else(|| stop(Category::Expired, "No package asset job is running."))?;
    job.lease.check().map_err(|e| e.lua())?;
    crate::lua::notes::check_budget(lua)?;
    if job.asset_attempts.get() >= MAX_ATTEMPTS {
        return Err(stop(
            Category::Budget,
            "At most32package asset opens may be attempted per job, including closed or failed opens.",
        ));
    }
    job.asset_attempts.set(job.asset_attempts.get() + 1);
    let Value::String(path) = path else {
        return Err(bad("A package asset needs a relative UTF-8 path string."));
    };
    if path.as_bytes().len() > MAX_PATH_BYTES {
        return Err(stop(
            Category::Budget,
            "Package asset path exceeds1024bytes.",
        ));
    }
    let rel = crate::lua::package::safe_relative(&path.to_str()?).map_err(bad)?;
    let native = job.native.reserve(METADATA_BYTES).map_err(|e| e.lua())?;
    let (plugin, root) = lua
        .app_data_ref::<crate::lua::plugin_api::PluginSlot>()
        .map(|s| (s.meta.id.clone(), s.meta.root.clone()))
        .ok_or_else(|| bad("Package assets are available only to plugins."))?;
    let mut file = chosen(lua, root.join(rel), false)?;
    {
        let state = file.state.borrow();
        if state.length > crate::lua::package::MAX_FILE_BYTES {
            return Err(stop(Category::Budget, "Package asset exceeds16MiB."));
        }
        single_link(state.file.as_ref().unwrap())?;
    }
    job.lease.check().map_err(|e| e.lua())?;
    crate::lua::notes::check_budget(lua)?;
    file.asset = Some(Rc::new(Scope {
        plugin,
        root,
        _native: native,
    }));
    Ok(file)
}
pub(super) fn install(lua: &Lua, plugin: &Table) -> mlua::Result<()> {
    plugin.set("max_asset_bytes", crate::lua::package::MAX_FILE_BYTES)?;
    plugin.set("max_asset_opens", MAX_ATTEMPTS)?;
    plugin.set("asset", lua.create_function(open)?)
}

#[cfg(test)]
#[path = "files_asset_tests.rs"]
mod tests;
