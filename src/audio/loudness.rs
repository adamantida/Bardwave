//! Loudness measurement (EBU R128 / ITU-R BS.1770) for track normalization.
//!
//! Isolated from the engine: this module only measures decoded audio and
//! computes a gain; applying it is up to the caller.

use std::thread;

use crossbeam_channel::{bounded, Receiver};
use ebur128::{EbuR128, Mode};

use super::decoder::DecodedAudio;

/// Minimum duration required for an integrated measurement (one gating block).
pub const MIN_DURATION_MS: usize = 400;
/// Lowest gain that `gain_db_for_target` will return.
pub const MIN_GAIN_DB: f32 = -24.0;
/// Highest gain that `gain_db_for_target` will return.
pub const MAX_GAIN_DB: f32 = 12.0;
/// True-peak ceiling (dBTP) that boosted audio must not exceed.
pub const TRUE_PEAK_CEILING_DBTP: f32 = -1.0;

/// Result of analysing one file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessInfo {
    /// Integrated loudness in LUFS.
    pub lufs: f32,
    /// Maximum true peak across channels, in dBTP (`None` if it could not be measured).
    pub true_peak_dbtp: Option<f32>,
}

impl LoudnessInfo {
    /// Gain (dB) that brings this file to `target_lufs`, clipped to the sane
    /// range and limited so the true peak stays under -1 dBTP.
    pub fn gain_db_for_target(&self, target_lufs: f32) -> f32 {
        gain_db_for_target_with_peak(self.lufs, self.true_peak_dbtp, target_lufs)
    }
}

/// Integrated loudness of `audio` in LUFS, or `None` for silence / audio
/// shorter than 400 ms / unsupported input.
pub fn measure_lufs(audio: &DecodedAudio) -> Option<f32> {
    analyze(audio).map(|i| i.lufs)
}

/// Integrated loudness plus true peak. Same `None` conditions as `measure_lufs`.
pub fn analyze(audio: &DecodedAudio) -> Option<LoudnessInfo> {
    let channels = audio.channels as usize;
    if channels == 0 || audio.sample_rate == 0 {
        return None;
    }
    let frames = audio.samples.len() / channels;
    if frames * 1000 < MIN_DURATION_MS * audio.sample_rate as usize {
        return None;
    }
    let mut meter =
        EbuR128::new(audio.channels, audio.sample_rate, Mode::I | Mode::TRUE_PEAK).ok()?;
    // A trailing partial frame, if any, is dropped.
    meter
        .add_frames_f32(&audio.samples[..frames * channels])
        .ok()?;
    let lufs = meter.loudness_global().ok()?;
    if !lufs.is_finite() {
        return None; // pure silence is reported as -inf
    }
    let mut peak = 0.0f64;
    let mut peak_ok = true;
    for ch in 0..audio.channels {
        match meter.true_peak(ch) {
            Ok(p) => peak = peak.max(p),
            Err(_) => peak_ok = false,
        }
    }
    let true_peak_dbtp = if peak_ok && peak > 0.0 {
        Some((20.0 * peak.log10()) as f32)
    } else {
        None
    };
    Some(LoudnessInfo {
        lufs: lufs as f32,
        true_peak_dbtp,
    })
}

/// Gain in dB to move `measured_lufs` to `target_lufs`, clamped to
/// `MIN_GAIN_DB..=MAX_GAIN_DB`. Non-finite inputs yield 0 dB.
pub fn gain_db_for_target(measured_lufs: f32, target_lufs: f32) -> f32 {
    gain_db_for_target_with_peak(measured_lufs, None, target_lufs)
}

/// Like `gain_db_for_target`, but when `true_peak_dbtp` is known a positive
/// gain is limited so the peak does not exceed -1 dBTP. Attenuation is never
/// limited by the peak.
pub fn gain_db_for_target_with_peak(
    measured_lufs: f32,
    true_peak_dbtp: Option<f32>,
    target_lufs: f32,
) -> f32 {
    if !measured_lufs.is_finite() || !target_lufs.is_finite() {
        return 0.0;
    }
    let mut gain = (target_lufs - measured_lufs).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    if gain > 0.0 {
        if let Some(peak) = true_peak_dbtp.filter(|p| p.is_finite()) {
            let headroom = TRUE_PEAK_CEILING_DBTP - peak;
            gain = gain.min(headroom.max(0.0));
        }
    }
    gain
}

