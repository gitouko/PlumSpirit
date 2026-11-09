//! The untrusted library runs only after an OS memory/child-process fence is attached.
use super::*;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) enum Error<E> {
    Stopped(E),
    Timeout,
    Refused(&'static str),
    Native(String),
}
#[derive(Clone, Copy)]
enum Mode {
    Shape,
    #[cfg(test)]
    Sleep,
    #[cfg(test)]
    Memory,
    #[cfg(test)]
    Flood,
    #[cfg(test)]
    Crash,
    #[cfg(test)]
    Cpu,
    #[cfg(test)]
    Process,
}
#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: unique live job handle. Kill-on-close terminates any remaining member.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(windows)]
impl Job {
    fn attach(child: &Child) -> Result<Self, &'static str> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: unnamed non-inheritable job, no security descriptor or borrowed name.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err("Cannot create the native font resource fence.");
        }
        let job = Self(raw);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        // Native cache construction must also stop if the parent is interrupted.
        limits.BasicLimitInformation.PerProcessUserTimeLimit = 10_000_000;
        limits.ProcessMemoryLimit = MEMORY_BYTES;
        // SAFETY: valid own job/child handles and exact initialized native input layout.
        let set = unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if set == 0 {
            return Err("Cannot set the native font resource limits.");
        }
        if unsafe { AssignProcessToJobObject(raw, child.as_raw_handle()) } == 0 {
            return Err("Cannot attach the native font resource fence; no font data was sent.");
        }
        Ok(job)
    }
}

pub(super) fn prepare_child() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::*;
        // SAFETY: process-local error policy only; no buffers or external handles.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        }
    }
}
pub(super) fn verify_fence() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::JobObjects::*;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: null job queries this helper's current job, with an exact output layout.
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                &mut limits as *mut _ as *mut _,
                std::mem::size_of_val(&limits) as u32,
                std::ptr::null_mut(),
            )
        };
        let required = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        if ok != 0
            && limits.BasicLimitInformation.LimitFlags & required == required
            && limits.BasicLimitInformation.ActiveProcessLimit == 1
            && limits.BasicLimitInformation.PerProcessUserTimeLimit > 0
            && limits.BasicLimitInformation.PerProcessUserTimeLimit <= 10_000_000
            && limits.ProcessMemoryLimit > 0
            && limits.ProcessMemoryLimit <= MEMORY_BYTES
        {
            return Ok(());
        }
    }
    Err("Native external font shaping requires its host-owned memory/CPU/process fence.".into())
}
#[cfg(windows)]
struct Guard {
    child: Child,
    _job: Job,
}
#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn command(mode: Mode) -> Result<Command, &'static str> {
    let mut command =
        Command::new(std::env::current_exe().map_err(|_| "Cannot locate the native font helper.")?);
    #[cfg(not(test))]
    {
        let _ = mode;
        command.arg(FLAG);
    }
    #[cfg(test)]
    {
        command.args([
            "--exact",
            "font_shape::tests::child_entrypoint",
            "--nocapture",
        ]);
        let mode = match mode {
            Mode::Shape => "shape",
            Mode::Sleep => "sleep",
            Mode::Memory => "memory",
            Mode::Flood => "flood",
            Mode::Crash => "crash",
            Mode::Cpu => "cpu",
            Mode::Process => "process",
        };
        command.env("PLUMSPIRIT_NATIVE_FONT_TEST", mode);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}
