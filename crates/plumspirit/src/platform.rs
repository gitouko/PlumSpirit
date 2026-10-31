//! Thin, isolated wrappers over Windows APIs that the Rust standard library does not
//! expose. This is the only module in PlumSpirit that uses `unsafe`; every call is a
//! plain FFI call with locally owned buffers, and each block states its invariants.
//! No pointers escape these functions.

/// Font files registered under the Windows "Fonts" registry keys whose registered
/// name contains `needle` (case-insensitive), as absolute paths.
#[cfg(windows)]
pub fn registered_font_files(needle: &str) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
        RegEnumValueW, RegOpenKeyExW,
    };

    let needle = needle.to_ascii_lowercase();
    let windir =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let fonts_dir = windir.join("Fonts");
    let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts\0"
        .encode_utf16()
        .collect();
    let mut out = Vec::new();
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `subkey` is NUL-terminated UTF-16 that outlives the call and `key`
        // is a valid out-pointer to a local.
        let status = unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, KEY_READ, &mut key) };
        if status != ERROR_SUCCESS {
            continue;
        }
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = [0u16; 1024];
            let mut data_bytes = (data.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: all buffers are local, their lengths are passed in the matching
            // in/out length parameters, and `key` is an open registry key.
            let status = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr().cast::<u8>(),
                    &mut data_bytes,
                )
            };
            index += 1;
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
                if index > 100_000 {
                    break;
                }
                continue;
            }
            let value_name = String::from_utf16_lossy(&name[..(name_len as usize).min(name.len())]);
            if !value_name.to_ascii_lowercase().contains(&needle) {
                continue;
            }
            let chars = ((data_bytes as usize) / 2).min(data.len());
            let file = String::from_utf16_lossy(&data[..chars]);
            let file = file.trim_end_matches('\0').trim();
            if file.is_empty() {
                continue;
            }
            let p = PathBuf::from(file);
            out.push(if p.is_absolute() {
                p
            } else {
                fonts_dir.join(p)
            });
        }
        // SAFETY: `key` was opened successfully above and is closed exactly once.
        unsafe {
            RegCloseKey(key);
        }
    }
    out
}

#[cfg(not(windows))]
pub fn registered_font_files(_needle: &str) -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Working set of the current process in bytes.
#[cfg(windows)]
pub fn working_set_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: PROCESS_MEMORY_COUNTERS is a plain C struct; all-zero is a valid value.
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
    // `counters` is a valid, correctly sized out-buffer.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(counters.WorkingSetSize as u64)
}

/// Private committed memory of this process (separate from its working set).
#[cfg(windows)]
pub fn private_commit_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain C output buffer, correctly sized; the pseudo-handle
    // returned by GetCurrentProcess does not need closing.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    (ok != 0).then_some(counters.PrivateUsage as u64)
}

/// Physical memory currently available to programs, in bytes.
#[cfg(windows)]
pub fn available_memory_bytes() -> Option<u64> {
    memory_status().map(|m| m.0)
}

/// Total physical memory, in bytes.
#[cfg(windows)]
pub fn total_memory_bytes() -> Option<u64> {
    memory_status().map(|m| m.1)
}

#[cfg(windows)]
fn memory_status() -> Option<(u64, u64)> {
    memory_status_ex().map(|m| (m.ullAvailPhys, m.ullTotalPhys))
}

#[cfg(windows)]
fn memory_status_ex() -> Option<windows_sys::Win32::System::SystemInformation::MEMORYSTATUSEX> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is a plain C struct; all-zero is valid, and dwLength is
    // set as the API requires before the call writes into the local struct.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `m` is a valid, correctly sized out-pointer to a local.
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) };
    (ok != 0).then_some(m)
}

/// L-H04: memory the system can still hand out before an allocation fails
/// (Windows: the commit charge left — physical memory plus the page file —
/// `ullAvailPageFile`), and the limit it is part of (`ullTotalPageFile`).
/// Past this point Rust aborts the process on the next allocation.
#[cfg(windows)]
pub fn commit_headroom_bytes() -> Option<(u64, u64)> {
    memory_status_ex().map(|m| (m.ullAvailPageFile, m.ullTotalPageFile))
}

