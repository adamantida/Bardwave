//! The AUDIO OUTPUT section of the network column: which sound card plays the
//! mix (OUTPUT) and which one plays sound previews (PREVIEW, usually the
//! headphones), with a status line for each.
//!
//! The device list is read by a short-lived worker thread and comes back
//! through a channel, so the render thread never waits for the sound system
//! (listing devices can stall on a sleeping Bluetooth speaker). A change is
//! applied at once: the engine reopens its streams without restarting the app,
//! falls back to the system default when a device is gone, and the status line
//! shows what happened. The selection is saved in `[audio]` with the config.
//!
//! Buffer size, sample rate and channel count are the existing `[audio]`
//! settings; they are shown here (read-only) next to the active device.

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use egui::Ui;

use crate::audio::devices::{
    list_output_devices, pick_output_device, DevicePick, OutputDeviceInfo, OutputSelection,
};
use crate::audio::engine::{LocalOutputState, LocalOutputStatus};
use crate::config::AudioConfig;
use crate::gui::app::SoundSharedApp;
use crate::gui::settings_extras::kit;
use crate::gui::theme::{micro_label, Metrics, Palette};
use crate::gui::widgets::{elide, glyph, ConsoleButton};

/// What a device dropdown can select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceChoice {
    /// The value stored in `[audio]`: `None` is "system default" (output) or
    /// "same as output" (preview).
    pub value: Option<String>,
    /// Text shown in the list.
    pub label: String,
}

/// Which of the two dropdowns an entry list is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The main output.
    Output,
    /// The preview (headphone) output.
    Preview,
}

impl Slot {
    fn none_label(self, devices: &[OutputDeviceInfo]) -> String {
        match self {
            Slot::Output => match devices.iter().find(|d| d.is_default) {
                Some(device) => format!("System default ({})", device.name),
                None => "System default".to_string(),
            },
            Slot::Preview => "Same as output".to_string(),
        }
    }
}

/// The entries of one dropdown: the "none" choice, every listed device, and the
/// configured device itself when it is not in the list (unplugged), so the
/// operator sees what is configured instead of a silent switch to something
/// else.
pub fn device_choices(
    slot: Slot,
    devices: &[OutputDeviceInfo],
    configured: Option<&str>,
) -> Vec<DeviceChoice> {
    let mut choices = vec![DeviceChoice {
        value: None,
        label: slot.none_label(devices),
    }];
    choices.extend(devices.iter().map(|device| DeviceChoice {
        value: Some(device.name.clone()),
        label: device.name.clone(),
    }));
    if let Some(name) = configured.map(str::trim).filter(|n| !n.is_empty()) {
        if matches!(
            pick_output_device(devices, Some(name)),
            DevicePick::Missing(_)
        ) {
            choices.push(DeviceChoice {
                value: Some(name.to_string()),
                label: format!("{name} (not connected)"),
            });
        }
    }
    choices
}

/// The text of the closed dropdown for the configured value.
pub fn selected_label(
    slot: Slot,
    devices: &[OutputDeviceInfo],
    configured: Option<&str>,
) -> String {
    let choices = device_choices(slot, devices, configured);
    let Some(name) = configured.map(str::trim).filter(|n| !n.is_empty()) else {
        return choices[0].label.clone();
    };
    choices
        .iter()
        .find(|choice| {
            choice
                .value
                .as_deref()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(name))
        })
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| name.to_string())
}

/// One line for the main output status: `Active: ...` or `Error: ...`.
pub fn output_status_line(status: &LocalOutputStatus) -> (String, bool) {
    let mut text = match &status.state {
        LocalOutputState::Running => format!(
            "Active: {} - {} Hz, {} ch",
            status.device.as_deref().unwrap_or("unknown device"),
            status.sample_rate,
            status.channels
        ),
        LocalOutputState::Headless => "Error: no output device opened (network only)".to_string(),
        LocalOutputState::Failed(message) => format!("Error: {message}"),
        LocalOutputState::Stopped => "Output not started".to_string(),
    };
    let error = !matches!(
        status.state,
        LocalOutputState::Running | LocalOutputState::Stopped
    );
    if let Some(reason) = &status.reason {
        // A failed state already carries its reason as the message.
        if !matches!(status.state, LocalOutputState::Failed(_)) {
            text.push_str(" (");
            text.push_str(reason);
            text.push(')');
        }
    }
    (text, error)
}

