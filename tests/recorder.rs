//! Session recorder: WAV files, splitting, finalizing, loss counting and the
//! tee that feeds it. Everything lives in a temporary directory; no audio
//! device, no config file and no network port is used.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use soundshared::audio::decoder::AudioDecoder;
use soundshared::audio::mixer::MixedOutput;
use soundshared::audio::recorder::{
    spawn_tee, Recorder, RecorderError, RecorderOptions, RecorderState, RecorderTap, SampleFormat,
};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-recorder-test-{}-{}-{}",
            std::process::id(),
            name,
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A WAV file read back by a small independent parser.
#[derive(Debug)]
struct Wav {
    format_tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    data: Vec<u8>,
}

impl Wav {
    fn frames(&self) -> usize {
        self.data.len() / (self.channels as usize * self.bits as usize / 8)
    }

    fn samples(&self) -> Vec<f32> {
        match (self.format_tag, self.bits) {
            (1, 16) => self
                .data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32767.0)
                .collect(),
            (3, 32) => self
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            other => panic!("unexpected format {other:?}"),
        }
    }
}

/// Parse and strictly validate: the RIFF size and the data size must match the
/// real file length.
fn parse_wav(path: &Path) -> Result<Wav, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if riff != bytes.len() - 8 {
        return Err(format!("RIFF size {riff} != {}", bytes.len() - 8));
    }
    let (mut format_tag, mut channels, mut rate, mut bits) = (0, 0, 0, 0);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        match id {
            b"fmt " => {
                format_tag = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
                channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
                rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
                let byte_rate = u32::from_le_bytes(bytes[body + 8..body + 12].try_into().unwrap());
                let align = u16::from_le_bytes(bytes[body + 12..body + 14].try_into().unwrap());
                bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
                if align != channels * bits / 8 || byte_rate != rate * u32::from(align) {
                    return Err("inconsistent fmt chunk".into());
                }
            }
            b"data" => {
                if body + size != bytes.len() {
                    return Err(format!(
                        "data size {size} does not reach the end of the file ({} left)",
                        bytes.len() - body
                    ));
                }
                return Ok(Wav {
                    format_tag,
                    channels,
                    rate,
                    bits,
                    data: bytes[body..].to_vec(),
                });
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    Err("no data chunk".into())
}

/// `frames` stereo frames of a slow ramp, so ordering mistakes show up.
fn ramp(start: usize, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    for i in start..start + frames {
        let v = ((i % 2000) as f32 / 2000.0) * 1.8 - 0.9;
        out.push(v);
        out.push(-v);
    }
    out
}

fn opts(dir: &TempDir) -> RecorderOptions {
    let mut o = RecorderOptions::new(&dir.0);
    o.pad_silence = false;
    o.file_stem = Some("session".into());
    o.flush_interval = Duration::from_millis(20);
    o
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn pcm16_file_is_a_valid_wav_that_symphonia_reads() {
    let dir = TempDir::new("pcm16");
    let tap = Arc::new(RecorderTap::new());
    let recorder = Recorder::start(&tap, opts(&dir)).unwrap();

    let mut expected = Vec::new();
    for chunk in 0..20 {
        let buf = ramp(chunk * 480, 480);
        tap.offer(&buf);
        expected.extend(buf);
    }
    let summary = recorder.finish();

    assert_eq!(summary.frames, 20 * 480);
    assert_eq!(summary.paths.len(), 1);
    assert!(summary.paths[0].ends_with("session.wav"));
    assert_eq!(summary.error, None);
    assert!(!tap.is_active());

    let wav = parse_wav(&summary.paths[0]).unwrap();
    assert_eq!(
        (wav.format_tag, wav.channels, wav.rate, wav.bits),
        (1, 2, 48_000, 16)
    );
    assert_eq!(wav.frames(), 20 * 480);
    for (got, want) in wav.samples().iter().zip(&expected) {
        assert!((got - want).abs() < 1.0 / 32000.0, "{got} vs {want}");
    }
    assert_eq!(
        std::fs::metadata(&summary.paths[0]).unwrap().len(),
        summary.bytes
    );

    // An independent decoder (Symphonia) agrees.
    let decoded = AudioDecoder::decode_file(&summary.paths[0]).unwrap();
    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.channels, 2);
    assert_eq!(decoded.frames, 20 * 480);
    for (got, want) in decoded.samples.iter().zip(&expected) {
        assert!((got - want).abs() < 1.0 / 16000.0, "{got} vs {want}");
    }
}

#[test]
fn float32_option_keeps_samples_exactly() {
    let dir = TempDir::new("f32");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.format = SampleFormat::Float32;
    let recorder = Recorder::start(&tap, o).unwrap();
    let buf = ramp(0, 1000);
    tap.offer(&buf);
    let summary = recorder.finish();

    let wav = parse_wav(&summary.paths[0]).unwrap();
    assert_eq!((wav.format_tag, wav.bits), (3, 32));
    assert_eq!(wav.samples(), buf);

    let decoded = AudioDecoder::decode_file(&summary.paths[0]).unwrap();
    assert_eq!(decoded.frames, 1000);
    for (got, want) in decoded.samples.iter().zip(&buf) {
        assert!((got - want).abs() < 1e-6);
    }
}

#[test]
fn long_recordings_split_into_parts() {
    let dir = TempDir::new("split");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.split_bytes = 4000; // 1000 frames of 16-bit stereo per file
    let recorder = Recorder::start(&tap, o).unwrap();

    let mut expected = Vec::new();
    for chunk in 0..5 {
        let buf = ramp(chunk * 500, 500); // 2500 frames in all
        tap.offer(&buf);
        expected.extend(buf);
    }
    let summary = recorder.finish();

    let names: Vec<String> = summary
        .paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["session.wav", "session_part2.wav", "session_part3.wav"]
    );
    let mut all = Vec::new();
    let frame_counts: Vec<usize> = summary
        .paths
        .iter()
        .map(|p| {
            let wav = parse_wav(p).unwrap();
            all.extend(wav.samples());
            wav.frames()
        })
        .collect();
    assert_eq!(frame_counts, [1000, 1000, 500]);
    assert_eq!(all.len(), expected.len());
    for (got, want) in all.iter().zip(&expected) {
        assert!((got - want).abs() < 1.0 / 32000.0);
    }
    assert_eq!(summary.frames, 2500);
}

