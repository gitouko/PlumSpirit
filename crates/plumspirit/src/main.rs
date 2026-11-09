//! PlumSpirit — an original Black MIDI editor (PlumSpirit v1.0.2).
//!
//! Startup is deliberately light: read settings, find the UI font, create the window.
//! No filesystem scanning beyond the font folders, no network, no external
//! plugins (the v0.8 built-in plugins are part of the program). The audio
//! device and the SoundFont are opened only when playback is first requested.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod assets;
mod audio;
mod diagnostics;
mod font_shape;
mod image_decode;
mod inspector;
mod lua;
mod piano_roll;
mod platform;
mod player;
mod recent;
mod renderer;
mod settings;
mod ui;

use std::path::PathBuf;

/// L-S45 (E0075): on Windows the process heap serialises every allocation
/// behind one lock and decommits freed segments while holding it, so
/// releasing a 4.5 GB project on the reaper thread stalled the window's own
/// allocations (130 ms frame gap). mimalloc keeps per-thread heaps and returns
/// memory to the system without a lock the UI thread waits for.
#[cfg(windows)]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The short release label shown on the start screen and in the window title.
/// `release_metadata_agrees_with_the_workspace_version` checks the label and PE resources.
macro_rules! release_label {
    () => {
        "v1.0.2"
    };
}
pub const RELEASE_LABEL: &str = release_label!();
/// The release name shown in the window title, the log, About and the logo
/// tooltip.
pub const RELEASE_NAME: &str = concat!("PlumSpirit ", release_label!());
use std::process::ExitCode;
use std::sync::Arc;

struct Args {
    file: Option<PathBuf>,
    dev_overlay: bool,
    /// `--bench-render N` (v0.8 §105): pan for N frames, log, exit.
    bench_frames: Option<usize>,
}

fn parse_args() -> Args {
    let mut args = Args {
        file: None,
        dev_overlay: false,
        bench_frames: None,
    };
    let mut it = std::env::args_os().skip(1).peekable();
    while let Some(a) = it.next() {
        if a == "--dev-overlay" {
            args.dev_overlay = true;
        } else if a == "--bench-render" {
            let n = it.peek().and_then(|v| v.to_str()?.parse::<usize>().ok());
            if n.is_some() {
                it.next();
            }
            args.bench_frames = Some(n.unwrap_or(600));
        } else if args.file.is_none() && !a.to_string_lossy().starts_with("--") {
            args.file = Some(PathBuf::from(a));
        }
    }
    args
}

fn install_panic_hook(log_path: Option<PathBuf>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("internal error (panic): {info}");
        log::logger().flush();
        let where_ = log_path
            .as_ref()
            .map(|p| format!("\n\nDetails were written to:\n{}", p.display()))
            .unwrap_or_default();
        platform::show_fatal_error(
            "PlumSpirit internal error",
            &format!("PlumSpirit hit an internal error and must close.{where_}"),
        );
        default(info);
    }));
}

/// Room for the window frame and title bar around the inner size.
const FRAME_ALLOWANCE: [f32; 2] = [16.0, 48.0];

/// Initial inner size and outer position: the saved (or default) size, shrunk to fit
/// the screen's work area when necessary (never below the minimum window size), and
/// centred in it. Without a known work area the size is used as is.
fn fit_window(saved: [f32; 2], work_area: Option<[f32; 4]>) -> ([f32; 2], Option<[f32; 2]>) {
    let Some([x, y, w, h]) = work_area else {
        return (saved, None);
    };
    let size = [
        saved[0]
            .min(w - FRAME_ALLOWANCE[0])
            .max(settings::MIN_WINDOW[0]),
        saved[1]
            .min(h - FRAME_ALLOWANCE[1])
            .max(settings::MIN_WINDOW[1]),
    ];
    let outer = [size[0] + FRAME_ALLOWANCE[0], size[1] + FRAME_ALLOWANCE[1]];
    let pos = [
        (x + (w - outer[0]) / 2.0).max(x),
        (y + (h - outer[1]) / 2.0).max(y),
    ];
    (size, Some(pos))
}

