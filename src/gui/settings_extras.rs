//! The MIX section of the network column: bus faders, the DUCK button and its
//! settings, loudness normalization, and the entry to the hotkey dialog.
//!
//! Everything goes through the console's typed commands
//! ([`AudioCommand::SetBusVolume`], `DuckToggle`, `DuckHold`, `SetNormalize`):
//! the widgets read the Director's current state and never keep their own
//! copy of it, so the faders follow changes made by hotkeys or the phone panel.
//! The ducking depth and timings are `[audio]` settings the Director re-reads
//! every frame; they are edited in the shared config and saved with it.
//!
//! The DUCK button has two gestures: a short click latches the duck (click
//! again to release), holding it ducks only while the mouse button is down.
//! [`DuckGesture`] decides which one it was.

use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use egui::{RichText, Ui};

use crate::audio::director::Director;
use crate::audio::DuckTarget;
use crate::config::Bus;
use crate::gui::app::SoundSharedApp;
use crate::gui::groups::Library;
use crate::gui::hotkeys_ui::{DuckOp, HotkeyUi};
use crate::gui::server_panel::GmLinkUi;
use crate::gui::theme::{hairline, micro_label, Metrics, Palette};
use crate::gui::types::{AppState, AudioCommand};
use crate::gui::widgets::{glyph, glyph_font, level_slider, BtnKind, ConsoleButton};

/// A press shorter than this is a click (latch); a longer one is a hold.
pub const HOLD_AFTER: Duration = Duration::from_millis(350);

/// Lowest and highest loudness target the console offers, LUFS. Broadcast
/// speech sits near -23, streaming music near -14.
pub const LUFS_RANGE: std::ops::RangeInclusive<f32> = -36.0..=-8.0;

/// The three buses in display order.
pub const BUSES: [Bus; 3] = [Bus::Music, Bus::Ambience, Bus::Sfx];

/// Per-window state of the extras the console adds to the network column.
pub struct ConsoleExtras {
    /// Hotkey runner and editor.
    pub hotkeys: HotkeyUi,
    /// Faders / duck / normalization widgets.
    pub mix: MixUi,
    /// Game-master link (QR, confirmation).
    pub gm: GmLinkUi,
    /// Session timers: the timeline thread and the Timers dialog.
    pub timers: crate::gui::timeline::TimersUi,
    /// Output / preview device dropdowns (see `audio_settings`).
    pub audio: crate::gui::audio_settings::AudioSettingsUi,
    /// Update check (off by default, see `crate::updates`).
    pub updates: crate::updates::UpdateChecker,
}

impl ConsoleExtras {
    /// Create the state; starts the system-wide hotkey service when enabled
    /// (main thread only, see [`HotkeyUi::start`]).
    pub fn new(ctx: &egui::Context, app_state: &AppState, library: &Library) -> Self {
        Self {
            hotkeys: HotkeyUi::start(ctx, app_state, library),
            mix: MixUi::default(),
            gm: GmLinkUi::default(),
            timers: crate::gui::timeline::TimersUi::start(
                ctx,
                app_state.config.lock().unwrap().timers.clone(),
            ),
            audio: Default::default(),
            updates: crate::updates::UpdateChecker::new(),
        }
    }
}