#[test]
fn exact_limit_does_not_leave_an_empty_part() {
    let dir = TempDir::new("exact");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.split_bytes = 4000;
    let recorder = Recorder::start(&tap, o).unwrap();
    tap.offer(&ramp(0, 1000));
    let summary = recorder.finish();
    assert_eq!(summary.paths.len(), 1);
    assert_eq!(parse_wav(&summary.paths[0]).unwrap().frames(), 1000);
}

#[test]
fn stop_finalizes_everything_that_was_queued() {
    let dir = TempDir::new("stop");
    let tap = Arc::new(RecorderTap::new());
    let recorder = Recorder::start(&tap, opts(&dir)).unwrap();
    for chunk in 0..100 {
        tap.offer(&ramp(chunk * 480, 480));
    }
    recorder.request_stop();
    // Detached at once: later buffers are not recorded.
    assert!(!tap.is_active());
    tap.offer(&ramp(0, 480));
    wait_until("writer to end", || recorder.is_finished());
    assert_eq!(recorder.stats().state, RecorderState::Finished);
    let summary = recorder.finish();
    assert_eq!(summary.frames, 100 * 480);
    assert_eq!(parse_wav(&summary.paths[0]).unwrap().frames(), 100 * 480);
}

#[test]
fn file_is_valid_while_recording_and_after_the_writer_is_dropped() {
    let dir = TempDir::new("live");
    let tap = Arc::new(RecorderTap::new());
    let recorder = Recorder::start(&tap, opts(&dir)).unwrap();
    tap.offer(&ramp(0, 4800));

    // The periodic header patch makes the file readable without stopping.
    let path = dir.0.join("session.wav");
    wait_until("a readable file mid-recording", || {
        parse_wav(&path).is_ok_and(|w| w.frames() == 4800)
    });
    assert_eq!(recorder.stats().state, RecorderState::Recording);

    // Dropping the handle ends the recording and still finalizes the file.
    drop(recorder);
    wait_until("tap to be released", || !tap.is_active());
    tap.offer(&ramp(0, 480));
    assert_eq!(parse_wav(&path).unwrap().frames(), 4800);
}

