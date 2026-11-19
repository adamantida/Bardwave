//! REC button, time / size readout and recordings folder of the top bar.
//!
//! Nothing here blocks a frame: starting a recording (folder, free-space check,
//! first file) and the native folder chooser run on short-lived threads and
//! answer through channels; stopping only detaches the tap, and the writer
//! thread finalizes the file by itself. See [`crate::audio::recorder`].

use crate::audio::recorder::{
    default_recordings_dir, format_clock, format_size, global_tap, Recorder, RecorderError,
    RecorderOptions, RecorderState, RecorderStats, RecorderTap, RecordingSummary, SampleFormat,
};
use crate::config::Config;
use crate::gui::app::SoundSharedApp;
use crate::gui::theme::{mono_font, Metrics, Palette};
use crate::gui::widgets::{glyph, signal_dot, BtnKind, ConsoleButton, IconButton};
use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::Ui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A line of feedback next to the REC button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

/// State of the REC controls.
pub struct RecorderUi {
    tap: Arc<RecorderTap>,
    ctx: egui::Context,
    /// The recording that takes audio now.
    active: Option<Recorder>,
    /// A recording that was asked to stop and is finalizing its file.
    stopping: Option<Recorder>,
    /// Answer of the start thread.
    starting: Option<Receiver<Result<Recorder, RecorderError>>>,
    /// Latest numbers of the active recording, for painting.
    stats: Option<RecorderStats>,
    /// Folder chooser results.
    folder_tx: Sender<Option<PathBuf>>,
    folder_rx: Receiver<Option<PathBuf>>,
    picking_folder: bool,
    /// What happened last (a saved file, a warning, an error).
    notice: Option<Notice>,
}

impl RecorderUi {
    /// Controls bound to the process-wide recorder tap.
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_tap(ctx, global_tap())
    }

    /// Controls bound to `tap` (tests use their own).
    pub fn with_tap(ctx: egui::Context, tap: Arc<RecorderTap>) -> Self {
        let (folder_tx, folder_rx) = unbounded();
        Self {
            tap,
            ctx,
            active: None,
            stopping: None,
            starting: None,
            stats: None,
            folder_tx,
            folder_rx,
            picking_folder: false,
            notice: None,
        }
    }

    /// Whether a recording is running (or starting).
    pub fn is_recording(&self) -> bool {
        self.active.is_some() || self.starting.is_some()
    }

    /// The last feedback line.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Latest stats of the running recording.
    pub fn stats(&self) -> Option<&RecorderStats> {
        self.stats.as_ref()
    }

    /// Start when idle, stop when recording. Never blocks.
    pub fn toggle(&mut self, config: &Arc<Mutex<Config>>) {
        if let Some(active) = self.active.take() {
            active.request_stop();
            self.stopping = Some(active);
            self.ctx.request_repaint();
            return;
        }
        if self.starting.is_some() || self.stopping.is_some() {
            return;
        }
        self.notice = None;
        let opts = options_from_config(&config.lock().unwrap());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let tap = self.tap.clone();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("recorder-start".into())
            .spawn(move || {
                let _ = tx.send(Recorder::start(&tap, opts));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.starting = Some(rx),
            Err(e) => {
                self.notice = Some(Notice {
                    text: format!("Cannot start recording: {e}"),
                    is_error: true,
                })
            }
        }
    }

    /// Open the native folder chooser on its own thread.
    pub fn choose_folder(&mut self, config: &Arc<Mutex<Config>>) {
        if self.picking_folder {
            return;
        }
        let start = recordings_dir(&config.lock().unwrap());
        let tx = self.folder_tx.clone();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("recorder-folder-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new().set_title("Recordings folder");
                if start.is_dir() {
                    dialog = dialog.set_directory(&start);
                }
                let _ = tx.send(dialog.pick_folder());
                ctx.request_repaint();
            });
        self.picking_folder = spawned.is_ok();
    }

    /// Collect answers and finished recordings. Call once per frame.
    pub fn poll(&mut self, config: &Arc<Mutex<Config>>) {
        // A folder the operator picked.
        while let Ok(picked) = self.folder_rx.try_recv() {
            self.picking_folder = false;
            if let Some(dir) = picked {
                let mut cfg = config.lock().unwrap();
                cfg.gui.recordings_dir = Some(dir.to_string_lossy().into_owned());
                if let Err(e) = cfg.save() {
                    tracing::error!("Failed to save the recordings folder: {e}");
                }
            }
        }

        // The start thread's answer.
        if let Some(rx) = &self.starting {
            if let Ok(result) = rx.try_recv() {
                self.starting = None;
                match result {
                    Ok(recorder) => self.active = Some(recorder),
                    Err(e) => {
                        self.notice = Some(Notice {
                            text: format!("Cannot start recording: {e}"),
                            is_error: true,
                        })
                    }
                }
            }
        }

        // The running recording: keep its numbers, notice a failure.
        if let Some(active) = &self.active {
            let stats = active.stats();
            let failed = stats.state == RecorderState::Failed;
            self.stats = Some(stats);
            if failed || active.is_finished() {
                self.stopping = self.active.take();
            } else {
                self.ctx.request_repaint_after(Duration::from_millis(500));
            }
        } else if self.stopping.is_none() {
            self.stats = None;
        }

        // A stopped recording whose file is final.
        if self.stopping.as_ref().is_some_and(|r| r.is_finished()) {
            if let Some(recorder) = self.stopping.take() {
                let summary = recorder.finish();
                self.notice = Some(summary_notice(&summary));
                self.stats = None;
            }
        } else if self.stopping.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

/// The recordings folder from the config (default: `<data dir>/recordings`).
pub fn recordings_dir(config: &Config) -> PathBuf {
    match config.gui.recordings_dir.as_deref().map(str::trim) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => default_recordings_dir(),
    }
}

