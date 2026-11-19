//! Pan controls: the widget and the glue around it.
//!
//! The mixer pans per sound (`set_sound_pan`); this file gives the console a
//! way to set it.
//!
//! * [`pan_slider`] is the compact control: a centred slider with a notch at
//!   zero, a double click resets to centre, the tooltip reads like `L 40%`.
//! * [`sound_pan_section`] is the block in the right-click menu of a sound row
//!   (the sound's own pan, stored in the library).
//! * The scene editor puts the same slider in every item row (`ItemForm::pan`,
//!   a non-zero value overrides the sound's pan inside that scene).
//!
//! Applying a pan is the Director's job (`audio::director_pan`): the GUI only
//! queues [`AudioCommand::SetPan`] (one sound, while the slider moves) or
//! [`AudioCommand::SyncPans`] (after a scene was saved) and never waits for
//! the answer.

use crossbeam_channel::bounded;
use egui::{CursorIcon, Key, Rect, Response, Rounding, Sense, Ui, Vec2, WidgetInfo};

use crate::gui::app::{SoundEntry, SoundSharedApp};
use crate::gui::groups::clamp_pan;
use crate::gui::theme::{hairline, micro_label, Metrics, Palette};
use crate::gui::types::AudioCommand;
use crate::gui::widgets::glyph_font;

/// Pans closer to the centre than this snap to exactly `0.0`.
const SNAP: f32 = 0.05;
/// Arrow key step; Shift makes it [`COARSE_STEP`].
const KEY_STEP: f32 = 0.05;
const COARSE_STEP: f32 = 0.25;
/// Height of the widget's hit area.
const HEIGHT: f32 = 18.0;

/// The pan under a pointer at `x` over a track that spans `left..left+width`:
/// `-1.0` at the left end, `1.0` at the right, snapped to `0.0` near the
/// middle and rounded to whole percents.
pub fn pan_from_pointer(x: f32, left: f32, width: f32) -> f32 {
    let t = ((x - left) / width.max(1.0)).clamp(0.0, 1.0);
    snap_pan(t * 2.0 - 1.0)
}

/// Round to whole percents and snap the middle to exactly zero.
pub fn snap_pan(pan: f32) -> f32 {
    let pan = clamp_pan(pan);
    if pan.abs() < SNAP {
        0.0
    } else {
        (pan * 100.0).round() / 100.0
    }
}

/// `Centre`, `L 40%` or `R 25%`: how far the sound is off the middle.
pub fn pan_label(pan: f32) -> String {
    let pan = clamp_pan(pan);
    let percent = (pan.abs() * 100.0).round() as u32;
    if percent == 0 {
        "Centre".to_string()
    } else if pan < 0.0 {
        format!("L {percent}%")
    } else {
        format!("R {percent}%")
    }
}

/// The compact pan control: a slider centred on zero, `-1.0..=1.0`.
///
/// `changed()` on the response tells that `pan` was modified. `hint` is added
/// to the tooltip (what a centred value means in this place).
pub fn pan_slider(ui: &mut Ui, pan: &mut f32, width: f32, hint: &str) -> Response {
    let (rect, mut resp) =
        ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click_and_drag());
    let track = Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - 1.0),
        Vec2::new(rect.width(), 2.0),
    );
    let before = clamp_pan(*pan);
    let mut value = before;

    if resp.double_clicked() {
        value = 0.0;
    } else if let Some(pointer) = resp.interact_pointer_pos() {
        if resp.is_pointer_button_down_on() || resp.dragged() {
            value = pan_from_pointer(pointer.x, track.left(), track.width());
        }
    }
    if resp.has_focus() {
        let (step, home) = ui.input(|i| {
            let step = if i.modifiers.shift {
                COARSE_STEP
            } else {
                KEY_STEP
            };
            let mut d = 0.0;
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp) {
                d += step;
            }
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown) {
                d -= step;
            }
            (d, i.key_pressed(Key::Home))
        });
        if home {
            value = 0.0;
        } else if step != 0.0 {
            value = snap_pan(value + step);
        }
    }
    if (value - before).abs() > f32::EPSILON {
        *pan = value;
        resp.mark_changed();
    }

    let shown = clamp_pan(*pan);
    let tip = if hint.is_empty() {
        format!("{}\nDouble-click to centre", pan_label(shown))
    } else {
        format!("{}\n{hint}\nDouble-click to centre", pan_label(shown))
    };
    resp = resp
        .on_hover_cursor(CursorIcon::ResizeHorizontal)
        .on_hover_text(tip);
    resp.widget_info(|| {
        WidgetInfo::slider(true, f64::from(shown), format!("Pan, {}", pan_label(shown)))
    });

    let hovered = resp.hovered() || resp.dragged();
    let painter = ui.painter();
    painter.rect_filled(track, Rounding::ZERO, Palette::BORDER_STRONG);
    let centre_x = track.center().x;
    let thumb_x = track.left() + track.width() * (shown + 1.0) / 2.0;
    if shown != 0.0 {
        let (a, b) = if thumb_x < centre_x {
            (thumb_x, centre_x)
        } else {
            (centre_x, thumb_x)
        };
        painter.rect_filled(
            Rect::from_min_max(egui::pos2(a, track.top()), egui::pos2(b, track.bottom())),
            Rounding::ZERO,
            Palette::ROSE_500,
        );
    }
    // The notch at zero.
    painter.rect_filled(
        Rect::from_center_size(egui::pos2(centre_x, track.center().y), Vec2::new(1.0, 10.0)),
        Rounding::ZERO,
        Palette::ZINC_500,
    );
    let thumb_size = if hovered { 6.0 } else { 4.5 };
    painter.rect_filled(
        Rect::from_center_size(
            egui::pos2(thumb_x, track.center().y),
            Vec2::splat(thumb_size),
        ),
        Rounding::ZERO,
        if shown == 0.0 {
            Palette::ZINC_300
        } else {
            Palette::ROSE_500
        },
    );
    resp
}

