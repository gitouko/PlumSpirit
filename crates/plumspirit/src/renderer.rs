//! Render backends (v0.8 contract §098 – §105, §250, §251).
//!
//! PlumSpirit draws only egui meshes and textures, so it runs unchanged on
//! both eframe renderers:
//!
//! * **OpenGL** (glow) — the default: small, and works on old / integrated
//!   graphics.
//! * **Vulkan** — eframe's wgpu renderer with wgpu restricted to its Vulkan
//!   backend (the instance is created with `Backends::VULKAN` only, so this
//!   really is Vulkan, never a silent GL / DX12 substitute).
//!
//! The backend is chosen before the window exists, so a change applies at
//! the next start ("Restart Required"). Safety net (§103 / §251):
//!
//! * a Vulkan start that fails cleanly (no Vulkan driver, no suitable GPU)
//!   falls back to OpenGL in the same run, saves OpenGL and says so;
//! * a Vulkan start that crashes the process is caught at the next start: a
//!   marker file is written before Vulkan starts and removed after the first
//!   frame has been drawn; a marker left behind means the last Vulkan start
//!   died, so PlumSpirit starts with OpenGL, saves OpenGL and says so — no
//!   crash loop.
//!
//! GPU preference (Automatic / Integrated / Dedicated) picks the adapter by
//! its type among the Vulkan adapters that can draw to the window; when the
//! requested type is missing the best other one is used and the fallback is
//! reported.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// The renderer setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    OpenGl,
    Vulkan,
}

impl Backend {
    #[cfg(test)]
    pub const ALL: [Backend; 2] = [Backend::OpenGl, Backend::Vulkan];

    pub fn as_str(self) -> &'static str {
        match self {
            Backend::OpenGl => "opengl",
            Backend::Vulkan => "vulkan",
        }
    }

    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "opengl" | "gl" | "glow" => Some(Backend::OpenGl),
            "vulkan" | "wgpu" => Some(Backend::Vulkan),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backend::OpenGl => "OpenGL",
            Backend::Vulkan => "Vulkan",
        }
    }
}

/// Which GPU the Vulkan renderer asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuPreference {
    #[default]
    Automatic,
    Integrated,
    Dedicated,
}

impl GpuPreference {
    #[cfg(test)]
    pub const ALL: [GpuPreference; 3] = [
        GpuPreference::Automatic,
        GpuPreference::Integrated,
        GpuPreference::Dedicated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GpuPreference::Automatic => "automatic",
            GpuPreference::Integrated => "integrated",
            GpuPreference::Dedicated => "dedicated",
        }
    }

    pub fn parse(s: &str) -> Option<GpuPreference> {
        match s.trim().to_ascii_lowercase().as_str() {
            "automatic" | "auto" => Some(GpuPreference::Automatic),
            "integrated" => Some(GpuPreference::Integrated),
            "dedicated" | "discrete" => Some(GpuPreference::Dedicated),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GpuPreference::Automatic => "Automatic",
            GpuPreference::Integrated => "Integrated GPU",
            GpuPreference::Dedicated => "Dedicated GPU",
        }
    }
}

/// File (in the settings folder) present while a Vulkan start has not yet
/// drawn its first frame.
pub const PENDING_MARKER: &str = "renderer-vulkan-pending";

pub fn marker_present(dir: &Path) -> bool {
    dir.join(PENDING_MARKER).exists()
}

pub fn write_marker(dir: &Path) {
    if let Err(e) = std::fs::write(dir.join(PENDING_MARKER), b"vulkan start in progress\n") {
        log::warn!("could not write the renderer marker: {e}");
    }
}

pub fn clear_marker(dir: &Path) {
    let p = dir.join(PENDING_MARKER);
    if p.exists()
        && let Err(e) = std::fs::remove_file(&p)
    {
        log::warn!("could not remove the renderer marker: {e}");
    }
}

/// How this run starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartPlan {
    pub backend: Backend,
    /// Shown to the user once the window is up.
    pub notice: Option<String>,
    /// Save OpenGL as the setting (the saved backend cannot be used).
    pub reset_setting: bool,
}

/// The start decision from the saved setting and whether the last Vulkan
/// start left its marker behind (it crashed before drawing a frame).
pub fn plan_start(setting: Backend, marker_left: bool) -> StartPlan {
    match (setting, marker_left) {
        (Backend::Vulkan, true) => StartPlan {
            backend: Backend::OpenGl,
            notice: Some(
                "The last start with the Vulkan renderer did not finish, so PlumSpirit started with OpenGL and saved OpenGL as the renderer. You can choose Vulkan again in Settings → Rendering / Onion → Renderer."
                    .into(),
            ),
            reset_setting: true,
        },
        (b, _) => StartPlan {
            backend: b,
            notice: None,
            reset_setting: false,
        },
    }
}

/// The notice after a Vulkan start failed cleanly and OpenGL took over.
pub fn fallback_notice(error: &str) -> String {
    format!(
        "The Vulkan renderer could not start ({}), so PlumSpirit started with OpenGL and saved OpenGL as the renderer.",
        error.trim_end_matches('.')
    )
}