#[test]
fn lost_frames_are_counted_when_the_writer_is_slow() {
    let dir = TempDir::new("loss");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.write_delay = Duration::from_millis(2);
    let recorder = Recorder::start(&tap, o).unwrap();

    let total_buffers = 1500u64;
    for _ in 0..total_buffers {
        tap.offer(&[0.25; 480 * 2]); // never blocks, even with a stalled writer
    }
    let summary = recorder.finish();

    assert!(summary.dropped_frames > 0, "a burst must overflow the tap");
    assert_eq!(
        summary.frames + summary.dropped_frames,
        total_buffers * 480,
        "every frame is either written or counted as lost"
    );
    assert_eq!(
        parse_wav(&summary.paths[0]).unwrap().frames() as u64,
        summary.frames
    );
}

#[test]
fn size_warning_is_raised_over_the_threshold() {
    let dir = TempDir::new("warn");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.warn_bytes = 1000;
    let recorder = Recorder::start(&tap, o).unwrap();
    assert!(recorder.stats().warning.is_none());
    tap.offer(&ramp(0, 500)); // 2000 bytes
    wait_until("the size warning", || recorder.stats().warning.is_some());
    let warning = recorder.stats().warning.unwrap();
    assert!(warning.contains("Large"), "{warning}");
    recorder.finish();
}

#[test]
fn idle_time_is_filled_with_silence() {
    let dir = TempDir::new("pad");
    let tap = Arc::new(RecorderTap::new());
    let mut o = opts(&dir);
    o.pad_silence = true;
    let recorder = Recorder::start(&tap, o).unwrap();
    tap.offer(&ramp(0, 480));
    std::thread::sleep(Duration::from_millis(500));
    let summary = recorder.finish();
    // ~0.5 s idle; allow for timer coarseness.
    assert!(summary.frames > 480 + 12_000, "frames = {}", summary.frames);
    let wav = parse_wav(&summary.paths[0]).unwrap();
    assert!(wav.samples()[480 * 2..].iter().all(|&s| s == 0.0));
}

#[test]
fn only_one_recording_at_a_time_and_names_never_collide() {
    let dir = TempDir::new("one");
    let tap = Arc::new(RecorderTap::new());
    let first = Recorder::start(&tap, opts(&dir)).unwrap();
    let second = Recorder::start(&tap, opts(&dir));
    assert!(matches!(second, Err(RecorderError::AlreadyRecording)));
    tap.offer(&ramp(0, 10));
    let first_summary = first.finish();

    // Same requested name again: the first file is kept, the new one gets a suffix.
    let again = Recorder::start(&tap, opts(&dir)).unwrap();
    let again_summary = again.finish();
    assert!(first_summary.paths[0].ends_with("session.wav"));
    assert!(again_summary.paths[0].ends_with("session-2.wav"));
    assert_eq!(parse_wav(&first_summary.paths[0]).unwrap().frames(), 10);
}

#[test]
fn missing_folder_is_created_and_a_bad_one_is_reported() {
    let dir = TempDir::new("folder");
    let tap = Arc::new(RecorderTap::new());

    let mut o = opts(&dir);
    o.dir = dir.0.join("a").join("b");
    let rec = Recorder::start(&tap, o).unwrap();
    rec.finish();
    assert!(dir.0.join("a/b/session.wav").is_file());

    // A file where the folder should be.
    let blocker = dir.0.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let mut o = opts(&dir);
    o.dir = blocker.join("sub");
    let err = Recorder::start(&tap, o).err().expect("must fail");
    assert!(matches!(err, RecorderError::Folder { .. }), "{err}");
    assert!(!tap.is_active(), "a failed start must release the tap");
}