/// State of the MIX widgets.
#[derive(Debug, Default)]
pub struct MixUi {
    duck: DuckGesture,
    /// The loudness target while it is being edited (applied when the edit
    /// ends, so dragging does not restart measurements for every value).
    target_edit: Option<f32>,
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Click-or-hold recognition for the DUCK button.
///
/// The duck starts the moment the button goes down (no delay for a hold), and
/// the release decides: after a short press the duck stays on (latched; the
/// next short press releases it), after a long one it ends.
#[derive(Debug, Default, Clone, Copy)]
pub struct DuckGesture {
    pressed_at: Option<Instant>,
}

impl DuckGesture {
    /// Feed one frame: whether the pointer button is down on the DUCK button,
    /// whether egui reported a click, and the time. Returns what to run.
    pub fn frame(&mut self, down: bool, clicked: bool, now: Instant) -> Vec<DuckOp> {
        match (self.pressed_at, down) {
            (None, true) => {
                self.pressed_at = Some(now);
                vec![DuckOp::HoldStart]
            }
            (Some(since), false) => {
                self.pressed_at = None;
                if now.saturating_duration_since(since) < HOLD_AFTER {
                    // Latch first, then end the hold: the latch keeps the
                    // background down, no release / re-duck flicker.
                    vec![DuckOp::Toggle, DuckOp::HoldEnd]
                } else {
                    vec![DuckOp::HoldEnd]
                }
            }
            // Press and release inside one frame: egui only reports a click.
            (None, false) if clicked => vec![DuckOp::Toggle],
            _ => Vec::new(),
        }
    }

    /// Whether the button is being held right now.
    pub fn is_down(&self) -> bool {
        self.pressed_at.is_some()
    }
}

/// A linear gain as decibels text: `0.3` -> `-10.5 dB`, `0.0` -> `-inf dB`.
pub fn format_db(level: f32) -> String {
    if level <= 0.0 || !level.is_finite() {
        "-inf dB".to_string()
    } else {
        format!("{:.1} dB", 20.0 * level.log10())
    }
}

/// A `0.0..=1.0` level as a whole percent.
pub fn format_percent(level: f32) -> String {
    format!("{:.0}%", level.clamp(0.0, 1.0) * 100.0)
}

/// A loudness target inside [`LUFS_RANGE`] (non-numbers fall back to -18).
pub fn clamp_lufs(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(*LUFS_RANGE.start(), *LUFS_RANGE.end())
    } else {
        -18.0
    }
}

/// Display name of a bus.
pub fn bus_label(bus: Bus) -> &'static str {
    match bus {
        Bus::Music => "Music",
        Bus::Ambience => "Ambience",
        Bus::Sfx => "Sfx",
    }
}

/// `buses` with `bus` switched on or off, in the fixed display order (no
/// duplicates, whatever the input order was).
pub fn with_duck_bus(buses: &[Bus], bus: Bus, on: bool) -> Vec<Bus> {
    BUSES
        .into_iter()
        .filter(|candidate| {
            if *candidate == bus {
                on
            } else {
                buses.contains(candidate)
            }
        })
        .collect()
}

/// The one-line state under the loudness switch.
pub fn normalize_status(enabled: bool, target_lufs: f32, measuring: usize) -> String {
    match (enabled, measuring) {
        (false, _) => "Off: sounds keep their own level".to_string(),
        (true, 0) => format!("On: new sounds are measured and set to {target_lufs:.0} LUFS"),
        (true, n) => format!("Measuring {n} sound{}...", if n == 1 { "" } else { "s" }),
    }
}

// ---------------------------------------------------------------------------
// Commands and reads (the app side)
// ---------------------------------------------------------------------------

/// Everything the MIX section shows, read in one go.
#[derive(Debug, Clone, PartialEq)]
pub struct MixView {
    /// Fader positions, in [`BUSES`] order.
    pub bus_volumes: [f32; 3],
    /// Whether any bus is ducked right now.
    pub ducked: bool,
    /// `[audio] duck_amount`.
    pub duck_amount: f32,
    /// `[audio] duck_attack_ms`.
    pub attack_ms: u32,
    /// `[audio] duck_release_ms`.
    pub release_ms: u32,
    /// `[audio] duck_buses`.
    pub duck_buses: Vec<Bus>,
    /// Whether normalization is on.
    pub normalize: bool,
    /// Loudness target, LUFS.
    pub target_lufs: f32,
    /// Sounds being measured.
    pub measuring: usize,
}