/// Recorder options for the current config.
pub fn options_from_config(config: &Config) -> RecorderOptions {
    let mut opts = RecorderOptions::new(recordings_dir(config));
    if config.gui.record_float32 {
        opts.format = SampleFormat::Float32;
    }
    opts
}

/// The feedback line for a finished recording.
pub fn summary_notice(summary: &RecordingSummary) -> Notice {
    if let Some(error) = &summary.error {
        return Notice {
            text: error.clone(),
            is_error: true,
        };
    }
    let name = summary
        .paths
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut text = format!(
        "Saved {name} ({}, {})",
        format_clock(summary.duration_secs),
        format_size(summary.bytes)
    );
    if summary.paths.len() > 1 {
        text.push_str(&format!(" in {} files", summary.paths.len()));
    }
    if summary.dropped_frames > 0 {
        text.push_str(&format!(
            "; {} frames were lost (disk too slow)",
            summary.dropped_frames
        ));
    }
    Notice {
        text,
        is_error: false,
    }
}

/// `00:12:34  ·  12.3 MB` for a running recording.
pub fn readout_text(stats: &RecorderStats) -> String {
    format!(
        "{}  ·  {}",
        format_clock(stats.elapsed_secs()),
        format_size(stats.bytes_written)
    )
}

/// Draw the REC controls into the top bar row.
pub fn top_bar_controls(ui: &mut Ui, state: &mut SoundSharedApp) {
    let config = state.app_state.config.clone();
    let reduced = state.reduced_motion();
    let rec = &mut state.recorder;

    let recording = rec.active.is_some();
    let busy = rec.starting.is_some() || rec.stopping.is_some();

    let (label, kind, tooltip) = if recording {
        ("Stop rec", BtnKind::Armed, "Stop and save the recording")
    } else {
        (
            "Rec",
            BtnKind::Ghost,
            "Record the mix the players hear to a WAV file",
        )
    };
    let button = ConsoleButton::new(label)
        .kind(kind)
        .width(92.0)
        .height(26.0)
        .enabled(!busy)
        .tooltip(tooltip)
        .id_salt("top_rec");
    if button.show(ui).clicked() {
        rec.toggle(&config);
    }

    if let Some(stats) = rec
        .stats
        .clone()
        .filter(|_| recording || rec.stopping.is_some())
    {
        ui.add_space(Metrics::SM);
        signal_dot(ui, Palette::ROSE_500, 7.0, !reduced);
        ui.add_space(Metrics::XS);
        ui.label(
            egui::RichText::new(readout_text(&stats))
                .font(mono_font(11.0))
                .color(Palette::WHITE),
        );
        if stats.dropped_frames > 0 {
            ui.label(
                egui::RichText::new(format!("{} lost", stats.dropped_frames))
                    .font(mono_font(10.0))
                    .color(Palette::AMBER_400),
            );
        }
        if let Some(warning) = &stats.warning {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(warning)
                        .font(mono_font(10.0))
                        .color(Palette::AMBER_400),
                )
                .truncate(),
            )
            .on_hover_text(warning);
        }
    } else if let Some(notice) = rec.notice.clone() {
        ui.add_space(Metrics::SM);
        let color = if notice.is_error {
            Palette::ROSE_400
        } else {
            Palette::ZINC_400
        };
        ui.allocate_ui(egui::vec2(260.0, ui.available_height()), |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&notice.text)
                        .font(mono_font(10.0))
                        .color(color),
                )
                .truncate(),
            )
            .on_hover_text(&notice.text);
        });
    }

    ui.add_space(Metrics::SM);
    let dir = recordings_dir(&config.lock().unwrap());
    let pick = IconButton::new(glyph::FOLDER)
        .size(26.0)
        .font_size(11.0)
        .tooltip(format!(
            "Recordings folder: {}\nClick to change (applies to the next recording)",
            dir.display()
        ))
        .id_salt("top_rec_folder")
        .show(ui);
    if pick.clicked() && !recording {
        rec.choose_folder(&config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn folder_defaults_to_the_data_dir() {
        let config = Config::default();
        assert_eq!(recordings_dir(&config), default_recordings_dir());
        assert!(recordings_dir(&config).ends_with("recordings"));
    }

    #[test]
    fn configured_folder_and_format_are_used() {
        let mut config = Config::default();
        config.gui.recordings_dir = Some("/tmp/somewhere".into());
        config.gui.record_float32 = true;
        let opts = options_from_config(&config);
        assert_eq!(opts.dir, Path::new("/tmp/somewhere"));
        assert_eq!(opts.format, SampleFormat::Float32);

        config.gui.recordings_dir = Some("   ".into());
        assert_eq!(recordings_dir(&config), default_recordings_dir());
    }

    #[test]
    fn notice_reports_errors_and_losses() {
        let ok = RecordingSummary {
            paths: vec![
                PathBuf::from("/x/bardwave-1.wav"),
                PathBuf::from("/x/b_part2.wav"),
            ],
            frames: 48_000 * 61,
            bytes: 5 * 1024 * 1024,
            dropped_frames: 10,
            duration_secs: 61.0,
            error: None,
        };
        let n = summary_notice(&ok);
        assert!(!n.is_error);
        assert!(n.text.contains("bardwave-1.wav") && n.text.contains("00:01:01"));
        assert!(n.text.contains("2 files") && n.text.contains("10 frames"));

        let bad = RecordingSummary {
            error: Some("Recording stopped: disk full".into()),
            ..ok
        };
        assert!(summary_notice(&bad).is_error);
    }
}