/// Adapter classes (wgpu's device types).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    Virtual,
    Cpu,
    Other,
}

impl From<wgpu::DeviceType> for AdapterKind {
    fn from(t: wgpu::DeviceType) -> Self {
        match t {
            wgpu::DeviceType::DiscreteGpu => AdapterKind::Discrete,
            wgpu::DeviceType::IntegratedGpu => AdapterKind::Integrated,
            wgpu::DeviceType::VirtualGpu => AdapterKind::Virtual,
            wgpu::DeviceType::Cpu => AdapterKind::Cpu,
            wgpu::DeviceType::Other => AdapterKind::Other,
        }
    }
}

impl AdapterKind {
    pub fn label(self) -> &'static str {
        match self {
            AdapterKind::Discrete => "dedicated GPU",
            AdapterKind::Integrated => "integrated GPU",
            AdapterKind::Virtual => "virtual GPU",
            AdapterKind::Cpu => "software renderer (CPU)",
            AdapterKind::Other => "other",
        }
    }
}

/// Picks an adapter for `pref` among `kinds` (enumeration order): the first
/// adapter of the most wanted type — Automatic and Dedicated prefer a
/// dedicated GPU, Integrated an integrated one; hardware before virtual
/// before software. Returns the index and, when the requested type was not
/// available, a sentence saying what was used instead.
pub fn choose_adapter(
    pref: GpuPreference,
    kinds: &[AdapterKind],
) -> Option<(usize, Option<String>)> {
    use AdapterKind::*;
    let order: [AdapterKind; 5] = match pref {
        GpuPreference::Integrated => [Integrated, Discrete, Virtual, Other, Cpu],
        _ => [Discrete, Integrated, Virtual, Other, Cpu],
    };
    let (i, kind) = order
        .iter()
        .find_map(|k| kinds.iter().position(|x| x == k).map(|i| (i, *k)))?;
    let wanted = match pref {
        GpuPreference::Automatic => None,
        GpuPreference::Integrated => Some(Integrated),
        GpuPreference::Dedicated => Some(Discrete),
    };
    let note = wanted.filter(|w| *w != kind).map(|w| {
        format!(
            "No {} was found for Vulkan; using the {} instead.",
            w.label(),
            kind.label()
        )
    });
    Some((i, note))
}

/// The adapter fallback sentence of this run (set by the adapter selector).
static ADAPTER_NOTE: Mutex<Option<String>> = Mutex::new(None);

pub fn take_adapter_note() -> Option<String> {
    ADAPTER_NOTE.lock().ok().and_then(|mut n| n.take())
}

/// eframe's wgpu configuration for the Vulkan renderer with GPU preference
/// `pref`.
pub fn wgpu_options(pref: GpuPreference) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    // Vulkan only: never a silent substitute.
    setup.instance_descriptor.backends = wgpu::Backends::VULKAN;
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        let usable: Vec<&wgpu::Adapter> = adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .collect();
        let kinds: Vec<AdapterKind> = usable
            .iter()
            .map(|a| a.get_info().device_type.into())
            .collect();
        let (i, note) = choose_adapter(pref, &kinds)
            .ok_or_else(|| "no Vulkan GPU can draw to this window".to_owned())?;
        if let Some(n) = note {
            log::warn!("{n}");
            if let Ok(mut slot) = ADAPTER_NOTE.lock() {
                *slot = Some(n);
            }
        }
        Ok(usable[i].clone())
    }));
    // The default asks for 8,192² textures; older GPUs may offer less. Ask for
    // what the adapter has (PlumSpirit's tiles are 256 × 128).
    setup.device_descriptor = Arc::new(|adapter| {
        let have = adapter.limits();
        wgpu::DeviceDescriptor {
            label: Some("PlumSpirit wgpu device"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: have.max_texture_dimension_2d.min(8192),
                ..wgpu::Limits::downlevel_defaults().using_resolution(have.clone())
            },
            ..Default::default()
        }
    });
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// What the running renderer is (Advanced / Diagnostics, §104).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererInfo {
    pub backend: Backend,
    /// e.g. "Vulkan (wgpu)" or "OpenGL 4.6 (glow)".
    pub api: String,
    pub adapter: String,
    /// Adapter type when known.
    pub kind: String,
    pub driver: String,
}

impl RendererInfo {
    /// One line for the diagnostics overlay.
    pub fn line(&self) -> String {
        let mut s = format!("{} · {}", self.api, self.adapter);
        if !self.kind.is_empty() {
            s.push_str(&format!(" ({})", self.kind));
        }
        if !self.driver.is_empty() {
            s.push_str(&format!(" · driver {}", self.driver));
        }
        s
    }

