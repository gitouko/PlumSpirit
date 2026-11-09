//! User-selected files. Paths never enter Lua. A file is usable only in the
//! invoking job; even userdata retained in a global is closed at job end.
//! Writes stage beside the chosen destination and publish only on commit.

use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::bridge::{FileDialogSpec, JobLease, NativeBudget, Session};
use super::{Capability, Category, Host, stop};

#[path = "files_assets.rs"]
mod assets;

pub const MAX_CHUNK: usize = 1024 * 1024;
pub const MAX_READ_ALL: usize = 8 * 1024 * 1024;
pub const MAX_JOB_BYTES: u64 = 1024 * 1024 * 1024;
const IO_CHECK: usize = 64 * 1024;

fn bad(message: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn io_error(action: &str, e: std::io::Error) -> mlua::Error {
    // OS messages can contain paths; expose only the stable error kind/code.
    bad(format!(
        "file_io: {action} failed ({:?}, OS code {:?}); check the chosen file's access and available disk space.",
        e.kind(),
        e.raw_os_error()
    ))
}

#[derive(Default)]
pub(super) struct FileSlot(Option<Rc<FileJob>>);

struct FileJob {
    lease: Arc<JobLease>,
    native: NativeBudget,
    read: Cell<u64>,
    written: Cell<u64>,
    asset_attempts: Cell<usize>,
    states: RefCell<Vec<Weak<RefCell<FileState>>>>,
}

pub(super) fn begin_run(lua: &Lua) {
    end_run(lua);
    let lease = lua.app_data_ref::<Session>().and_then(|s| s.lease.clone());
    let native = lua
        .app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .unwrap_or_default();
    lua.set_app_data(FileSlot(lease.map(|lease| {
        Rc::new(FileJob {
            lease,
            native,
            read: Cell::new(0),
            written: Cell::new(0),
            asset_attempts: Cell::new(0),
            states: RefCell::new(Vec::new()),
        })
    })));
}

pub(super) fn end_run(lua: &Lua) {
    if let Some(job) = lua.app_data_mut::<FileSlot>().and_then(|mut s| s.0.take()) {
        for state in job.states.borrow().iter().filter_map(Weak::upgrade) {
            state.borrow_mut().close();
        }
    }
}

fn require(lua: &Lua, capability: Capability) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, capability)
}

fn count(v: &Value, default: usize, maximum: usize, what: &str) -> mlua::Result<usize> {
    match v {
        Value::Nil => Ok(default),
        Value::Integer(n) if *n > 0 && (*n as u64) <= maximum as u64 => Ok(*n as usize),
        _ => Err(bad(format!(
            "{what} must be an integer from 1 to {maximum}."
        ))),
    }
}

fn account(counter: &Cell<u64>, n: usize) -> mlua::Result<()> {
    let total = counter
        .get()
        .checked_add(n as u64)
        .filter(|n| *n <= MAX_JOB_BYTES)
        .ok_or_else(|| {
            stop(
                Category::Budget,
                "File I/O exceeds the 1 GiB cumulative per-job byte budget.",
            )
        })?;
    counter.set(total);
    Ok(())
}

fn valid_name(name: &str) -> bool {
    if name.is_empty()
        || name.chars().count() > 240
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
    {
        return false;
    }
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    if let Some(suffix) = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"))
        && matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    {
        return false;
    }
    name != "." && name != ".."
}

fn bounded_text(value: Value, max_bytes: usize) -> mlua::Result<String> {
    let Value::String(text) = value else {
        return Err(bad("A file dialog text field must be a UTF-8 string."));
    };
    if text.as_bytes().len() > max_bytes {
        return Err(bad("A file dialog text field exceeds its size limit."));
    }
    Ok(text.to_str()?.to_owned())
}

