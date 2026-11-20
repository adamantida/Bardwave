//! Output devices: listing, picking by name and routing the preview.
//!
//! The selection logic is pure (it works on [`OutputDeviceInfo`] lists), so it
//! is tested without a sound card. Only [`list_output_devices`] and the engine
//! talk to cpal.
//!
//! Platform notes: enumeration and name matching are written against the cpal
//! API shared by all hosts and exercised on macOS (CoreAudio) only; the WASAPI
//! (Windows) and ALSA/JACK (Linux) hosts are unverified. Device names are the
//! host's own strings, so a name saved on one OS means nothing on another, and
//! on some hosts two devices can share a name (the first one wins).

use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

use crate::config::AudioConfig;

/// Sample rates worth showing for a device, in Hz.
const COMMON_RATES: [u32; 6] = [22_050, 44_100, 48_000, 88_200, 96_000, 192_000];

/// One output device as the settings UI and the picker see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDeviceInfo {
    /// The host's name for the device (what `[audio] output_device` stores).
    pub name: String,
    /// Most output channels any of its modes offers (`0` when unknown).
    pub channels: u16,
    /// Supported sample rate ranges `(min, max)` in Hz; empty when unknown.
    pub sample_rate_ranges: Vec<(u32, u32)>,
    /// Whether this is the system default output.
    pub is_default: bool,
}

impl OutputDeviceInfo {
    /// A device known only by its name (cheap to build, used by the picker).
    pub fn named(name: impl Into<String>, is_default: bool) -> Self {
        Self {
            name: name.into(),
            channels: 0,
            sample_rate_ranges: Vec::new(),
            is_default,
        }
    }

    /// Whether some mode of the device runs at exactly `hz`.
    pub fn supports_rate(&self, hz: u32) -> bool {
        self.sample_rate_ranges
            .iter()
            .any(|(min, max)| (*min..=*max).contains(&hz))
    }

    /// The common rates (see [`COMMON_RATES`]) the device supports, ascending.
    pub fn common_rates(&self) -> Vec<u32> {
        COMMON_RATES
            .into_iter()
            .filter(|hz| self.supports_rate(*hz))
            .collect()
    }

