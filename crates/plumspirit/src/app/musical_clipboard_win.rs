//! Eager Win32 clipboard data survives the publisher's process. The system
//! owns the movable block after a successful SetClipboardData.

use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::time::Duration;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::packing;

type Result<T> = std::result::Result<T, String>;
const BUSY: &str = "Windows clipboard is busy. Try again in a moment.";
const EMPTY: &str = "The Windows clipboard has no PlumSpirit musical data.";
const INVALID: &str = "The Windows clipboard contains damaged PlumSpirit musical data.";

fn format() -> u32 {
    static FORMAT: OnceLock<u32> = OnceLock::new();
    *FORMAT.get_or_init(|| {
        let name: Vec<u16> = "PlumSpirit.MusicalClipboard.v1\0".encode_utf16().collect();
        // SAFETY: name is a live NUL-terminated UTF-16 string.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    })
}

pub(super) fn available() -> bool {
    let format = format();
    // SAFETY: the only argument is a registered clipboard format number.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) != 0 }
}

struct Open;

impl Open {
    fn new(owner: HWND) -> Result<Self> {
        for attempt in 0..4 {
            // SAFETY: owner is NULL for a read, or a live window on this thread.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_millis(4));
            }
        }
        Err(BUSY.into())
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard through this guard.
        unsafe { CloseClipboard() };
    }
}

struct Owner(HWND);

impl Owner {
    fn new() -> Result<Self> {
        let class = [
            b'S' as u16,
            b'T' as u16,
            b'A' as u16,
            b'T' as u16,
            b'I' as u16,
            b'C' as u16,
            0,
        ];
        // SAFETY: STATIC is a predefined Windows class. The message-only window
        // has no children or application-owned state and is destroyed on this thread.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if hwnd.is_null() {
            Err("Could not open a Windows clipboard owner window.".into())
        } else {
            Ok(Self(hwnd))
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: this guard owns the message-only window on the creating thread.
        unsafe { DestroyWindow(self.0) };
    }
}

struct Memory(HGLOBAL);

impl Drop for Memory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: only blocks whose ownership has not transferred are freed here.
            unsafe { GlobalFree(self.0) };
        }
    }
}

struct Lock {
    handle: HGLOBAL,
    ptr: *mut u8,
}

impl Lock {
    fn new(handle: HGLOBAL) -> Result<Self> {
        // SAFETY: callers obtained handle from the clipboard or GlobalAlloc.
        let ptr = unsafe { GlobalLock(handle) }.cast::<u8>();
        if ptr.is_null() {
            return Err("Could not lock the Windows clipboard data.".into());
        }
        Ok(Self { handle, ptr })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: there was a successful GlobalLock on this handle.
        unsafe { GlobalUnlock(self.handle) };
    }
}

pub(super) fn sequence() -> u32 {
    // SAFETY: this is a read-only system clipboard sequence query.
    unsafe { GetClipboardSequenceNumber() }
}

pub(super) fn write(bytes: &[u8]) -> Result<u32> {
    let format = format();
    if format == 0 {
        return Err("Could not register the PlumSpirit clipboard format.".into());
    }
    // SAFETY: allocation size is the already bounded serialized payload size.
    let mut memory = Memory(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) });
    if memory.0.is_null() {
        return Err("There is not enough memory for the Windows clipboard.".into());
    }
    {
        let lock = Lock::new(memory.0)?;
        // SAFETY: the blocks are distinct and both contain at least bytes.len() bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), lock.ptr, bytes.len()) };
    }
    let owner = Owner::new()?;
    let _open = Open::new(owner.0)?;
    // SAFETY: the clipboard is open with a non-NULL HWND. The unlocked,
    // moveable HGLOBAL is transferred to Windows only on success.
    unsafe {
        if EmptyClipboard() == 0 || SetClipboardData(format, memory.0).is_null() {
            return Err(
                "Windows could not publish the musical clipboard. Try copying again.".into(),
            );
        }
    }
    memory.0 = null_mut();
    Ok(sequence())
}

pub(super) fn read() -> Result<(u32, Vec<u8>)> {
    let format = format();
    if format == 0 {
        return Err(EMPTY.into());
    }
    let _open = Open::new(null_mut())?;
    // SAFETY: the clipboard is open, and Windows holds this handle until close.
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Err(EMPTY.into());
    }
    // SAFETY: GlobalSize returns zero for invalid handles.
    let size = unsafe { GlobalSize(handle) };
    if size < 16 {
        return Err(INVALID.into());
    }
    let lock = Lock::new(handle)?;
    // GlobalAlloc can round up. The header determines the exact encoded length.
    // SAFETY: the locked block has at least sixteen bytes.
    let header = unsafe { std::slice::from_raw_parts(lock.ptr, 16) };
    let len = packing::declared_size(header, size).ok_or(INVALID)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| "There is not enough memory to read the musical clipboard.")?;
    // SAFETY: len is checked against GlobalSize and the block is still locked.
    bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(lock.ptr, len) });
    Ok((sequence(), bytes))
}
