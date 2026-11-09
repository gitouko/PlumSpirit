//! One application-owned native picker at a time. The dedicated native UI
//! thread owns COM and the modal dialog; neither egui nor Lua workers run
//! its message loop. Cancellation closes only that thread's windows.

use crate::lua::bridge::{BridgeError, ProjectStamp, UiRequest};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub struct NativeFileDialog {
    request: Option<UiRequest>,
    result: Receiver<Option<PathBuf>>,
    thread: Arc<AtomicU32>,
    finished: Arc<AtomicBool>,
}

impl NativeFileDialog {
    #[cfg(any(windows, target_os = "macos"))]
    pub fn start(request: UiRequest) -> Option<Self> {
        let spec = request.spec.clone();
        // Plugin-provided names/titles cannot masquerade as host consent.
        let title = format!(
            "PlumSpirit — {} — {}",
            request.lease.plugin,
            if spec.save {
                "Save Selected File"
            } else {
                "Open Selected File"
            }
        );
        let (sender, result) = mpsc::sync_channel(1);
        let thread = Arc::new(AtomicU32::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::clone(&thread);
        let done = Arc::clone(&finished);
        let spawned = std::thread::Builder::new()
            .name("plumspirit-plugin-picker".into())
            .spawn(move || {
                struct Finished(Arc<AtomicBool>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _finished = Finished(done);
                #[cfg(windows)]
                // SAFETY: reads this native UI thread's identifier; no shared OS object.
                thread_id.store(
                    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
                    Ordering::Release,
                );
                #[cfg(not(windows))]
                let _ = thread_id;
                crate::diagnostics::native_dialog_shown();
                let mut dialog = rfd::FileDialog::new().set_title(title);
                for (name, extensions) in spec.filters {
                    dialog = dialog.add_filter(name, &extensions);
                }
                dialog = dialog.add_filter("All Files", &["*"]);
                if let Some(name) = spec.suggested_name {
                    dialog = dialog.set_file_name(name);
                }
                let path = if spec.save {
                    dialog.save_file()
                } else {
                    dialog.pick_file()
                };
                let _ = sender.try_send(path);
            });
        match spawned {
            Ok(_) => Some(Self {
                request: Some(request),
                result,
                thread,
                finished,
            }),
            Err(_) => {
                request.reject(BridgeError::Unavailable);
                None
            }
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn start(request: UiRequest) -> Option<Self> {
        request.reject(BridgeError::Unavailable);
        None
    }

    /// Called by the app every frame, never waits. An expired dialog remains
    /// in this slot until its native thread returns, preventing overlapping pickers.
    pub fn poll(&mut self, current: Option<ProjectStamp>, allowed: bool) -> bool {
        let Some(request) = &self.request else {
            return true;
        };
        if !allowed || !request.lease.matches_project(current) {
            request.lease.finish();
            self.close_native();
        }
        match self.result.try_recv() {
            Ok(path) => {
                if let Some(request) = self.request.take() {
                    let _ = request.respond(current, path);
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                if let Some(request) = self.request.take() {
                    request.reject(BridgeError::Unavailable);
                }
                true
            }
        }
    }

    pub fn plugin(&self) -> Option<&str> {
        self.request.as_ref().map(|r| r.lease.plugin.as_ref())
    }
    pub fn save(&self) -> bool {
        self.request.as_ref().is_some_and(|r| r.spec.save)
    }

    fn close_native(&self) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{HWND, LPARAM};
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumThreadWindows, PostMessageW, WM_CLOSE,
            };
            unsafe extern "system" fn close(hwnd: HWND, finished: LPARAM) -> i32 {
                // Re-check before touching windows: numeric thread IDs can be
                // reused after the old thread publishes its finished flag.
                let finished = unsafe { &*(finished as *const AtomicBool) };
                if finished.load(Ordering::Acquire) {
                    return 0;
                }
                // SAFETY: HWND is supplied by EnumThreadWindows for our own thread.
                unsafe {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                1
            }
            let id = self.thread.load(Ordering::Acquire);
            if id != 0 {
                // SAFETY: this identifier belongs to the live picker thread, not
                // another plugin or application. Posting does not block egui.
                unsafe {
                    EnumThreadWindows(id, Some(close), Arc::as_ptr(&self.finished) as LPARAM);
                }
            }
        }
    }
}

impl Drop for NativeFileDialog {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            request.lease.finish();
        }
        self.close_native();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::lua::bridge::{self, FileDialogSpec, JobLease};
    use std::time::{Duration, Instant};

    fn visible_windows(dialog: &NativeFileDialog) -> usize {
        use windows_sys::Win32::Foundation::{HWND, LPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, IsWindowVisible};
        unsafe extern "system" fn count(hwnd: HWND, output: LPARAM) -> i32 {
            // SAFETY: EnumThreadWindows calls synchronously; output is the live
            // stack counter below, and HWND is supplied for our own UI thread.
            if unsafe { IsWindowVisible(hwnd) } != 0 {
                unsafe {
                    *(output as *mut usize) += 1;
                }
            }
            1
        }
        if dialog.finished.load(Ordering::Acquire) {
            return 0;
        }
        let id = dialog.thread.load(Ordering::Acquire);
        let mut windows = 0usize;
        if id != 0 {
            // SAFETY: native test fixture owns this thread; no input or window
            // from another application is read or modified.
            unsafe {
                EnumThreadWindows(id, Some(count), &mut windows as *mut usize as LPARAM);
            }
        }
        windows
    }

    type Reply = std::thread::JoinHandle<Result<Option<PathBuf>, BridgeError>>;
    fn picker(id: &str) -> (NativeFileDialog, Reply) {
        let (client, requests) = bridge::channel(id);
        let lease = JobLease::fixture(id, None, Duration::from_secs(30), true);
        let worker = std::thread::spawn(move || {
            client.request(
                &lease,
                FileDialogSpec {
                    save: false,
                    filters: vec![],
                    suggested_name: None,
                },
            )
        });
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        let dialog = NativeFileDialog::start(request).expect("Native picker thread starts");
        let start = Instant::now();
        while visible_windows(&dialog) == 0 {
            assert!(
                !dialog.finished.load(Ordering::Acquire),
                "Picker returned before a native window appeared"
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Native picker did not appear"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        (dialog, worker)
    }

    #[test]
    #[ignore = "Manual actual Windows native dialogs: verify lease revocation and Drop close only their own UI thread"]
    fn actual_windows_revocation_and_drop_close_only_the_owned_native_picker() {
        let (mut first, first_reply) = picker("test.dialog.first");
        let (second, second_reply) = picker("test.dialog.second");
        assert!(visible_windows(&first) > 0 && visible_windows(&second) > 0);
        let start = Instant::now();
        while !first.poll(None, false)
            || !first_reply.is_finished()
            || !first.finished.load(Ordering::Acquire)
        {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "Revoked picker did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            first_reply.join().unwrap(),
            Err(BridgeError::Expired)
        ));
        assert!(first.finished.load(Ordering::Acquire));
        assert!(
            visible_windows(&second) > 0,
            "Revoking the first picker closed another thread's window"
        );
        let finished = Arc::clone(&second.finished);
        drop(second); // same native cleanup used when the application closes
        let start = Instant::now();
        while !finished.load(Ordering::Acquire) || !second_reply.is_finished() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "Dropped picker did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            second_reply.join().unwrap(),
            Err(BridgeError::Expired)
        ));
        println!(
            "Actual Windows: two visible native pickers; first revoked without closing second; second Drop closed its window; both worker requests expired without a selected path."
        );
    }
}