fn dialog_spec(options: Option<Table>, save: bool) -> mlua::Result<FileDialogSpec> {
    let mut spec = FileDialogSpec {
        save,
        filters: Vec::new(),
        suggested_name: None,
    };
    let Some(options) = options else {
        return Ok(spec);
    };
    for pair in options.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if !matches!(key, Value::String(s) if s.as_bytes().as_ref() == b"filters" || s.as_bytes().as_ref() == b"name")
        {
            return Err(bad(
                "Unknown file dialog option; use only 'filters' and a suggested save 'name'.",
            ));
        }
    }
    if let value @ Value::String(_) = options.get::<Value>("name")? {
        let name = bounded_text(value, 960)?;
        if !save || !valid_name(&name) {
            return Err(bad(
                "A suggested save name must be a safe basename, without directories or device names.",
            ));
        }
        spec.suggested_name = Some(name);
    } else if !options.get::<Value>("name")?.is_nil() {
        return Err(bad("A suggested name must be a UTF-8 string."));
    }
    if let Some(filters) = options.get::<Option<Table>>("filters")? {
        if filters.raw_len() > 8 {
            return Err(bad("A dialog accepts at most eight filters."));
        }
        for filter in filters.sequence_values::<Table>() {
            let filter = filter?;
            let label = bounded_text(filter.get("name")?, 256)?;
            if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control)
            {
                return Err(bad("A filter name must be 1–64 printable characters."));
            }
            let extensions: Table = filter.get("extensions")?;
            if extensions.raw_len() == 0 || extensions.raw_len() > 16 {
                return Err(bad("A filter needs 1–16 extensions."));
            }
            let mut values = Vec::new();
            for ext in extensions.sequence_values::<Value>() {
                let ext = bounded_text(ext?, 16)?;
                if ext.is_empty()
                    || ext.len() > 16
                    || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    return Err(bad(
                        "Extensions must be 1–16 ASCII letters/digits without dots, wildcards or paths.",
                    ));
                }
                values.push(ext);
            }
            spec.filters.push((label, values));
        }
    }
    Ok(spec)
}

/// Pins each parent on Windows without FILE_SHARE_DELETE. An attacker cannot
/// rename a pinned parent into a junction between consent and file access.
fn pin_parents(path: &Path) -> mlua::Result<Vec<File>> {
    if !path.is_absolute() {
        return Err(bad("The selected file must have an absolute local path."));
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(bad(
            "Choose a local disk file; UNC, network and device paths are not supported.",
        ));
    }
    let mut parent = PathBuf::new();
    let mut pins = Vec::new();
    let parts: Vec<_> = path.components().collect();
    if parts.len() > 66 {
        return Err(bad(
            "The selected path exceeds the 64-directory handle limit; choose a shallower location.",
        ));
    }
    for (index, part) in parts.iter().enumerate() {
        match part {
            Component::ParentDir | Component::CurDir => {
                return Err(bad("Relative traversal is not allowed in a selected path."));
            }
            Component::Normal(n) if !valid_name(&n.to_string_lossy()) => {
                return Err(bad(
                    "The selected path contains an unsafe name, stream or device component.",
                ));
            }
            _ => (),
        }
        parent.push(part.as_os_str());
        if index + 1 == parts.len() || matches!(part, Component::Prefix(_)) {
            continue;
        }
        let file = open_checked(&parent, true, false)?;
        pins.push(file);
    }
    if pins.is_empty() {
        return Err(bad("The selected file has no valid parent directory."));
    }
    Ok(pins)
}