pub(crate) fn run<E>(
    request: &Request,
    font: &[u8],
    check: impl FnMut() -> Result<(), E>,
) -> Result<Run, Error<E>> {
    controlled(
        request,
        font,
        Mode::Shape,
        Duration::from_millis(MAX_MILLIS),
        check,
    )
}
#[cfg(not(windows))]
fn controlled<E>(
    _: &Request,
    _: &[u8],
    _: Mode,
    _: Duration,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Run, Error<E>> {
    check().map_err(Error::Stopped)?;
    Err(Error::Refused(
        "Resource-isolated external font shaping is supported on Windows only.",
    ))
}
#[cfg(windows)]
fn controlled<E>(
    request: &Request,
    font: &[u8],
    mode: Mode,
    timeout: Duration,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Run, Error<E>> {
    check().map_err(Error::Stopped)?;
    let header = request.header(font.len()).map_err(Error::Native)?;
    let started = Instant::now();
    let mut child = command(mode)
        .map_err(Error::Refused)?
        .spawn()
        .map_err(|_| Error::Refused("Cannot start the native font helper."))?;
    let job = match Job::attach(&child) {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Refused(e));
        }
    };
    let guard = Guard { child, _job: job };
    let overflow = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        // Own the guard inside the scope closure, so unwinding kills the process
        // before scoped pipe threads are joined, including unexpected parent panic.
        let mut guard = guard;
        let mut input = guard
            .child
            .stdin
            .take()
            .ok_or(Error::Refused("Native font input pipe unavailable."))?;
        let mut output = guard
            .child
            .stdout
            .take()
            .ok_or(Error::Refused("Native font output pipe unavailable."))?;
        let writer = std::thread::Builder::new()
            .name("font-input".into())
            .spawn_scoped(scope, move || {
                input.write_all(REQUEST)?;
                input.write_all(&(header.len() as u32).to_le_bytes())?;
                input.write_all(&header)?;
                for chunk in font.chunks(64 * 1024) {
                    input.write_all(chunk)?;
                }
                Ok::<(), std::io::Error>(())
            })
            .map_err(|_| Error::Refused("Cannot start bounded font input delivery."))?;
        let too_large = Arc::clone(&overflow);
        let reader = std::thread::Builder::new()
            .name("font-output".into())
            .spawn_scoped(scope, move || {
                // Test libtest writes one fixed header before its helper entrypoint.
                let limit = MAX_REPLY + 64;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(limit)
                    .map_err(|_| std::io::Error::other("Cannot reserve native font reply"))?;
                let mut chunk = [0u8; 4096];
                loop {
                    let n = output.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    if bytes.len() + n > limit {
                        too_large.store(true, Ordering::Release);
                        return Err(std::io::Error::other("Font reply overflow"));
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                Ok::<_, std::io::Error>(bytes)
            })
            .map_err(|_| Error::Refused("Cannot start bounded font output delivery."))?;
        let status = loop {
            if let Err(e) = check() {
                break Err(Error::Stopped(e));
            }
            if overflow.load(Ordering::Acquire) {
                break Err(Error::Refused(
                    "Native font reply exceeded its output limit.",
                ));
            }
            if started.elapsed() >= timeout {
                break Err(Error::Timeout);
            }
            match guard.child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => (),
                Err(_) => break Err(Error::Refused("Cannot monitor the native font helper.")),
            }
            std::thread::sleep(Duration::from_millis(3));
        };
        // Kill/wait before joining blocked pipe I/O on every failed/canceled path.
        if status.is_err() {
            let _ = guard.child.kill();
            let _ = guard.child.wait();
        }
        let written = writer
            .join()
            .map_err(|_| Error::Refused("Native font input worker failed."))?;
        let bytes = reader
            .join()
            .map_err(|_| Error::Refused("Native font output worker failed."))?;
        let status = status?;
        if !status.success() {
            return Err(Error::Refused(
                "Native font helper stopped; invalid font, memory limit or native failure. No result was accepted.",
            ));
        }
        written.map_err(|_| Error::Refused("Native font input was not delivered completely."))?;
        let bytes = bytes.map_err(|_| Error::Refused("Native font reply delivery failed."))?;
        #[cfg(test)]
        let bytes = bytes.strip_prefix(b"\nrunning 1 test\n").unwrap_or(&bytes);
        #[cfg(not(test))]
        let bytes = bytes.as_slice();
        check().map_err(Error::Stopped)?;
        decode(bytes).map_err(Error::Native)
    })
}

#[cfg(test)]
pub(super) fn test_run<E>(
    request: &Request,
    font: &[u8],
    mode: &str,
    timeout: Duration,
    check: impl FnMut() -> Result<(), E>,
) -> Result<Run, Error<E>> {
    let mode = match mode {
        "sleep" => Mode::Sleep,
        "memory" => Mode::Memory,
        "flood" => Mode::Flood,
        "crash" => Mode::Crash,
        "cpu" => Mode::Cpu,
        "process" => Mode::Process,
        _ => Mode::Shape,
    };
    controlled(request, font, mode, timeout, check)
}