fn main() -> ExitCode {
    if let Some(code) = font_shape::entry() {
        return code;
    }
    let args = parse_args();
    let config_dir = settings::config_dir();
    let log_path = diagnostics::logging::init(config_dir.as_deref());
    install_panic_hook(log_path.clone());
    log::info!(
        "{RELEASE_NAME} ({}) starting on {}/{}",
        plumspirit_engine::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if let Some(p) = &log_path {
        log::info!("log file: {}", p.display());
    }
    let loaded = settings::load(config_dir.as_deref());
    for p in &loaded.problems {
        log::warn!("settings: {p}");
    }
    let font = ui::fonts::discover();
    let saved = loaded.settings.window_size.unwrap_or([1280.0, 800.0]);
    let work_area = platform::primary_work_area_points();
    let (size, position) = fit_window(saved, work_area);
    log::info!("screen work area {work_area:?} (points); window {size:?} at {position:?}");
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(RELEASE_NAME)
        .with_app_id("plumspirit")
        .with_inner_size(size)
        .with_min_inner_size(settings::MIN_WINDOW)
        .with_icon(Arc::new(assets::window_icon()));
    if let Some(p) = position {
        viewport = viewport.with_position(p);
    }
    // Renderer (v0.8): the saved choice, unless the last Vulkan start died
    // before drawing (marker left behind) — then OpenGL, saved (no loop).
    let mut settings = loaded.settings;
    let marker_left = config_dir.as_deref().is_some_and(renderer::marker_present);
    let plan = renderer::plan_start(settings.renderer, marker_left);
    let mut notices: Vec<String> = plan.notice.iter().cloned().collect();
    if plan.reset_setting {
        log::warn!("the last Vulkan start did not draw a frame; starting with OpenGL");
        settings.renderer = renderer::Backend::OpenGl;
        save_settings(config_dir.as_deref(), &settings);
    }
    if let Some(dir) = config_dir.as_deref() {
        renderer::clear_marker(dir);
    }
    let boot = std::rc::Rc::new(std::cell::RefCell::new(Some(app::Boot {
        config_dir: config_dir.clone(),
        settings: settings.clone(),
        font,
        startup_file: args.file,
        dev_overlay: args.dev_overlay,
        renderer: renderer::RunInfo::default(),
        bench_frames: args.bench_frames,
    })));
    let mut backend = plan.backend;
    let result = loop {
        let mut options = eframe::NativeOptions {
            viewport: viewport.clone(),
            renderer: match backend {
                renderer::Backend::OpenGl => eframe::Renderer::Glow,
                renderer::Backend::Vulkan => eframe::Renderer::Wgpu,
            },
            ..Default::default()
        };
        // The render benchmark measures frames, not the display's refresh.
        options.glow_options.vsync = args.bench_frames.is_none();
        if backend == renderer::Backend::Vulkan {
            options.wgpu_options = renderer::wgpu_options(settings.gpu_preference);
            if args.bench_frames.is_some() {
                options.wgpu_options.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
            }
            if let Some(dir) = config_dir.as_deref() {
                renderer::write_marker(dir);
            }
        }
        log::info!(
            "renderer: {} (GPU preference {})",
            backend.label(),
            settings.gpu_preference.label()
        );
        let run_notices = notices.clone();
        let run_boot = boot.clone();
        let running = backend;
        let started = std::time::Instant::now();
        let result = eframe::run_native(
            "PlumSpirit",
            options,
            Box::new(move |cc| {
                let mut b = run_boot
                    .borrow_mut()
                    .take()
                    .ok_or("PlumSpirit was already started")?;
                b.renderer = renderer::RunInfo {
                    running,
                    notices: run_notices,
                    started: Some(started),
                };
                Ok(Box::new(app::PlumSpiritApp::new(cc, b)))
            }),
        );
        match result {
            // A Vulkan start that failed before the app existed: OpenGL now.
            Err(e) if backend == renderer::Backend::Vulkan && boot.borrow().is_some() => {
                log::error!("the Vulkan renderer could not start: {e}");
                if let Some(dir) = config_dir.as_deref() {
                    renderer::clear_marker(dir);
                }
                notices.push(renderer::fallback_notice(&e.to_string()));
                settings.renderer = renderer::Backend::OpenGl;
                save_settings(config_dir.as_deref(), &settings);
                if let Some(b) = boot.borrow_mut().as_mut() {
                    b.settings.renderer = renderer::Backend::OpenGl;
                }
                backend = renderer::Backend::OpenGl;
            }
            r => break r,
        }
    };
    if let Some(dir) = config_dir.as_deref() {
        renderer::clear_marker(dir);
    }
    match result {
        Ok(()) => {
            log::info!("exited normally");
            log::logger().flush();
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("could not start the user interface: {e}");
            log::logger().flush();
            let where_ = log_path
                .as_ref()
                .map(|p| format!("\n\nDetails were written to:\n{}", p.display()))
                .unwrap_or_default();
            platform::show_fatal_error(
                "PlumSpirit could not start",
                &format!(
                    "PlumSpirit could not create its window ({e}).\n\nPlumSpirit draws with OpenGL 2.0 or newer. Updating the graphics driver usually fixes this.{where_}"
                ),
            );
            ExitCode::FAILURE
        }
    }
}

/// Saves the settings now (renderer fallbacks before the window exists).
fn save_settings(dir: Option<&std::path::Path>, s: &settings::Settings) {
    if let Some(dir) = dir
        && let Err(e) = settings::save(dir, s)
    {
        log::warn!("could not save the settings: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_fits_small_screens_and_is_centred() {
        // 1366x768 laptop with a 48 px taskbar.
        let (size, pos) = fit_window([1280.0, 800.0], Some([0.0, 0.0, 1366.0, 720.0]));
        assert_eq!(size, [1280.0, 672.0]);
        let [x, y] = pos.unwrap();
        assert!(x >= 0.0 && x + size[0] + FRAME_ALLOWANCE[0] <= 1366.0);
        assert!(y >= 0.0 && y + size[1] + FRAME_ALLOWANCE[1] <= 720.0);
        // Large screen: the saved size is kept.
        let (size, _) = fit_window([1280.0, 800.0], Some([0.0, 0.0, 2560.0, 1400.0]));
        assert_eq!(size, [1280.0, 800.0]);
        // Tiny screen: never below the minimum window size.
        let (size, _) = fit_window([1280.0, 800.0], Some([0.0, 0.0, 640.0, 400.0]));
        assert_eq!(size, settings::MIN_WINDOW);
        // Unknown work area: unchanged, no position.
        assert_eq!(fit_window([900.0, 700.0], None), ([900.0, 700.0], None));
    }

    #[test]
    fn release_metadata_agrees_with_the_workspace_version() {
        // v0.9 §007 / §311, beta1 §002: package text, title, Windows resources
        // and the manifest name one version; the description carries none (it
        // went stale at "v0.5" before). A pre-release "1.0.0-beta.N" is shown as
        // "v1.0-betaN" and its Windows numeric version is 1,0,0,N. 1.0.0 is the
        // owner's "v1.0-Launch" (launch contract §000: an engineering milestone,
        // not the public release), numeric 1,0,0,0.
        let v = env!("CARGO_PKG_VERSION");
        let (core, pre) = v.split_once('-').unwrap_or((v, ""));
        let mut it = core.split('.');
        let (maj, min, pat) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
        let (tag, build) = match pre.split_once('.') {
            Some((kind, n)) => (format!("-{kind}{n}"), n.to_owned()),
            None if pre.is_empty() && (maj, min, pat) == ("1", "0", "0") => {
                ("-Launch".to_owned(), "0".to_owned())
            }
            None if pre.is_empty() => (String::new(), "0".to_owned()),
            None => panic!("pre-release {pre:?} needs a number"),
        };
        let short = if pre.is_empty() && pat != "0" {
            format!("v{core}")
        } else {
            format!("v{maj}.{min}{tag}")
        };
        assert_eq!(RELEASE_LABEL, short);
        let label = format!("PlumSpirit {short}");
        assert_eq!(RELEASE_NAME, label);
        let desc = env!("CARGO_PKG_DESCRIPTION");
        assert!(
            !desc.contains(" v0.") && !desc.contains("Prototype"),
            "{desc}"
        );
        let rc = include_str!("../windows/plumspirit.rc");
        let commas = format!("{maj},{min},{pat},{build}");
        assert!(
            rc.contains(&format!("FILEVERSION     {commas}")),
            "rc FILEVERSION"
        );
        assert!(
            rc.contains(&format!("PRODUCTVERSION  {commas}")),
            "rc PRODUCTVERSION"
        );
        assert!(
            rc.contains(&format!("\"FileVersion\",      \"{v}\"")),
            "rc FileVersion"
        );
        assert!(
            rc.contains(&format!("\"ProductVersion\",   \"{v} ({label})\"")),
            "rc ProductVersion"
        );
        assert!(rc.contains(&format!("({label})\"")), "rc FileDescription");
        let man = include_str!("../windows/plumspirit.manifest");
        assert!(
            man.contains(&format!("version=\"{maj}.{min}.{pat}.{build}\"")),
            "manifest version"
        );
        assert!(
            man.contains(&format!("({label})</description>")),
            "manifest description"
        );
        for stale in [
            "v0.5 ~",
            "v0.6 ~",
            "v0.7 ~",
            "v0.8 ~",
            "v0.9 ~",
            "Prototype",
        ] {
            assert!(!rc.contains(stale) && !man.contains(stale), "{stale}");
        }
    }
}