impl MixView {
    fn read(director: &Director, audio: &crate::config::AudioConfig) -> Self {
        let norm = director.normalization();
        Self {
            bus_volumes: BUSES.map(|bus| director.bus_volume(bus)),
            ducked: BUSES.into_iter().any(|bus| director.is_ducked(bus)),
            duck_amount: audio.duck_amount,
            attack_ms: audio.duck_attack_ms,
            release_ms: audio.duck_release_ms,
            duck_buses: audio.duck_buses.clone(),
            normalize: norm.enabled,
            target_lufs: norm.target_lufs,
            measuring: director.measuring_count(),
        }
    }
}

impl SoundSharedApp {
    /// Snapshot for the MIX section.
    pub fn mix_view(&self) -> MixView {
        let audio = self.app_state.config.lock().unwrap().audio.clone();
        MixView::read(&self.director, &audio)
    }

    /// Set a bus fader.
    pub fn request_bus_volume(&self, bus: Bus, volume: f32) {
        let (response, _rx) = bounded(1);
        self.send_audio_command(AudioCommand::SetBusVolume {
            bus,
            volume: volume.clamp(0.0, 1.0),
            response,
        });
    }

    /// Run a DUCK button request on the configured duck buses.
    pub fn request_duck(&self, op: DuckOp) {
        let target = DuckTarget::Configured;
        let (response, _rx) = bounded(1);
        self.send_audio_command(match op {
            DuckOp::Toggle => AudioCommand::DuckToggle { target, response },
            DuckOp::HoldStart => AudioCommand::DuckHold {
                target,
                active: true,
                response,
            },
            DuckOp::HoldEnd => AudioCommand::DuckHold {
                target,
                active: false,
                response,
            },
        });
    }

    /// Switch loudness normalization on or off / change its target.
    pub fn request_normalize(&self, enabled: bool, target_lufs: f32) {
        let (response, _rx) = bounded(1);
        self.send_audio_command(AudioCommand::SetNormalize {
            enabled,
            target_lufs: Some(clamp_lufs(target_lufs)),
            response,
        });
    }

    /// Edit `[audio]` (ducking depth and timings) and queue a save. The
    /// Director picks the values up on its next frame.
    pub fn edit_audio_config(&mut self, edit: impl FnOnce(&mut crate::config::AudioConfig)) {
        edit(&mut self.app_state.config.lock().unwrap().audio);
        self.library_dirty = true;
    }

    fn send_audio_command(&self, command: AudioCommand) {
        if let Err(e) = self.app_state.audio_cmd_tx.try_send(command) {
            tracing::error!("Could not queue an audio command (channel full): {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// The MIX section. `ui` is the network column.
pub fn mix_section(ui: &mut Ui, state: &mut SoundSharedApp) {
    let view = state.mix_view();

    ui.label(micro_label("Mix"));
    ui.add_space(Metrics::SM);

    // ---- Bus faders ----
    for (index, bus) in BUSES.into_iter().enumerate() {
        let mut value = view.bus_volumes[index];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Metrics::SM;
            ui.add_sized([68.0, 18.0], egui::Label::new(micro_label(bus_label(bus))));
            let width = (ui.available_width() - 44.0).max(60.0);
            let salt = format!("bus_fader_{index}");
            if level_slider(ui, &mut value, width, &salt).changed() {
                state.request_bus_volume(bus, value);
            }
            ui.label(
                RichText::new(format_percent(value))
                    .font(glyph_font(11.0))
                    .color(Palette::WHITE),
            );
        });
        ui.add_space(2.0);
    }

    ui.add_space(Metrics::SM);

    // ---- DUCK + Hotkeys ----
    let hotkeys_w = 112.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        let duck_w = (ui.available_width() - hotkeys_w - Metrics::SM).max(96.0);
        let kind = if view.ducked || state.extras.mix.duck.is_down() {
            BtnKind::Armed
        } else {
            BtnKind::Ghost
        };
        let response = ConsoleButton::new(if view.ducked { "Ducked" } else { "Duck" })
            .kind(kind)
            .glyph(glyph::VOLUME)
            .width(duck_w)
            .height(Metrics::BTN_H)
            .tooltip(
                "Lower the background (music and ambience by default).\n\
                 Click: stay ducked until you click again.\n\
                 Hold: ducked only while the button is held.",
            )
            .id_salt("mix_duck")
            .show(ui);
        let ops = state.extras.mix.duck.frame(
            response.is_pointer_button_down_on(),
            response.clicked(),
            Instant::now(),
        );
        for op in ops {
            state.request_duck(op);
        }

        let hotkey_count = state.extras.hotkeys.active_count();
        if ConsoleButton::ghost("Hotkeys")
            .width(hotkeys_w)
            .height(Metrics::BTN_H)
            .tooltip(format!(
                "Keyboard shortcuts ({hotkey_count} active): assign keys to sounds, scenes, \
                 stop all, volume and ducking"
            ))
            .id_salt("mix_hotkeys")
            .show(ui)
            .clicked()
        {
            state.open_hotkeys();
        }
    });
    crate::gui::timeline::timers_button(ui, state);