/// L-H04: Linux overcommits; what can still be backed is the available
/// memory plus free swap (the out-of-memory killer acts past it), or less
/// when the process's memory cgroup has a lower limit (containers: measured
/// in the cloud sandbox — 6.3 GB under 8 GB of RAM, the 500M probe was
/// killed at 6.0 GB while /proc/meminfo still showed memory available).
#[cfg(not(windows))]
pub fn commit_headroom_bytes() -> Option<(u64, u64)> {
    let avail = meminfo_kib("MemAvailable:")?;
    let swap_free = meminfo_kib("SwapFree:").unwrap_or(0);
    let total = meminfo_kib("MemTotal:")?;
    let swap_total = meminfo_kib("SwapTotal:").unwrap_or(0);
    let system = ((avail + swap_free) * 1024, (total + swap_total) * 1024);
    Some(match cgroup_headroom() {
        Some(c) if c.0 < system.0 => c,
        _ => system,
    })
}

/// (limit − usage without the file cache, limit) of this process's
/// memory cgroup (v2 `memory.max`, v1 `memory.limit_in_bytes`); `None`
/// without a limit.
#[cfg(not(windows))]
fn cgroup_headroom() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(_), Some(ctrl), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (dir, limit_file, usage_file, file_cache) = if ctrl == "memory" {
            (
                format!("/sys/fs/cgroup/memory{path}"),
                "memory.limit_in_bytes",
                "memory.usage_in_bytes",
                ["total_inactive_file ", "total_active_file "],
            )
        } else if ctrl.is_empty() {
            (
                format!("/sys/fs/cgroup{path}"),
                "memory.max",
                "memory.current",
                ["inactive_file ", "active_file "],
            )
        } else {
            continue;
        };
        let read = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).ok();
        // "max" (v2) or a page-rounded i64::MAX (v1): no limit.
        let Some(limit) = read(limit_file)
            .and_then(|t| t.trim().parse::<u64>().ok())
            .filter(|&l| l < 1 << 60)
        else {
            continue;
        };
        let Some(usage) = read(usage_file).and_then(|t| t.trim().parse::<u64>().ok()) else {
            continue;
        };
        // The file cache (the MIDI file just read among it) is given back
        // under pressure: measured, counting the active part as used
        // stopped a 250M-note open that fits.
        let stat = read("memory.stat").unwrap_or_default();
        let cache: u64 = file_cache
            .iter()
            .filter_map(|k| {
                stat.lines()
                    .find_map(|l| l.strip_prefix(k)?.trim().parse::<u64>().ok())
            })
            .sum();
        return Some((limit.saturating_sub(usage.saturating_sub(cache)), limit));
    }
    None
}

#[cfg(not(windows))]
fn meminfo_kib(field: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    text.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok())
}

#[cfg(not(windows))]
pub fn available_memory_bytes() -> Option<u64> {
    meminfo_kib("MemAvailable:").map(|k| k * 1024)
}

#[cfg(not(windows))]
pub fn total_memory_bytes() -> Option<u64> {
    meminfo_kib("MemTotal:").map(|k| k * 1024)
}

/// CPU time (user + kernel) used by this process so far, in nanoseconds.
/// v1.0-Launch L-D03: free bytes on the volume of `dir` (archive spool).
#[cfg(windows)]
pub fn free_disk_bytes(dir: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated path; the out pointer is valid.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_disk_bytes(_dir: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(windows)]
pub fn process_cpu_nanos() -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing; the
    // four out-pointers are valid locals.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    // FILETIME counts 100-nanosecond intervals.
    Some((ft(kernel) + ft(user)).saturating_mul(100))
}

/// CPU time (user + system) used by this process so far, in nanoseconds, from
/// `/proc/self/stat` (fields 14 and 15, in clock ticks of USER_HZ = 100, which
/// Linux uses on every mainstream architecture).
#[cfg(not(windows))]
pub fn process_cpu_nanos() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The command name (field 2) is in parentheses and may contain spaces.
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After ")" the fields start at 3 (state): utime = 14 → index 11, stime = 15 → 12.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some((utime + stime).saturating_mul(10_000_000))
}

/// Names of the Windows MIDI output devices, in device-id order.
#[cfg(windows)]
pub fn midi_output_names() -> Vec<String> {
    use windows_sys::Win32::Media::Audio::{MIDIOUTCAPSW, midiOutGetDevCapsW, midiOutGetNumDevs};
    // SAFETY: no arguments.
    let n = unsafe { midiOutGetNumDevs() };
    let mut out = Vec::new();
    for id in 0..n.min(256) {
        let mut caps = MIDIOUTCAPSW::default();
        // SAFETY: `caps` is a valid, correctly sized local out-buffer.
        let r = unsafe {
            midiOutGetDevCapsW(
                id as usize,
                &mut caps,
                std::mem::size_of::<MIDIOUTCAPSW>() as u32,
            )
        };
        if r != 0 {
            out.push(format!("MIDI output {}", id + 1));
            continue;
        }
        let name = caps.szPname;
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        out.push(String::from_utf16_lossy(&name[..len]).trim().to_owned());
    }
    out
}