    /// One line for a tooltip: `2 ch · 44.1 / 48 kHz`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.channels > 0 {
            parts.push(format!("{} ch", self.channels));
        }
        let rates = self.common_rates();
        if !rates.is_empty() {
            let list = rates
                .iter()
                .map(|hz| {
                    let khz = *hz as f32 / 1000.0;
                    if khz.fract() == 0.0 {
                        format!("{khz:.0}")
                    } else {
                        format!("{khz:.1}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" / ");
            parts.push(format!("{list} kHz"));
        }
        if self.is_default {
            parts.push("system default".to_string());
        }
        if parts.is_empty() {
            "no details".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Result of looking a configured device name up in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePick {
    /// No device was asked for: use the system default.
    Default,
    /// The device at this index of the list.
    Named(usize),
    /// A name was asked for but is not in the list: use the system default and
    /// tell the operator.
    Missing(String),
}

impl DevicePick {
    /// Why the default is being used instead of what was configured.
    pub fn fallback_message(&self) -> Option<String> {
        match self {
            DevicePick::Missing(name) => Some(format!(
                "output device \"{name}\" not found; using the system default"
            )),
            _ => None,
        }
    }
}

/// Normalise a configured name: `None` for absent or blank.
fn wanted_name(wanted: Option<&str>) -> Option<&str> {
    wanted.map(str::trim).filter(|name| !name.is_empty())
}

/// Index of the device called `name`: exact match first, then ignoring case and
/// surrounding whitespace.
fn find_by_name(devices: &[OutputDeviceInfo], name: &str) -> Option<usize> {
    devices
        .iter()
        .position(|device| device.name == name)
        .or_else(|| {
            devices
                .iter()
                .position(|device| device.name.trim().eq_ignore_ascii_case(name))
        })
}

/// Choose a device from `devices` for a configured name.
///
/// `None` or a blank name is [`DevicePick::Default`]; a name that is found
/// is [`DevicePick::Named`]; any other name is [`DevicePick::Missing`], which
/// the caller treats as the default plus a message.
pub fn pick_output_device(devices: &[OutputDeviceInfo], wanted: Option<&str>) -> DevicePick {
    match wanted_name(wanted) {
        None => DevicePick::Default,
        Some(name) => match find_by_name(devices, name) {
            Some(index) => DevicePick::Named(index),
            None => DevicePick::Missing(name.to_string()),
        },
    }
}

/// Where previews are played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewPlan {
    /// On the main output, as before (nothing configured, or the same device).
    SharedWithMain,
    /// On the device at this index of the list, independently of the main one.
    Separate(usize),
    /// A preview device was configured but cannot be used; previews play on the
    /// main output and the message says why.
    Unavailable(String),
}

impl PreviewPlan {
    /// The reason previews are not on their own device, when it is a problem.
    pub fn message(&self) -> Option<&str> {
        match self {
            PreviewPlan::Unavailable(message) => Some(message),
            _ => None,
        }
    }
}

/// Decide where previews go, given the name of the device the main output
/// actually opened.
///
/// A preview device equal to the main one (by name) is [`PreviewPlan::SharedWithMain`]:
/// opening the same device twice fails on exclusive-mode hosts and gains
/// nothing.
pub fn plan_preview_route(
    devices: &[OutputDeviceInfo],
    main_name: &str,
    preview_wanted: Option<&str>,
) -> PreviewPlan {
    let Some(name) = wanted_name(preview_wanted) else {
        return PreviewPlan::SharedWithMain;
    };
    match find_by_name(devices, name) {
        None => PreviewPlan::Unavailable(format!(
            "preview device \"{name}\" not found; previews play on the main output"
        )),
        Some(index) if devices[index].name == main_name => PreviewPlan::SharedWithMain,
        Some(index) => PreviewPlan::Separate(index),
    }
}

/// What the `[audio]` section selects, as the engine consumes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSelection {
    /// Main output device name; `None` is the system default.
    pub output: Option<String>,
    /// Preview device name; `None` is the main output.
    pub preview: Option<String>,
}

impl OutputSelection {
    /// Read the selection from `[audio]`. Blank names count as unset.
    pub fn from_audio_config(audio: &AudioConfig) -> Self {
        let clean = |name: &Option<String>| wanted_name(name.as_deref()).map(str::to_string);
        Self {
            output: clean(&audio.output_device),
            preview: clean(&audio.preview_device),
        }
    }

    /// Write the selection into `[audio]`.
    pub fn apply_to(&self, audio: &mut AudioConfig) {
        audio.output_device = self.output.clone();
        audio.preview_device = self.preview.clone();
    }
}

/// Describe a device in the way the engine needs: name and default flag.
pub(crate) fn name_of(device: &cpal::Device) -> String {
    device
        .name()
        .unwrap_or_else(|_| "<unknown device>".to_string())
}

/// The host's output devices with their names, in host order, plus the name of
/// the system default. Cheap: no configuration queries.
pub(crate) fn output_devices_by_name() -> (Vec<cpal::Device>, Vec<OutputDeviceInfo>) {
    let host = cpal::default_host();
    let default_name = host.default_output_device().map(|d| name_of(&d));
    let devices: Vec<cpal::Device> = match host.output_devices() {
        Ok(iter) => iter.collect(),
        Err(e) => {
            tracing::warn!("output_devices() failed: {e}");
            Vec::new()
        }
    };
    let infos = devices
        .iter()
        .map(|device| {
            let name = name_of(device);
            let is_default = default_name.as_deref() == Some(name.as_str());
            OutputDeviceInfo::named(name, is_default)
        })
        .collect();
    (devices, infos)
}

/// List the output devices of the default host with their capabilities.
///
/// This asks every device for its configurations, which can take a moment (and
/// block on a sleeping Bluetooth device), so call it from a worker thread. A
/// device that refuses to describe itself is still listed, by name.
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, String> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().map(|d| name_of(&d));
    let devices = host
        .output_devices()
        .map_err(|e| format!("could not list output devices: {e}"))?;

    let mut list = Vec::new();
    for device in devices {
        let name = name_of(&device);
        let mut info =
            OutputDeviceInfo::named(name.clone(), default_name.as_deref() == Some(name.as_str()));
        match device.supported_output_configs() {
            Ok(configs) => {
                for config in configs {
                    info.channels = info.channels.max(config.channels());
                    info.sample_rate_ranges
                        .push((config.min_sample_rate().0, config.max_sample_rate().0));
                }
            }
            Err(e) => tracing::debug!("supported_output_configs() for {name} failed: {e}"),
        }
        info.sample_rate_ranges.sort_unstable();
        info.sample_rate_ranges.dedup();
        list.push(info);
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<OutputDeviceInfo> {
        vec![
            OutputDeviceInfo::named("Built-in Output", true),
            OutputDeviceInfo::named("USB Headphones", false),
            OutputDeviceInfo::named("HDMI", false),
        ]
    }

    #[test]
    fn nothing_configured_is_the_default() {
        assert_eq!(pick_output_device(&list(), None), DevicePick::Default);
        assert_eq!(pick_output_device(&list(), Some("  ")), DevicePick::Default);
    }

    #[test]
    fn an_exact_name_is_found() {
        assert_eq!(
            pick_output_device(&list(), Some("USB Headphones")),
            DevicePick::Named(1)
        );
    }

    #[test]
    fn case_and_padding_are_forgiven() {
        assert_eq!(
            pick_output_device(&list(), Some("  usb headphones ")),
            DevicePick::Named(1)
        );
    }

    #[test]
    fn an_exact_match_beats_a_case_insensitive_one() {
        let devices = vec![
            OutputDeviceInfo::named("speakers", false),
            OutputDeviceInfo::named("Speakers", false),
        ];
        assert_eq!(
            pick_output_device(&devices, Some("Speakers")),
            DevicePick::Named(1)
        );
    }

    #[test]
    fn a_missing_device_falls_back_with_a_message() {
        let pick = pick_output_device(&list(), Some("Bluetooth Speaker"));
        assert_eq!(pick, DevicePick::Missing("Bluetooth Speaker".to_string()));
        let message = pick.fallback_message().unwrap();
        assert!(message.contains("Bluetooth Speaker"));
        assert!(message.contains("system default"));
        assert!(DevicePick::Default.fallback_message().is_none());
        assert!(DevicePick::Named(0).fallback_message().is_none());
    }

    #[test]
    fn an_empty_list_never_panics() {
        assert_eq!(pick_output_device(&[], None), DevicePick::Default);
        assert_eq!(
            pick_output_device(&[], Some("x")),
            DevicePick::Missing("x".to_string())
        );
    }

    #[test]
    fn the_preview_defaults_to_the_main_output() {
        assert_eq!(
            plan_preview_route(&list(), "Built-in Output", None),
            PreviewPlan::SharedWithMain
        );
    }

    #[test]
    fn a_different_preview_device_is_separate() {
        assert_eq!(
            plan_preview_route(&list(), "Built-in Output", Some("USB Headphones")),
            PreviewPlan::Separate(1)
        );
    }

    #[test]
    fn the_same_device_is_not_opened_twice() {
        assert_eq!(
            plan_preview_route(&list(), "Built-in Output", Some("built-in output")),
            PreviewPlan::SharedWithMain
        );
    }

    #[test]
    fn a_missing_preview_device_falls_back_to_the_main_output() {
        let plan = plan_preview_route(&list(), "Built-in Output", Some("Gone"));
        assert!(plan.message().unwrap().contains("Gone"));
        assert!(matches!(plan, PreviewPlan::Unavailable(_)));
        assert!(PreviewPlan::SharedWithMain.message().is_none());
    }

    #[test]
    fn rates_and_summary_describe_a_device() {
        let device = OutputDeviceInfo {
            name: "X".into(),
            channels: 2,
            sample_rate_ranges: vec![(44_100, 48_000), (96_000, 96_000)],
            is_default: true,
        };
        assert!(device.supports_rate(48_000));
        assert!(!device.supports_rate(88_200));
        assert_eq!(device.common_rates(), vec![44_100, 48_000, 96_000]);
        assert_eq!(
            device.summary(),
            "2 ch · 44.1 / 48 / 96 kHz · system default"
        );
        assert_eq!(OutputDeviceInfo::named("Y", false).summary(), "no details");
    }

    #[test]
    fn the_selection_round_trips_through_audio_config() {
        let selection = OutputSelection {
            output: Some("USB DAC".into()),
            preview: Some("Headphones".into()),
        };
        let mut audio = AudioConfig::default();
        selection.apply_to(&mut audio);
        assert_eq!(OutputSelection::from_audio_config(&audio), selection);

        audio.output_device = Some("   ".into());
        audio.preview_device = None;
        assert_eq!(
            OutputSelection::from_audio_config(&audio),
            OutputSelection::default()
        );
    }
}
