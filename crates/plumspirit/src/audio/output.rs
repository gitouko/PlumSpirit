//! Output device, sample rate and buffer choices (v0.8 contract §134): only
//! what the device reports is offered; anything unavailable falls back to the
//! device's default (said in the log and the Audio Settings window).

use cpal::traits::{DeviceTrait, HostTrait};

/// Sample rates PlumSpirit offers when the device supports them.
pub const COMMON_RATES: [u32; 6] = [32_000, 44_100, 48_000, 88_200, 96_000, 192_000];
/// Buffer sizes offered (frames per callback) when the device has a range.
pub const COMMON_BUFFERS: [u32; 8] = [64, 128, 256, 512, 1_024, 2_048, 4_096, 8_192];

/// What the user chose (`None`: the system / device default).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputConfig {
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
}

/// One output device as the Audio Settings window lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    /// Offered sample rates the device supports (stereo or more channels).
    pub rates: Vec<u32>,
    pub default_rate: Option<u32>,
    /// Buffer sizes within the device's range (empty: the host decides).
    pub buffers: Vec<u32>,
}

pub fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "unnamed device".into())
}

/// The output devices of the default host (may be slow: call off the audio
/// thread, when the settings window opens or Refresh is pressed).
pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_output_device().map(|d| device_name(&d));
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in devices {
        let name = device_name(&d);
        let (mut rates, mut buffers) = (Vec::new(), Vec::new());
        if let Ok(cfgs) = d.supported_output_configs() {
            for c in cfgs {
                for r in COMMON_RATES {
                    if (c.min_sample_rate()..=c.max_sample_rate()).contains(&r)
                        && !rates.contains(&r)
                    {
                        rates.push(r);
                    }
                }
                if let cpal::SupportedBufferSize::Range { min, max } = *c.buffer_size() {
                    for b in COMMON_BUFFERS {
                        if (min..=max).contains(&b) && !buffers.contains(&b) {
                            buffers.push(b);
                        }
                    }
                }
            }
        }
        rates.sort_unstable();
        buffers.sort_unstable();
        let default_rate = d.default_output_config().ok().map(|c| c.sample_rate());
        out.push(DeviceInfo {
            is_default: default.as_deref() == Some(name.as_str()),
            name,
            rates,
            default_rate,
            buffers,
        });
    }
    out
}

/// The device and stream configuration for `cfg`, with notes for every
/// choice that could not be honoured.
pub fn resolve(
    cfg: &OutputConfig,
) -> Result<
    (
        cpal::Device,
        String,
        cpal::SampleFormat,
        cpal::StreamConfig,
        Vec<String>,
    ),
    String,
> {
    let host = cpal::default_host();
    let mut notes = Vec::new();
    let mut device = None;
    if let Some(want) = &cfg.device {
        if let Ok(list) = host.output_devices() {
            device = list.into_iter().find(|d| &device_name(d) == want);
        }
        if device.is_none() {
            notes.push(format!(
                "The audio device “{want}” was not found; using the default device."
            ));
        }
    }
    let device = match device {
        Some(d) => d,
        None => host
            .default_output_device()
            .ok_or_else(|| "No audio output device was found.".to_owned())?,
    };
    let name = device_name(&device);
    let default = device.default_output_config().map_err(|e| {
        format!(
            "The audio device could not be configured: {}.",
            super::engine::sentence(&e)
        )
    })?;
    let mut chosen = default;
    if let Some(rate) = cfg.sample_rate.filter(|&r| r != default.sample_rate()) {
        let range = device.supported_output_configs().ok().and_then(|cs| {
            let cs: Vec<_> = cs.collect();
            // Same sample format and channels as the default first.
            cs.iter()
                .find(|c| {
                    c.sample_format() == default.sample_format()
                        && c.channels() == default.channels()
                        && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
                })
                .or_else(|| {
                    cs.iter().find(|c| {
                        c.channels() >= 2
                            && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
                    })
                })
                .cloned()
        });
        match range {
            Some(r) => chosen = r.with_sample_rate(rate),
            None => notes.push(format!(
                "{rate} Hz is not supported by “{name}”; using {} Hz.",
                default.sample_rate()
            )),
        }
    }
    let format = chosen.sample_format();
    let buffer_range = *chosen.buffer_size();
    let mut config: cpal::StreamConfig = chosen.into();
    if let Some(b) = cfg.buffer_frames {
        match buffer_range {
            cpal::SupportedBufferSize::Range { min, max } if (min..=max).contains(&b) => {
                config.buffer_size = cpal::BufferSize::Fixed(b);
            }
            cpal::SupportedBufferSize::Range { min, max } => {
                let c = b.clamp(min, max);
                config.buffer_size = cpal::BufferSize::Fixed(c);
                notes.push(format!(
                    "A buffer of {b} frames is outside {min}–{max}; using {c}."
                ));
            }
            cpal::SupportedBufferSize::Unknown => {
                notes.push("This device does not report buffer sizes; its default is used.".into());
            }
        }
    }
    Ok((device, name, format, config, notes))
}

/// Latency of a buffer of `frames` at `rate`, in milliseconds.
pub fn latency_ms(frames: u32, rate: u32) -> f64 {
    f64::from(frames) * 1000.0 / f64::from(rate.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_is_frames_over_rate() {
        assert!((latency_ms(480, 48_000) - 10.0).abs() < 1e-9);
        assert_eq!(latency_ms(1, 0), 1000.0);
    }

    #[test]
    fn listing_and_resolving_never_panic_without_a_device() {
        // The sandbox has no audio device: an empty list and a clear error
        // (or a working default where one exists).
        let _ = list_devices();
        match resolve(&OutputConfig {
            device: Some("nonexistent device".into()),
            sample_rate: Some(12_345),
            buffer_frames: Some(1),
        }) {
            Ok((_, _, _, _, notes)) => assert!(notes.iter().any(|n| n.contains("not found"))),
            Err(e) => assert!(e.ends_with('.'), "{e}"),
        }
    }
}