#[cfg(unix)]
#[test]
fn start_is_refused_when_there_is_not_enough_free_space() {
    let dir = TempDir::new("space");
    let tap = Arc::new(RecorderTap::new());
    assert!(soundshared::audio::recorder::free_space(&dir.0).is_some());
    let mut o = opts(&dir);
    o.min_free_bytes = u64::MAX;
    let err = Recorder::start(&tap, o).err().expect("must fail");
    assert!(matches!(err, RecorderError::DiskSpace { .. }), "{err}");
    assert!(!tap.is_active());
    assert!(
        !dir.0.join("session.wav").exists(),
        "no file is left behind"
    );
}

#[test]
fn write_error_stops_cleanly_and_keeps_what_was_written() {
    let dir = TempDir::new("fail");
    let tap = Arc::new(RecorderTap::new());
    // The second part cannot be created: its name is taken.
    std::fs::write(dir.0.join("session_part2.wav"), b"occupied").unwrap();
    let mut o = opts(&dir);
    o.split_bytes = 4000;
    let recorder = Recorder::start(&tap, o).unwrap();
    tap.offer(&ramp(0, 1500));

    wait_until("the failure", || recorder.is_finished());
    let stats = recorder.stats();
    assert_eq!(stats.state, RecorderState::Failed);
    let error = stats.error.expect("an error message");
    assert!(error.starts_with("Recording stopped"), "{error}");
    assert!(!tap.is_active(), "a failed recording releases the tap");

    let summary = recorder.finish();
    assert!(summary.error.is_some());
    // The first part is final and holds everything up to the failure.
    assert_eq!(parse_wav(&summary.paths[0]).unwrap().frames(), 1000);
    // The occupied file was not overwritten.
    assert_eq!(
        std::fs::read(dir.0.join("session_part2.wav")).unwrap(),
        b"occupied"
    );
}

#[test]
fn tee_feeds_both_the_players_and_the_recording() {
    let dir = TempDir::new("tee");
    let (mixer_tx, mixer_rx) = bounded::<MixedOutput>(64);
    let tap = Arc::new(RecorderTap::new());
    let players = spawn_tee(mixer_rx, tap.clone());
    let recorder = Recorder::start(&tap, opts(&dir)).unwrap();

    let n = 30usize;
    for i in 0..n {
        mixer_tx
            .send(MixedOutput {
                buffer: ramp(i * 480, 480),
                seq_no: i as u64,
                timestamp: i as u64,
            })
            .unwrap();
    }
    // The players' side sees every buffer, in order, untouched.
    for i in 0..n {
        let out = players.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(out.seq_no, i as u64);
        assert_eq!(out.buffer, ramp(i * 480, 480));
    }
    let summary = recorder.finish();
    assert_eq!(summary.frames, (n * 480) as u64);
    let wav = parse_wav(&summary.paths[0]).unwrap();
    let want: Vec<f32> = (0..n).flat_map(|i| ramp(i * 480, 480)).collect();
    for (got, want) in wav.samples().iter().zip(&want) {
        assert!((got - want).abs() < 1.0 / 32000.0);
    }

    // With nothing recording the tee only passes audio on.
    mixer_tx
        .send(MixedOutput {
            buffer: vec![0.0; 960],
            seq_no: 99,
            timestamp: 99,
        })
        .unwrap();
    assert_eq!(
        players.recv_timeout(Duration::from_secs(5)).unwrap().seq_no,
        99
    );
}

#[test]
fn recorder_settings_are_backward_compatible_in_config_toml() {
    use soundshared::config::Config;

    // A config from before recording existed loads with the defaults.
    let old: Config = toml::from_str("[gui]\nreduce_motion = true\n").unwrap();
    assert_eq!(old.gui.recordings_dir, None);
    assert!(!old.gui.record_float32);

    // Defaults are not written out, so untouched configs stay untouched.
    let text = toml::to_string(&Config::default()).unwrap();
    assert!(!text.contains("recordings_dir") && !text.contains("record_float32"));

    // Chosen values survive a round trip.
    let mut config = Config::default();
    config.gui.recordings_dir = Some("/tmp/takes".into());
    config.gui.record_float32 = true;
    let back: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(back.gui, config.gui);
}