/// Analyse `audio` on a background thread. The receiver yields exactly one
/// `Option<LoudnessInfo>` (`None` = silent / too short); the caller is never
/// blocked. If the receiver is dropped the result is discarded.
pub fn analyze_in_background(audio: DecodedAudio) -> Receiver<Option<LoudnessInfo>> {
    let (tx, rx) = bounded(1);
    let spawned = thread::Builder::new()
        .name("loudness-analyze".into())
        .spawn({
            let tx = tx.clone();
            move || {
                let _ = tx.send(analyze(&audio));
            }
        });
    if spawned.is_err() {
        // Could not start a thread: report "unknown" rather than hang the receiver.
        let _ = tx.send(None);
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SR: u32 = 48_000;

    fn sine(amp: f32, secs: f32) -> DecodedAudio {
        let frames = (SR as f32 * secs) as usize;
        let mut s = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR as f32).sin();
            s.push(v);
            s.push(v);
        }
        DecodedAudio::new(s, SR, 2)
    }

    fn db(x: f32) -> f32 {
        10f32.powf(x / 20.0)
    }

    fn scaled(a: &DecodedAudio, gain_db: f32) -> DecodedAudio {
        let g = db(gain_db);
        DecodedAudio::new(
            a.samples.iter().map(|x| x * g).collect(),
            a.sample_rate,
            a.channels,
        )
    }

    #[test]
    fn full_scale_sine_levels_match_bs1770() {
        // BS.1770: a full-scale 1 kHz sine on ONE channel is -3.01 LUFS; the
        // same sine on both stereo channels sums power and reads 0 LUFS.
        let stereo = measure_lufs(&sine(1.0, 3.0)).unwrap();
        assert!(stereo.abs() < 0.2, "stereo got {stereo}");
        let mono_samples: Vec<f32> = sine(1.0, 3.0).samples.iter().step_by(2).copied().collect();
        let mono = measure_lufs(&DecodedAudio::new(mono_samples, SR, 1)).unwrap();
        assert!((mono + 3.01).abs() < 0.2, "mono got {mono}");
    }

    #[test]
    fn minus_20_dbfs_stereo_sine_is_17_db_below_full_scale_mono() {
        // Stereo -20 dBFS = -20 LUFS, i.e. 17 dB under the -3.01 LUFS of a
        // full-scale mono sine, and exactly 20 dB under full-scale stereo.
        let quiet = measure_lufs(&sine(db(-20.0), 3.0)).unwrap();
        assert!((quiet + 20.0).abs() < 0.2, "got {quiet}");
        assert!(((-3.01 - quiet) - 17.0).abs() < 0.3);
        let loud = measure_lufs(&sine(1.0, 3.0)).unwrap();
        assert!(((loud - quiet) - 20.0).abs() < 0.2);
    }

    #[test]
    fn silence_and_short_files_are_none() {
        let silence = DecodedAudio::new(vec![0.0; SR as usize * 2 * 2], SR, 2);
        assert_eq!(measure_lufs(&silence), None);
        assert_eq!(measure_lufs(&sine(0.5, 0.2)), None);
        assert_eq!(measure_lufs(&DecodedAudio::new(Vec::new(), SR, 2)), None);
    }

    #[test]
    fn gain_is_clamped() {
        assert_eq!(gain_db_for_target(-70.0, -14.0), MAX_GAIN_DB);
        assert_eq!(gain_db_for_target(0.0, -40.0), MIN_GAIN_DB);
        assert!((gain_db_for_target(-20.0, -16.0) - 4.0).abs() < 1e-6);
        assert_eq!(gain_db_for_target(f32::NEG_INFINITY, -16.0), 0.0);
    }

    #[test]
    fn true_peak_limits_boost_but_not_cut() {
        // peak at -3 dBTP, wants +10 dB -> only 2 dB of headroom to -1 dBTP
        let g = gain_db_for_target_with_peak(-26.0, Some(-3.0), -16.0);
        assert!((g - 2.0).abs() < 1e-6, "got {g}");
        // already above the ceiling: no boost
        assert_eq!(gain_db_for_target_with_peak(-26.0, Some(0.0), -16.0), 0.0);
        // attenuation untouched
        assert_eq!(gain_db_for_target_with_peak(-10.0, Some(0.0), -16.0), -6.0);
    }

    #[test]
    fn two_files_converge_to_target() {
        let target = -16.0;
        for amp_db in [-6.0f32, -20.0] {
            let a = sine(db(amp_db), 3.0);
            let info = analyze(&a).unwrap();
            let g = info.gain_db_for_target(target);
            let after = measure_lufs(&scaled(&a, g)).unwrap();
            assert!((after - target).abs() < 1.0, "amp {amp_db}: {after}");
        }
    }

    #[test]
    fn analyze_reports_true_peak() {
        let info = analyze(&sine(db(-6.0), 2.0)).unwrap();
        let p = info.true_peak_dbtp.unwrap();
        assert!((p + 6.0).abs() < 0.5, "got {p}");
    }

    #[test]
    fn background_analysis_delivers_result() {
        let rx = analyze_in_background(sine(0.5, 2.0));
        let info = rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert!(info.lufs.is_finite());
        let rx = analyze_in_background(DecodedAudio::new(vec![0.0; 1000], SR, 2));
        assert_eq!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), None);
    }
}