fn open_checked(path: &Path, directory: bool, allow_changes: bool) -> mlua::Result<File> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| io_error("Inspect selected file", e))?;
    if metadata.file_type().is_symlink() {
        return Err(bad(
            "Symbolic links and junctions cannot grant file access; choose the original file.",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::*;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(bad("Reparse points are not allowed in selected paths."));
        }
        let sharing = if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else if allow_changes {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ
        };
        options.share_mode(sharing).custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );
        if directory {
            options.access_mode(FILE_READ_ATTRIBUTES);
        }
    }
    #[cfg(not(windows))]
    let _ = allow_changes;
    let file = options
        .open(path)
        .map_err(|e| io_error("Open selected file", e))?;
    let actual = file
        .metadata()
        .map_err(|e| io_error("Inspect opened file", e))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if actual.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(bad("The selected file changed into a reparse point."));
        }
    }
    if (directory && !actual.is_dir()) || (!directory && !actual.is_file()) {
        return Err(bad(
            "Only ordinary files and their parent directories are supported.",
        ));
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp([u64; 4]);

fn stamp(file: &File) -> mlua::Result<FileStamp> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live borrowed file handle and correctly sized writable output.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io_error(
                "Identify selected file",
                std::io::Error::last_os_error(),
            ));
        }
        let info = unsafe { info.assume_init() };
        Ok(FileStamp([
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
            (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
                | u64::from(info.ftLastWriteTime.dwLowDateTime),
        ]))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file
            .metadata()
            .map_err(|e| io_error("Identify selected file", e))?;
        Ok(FileStamp([
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime_nsec() as u64 ^ m.mtime() as u64,
        ]))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Err(bad(
            "File identity checks are not supported on this platform.",
        ))
    }
}

struct FileState {
    file: Option<File>,
    name: String,
    length: u64,
    position: u64,
    pins: Vec<File>,
    output: Option<Output>,
}

struct Output {
    destination: PathBuf,
    temporary: PathBuf,
    original: Option<FileStamp>,
    ready: bool,
}

impl FileState {
    fn close(&mut self) {
        if let Some(output) = self.output.take()
            && !output.ready
        {
            #[cfg(windows)]
            if let Some(file) = self.file.as_ref() {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::Storage::FileSystem::{
                    FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
                };
                let info = FILE_DISPOSITION_INFO { DeleteFile: true };
                // SAFETY: own live DELETE-capable staging handle, correct input layout.
                unsafe {
                    SetFileInformationByHandle(
                        file.as_raw_handle(),
                        FileDispositionInfo,
                        &info as *const _ as *const _,
                        std::mem::size_of_val(&info) as u32,
                    );
                }
            }
            #[cfg(not(windows))]
            {
                let _ = std::fs::remove_file(output.temporary);
            }
        }
        self.file = None;
        self.pins.clear();
    }
}
impl Drop for FileState {
    fn drop(&mut self) {
        self.close();
    }
}

/// Rename the actual open staging object, without a close/reopen race.
fn publish(file: &File, output: &Output) -> mlua::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
        };
        let name: Vec<u16> = output.destination.as_os_str().encode_wide().collect();
        if name.len() > 32_760 {
            return Err(bad(
                "The output path exceeds Windows' supported path length.",
            ));
        }
        let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        // SetFileInformationByHandle also converts this Win32 path through
        // a NUL-terminated API. Include a terminator even though FileNameLength
        // excludes it; allocator padding is not a terminator guarantee.
        let bytes = offset + (name.len() + 1) * 2;
        let mut storage = vec![
            0u64;
            bytes
                .max(std::mem::size_of::<FILE_RENAME_INFO>())
                .div_ceil(8)
        ];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: u64-aligned storage sized for the header and full UTF-16 name;
        // no pointer escapes this synchronous API call. The live handle has DELETE.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = true;
            (*info).RootDirectory = std::ptr::null_mut();
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
            if SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            ) == 0
            {
                return Err(io_error(
                    "Publish selected output atomically",
                    std::io::Error::last_os_error(),
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        std::fs::rename(&output.temporary, &output.destination)
            .map_err(|e| io_error("Publish selected output atomically", e))
    }
}

#[derive(Clone)]
struct LuaFile {
    job: Rc<FileJob>,
    state: Rc<RefCell<FileState>>,
    write: bool,
    asset: Option<Rc<assets::Scope>>,
    host_owned_output: bool,
}

/// Native consumers receive only an already-authorized stream, never a path
/// or duplicate OS handle. Every read/seek checks the same job lease and quota.
pub(super) trait ReadSource: Read + Seek {}
impl<T: Read + Seek> ReadSource for T {}

struct NativeReader<'a> {
    state: &'a mut FileState,
    job: &'a FileJob,
    fault: Option<mlua::Error>,
}