    ui.add_space(Metrics::SM);
    duck_settings(ui, state, &view);

    ui.add_space(Metrics::MD);
    hairline(ui);
    ui.add_space(Metrics::MD);
    normalize_controls(ui, state, &view);
}

/// Depth, timings and the buses the duck acts on.
fn duck_settings(ui: &mut Ui, state: &mut SoundSharedApp, view: &MixView) {
    egui::CollapsingHeader::new(micro_label("Duck settings"))
        .id_salt("mix_duck_settings")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(Metrics::XS);

            // Level the ducked buses drop to (1.0 = no ducking).
            let mut amount = view.duck_amount.clamp(0.0, 1.0);
            ui.horizontal(|ui| {
                ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Level")));
                let width = (ui.available_width() - 92.0).max(60.0);
                if level_slider(ui, &mut amount, width, "duck_level").changed() {
                    state.edit_audio_config(|audio| audio.duck_amount = amount);
                }
                ui.label(
                    RichText::new(format!("{} {}", format_percent(amount), format_db(amount)))
                        .font(glyph_font(10.0))
                        .color(Palette::ZINC_300),
                );
            });
            ui.add_space(Metrics::XS);

            ui.horizontal(|ui| {
                ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Attack")));
                let mut attack = view.attack_ms;
                if ui
                    .add(
                        egui::DragValue::new(&mut attack)
                            .range(0..=10_000)
                            .speed(10)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    state.edit_audio_config(|audio| audio.duck_attack_ms = attack);
                }
                ui.add_space(Metrics::MD);
                ui.label(micro_label("Release"));
                let mut release = view.release_ms;
                if ui
                    .add(
                        egui::DragValue::new(&mut release)
                            .range(0..=30_000)
                            .speed(10)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    state.edit_audio_config(|audio| audio.duck_release_ms = release);
                }
            });
            ui.add_space(Metrics::XS);

            ui.horizontal(|ui| {
                ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Ducks")));
                for bus in BUSES {
                    let mut on = view.duck_buses.contains(&bus);
                    if ui.checkbox(&mut on, bus_label(bus)).changed() {
                        let next = with_duck_bus(&view.duck_buses, bus, on);
                        state.edit_audio_config(|audio| audio.duck_buses = next);
                    }
                }
            });
            ui.add_space(Metrics::XS);
        });
}