#[cfg(not(windows))]
pub fn midi_output_names() -> Vec<String> {
    Vec::new()
}

/// OmniMIDI's library (`OmniMIDI.dll`), once found: kept loaded for the rest of
/// the process (never unloaded under a driver that may have started threads).
#[cfg(windows)]
static KDMAPI_LIB: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// The loaded `OmniMIDI.dll` module, searched only in System32 and the program's
/// folder (never the current or PATH directories), or why it is missing.
#[cfg(windows)]
pub fn kdmapi_module() -> Result<usize, String> {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    let mut lib = KDMAPI_LIB.lock().unwrap_or_else(|p| p.into_inner());
    if *lib != 0 {
        return Ok(*lib);
    }
    let name: Vec<u16> = "OmniMIDI.dll\0".encode_utf16().collect();
    // SAFETY: `name` is NUL-terminated UTF-16 that outlives the call; no file
    // handle is passed (it must be null).
    let h = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
        )
    };
    if h.is_null() {
        // SAFETY: no arguments; reads this thread's last error.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        const ERROR_BAD_EXE_FORMAT: u32 = 193;
        return Err(if code == ERROR_BAD_EXE_FORMAT {
            "OmniMIDI.dll was found but is not a 64-bit library (install the 64-bit OmniMIDI)."
                .to_owned()
        } else {
            "OmniMIDI is not installed (OmniMIDI.dll is neither in System32 nor next to PlumSpirit.exe)."
                .to_owned()
        });
    }
    *lib = h as usize;
    Ok(*lib)
}

/// The address of `symbol` (NUL-terminated) in OmniMIDI's library, if exported.
#[cfg(windows)]
pub fn kdmapi_symbol(
    module: usize,
    symbol: &std::ffi::CStr,
) -> Option<unsafe extern "system" fn() -> isize> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    // SAFETY: `module` came from `kdmapi_module` and is never unloaded; `symbol`
    // is NUL-terminated and outlives the call.
    unsafe { GetProcAddress(module as _, symbol.as_ptr().cast()) }
}

/// Whether OmniMIDI's KDMAPI can be used: `Ok` with its version text, or the
/// reason as a sentence. Starts no stream (`IsKDMAPIAvailable` and
/// `ReturnKDMAPIVer` only).
#[cfg(windows)]
pub fn kdmapi_probe() -> Result<String, String> {
    let module = kdmapi_module()?;
    let Some(avail) = kdmapi_symbol(module, c"IsKDMAPIAvailable") else {
        return Err(
            "This OmniMIDI has no KDMAPI (IsKDMAPIAvailable is missing); version 1.2 or newer is needed."
                .to_owned(),
        );
    };
    // SAFETY: OmniMIDI documents `BOOL IsKDMAPIAvailable(void)` (stdcall = the
    // x64 C convention); both are plain function pointers of equal size.
    let avail: unsafe extern "system" fn() -> i32 = unsafe { std::mem::transmute(avail) };
    // SAFETY: no arguments; the library stays loaded.
    if unsafe { avail() } == 0 {
        return Err(
            "OmniMIDI reports that KDMAPI is not available (turned off in its settings or too old)."
                .to_owned(),
        );
    }
    let Some(ver) = kdmapi_symbol(module, c"ReturnKDMAPIVer") else {
        return Ok("KDMAPI".to_owned());
    };
    type VerFn = unsafe extern "system" fn(*mut u32, *mut u32, *mut u32, *mut u32) -> i32;
    // SAFETY: documented `BOOL ReturnKDMAPIVer(LPDWORD, LPDWORD, LPDWORD, LPDWORD)`.
    let ver: VerFn = unsafe { std::mem::transmute(ver) };
    let (mut a, mut b, mut c, mut d) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: four valid out-pointers to locals.
    if unsafe { ver(&mut a, &mut b, &mut c, &mut d) } == 0 {
        return Ok("KDMAPI".to_owned());
    }
    if (a, b) < (1, 2) {
        return Err(format!(
            "OmniMIDI's KDMAPI {a}.{b} is too old; version 1.2 or newer is needed."
        ));
    }
    Ok(format!("KDMAPI {a}.{b}.{c} (revision {d})"))
}

#[cfg(not(windows))]
pub fn kdmapi_probe() -> Result<String, String> {
    Err("OmniMIDI KDMAPI is available in the Windows version.".to_owned())
}