/// The pan block of a sound's right-click menu. Dragging the slider moves the
/// sound at once (also while it plays) and saves the value in the library.
pub fn sound_pan_section(ui: &mut Ui, state: &mut SoundSharedApp, sound: &SoundEntry) {
    let Some(mut pan) = state.library().sound(&sound.path).map(|s| s.pan) else {
        return;
    };
    ui.add_space(Metrics::SM);
    hairline(ui);
    ui.add_space(Metrics::SM);
    ui.label(micro_label("Pan"));
    ui.add_space(Metrics::XS);
    ui.horizontal(|ui| {
        let resp = pan_slider(ui, &mut pan, 120.0, "");
        ui.label(
            egui::RichText::new(pan_label(pan))
                .font(glyph_font(10.0))
                .color(Palette::ZINC_400),
        );
        if resp.changed() {
            state.request_set_sound_pan(sound.id, &sound.path, pan);
        }
    });
}

impl SoundSharedApp {
    /// Set a sound's pan: stored in the library (so it persists) and pushed to
    /// the Director, which moves the live track. Fire and forget.
    pub fn request_set_sound_pan(
        &mut self,
        id: crate::audio::SoundId,
        path: &std::path::Path,
        pan: f32,
    ) {
        let pan = clamp_pan(pan);
        if self.library.set_pan(path, pan) {
            self.library_dirty = true;
        }
        let (response, _rx) = bounded(1);
        self.queue_pan_command(AudioCommand::SetPan { id, pan, response });
    }

    /// Re-apply every pan of the library to what plays now (after a scene
    /// item's pan was edited). Fire and forget.
    pub fn request_sync_pans(&self) {
        let (response, _rx) = bounded(1);
        self.queue_pan_command(AudioCommand::SyncPans { response });
    }

    fn queue_pan_command(&self, command: AudioCommand) {
        if let Err(e) = self.app_state.audio_cmd_tx.try_send(command) {
            tracing::error!("Could not queue a pan command (channel full): {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_maps_to_the_whole_range() {
        assert_eq!(pan_from_pointer(100.0, 100.0, 80.0), -1.0);
        assert_eq!(pan_from_pointer(180.0, 100.0, 80.0), 1.0);
        assert_eq!(pan_from_pointer(140.0, 100.0, 80.0), 0.0);
        assert_eq!(pan_from_pointer(-500.0, 100.0, 80.0), -1.0);
        assert_eq!(pan_from_pointer(900.0, 100.0, 80.0), 1.0);
        // A degenerate width never divides by zero.
        assert!(pan_from_pointer(5.0, 0.0, 0.0).abs() <= 1.0);
    }

    #[test]
    fn the_middle_snaps_to_exact_zero() {
        assert_eq!(snap_pan(0.04), 0.0);
        assert_eq!(snap_pan(-0.049), 0.0);
        assert_eq!(snap_pan(0.5), 0.5);
        assert_eq!(snap_pan(f32::NAN), 0.0);
        assert_eq!(snap_pan(3.0), 1.0);
    }

    #[test]
    fn labels_read_left_and_right() {
        assert_eq!(pan_label(0.0), "Centre");
        assert_eq!(pan_label(-0.4), "L 40%");
        assert_eq!(pan_label(0.25), "R 25%");
        assert_eq!(pan_label(-1.0), "L 100%");
        assert_eq!(pan_label(2.0), "R 100%");
        assert_eq!(pan_label(0.004), "Centre");
        assert_eq!(pan_label(f32::NAN), "Centre");
    }
}