impl NativeReader<'_> {
    fn check(&mut self) -> std::io::Result<()> {
        self.job.lease.check().map_err(|error| {
            self.fault = Some(error.lua());
            std::io::Error::other("Selected-file job is no longer active")
        })
    }
}

impl Read for NativeReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        let remaining = (self.state.length - self.state.position) as usize;
        let n = buffer.len().min(IO_CHECK).min(remaining);
        if n == 0 {
            return Ok(0);
        }
        let quota = MAX_JOB_BYTES - self.job.read.get();
        if quota == 0 {
            self.fault = Some(stop(
                Category::Budget,
                "Native decoding exhausted the selected-file read byte budget.",
            ));
            return Err(std::io::Error::other("Selected-file read budget exceeded"));
        }
        let n = n.min(quota as usize);
        let read = self.state.file.as_mut().unwrap().read(&mut buffer[..n])?;
        self.state.position += read as u64;
        self.job.read.set(self.job.read.get() + read as u64);
        Ok(read)
    }
}

impl Seek for NativeReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        self.check()?;
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => i128::from(self.state.length) + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.state.position) + i128::from(n),
        };
        if position < 0 || position > i128::from(self.state.length) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Decode seek is outside the selected file",
            ));
        }
        let position = self
            .state
            .file
            .as_mut()
            .unwrap()
            .seek(SeekFrom::Start(position as u64))?;
        self.state.position = position;
        Ok(position)
    }
}

pub(super) fn install_assets(lua: &Lua, plugin: &Table) -> mlua::Result<()> {
    assets::install(lua, plugin)
}
pub(super) fn source_kind(lua: &Lua, file: &mlua::AnyUserData) -> mlua::Result<&'static str> {
    let file = file.borrow::<LuaFile>()?;
    file.check(lua)?;
    Ok(if file.asset.is_some() {
        "package_asset"
    } else {
        "selected_file"
    })
}

/// Native resource loading reads from the start, then restores the script's cursor.
/// Actual bytes read still count, including decoder seeks/repeated reads.
pub(super) fn with_reader<T>(
    lua: &Lua,
    file: &mlua::AnyUserData,
    operation: impl FnOnce(&mut dyn ReadSource, u64, &str) -> mlua::Result<T>,
) -> mlua::Result<T> {
    let file = file.borrow::<LuaFile>()?;
    file.check(lua)?;
    if file.write {
        return Err(bad(
            "Native resource loading needs a read-only selected file.",
        ));
    }
    let mut state = file.state.borrow_mut();
    let original = state.position;
    let length = state.length;
    let name = state.name.clone();
    let mut reader = NativeReader {
        state: &mut state,
        job: &file.job,
        fault: None,
    };
    let positioned = reader.seek(SeekFrom::Start(0));
    if let Some(error) = reader.fault.take() {
        return Err(error);
    }
    positioned.map_err(|e| io_error("Seek selected resource input", e))?;
    let result = operation(&mut reader, length, &name);
    let fault = reader.fault.take();
    // Restoration does not grant further reading and is attempted even on cancel.
    let restored = reader
        .state
        .file
        .as_mut()
        .unwrap()
        .seek(SeekFrom::Start(original));
    if restored.is_ok() {
        reader.state.position = original;
    } else {
        reader.state.close();
    }
    if let Some(error) = fault {
        return Err(error);
    }
    restored.map_err(|e| io_error("Restore selected-file cursor", e))?;
    file.job.lease.check().map_err(|e| e.lua())?;
    result
}