/// One line for the preview status.
pub fn preview_status_line(status: &LocalOutputStatus) -> (String, bool) {
    match &status.state {
        LocalOutputState::Running => (
            format!(
                "Active: {} - {} Hz, {} ch",
                status.device.as_deref().unwrap_or("unknown device"),
                status.sample_rate,
                status.channels
            ),
            false,
        ),
        LocalOutputState::Failed(message) => (format!("Error: {message}"), true),
        _ => ("Previews play on the main output".to_string(), false),
    }
}

/// The `[audio]` device settings shown under the dropdowns.
pub fn format_buffer_info(audio: &AudioConfig) -> String {
    let rate = audio.sample_rate.max(1);
    let buffer = match audio.buffer_size {
        0 => "buffer: driver default".to_string(),
        frames => format!(
            "buffer: {frames} frames ({:.1} ms)",
            frames as f32 * 1000.0 / rate as f32
        ),
    };
    format!("{buffer} - {rate} Hz - {} ch", audio.channels)
}

/// Whether applying the stored selection again could improve on the current
/// state: a configured device that exists now but is not what is playing (it
/// was unplugged at start-up), or a stream that failed. Used after a refresh so
/// plugging the device back in and pressing the button is enough.
pub fn needs_reapply(
    selection: &OutputSelection,
    devices: &[OutputDeviceInfo],
    output: &LocalOutputStatus,
    preview: &LocalOutputStatus,
) -> bool {
    if matches!(
        output.state,
        LocalOutputState::Failed(_) | LocalOutputState::Headless
    ) && !devices.is_empty()
    {
        return true;
    }
    if let DevicePick::Named(index) = pick_output_device(devices, selection.output.as_deref()) {
        if output.device.as_deref() != Some(devices[index].name.as_str()) {
            return true;
        }
    }
    if let DevicePick::Named(index) = pick_output_device(devices, selection.preview.as_deref()) {
        if selection.preview.is_some()
            && preview.device.as_deref() != Some(devices[index].name.as_str())
            && !matches!(preview.state, LocalOutputState::Stopped)
        {
            return true;
        }
        // A preview device that was missing earlier is `Failed` with a name.
        if matches!(preview.state, LocalOutputState::Failed(_)) {
            return true;
        }
    }
    false
}

type ScanResult = Result<Vec<OutputDeviceInfo>, String>;

/// Read the device list on a worker thread; the result arrives on the channel.
fn spawn_device_scan(ctx: egui::Context) -> Receiver<ScanResult> {
    let (tx, rx) = bounded(1);
    let spawned = std::thread::Builder::new()
        .name("audio-device-scan".into())
        .spawn(move || {
            let _ = tx.send(list_output_devices());
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        tracing::error!("Could not start the device scan thread: {e}");
    }
    rx
}

/// Per-window state of the section.
#[derive(Default)]
pub struct AudioSettingsUi {
    devices: Vec<OutputDeviceInfo>,
    scan: Option<Receiver<ScanResult>>,
    scanned_once: bool,
    scan_error: Option<String>,
    /// A refresh was asked for: when it finishes, retry the stored selection if
    /// that could help (see [`needs_reapply`]).
    reapply_after_scan: bool,
    /// Why the last attempt to apply a selection failed outright.
    apply_error: Option<String>,
}

impl AudioSettingsUi {
    /// Start reading the device list unless a read is already in flight.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.scan.is_none() {
            self.scan = Some(spawn_device_scan(ctx.clone()));
        }
        self.scanned_once = true;
    }

    /// The listed devices (empty until the first scan finishes).
    pub fn devices(&self) -> &[OutputDeviceInfo] {
        &self.devices
    }

    /// Whether a device scan is running.
    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Take a finished scan, if any. Returns `true` when a result arrived.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.scan else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the device scan stopped unexpectedly".into()),
        };
        self.scan = None;
        match result {
            Ok(devices) => {
                self.devices = devices;
                self.scan_error = None;
            }
            Err(message) => self.scan_error = Some(message),
        }
        true
    }

    /// Feed a scan result directly (tests, and callers that scanned elsewhere).
    pub fn set_devices(&mut self, devices: Vec<OutputDeviceInfo>) {
        self.devices = devices;
        self.scan_error = None;
        self.scanned_once = true;
    }
}