/// OmniMIDI's KDMAPI functions (B1-0136) after the probe: the four the stream
/// needs, or the missing one named; Reset and the SysEx trio when exported.
/// Every closure is a plain FFI call that cannot panic.
#[cfg(windows)]
pub fn kdmapi_fns() -> Result<crate::audio::midi_out::KdmapiFns, String> {
    use windows_sys::Win32::Media::Audio::MIDIHDR;
    kdmapi_probe()?;
    let module = kdmapi_module()?;
    let need = |name: &std::ffi::CStr| {
        kdmapi_symbol(module, name).ok_or_else(|| {
            format!(
                "This OmniMIDI does not export {} (a KDMAPI function PlumSpirit needs); update OmniMIDI.",
                name.to_string_lossy()
            )
        })
    };
    type RawFn = unsafe extern "system" fn() -> isize;
    type BoolFn = unsafe extern "system" fn() -> i32;
    type VoidFn = unsafe extern "system" fn();
    type SendFn = unsafe extern "system" fn(u32);
    type LongFn = unsafe extern "system" fn(*mut MIDIHDR, u32) -> u32;
    // SAFETY (all transmutes below): OmniMIDI documents these signatures
    // (stdcall = the x64 C convention); function pointers have equal size.
    let init = unsafe { std::mem::transmute::<RawFn, BoolFn>(need(c"InitializeKDMAPIStream")?) };
    let terminate =
        unsafe { std::mem::transmute::<RawFn, BoolFn>(need(c"TerminateKDMAPIStream")?) };
    let send = unsafe { std::mem::transmute::<RawFn, SendFn>(need(c"SendDirectData")?) };
    let reset: Option<VoidFn> = kdmapi_symbol(module, c"ResetKDMAPIStream")
        .map(|f| unsafe { std::mem::transmute::<RawFn, VoidFn>(f) });
    let long = match (
        kdmapi_symbol(module, c"PrepareLongData"),
        kdmapi_symbol(module, c"SendDirectLongData"),
        kdmapi_symbol(module, c"UnprepareLongData"),
    ) {
        (Some(p), Some(s), Some(u)) => Some(unsafe {
            (
                std::mem::transmute::<RawFn, LongFn>(p),
                std::mem::transmute::<RawFn, LongFn>(s),
                std::mem::transmute::<RawFn, LongFn>(u),
            )
        }),
        _ => None,
    };
    let sysex = long.map(|(prepare, send_long, unprepare)| {
        Box::new(move |bytes: &[u8]| -> bool {
            const MIDIERR_STILLPLAYING: u32 = 65;
            let mut data = bytes.to_vec();
            // Boxed: if OmniMIDI keeps the header, both stay allocated.
            let mut hdr = Box::new(MIDIHDR {
                lpData: data.as_mut_ptr(),
                dwBufferLength: data.len() as u32,
                dwBytesRecorded: data.len() as u32,
                ..MIDIHDR::default()
            });
            let size = std::mem::size_of::<MIDIHDR>() as u32;
            // SAFETY: `hdr` and `data` outlive every call; the header is
            // unprepared before they drop (waiting at most 200 ms), or leaked.
            unsafe {
                if prepare(&mut *hdr, size) != 0 {
                    return false;
                }
                let sent = send_long(&mut *hdr, size) == 0;
                for _ in 0..200 {
                    if unprepare(&mut *hdr, size) != MIDIERR_STILLPLAYING {
                        return sent;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // Still held by the driver: keep the buffer alive rather than
                // free memory OmniMIDI may read.
                std::mem::forget(data);
                std::mem::forget(hdr);
                false
            }
        }) as Box<crate::audio::midi_out::SysexFn>
    });
    Ok(crate::audio::midi_out::KdmapiFns {
        // SAFETY: documented `BOOL InitializeKDMAPIStream(void)`; no arguments.
        init: Box::new(move || unsafe { init() } != 0),
        // SAFETY: documented `BOOL TerminateKDMAPIStream(void)`.
        terminate: Box::new(move || unsafe { terminate() } != 0),
        // SAFETY: documented `void ResetKDMAPIStream(void)`.
        reset: reset.map(|r| Box::new(move || unsafe { r() }) as Box<dyn Fn() + Send + Sync>),
        // SAFETY: documented `void SendDirectData(DWORD)`; any DWORD is valid.
        send: Box::new(move |m| unsafe { send(m) }),
        sysex,
    })
}

#[cfg(not(windows))]
pub fn kdmapi_fns() -> Result<crate::audio::midi_out::KdmapiFns, String> {
    Err("OmniMIDI KDMAPI is available in the Windows version.".to_owned())
}

/// An open Windows MIDI output device (winmm). Closed on drop, after a reset
/// (which turns every note off).
#[cfg(windows)]
pub struct MidiOut {
    /// The HMIDIOUT handle, stored as an integer so the type is `Send + Sync`
    /// (winmm output handles may be used from any thread; callers serialize).
    handle: usize,
}

#[cfg(windows)]
impl MidiOut {
    /// Opens device `id` (index in [`midi_output_names`]). Returns the winmm error
    /// code on failure.
    pub fn open(id: u32) -> Result<MidiOut, u32> {
        use windows_sys::Win32::Media::Audio::{CALLBACK_NULL, HMIDIOUT, midiOutOpen};
        let mut h: HMIDIOUT = std::ptr::null_mut();
        // SAFETY: `h` is a valid out-pointer; no callback is registered.
        let r = unsafe { midiOutOpen(&mut h, id, 0, 0, CALLBACK_NULL) };
        if r != 0 || h.is_null() {
            return Err(r);
        }
        Ok(MidiOut { handle: h as usize })
    }

    /// Sends a short message (status | d0 << 8 | d1 << 16). False on failure (for
    /// example the device was unplugged).
    pub fn short(&self, msg: u32) -> bool {
        use windows_sys::Win32::Media::Audio::midiOutShortMsg;
        // SAFETY: the handle was opened by `open` and is closed only in `drop`.
        unsafe { midiOutShortMsg(self.handle as _, msg) == 0 }
    }

    /// Sends a complete System Exclusive message (F0 … F7) and waits (at most
    /// 200 ms) until the driver is done with the buffer.
    pub fn sysex(&self, bytes: &[u8]) -> bool {
        use windows_sys::Win32::Media::Audio::{
            MIDIHDR, midiOutLongMsg, midiOutPrepareHeader, midiOutReset, midiOutUnprepareHeader,
        };
        const MIDIERR_STILLPLAYING: u32 = 65;
        let mut data = bytes.to_vec();
        let mut hdr = MIDIHDR {
            lpData: data.as_mut_ptr(),
            dwBufferLength: data.len() as u32,
            dwBytesRecorded: data.len() as u32,
            ..MIDIHDR::default()
        };
        let size = std::mem::size_of::<MIDIHDR>() as u32;
        let h = self.handle as _;
        // SAFETY: `hdr` and `data` are locals that outlive every call below; the
        // header is unprepared (the driver releases the buffer) before they drop.
        unsafe {
            if midiOutPrepareHeader(h, &mut hdr, size) != 0 {
                return false;
            }
            let sent = midiOutLongMsg(h, &hdr, size) == 0;
            let mut waited = 0;
            loop {
                let r = midiOutUnprepareHeader(h, &mut hdr, size);
                if r != MIDIERR_STILLPLAYING {
                    break;
                }
                if waited >= 200 {
                    // The driver is stuck: reset returns the buffer, then unprepare.
                    midiOutReset(h);
                    midiOutUnprepareHeader(h, &mut hdr, size);
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
                waited += 1;
            }
            sent
        }
    }
}

#[cfg(windows)]
impl Drop for MidiOut {
    fn drop(&mut self) {
        use windows_sys::Win32::Media::Audio::{midiOutClose, midiOutReset};
        // SAFETY: the handle is valid until this close, which happens exactly once.
        unsafe {
            midiOutReset(self.handle as _);
            midiOutClose(self.handle as _);
        }
    }
}

/// Work area of the primary monitor (the screen minus the taskbar) in logical
/// points: `[x, y, width, height]`. Used to keep the initial window on screen on
/// small displays (e.g. 1366×768 laptops).
#[cfg(windows)]
pub fn primary_work_area_points() -> Option<[f32; 4]> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT into the valid, local out-pointer.
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut r).cast(), 0) };
    if ok == 0 {
        return None;
    }
    // SAFETY: no arguments; returns the system DPI (96 = 100% scaling).
    let dpi = unsafe { GetDpiForSystem() };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let w = (r.right - r.left) as f32 / scale;
    let h = (r.bottom - r.top) as f32 / scale;
    (w > 0.0 && h > 0.0).then(|| [r.left as f32 / scale, r.top as f32 / scale, w, h])
}

#[cfg(not(windows))]
pub fn primary_work_area_points() -> Option<[f32; 4]> {
    None
}

/// Shows a blocking error message box (used only when the window cannot be created,
/// so there is no in-app place to show the error).
#[cfg(windows)]
pub fn show_fatal_error(title: &str, message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let m: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated UTF-16 buffers alive for the call; a
    // null owner window is allowed.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            m.as_ptr(),
            t.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub fn show_fatal_error(title: &str, message: &str) {
    eprintln!("{title}: {message}");
}