impl LuaFile {
    fn check(&self, lua: &Lua) -> mlua::Result<()> {
        self.job.lease.check().map_err(|e| e.lua())?;
        let current = lua.app_data_ref::<FileSlot>().and_then(|s| s.0.clone());
        if !current.is_some_and(|j| Rc::ptr_eq(&j, &self.job)) {
            return Err(stop(
                Category::Expired,
                "This file belongs to an earlier job; choose it again.",
            ));
        }
        if let Some(scope) = &self.asset {
            scope.check(lua)?;
        } else {
            require(
                lua,
                if self.write {
                    Capability::WriteUserFile
                } else {
                    Capability::ReadUserFile
                },
            )?;
        }
        if self.state.borrow().file.is_none() {
            return Err(stop(Category::Expired, "This file is closed."));
        }
        Ok(())
    }

    fn read(&self, lua: &Lua, n: usize) -> mlua::Result<mlua::LuaString> {
        self.check(lua)?;
        if self.write {
            return Err(bad("This is a write-only selected file."));
        }
        let mut state = self.state.borrow_mut();
        let n = n.min((state.length - state.position) as usize);
        account(&self.job.read, n)?;
        let _reservation = self.job.native.reserve(n).map_err(|e| e.lua())?;
        let mut bytes = vec![0; n];
        let mut done = 0;
        while done < n {
            self.job.lease.check().map_err(|e| e.lua())?;
            let end = (done + IO_CHECK).min(n);
            let read = state
                .file
                .as_mut()
                .unwrap()
                .read(&mut bytes[done..end])
                .map_err(|e| io_error("Read selected file", e))?;
            if read == 0 {
                return Err(bad(
                    "The selected file became shorter during the read; choose it again.",
                ));
            }
            done += read;
        }
        state.position += n as u64;
        self.job.lease.check().map_err(|e| e.lua())?;
        lua.create_string(&bytes)
    }
}