    /// Reads it from the creation context of the running renderer.
    pub fn from_creation(cc: &eframe::CreationContext<'_>) -> RendererInfo {
        if let Some(rs) = &cc.wgpu_render_state {
            let i = rs.adapter.get_info();
            let driver = [i.driver.trim(), i.driver_info.trim()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            return RendererInfo {
                backend: Backend::Vulkan,
                api: format!("{:?} (wgpu)", i.backend),
                adapter: i.name.clone(),
                kind: AdapterKind::from(i.device_type).label().to_owned(),
                driver,
            };
        }
        if let Some(gl) = &cc.gl {
            use eframe::glow::HasContext;
            // SAFETY: plain string queries on the current GL context, on the
            // thread that owns it, during app creation.
            let (version, renderer, vendor) = unsafe {
                (
                    gl.get_parameter_string(eframe::glow::VERSION),
                    gl.get_parameter_string(eframe::glow::RENDERER),
                    gl.get_parameter_string(eframe::glow::VENDOR),
                )
            };
            return RendererInfo {
                backend: Backend::OpenGl,
                api: "OpenGL (glow)".into(),
                adapter: format!("{} {}", vendor.trim(), renderer.trim())
                    .trim()
                    .to_owned(),
                kind: String::new(),
                driver: version.trim().to_owned(),
            };
        }
        RendererInfo {
            api: "unknown".into(),
            ..RendererInfo::default()
        }
    }
}

/// What `main` tells the app about the renderer of this run.
#[derive(Clone, Debug, Default)]
pub struct RunInfo {
    /// The backend this run uses.
    pub running: Backend,
    /// Sentences to show once the window is up (fallbacks).
    pub notices: Vec<String>,
    /// v0.8 §210: when `main` asked eframe for the window and GPU context
    /// (the first drawn frame reports "Started … renderer" with the time).
    pub started: Option<std::time::Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_values_round_trip_and_unknown_values_are_refused() {
        for b in Backend::ALL {
            assert_eq!(Backend::parse(b.as_str()), Some(b));
        }
        for g in GpuPreference::ALL {
            assert_eq!(GpuPreference::parse(g.as_str()), Some(g));
        }
        assert_eq!(Backend::parse("Vulkan"), Some(Backend::Vulkan));
        assert_eq!(Backend::parse("dx12"), None);
        assert_eq!(GpuPreference::parse("fastest"), None);
        assert_eq!(Backend::default(), Backend::OpenGl);
    }

    #[test]
    fn a_vulkan_start_that_never_drew_falls_back_to_opengl_once() {
        // §251: Vulkan chosen, previous start fine → Vulkan.
        let p = plan_start(Backend::Vulkan, false);
        assert_eq!(
            (p.backend, p.reset_setting, p.notice.is_none()),
            (Backend::Vulkan, false, true)
        );
        // The marker was left behind → OpenGL now, setting reset (no loop).
        let p = plan_start(Backend::Vulkan, true);
        assert_eq!((p.backend, p.reset_setting), (Backend::OpenGl, true));
        assert!(p.notice.unwrap().contains("OpenGL"));
        // OpenGL ignores a stale marker.
        let p = plan_start(Backend::OpenGl, true);
        assert_eq!((p.backend, p.reset_setting), (Backend::OpenGl, false));
        assert!(fallback_notice("no adapter.").contains("(no adapter)"));
    }

    #[test]
    fn the_marker_lives_in_the_settings_folder() {
        let dir = std::env::temp_dir().join(format!("ps-renderer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!marker_present(&dir));
        write_marker(&dir);
        assert!(marker_present(&dir));
        clear_marker(&dir);
        assert!(!marker_present(&dir));
        clear_marker(&dir); // twice: harmless
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adapters_are_chosen_by_preference_with_a_clear_fallback() {
        use AdapterKind::*;
        let both = [Integrated, Discrete, Cpu];
        assert_eq!(
            choose_adapter(GpuPreference::Automatic, &both),
            Some((1, None))
        );
        assert_eq!(
            choose_adapter(GpuPreference::Dedicated, &both),
            Some((1, None))
        );
        assert_eq!(
            choose_adapter(GpuPreference::Integrated, &both),
            Some((0, None))
        );
        // Missing class → the best other one, with a sentence.
        let (i, note) = choose_adapter(GpuPreference::Dedicated, &[Cpu, Integrated]).unwrap();
        assert_eq!(i, 1);
        assert!(note.unwrap().contains("No dedicated GPU"));
        let (i, note) = choose_adapter(GpuPreference::Integrated, &[Discrete]).unwrap();
        assert_eq!(i, 0);
        assert!(note.unwrap().contains("No integrated GPU"));
        // Software only (e.g. lavapipe): still usable; Automatic says nothing.
        assert_eq!(
            choose_adapter(GpuPreference::Automatic, &[Cpu]),
            Some((0, None))
        );
        // Two of a kind: the first enumerated.
        assert_eq!(
            choose_adapter(GpuPreference::Automatic, &[Discrete, Discrete]),
            Some((0, None))
        );
        assert_eq!(choose_adapter(GpuPreference::Automatic, &[]), None);
    }
}