impl SoundSharedApp {
    /// The device selection stored in `[audio]`.
    pub fn output_selection(&self) -> OutputSelection {
        OutputSelection::from_audio_config(&self.app_state.config.lock().unwrap().audio)
    }

    /// Save a new device selection in `[audio]` and switch the engine to it.
    ///
    /// Runs on the GUI thread, which owns the engine (its streams cannot leave
    /// the thread that built them); reopening a stream is a matter of
    /// milliseconds. The returned status says what the engine ended up with.
    pub fn apply_output_devices(
        &mut self,
        selection: OutputSelection,
    ) -> Result<LocalOutputStatus, String> {
        self.edit_audio_config(|audio| selection.apply_to(audio));
        let guard = self.audio_engine.lock().unwrap();
        let engine = guard
            .as_ref()
            .ok_or_else(|| "the audio engine is not running".to_string())?;
        engine
            .apply_output_selection(selection)
            .map_err(|e| e.to_string())
    }

    /// State of the preview output (see
    /// [`AudioEngine::preview_output_status`](crate::audio::AudioEngine::preview_output_status)).
    pub fn preview_output_status(&self) -> LocalOutputStatus {
        match self.audio_engine.lock().unwrap().as_ref() {
            Some(engine) => engine.preview_output_status(),
            None => LocalOutputStatus::stopped(),
        }
    }
}

/// Dropdown for one slot. Returns the newly chosen value when it changed.
fn device_dropdown(
    ui: &mut Ui,
    slot: Slot,
    devices: &[OutputDeviceInfo],
    configured: Option<&str>,
) -> Option<Option<String>> {
    let (label, salt) = match slot {
        Slot::Output => ("Output", "audio_output_device"),
        Slot::Preview => ("Preview", "audio_preview_device"),
    };
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        ui.add_sized([68.0, 18.0], egui::Label::new(micro_label(label)));
        let width = (ui.available_width() - 4.0).max(120.0);
        egui::ComboBox::from_id_salt(salt)
            .width(width)
            .selected_text(elide(&selected_label(slot, devices, configured), 30))
            .show_ui(ui, |ui| {
                for choice in device_choices(slot, devices, configured) {
                    let selected = match (&choice.value, configured) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
                        _ => false,
                    };
                    let mut response = ui.selectable_label(selected, elide(&choice.label, 36));
                    if let Some(info) = devices
                        .iter()
                        .find(|d| Some(&d.name) == choice.value.as_ref())
                    {
                        response = response.on_hover_text(info.summary());
                    }
                    if response.clicked() && !selected {
                        chosen = Some(choice.value.clone());
                    }
                }
            });
    });
    chosen
}

/// One wrapped status line, rose when it is an error.
fn status_label(ui: &mut Ui, text: &str, error: bool) {
    let color = if error {
        Palette::STATUS_ERROR
    } else {
        Palette::ZINC_500
    };
    kit::note_under(ui, text, color);
}