/// The loudness-normalization switch with its LUFS target.
fn normalize_controls(ui: &mut Ui, state: &mut SoundSharedApp, view: &MixView) {
    let mut enabled = view.normalize;
    ui.horizontal(|ui| {
        if ui
            .checkbox(
                &mut enabled,
                RichText::new("NORMALIZE LOUDNESS")
                    .font(glyph_font(Metrics::MICRO))
                    .color(Palette::ZINC_300)
                    .extra_letter_spacing(0.8),
            )
            .on_hover_text(
                "Measure each sound once (EBU R128) and store a gain that brings it to the \
                 target loudness. Off by default so existing libraries sound the same.",
            )
            .changed()
        {
            let target = state
                .extras
                .mix
                .target_edit
                .take()
                .unwrap_or(view.target_lufs);
            state.request_normalize(enabled, target);
        }
    });

    ui.horizontal(|ui| {
        ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Target")));
        let mut target = state.extras.mix.target_edit.unwrap_or(view.target_lufs);
        let response = ui.add(
            egui::DragValue::new(&mut target)
                .range(LUFS_RANGE)
                .speed(0.1)
                .fixed_decimals(1)
                .suffix(" LUFS"),
        );
        if response.changed() {
            state.extras.mix.target_edit = Some(target);
        }
        // Apply when the edit ends: a drag passes through many values, and a
        // new target makes the library measurable again.
        if (response.drag_stopped() || response.lost_focus())
            && state.extras.mix.target_edit.is_some()
        {
            let target = state.extras.mix.target_edit.take().unwrap_or(target);
            state.request_normalize(view.normalize, target);
        }
    });
    ui.add_space(2.0);
    ui.add(
        egui::Label::new(
            RichText::new(normalize_status(
                view.normalize,
                view.target_lufs,
                view.measuring,
            ))
            .font(glyph_font(Metrics::MICRO))
            .color(Palette::ZINC_500),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_short_press_latches_the_duck() {
        let t0 = Instant::now();
        let mut gesture = DuckGesture::default();
        assert_eq!(gesture.frame(true, false, t0), vec![DuckOp::HoldStart]);
        assert!(gesture.is_down());
        // Held, nothing new.
        assert!(gesture.frame(true, false, at(100, t0)).is_empty());
        // Released after 150 ms: latch first, then end the hold.
        assert_eq!(
            gesture.frame(false, true, at(150, t0)),
            vec![DuckOp::Toggle, DuckOp::HoldEnd]
        );
        assert!(!gesture.is_down());
    }

    #[test]
    fn a_long_press_ducks_only_while_held() {
        let t0 = Instant::now();
        let mut gesture = DuckGesture::default();
        assert_eq!(gesture.frame(true, false, t0), vec![DuckOp::HoldStart]);
        assert_eq!(
            gesture.frame(false, false, at(2000, t0)),
            vec![DuckOp::HoldEnd]
        );
    }

    #[test]
    fn the_threshold_is_inclusive_of_the_hold() {
        let t0 = Instant::now();
        let mut gesture = DuckGesture::default();
        gesture.frame(true, false, t0);
        // Exactly the threshold counts as a hold.
        assert_eq!(
            gesture.frame(false, false, at(350, t0)),
            vec![DuckOp::HoldEnd]
        );
        gesture.frame(true, false, t0);
        assert_eq!(
            gesture.frame(false, true, at(349, t0)),
            vec![DuckOp::Toggle, DuckOp::HoldEnd]
        );
    }

    #[test]
    fn a_click_inside_one_frame_is_a_toggle() {
        let t0 = Instant::now();
        let mut gesture = DuckGesture::default();
        assert_eq!(gesture.frame(false, true, t0), vec![DuckOp::Toggle]);
        // An idle frame does nothing.
        assert!(gesture.frame(false, false, t0).is_empty());
    }

    #[test]
    fn gesture_commands_drive_the_director_like_the_user_expects() {
        use crate::audio::director::Director;
        use crate::config::Bus;
        use crate::gui::hotkeys_ui::run_duck_op;
        use crate::hotkeys::testing::SpyPlayback;

        let pb = SpyPlayback::new();
        let mut director = Director::new();
        let t0 = Instant::now();
        let mut gesture = DuckGesture::default();
        let run = |ops: Vec<DuckOp>, director: &mut Director| {
            for op in ops {
                run_duck_op(director, &pb, op).unwrap();
            }
        };

        // Click: ducked and stays ducked.
        run(gesture.frame(true, false, t0), &mut director);
        run(gesture.frame(false, true, at(100, t0)), &mut director);
        assert!(director.is_ducked(Bus::Music));
        // Second click: released.
        run(gesture.frame(true, false, at(1000, t0)), &mut director);
        run(gesture.frame(false, true, at(1100, t0)), &mut director);
        assert!(!director.is_ducked(Bus::Music));
        // Hold: ducked while down, released after.
        run(gesture.frame(true, false, at(2000, t0)), &mut director);
        assert!(director.is_ducked(Bus::Music));
        run(gesture.frame(false, false, at(3000, t0)), &mut director);
        assert!(!director.is_ducked(Bus::Music));
    }

    #[test]
    fn decibels_and_percent_read_naturally() {
        assert_eq!(format_db(1.0), "0.0 dB");
        assert_eq!(format_db(0.5), "-6.0 dB");
        assert_eq!(format_db(0.0), "-inf dB");
        assert_eq!(format_db(f32::NAN), "-inf dB");
        assert_eq!(format_percent(0.3), "30%");
        assert_eq!(format_percent(2.0), "100%");
        assert_eq!(format_percent(-1.0), "0%");
    }

    #[test]
    fn loudness_targets_are_kept_in_range() {
        assert_eq!(clamp_lufs(-18.0), -18.0);
        assert_eq!(clamp_lufs(-70.0), *LUFS_RANGE.start());
        assert_eq!(clamp_lufs(3.0), *LUFS_RANGE.end());
        assert_eq!(clamp_lufs(f32::NAN), -18.0);
    }

    #[test]
    fn duck_bus_selection_keeps_a_stable_order_without_duplicates() {
        assert_eq!(
            with_duck_bus(&[Bus::Ambience, Bus::Music], Bus::Sfx, true),
            vec![Bus::Music, Bus::Ambience, Bus::Sfx]
        );
        assert_eq!(
            with_duck_bus(&[Bus::Music, Bus::Ambience], Bus::Music, false),
            vec![Bus::Ambience]
        );
        assert_eq!(
            with_duck_bus(&[Bus::Music, Bus::Music], Bus::Music, true),
            vec![Bus::Music]
        );
        assert!(with_duck_bus(&[Bus::Music], Bus::Music, false).is_empty());
    }

    #[test]
    fn normalize_status_tells_what_is_happening() {
        assert!(normalize_status(false, -18.0, 0).starts_with("Off"));
        assert!(normalize_status(true, -18.0, 0).contains("-18 LUFS"));
        assert_eq!(normalize_status(true, -18.0, 1), "Measuring 1 sound...");
        assert_eq!(normalize_status(true, -18.0, 3), "Measuring 3 sounds...");
    }

    #[test]
    fn bus_labels_cover_every_bus() {
        for bus in BUSES {
            assert!(!bus_label(bus).is_empty());
        }
        assert_eq!(BUSES.len(), 3);
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::*;

    fn draw(h: &mut Harness) -> String {
        settle(&h.ctx, 2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| mix_section(ui, &mut h.app));
        })
    }

    fn next_command(h: &Harness) -> AudioCommand {
        h.audio_rx.try_recv().expect("a queued audio command")
    }

    #[test]
    fn the_section_shows_faders_duck_hotkeys_and_loudness() {
        let mut h = harness(Config::default(), false);
        let text = draw(&mut h);
        for expected in [
            "MIX",
            "MUSIC",
            "AMBIENCE",
            "SFX",
            "DUCK",
            "HOTKEYS",
            "DUCK SETTINGS",
            "NORMALIZE LOUDNESS",
            "TARGET",
            "LUFS",
            "OFF: SOUNDS KEEP",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }

    #[test]
    fn a_fader_sends_a_typed_bus_command() {
        let h = harness(Config::default(), false);
        h.app.request_bus_volume(Bus::Music, 0.4);
        h.app.request_bus_volume(Bus::Sfx, 7.0);
        match next_command(&h) {
            AudioCommand::SetBusVolume { bus, volume, .. } => {
                assert_eq!(bus, Bus::Music);
                assert_eq!(volume, 0.4);
            }
            other => panic!("unexpected {other:?}"),
        }
        match next_command(&h) {
            AudioCommand::SetBusVolume { bus, volume, .. } => {
                assert_eq!(bus, Bus::Sfx);
                assert_eq!(volume, 1.0, "out-of-range input is clamped");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_faders_follow_what_the_engine_thread_applied() {
        let mut h = harness(Config::default(), true);
        h.app.request_bus_volume(Bus::Ambience, 0.25);
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        let view = h.app.mix_view();
        assert_eq!(view.bus_volumes[1], 0.25);
        assert_eq!(view.bus_volumes[0], 1.0);
    }

    #[test]
    fn duck_requests_run_through_the_command_channel() {
        let mut h = harness(Config::default(), true);
        // Click: latch.
        h.app.request_duck(DuckOp::HoldStart);
        h.app.request_duck(DuckOp::Toggle);
        h.app.request_duck(DuckOp::HoldEnd);
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        assert!(h.app.mix_view().ducked);
        // Second click: release.
        h.app.request_duck(DuckOp::HoldStart);
        h.app.request_duck(DuckOp::Toggle);
        h.app.request_duck(DuckOp::HoldEnd);
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        assert!(!h.app.mix_view().ducked);
        // Hold: only while down.
        h.app.request_duck(DuckOp::HoldStart);
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        assert!(h.app.mix_view().ducked);
        h.app.request_duck(DuckOp::HoldEnd);
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        assert!(!h.app.mix_view().ducked);
    }

    #[test]
    fn duck_settings_are_edited_in_the_config_and_queued_for_saving() {
        let mut h = harness(Config::default(), false);
        h.app.edit_audio_config(|audio| {
            audio.duck_amount = 0.2;
            audio.duck_attack_ms = 50;
            audio.duck_release_ms = 900;
            audio.duck_buses = vec![Bus::Music];
        });
        assert!(h.app.library_dirty);
        let view = h.app.mix_view();
        assert_eq!(view.duck_amount, 0.2);
        assert_eq!(view.attack_ms, 50);
        assert_eq!(view.release_ms, 900);
        assert_eq!(view.duck_buses, vec![Bus::Music]);
        let audio = h.config.lock().unwrap().audio.clone();
        assert_eq!(audio.duck_amount, 0.2);
    }

    #[test]
    fn normalization_requests_carry_the_clamped_target() {
        let h = harness(Config::default(), false);
        h.app.request_normalize(true, -20.0);
        h.app.request_normalize(false, -70.0);
        match next_command(&h) {
            AudioCommand::SetNormalize {
                enabled,
                target_lufs,
                ..
            } => {
                assert!(enabled);
                assert_eq!(target_lufs, Some(-20.0));
            }
            other => panic!("unexpected {other:?}"),
        }
        match next_command(&h) {
            AudioCommand::SetNormalize {
                enabled,
                target_lufs,
                ..
            } => {
                assert!(!enabled);
                assert_eq!(target_lufs, Some(*LUFS_RANGE.start()));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_view_reflects_the_saved_normalization_settings() {
        let mut config = Config::default();
        config.audio.normalize = true;
        config.audio.normalize_target_lufs = -16.0;
        let mut h = harness(config, true);
        // The Director takes `[audio]` on its first frame.
        frame(&h.ctx, vec![], |ctx| h.app.update(ctx));
        let view = h.app.mix_view();
        assert!(view.normalize);
        assert_eq!(view.target_lufs, -16.0);
        let text = draw(&mut h);
        assert!(text.contains("ON: NEW SOUNDS ARE MEASURED"), "{text}");
    }
}