impl UserData for LuaFile {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("info", |lua, this, ()| {
            this.check(lua)?;
            let state = this.state.borrow();
            let info = lua.create_table()?;
            info.set("name", state.name.clone())?;
            info.set("size", state.length as i64)?;
            info.set("offset", state.position as i64)?;
            info.set("mode", if this.write { "write" } else { "read" })?;
            info.set(
                "source",
                if this.asset.is_some() {
                    "package_asset"
                } else {
                    "user_choice"
                },
            )?;
            Ok(info)
        });
        methods.add_method("read", |lua, this, n: Value| {
            this.read(lua, count(&n, IO_CHECK, MAX_CHUNK, "Read size")?)
        });
        methods.add_method("read_all", |lua, this, limit: Value| {
            this.check(lua)?;
            let maximum = count(&limit, MAX_READ_ALL, MAX_READ_ALL, "Read-all maximum")?;
            let remaining = {
                let state = this.state.borrow();
                state.length - state.position
            };
            if remaining > maximum as u64 {
                return Err(stop(
                    Category::Budget,
                    "read_all exceeds its small-file limit; use read() chunks instead.",
                ));
            }
            this.read(lua, remaining as usize)
        });
        methods.add_method("seek", |lua, this, offset: Value| {
            this.check(lua)?;
            if this.write {
                return Err(bad("Streaming writes are append-only."));
            }
            let offset = match offset {
                Value::Integer(n) if n >= 0 => n as u64,
                _ => {
                    return Err(bad(
                        "File offset must be a non-negative integer byte offset.",
                    ));
                }
            };
            let mut state = this.state.borrow_mut();
            if offset > state.length {
                return Err(bad("The file offset exceeds its size."));
            }
            state
                .file
                .as_mut()
                .unwrap()
                .seek(SeekFrom::Start(offset))
                .map_err(|e| io_error("Seek selected file", e))?;
            state.position = offset;
            Ok(offset as i64)
        });
        methods.add_method("write", |lua, this, bytes: mlua::LuaString| {
            this.check(lua)?;
            if !this.write {
                return Err(bad("This is a read-only selected file."));
            }
            let mut state = this.state.borrow_mut();
            let result = (|| {
                let bytes = bytes.as_bytes();
                if bytes.len() > MAX_CHUNK {
                    return Err(stop(
                        Category::Budget,
                        "A write chunk may contain at most 1 MiB.",
                    ));
                }
                account(&this.job.written, bytes.len())?;
                for chunk in bytes.chunks(IO_CHECK) {
                    this.job.lease.check().map_err(|e| e.lua())?;
                    state
                        .file
                        .as_mut()
                        .unwrap()
                        .write_all(chunk)
                        .map_err(|e| io_error("Stage selected output", e))?;
                }
                state.position += bytes.len() as u64;
                state.length = state.position;
                Ok(bytes.len() as i64)
            })();
            if result.is_err() {
                state.close();
            }
            result
        });
        methods.add_method("commit", |lua, this, ()| {
            if this.host_owned_output {
                return Err(stop(
                    Category::Capability,
                    "The host publishes format output only after the whole adapter succeeds.",
                ));
            }
            this.publish(lua)
        });
        methods.add_method("close", |_, this, ()| {
            this.state.borrow_mut().close();
            Ok(())
        });
    }
}
impl LuaFile {
    fn publish(&self, lua: &Lua) -> mlua::Result<bool> {
        let this = self;
        this.check(lua)?;
        if !this.write {
            return Err(bad("Only a staged output can be committed."));
        }
        let mut state = this.state.borrow_mut();
        let result = (|| {
            let output = state.output.as_ref().unwrap();
            let current = match std::fs::symlink_metadata(&output.destination) {
                Ok(_) => Some(stamp(&open_checked(&output.destination, false, true)?)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(io_error("Check output destination", e)),
            };
            if current != output.original {
                return Err(bad(
                    "The output destination changed after selection; choose it again. The existing file was kept.",
                ));
            }
            state
                .file
                .as_mut()
                .unwrap()
                .flush()
                .map_err(|e| io_error("Flush staged output", e))?;
            state
                .file
                .as_ref()
                .unwrap()
                .sync_all()
                .map_err(|e| io_error("Sync staged output", e))?;
            this.job.lease.check().map_err(|e| e.lua())?;
            publish(state.file.as_ref().unwrap(), state.output.as_ref().unwrap())?;
            let output = state.output.as_mut().unwrap();
            output.ready = true;
            Ok(true)
        })();
        state.close();
        result
    }
}

fn chosen(lua: &Lua, path: PathBuf, write: bool) -> mlua::Result<LuaFile> {
    let job = lua
        .app_data_ref::<FileSlot>()
        .and_then(|s| s.0.clone())
        .ok_or_else(|| stop(Category::Expired, "No file job is running."))?;
    job.lease.check().map_err(|e| e.lua())?;
    let pins = pin_parents(&path)?;
    let name = path
        .file_name()
        .ok_or_else(|| bad("Choose a file, not a directory."))?
        .to_string_lossy()
        .into_owned();
    let (file, output, length) = if write {
        let original = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(stamp(&open_checked(&path, false, true)?)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error("Inspect chosen output", e)),
        };
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
        let mut staged = None;
        for _ in 0..16 {
            let temporary = path.with_file_name(format!(
                ".plumspirit-{}-{}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_WRITE};
                options
                    .share_mode(0)
                    .access_mode(FILE_GENERIC_WRITE | DELETE);
            }
            match options.open(&temporary) {
                Ok(file) => {
                    staged = Some((file, temporary));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(io_error("Create private staging file", e)),
            }
        }
        let (file, temporary) = staged.ok_or_else(|| {
            bad("Could not create a unique staging file; retry in another directory.")
        })?;
        (
            file,
            Some(Output {
                destination: path,
                temporary,
                original,
                ready: false,
            }),
            0,
        )
    } else {
        let file = open_checked(&path, false, false)?;
        let length = file
            .metadata()
            .map_err(|e| io_error("Read chosen file size", e))?
            .len();
        if length > MAX_JOB_BYTES {
            return Err(stop(
                Category::Budget,
                "The selected file exceeds the 1 GiB job input limit.",
            ));
        }
        (file, None, length)
    };
    let state = Rc::new(RefCell::new(FileState {
        file: Some(file),
        name,
        length,
        position: 0,
        pins,
        output,
    }));
    job.states.borrow_mut().push(Rc::downgrade(&state));
    Ok(LuaFile {
        job,
        state,
        write,
        asset: None,
        host_owned_output: false,
    })
}

pub(super) fn choose_format(
    lua: &Lua,
    save: bool,
    name: &str,
    extensions: &[String],
) -> mlua::Result<Option<mlua::AnyUserData>> {
    require(
        lua,
        if save {
            Capability::WriteUserFile
        } else {
            Capability::ReadUserFile
        },
    )?;
    let (client, lease) = {
        let session = lua
            .app_data_ref::<Session>()
            .ok_or_else(|| bad("No format UI session."))?;
        (
            session
                .client
                .clone()
                .ok_or_else(|| bad("The format file dialog is not connected."))?,
            session
                .lease
                .clone()
                .ok_or_else(|| stop(Category::Expired, "No format job is active."))?,
        )
    };
    let spec = FileDialogSpec {
        save,
        filters: vec![(name.to_owned(), extensions.to_vec())],
        suggested_name: save.then(|| format!("export.{}", extensions[0])),
    };
    let Some(path) = client.request(&lease, spec).map_err(|e| e.lua())? else {
        return Ok(None);
    };
    if !path
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|e| extensions.iter().any(|v| v.eq_ignore_ascii_case(e)))
    {
        return Err(bad(
            "The selected filename does not match this adapter's registered extensions; native MIDI/LUP handlers are unchanged.",
        ));
    }
    let mut file = chosen(lua, path, save)?;
    file.host_owned_output = save;
    lua.create_userdata(file).map(Some)
}
pub(super) fn publish_format(lua: &Lua, file: &mlua::AnyUserData) -> mlua::Result<()> {
    let f = file.borrow::<LuaFile>()?;
    if !f.host_owned_output {
        return Err(bad("Format output authority is missing."));
    }
    f.publish(lua).map(|_| ())
}

pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let files = lua.create_table()?;
    for (name, save) in [("open", false), ("save", true)] {
        files.set(
            name,
            lua.create_function(move |lua, options: Option<Table>| {
                if super::formats::active(lua) {
                    return Err(stop(Category::Capability,"Format adapters use their single host-selected ctx.file, without additional file dialogs."));
                }
                require(
                    lua,
                    if save {
                        Capability::WriteUserFile
                    } else {
                        Capability::ReadUserFile
                    },
                )?;
                let spec = dialog_spec(options, save)?;
                let (client, lease) = {
                    let session = lua
                        .app_data_ref::<Session>()
                        .ok_or_else(|| bad("No UI session is active."))?;
                    (
                        session
                            .client
                            .clone()
                            .ok_or_else(|| bad("The native file dialog UI is not connected."))?,
                        session
                            .lease
                            .clone()
                            .ok_or_else(|| stop(Category::Expired, "No job is active."))?,
                    )
                };
                match client.request(&lease, spec).map_err(|e| e.lua())? {
                    None => Ok(None),
                    Some(path) => chosen(lua, path, save).map(Some),
                }
            })?,
        )?;
    }
    files.set("max_chunk", MAX_CHUNK as i64)?;
    files.set("max_read_all", MAX_READ_ALL as i64)?;
    files.set("max_job_bytes", MAX_JOB_BYTES as i64)?;
    plum.set("files", files)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