/// The AUDIO OUTPUT section. `ui` is the network column.
pub fn audio_section(ui: &mut Ui, state: &mut SoundSharedApp) {
    // The section's state is taken out of the app for the frame, so the
    // widgets can call app methods without a double borrow.
    let mut section = std::mem::take(&mut state.extras.audio);
    if !section.scanned_once {
        section.refresh(ui.ctx());
    }

    let selection = state.output_selection();
    let output_status = state.local_output().clone();
    let preview_status = state.preview_output_status();

    if section.poll() && std::mem::take(&mut section.reapply_after_scan) {
        let wanted = needs_reapply(
            &selection,
            section.devices(),
            &output_status,
            &preview_status,
        );
        if wanted {
            section.apply_error = state.apply_output_devices(selection.clone()).err();
        }
    }

    ui.label(micro_label("Audio output"));
    ui.add_space(Metrics::SM);

    let mut new_selection = selection.clone();
    let mut changed = false;
    if let Some(value) = device_dropdown(
        ui,
        Slot::Output,
        section.devices(),
        selection.output.as_deref(),
    ) {
        new_selection.output = value;
        changed = true;
    }
    let (text, error) = output_status_line(&output_status);
    status_label(ui, &text, error);
    ui.add_space(Metrics::MD);

    if let Some(value) = device_dropdown(
        ui,
        Slot::Preview,
        section.devices(),
        selection.preview.as_deref(),
    ) {
        new_selection.preview = value;
        changed = true;
    }
    let (text, error) = preview_status_line(&preview_status);
    status_label(ui, &text, error);
    ui.add_space(Metrics::MD);

    let info = format_buffer_info(&state.app_state.config.lock().unwrap().audio);
    status_label(ui, &info, false);
    if let Some(message) = section.scan_error.clone().or(section.apply_error.clone()) {
        status_label(ui, &format!("Error: {message}"), true);
    }
    ui.add_space(Metrics::MD);

    let scanning = section.is_scanning();
    if ConsoleButton::ghost(if scanning {
        "Refreshing..."
    } else {
        "Refresh devices"
    })
    .glyph(glyph::REFRESH)
    .height(Metrics::BTN_H_SM)
    .enabled(!scanning)
    .tooltip("Read the list of sound cards again, and retry a device that was unplugged.")
    .id_salt("audio_refresh_devices")
    .show(ui)
    .clicked()
    {
        section.reapply_after_scan = true;
        section.refresh(ui.ctx());
    }

    if changed {
        section.apply_error = state.apply_output_devices(new_selection).err();
    }
    state.extras.audio = section;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::*;

    fn devices() -> Vec<OutputDeviceInfo> {
        vec![
            OutputDeviceInfo::named("Built-in Output", true),
            OutputDeviceInfo::named("USB Headphones", false),
        ]
    }

    fn running(device: &str) -> LocalOutputStatus {
        LocalOutputStatus {
            state: LocalOutputState::Running,
            device: Some(device.into()),
            sample_rate: 48_000,
            channels: 2,
            sample_format: "f32".into(),
            reason: None,
        }
    }

    #[test]
    fn the_output_list_starts_with_the_system_default() {
        let choices = device_choices(Slot::Output, &devices(), None);
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].value, None);
        assert_eq!(choices[0].label, "System default (Built-in Output)");
        assert_eq!(choices[2].value.as_deref(), Some("USB Headphones"));
        assert_eq!(
            device_choices(Slot::Preview, &devices(), None)[0].label,
            "Same as output"
        );
    }

    #[test]
    fn a_configured_device_that_is_gone_stays_visible() {
        let choices = device_choices(Slot::Preview, &devices(), Some("Old DAC"));
        assert_eq!(choices.len(), 4);
        assert_eq!(choices[3].label, "Old DAC (not connected)");
        assert_eq!(
            selected_label(Slot::Preview, &devices(), Some("Old DAC")),
            "Old DAC (not connected)"
        );
        assert_eq!(
            selected_label(Slot::Output, &devices(), Some("usb headphones")),
            "USB Headphones"
        );
        assert_eq!(
            selected_label(Slot::Output, &devices(), None),
            "System default (Built-in Output)"
        );
    }

    #[test]
    fn status_lines_say_active_or_error() {
        let (text, error) = output_status_line(&running("DAC"));
        assert_eq!(text, "Active: DAC - 48000 Hz, 2 ch");
        assert!(!error);

        let mut fell_back = running("Built-in Output");
        fell_back.reason = Some("output device \"X\" not found; using the system default".into());
        let (text, _) = output_status_line(&fell_back);
        assert!(text.starts_with("Active: Built-in Output"));
        assert!(text.contains("not found"));

        let failed = LocalOutputStatus {
            state: LocalOutputState::Failed("device lost".into()),
            reason: Some("device lost".into()),
            ..LocalOutputStatus::stopped()
        };
        let (text, error) = output_status_line(&failed);
        assert_eq!(text, "Error: device lost");
        assert!(error);

        let (text, error) = preview_status_line(&LocalOutputStatus::stopped());
        assert!(text.contains("output"));
        assert!(!error);
        let (text, error) = preview_status_line(&failed);
        assert!(text.starts_with("Error:") && error);
    }

    #[test]
    fn buffer_info_comes_from_the_audio_config() {
        let audio = AudioConfig {
            buffer_size: 480,
            sample_rate: 48_000,
            channels: 2,
            ..AudioConfig::default()
        };
        assert_eq!(
            format_buffer_info(&audio),
            "buffer: 480 frames (10.0 ms) - 48000 Hz - 2 ch"
        );
        let audio = AudioConfig {
            buffer_size: 0,
            ..audio
        };
        assert!(format_buffer_info(&audio).starts_with("buffer: driver default"));
    }

    #[test]
    fn a_refresh_retries_a_device_that_came_back() {
        let wanted = OutputSelection {
            output: Some("USB Headphones".into()),
            preview: None,
        };
        // Fell back to the built-in output while the DAC was unplugged.
        assert!(needs_reapply(
            &wanted,
            &devices(),
            &running("Built-in Output"),
            &LocalOutputStatus::stopped()
        ));
        // Already on it: nothing to do.
        assert!(!needs_reapply(
            &wanted,
            &devices(),
            &running("USB Headphones"),
            &LocalOutputStatus::stopped()
        ));
        // Default selection on a healthy output: nothing to do.
        assert!(!needs_reapply(
            &OutputSelection::default(),
            &devices(),
            &running("Built-in Output"),
            &LocalOutputStatus::stopped()
        ));
        // A dead stream is worth a retry once there are devices.
        let failed = LocalOutputStatus {
            state: LocalOutputState::Failed("lost".into()),
            ..LocalOutputStatus::stopped()
        };
        assert!(needs_reapply(
            &OutputSelection::default(),
            &devices(),
            &failed,
            &LocalOutputStatus::stopped()
        ));
        assert!(!needs_reapply(
            &OutputSelection::default(),
            &[],
            &failed,
            &LocalOutputStatus::stopped()
        ));
    }

    fn draw(h: &mut Harness) -> String {
        settle(&h.ctx, 3, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| audio_section(ui, &mut h.app));
        })
    }

    #[test]
    fn the_section_shows_both_dropdowns_and_the_refresh_button() {
        let mut h = harness(Config::default(), true);
        h.app.extras.audio.set_devices(devices());
        let text = draw(&mut h);
        for expected in [
            "AUDIO OUTPUT",
            "OUTPUT",
            "PREVIEW",
            "SYSTEM DEFAULT",
            "SAME AS OUTPUT",
            "REFRESH DEVICES",
            "BUFFER: 960 FRAMES",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }

    #[test]
    fn applying_a_selection_saves_it_in_the_config_without_a_started_engine() {
        let mut h = harness(Config::default(), true);
        let selection = OutputSelection {
            output: Some("USB Headphones".into()),
            preview: Some("Built-in Output".into()),
        };
        let status = h.app.apply_output_devices(selection.clone()).unwrap();
        assert!(!status.is_local_audio_running());
        assert_eq!(h.app.output_selection(), selection);
        let config = h.config.lock().unwrap();
        assert_eq!(
            config.audio.output_device.as_deref(),
            Some("USB Headphones")
        );
        assert_eq!(
            config.audio.preview_device.as_deref(),
            Some("Built-in Output")
        );
    }

    #[test]
    fn applying_without_an_engine_reports_it() {
        let mut h = harness(Config::default(), false);
        let error = h
            .app
            .apply_output_devices(OutputSelection::default())
            .unwrap_err();
        assert!(error.contains("not running"));
    }
}
