//! The streaming source: long files decoded on a worker thread, read through a
//! ring buffer. Test files are generated here as WAV and live in a temporary
//! directory; nothing touches the user's config or any network port.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use soundshared::audio::decoder::AudioDecoder;
use soundshared::audio::mixer::AudioMixer;
use soundshared::audio::stream_source::{StreamEvent, StreamOptions, StreamSource};
use soundshared::audio::AudioEngine;
use uuid::Uuid;

/// A unique temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-stream-test-{}-{}-{}",
            std::process::id(),
            name,
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Deterministic, never-zero test signal: a sawtooth that differs per channel
/// and per frame, so any shift, repeat or dropped frame shows up.
fn signal(frames: usize, channels: usize) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for c in 0..channels {
            out.push(((i * (7 + c * 4)) % 20_000 + 1) as i16);
        }
    }
    out
}

/// A 16-bit PCM WAV file, optionally with a `LIST/INFO` tag chunk (before the
/// audio, where players expect it).
fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16], info: &[(&str, &str)]) {
    let data_len = (samples.len() * 2) as u32;
    let mut list = Vec::new();
    if !info.is_empty() {
        list.extend_from_slice(b"INFO");
        for (id, text) in info {
            let mut value = text.as_bytes().to_vec();
            value.push(0);
            if value.len() % 2 == 1 {
                value.push(0);
            }
            list.extend_from_slice(id.as_bytes());
            list.extend_from_slice(&(value.len() as u32).to_le_bytes());
            list.extend_from_slice(&value);
        }
    }
    let list_chunk = if list.is_empty() {
        0
    } else {
        8 + list.len() as u32
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(4 + 24 + 8 + data_len + list_chunk).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    if !list.is_empty() {
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&(list.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&list);
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn wav(dir: &TempDir, name: &str, rate: u32, channels: u16, frames: usize) -> PathBuf {
    let path = dir.path(name);
    write_wav(
        &path,
        rate,
        channels,
        &signal(frames, channels as usize),
        &[],
    );
    path
}

/// Read a source to its end without ever underrunning: a chunk is read only
/// when the ring holds it (or the file is fully buffered).
fn read_all(source: &mut StreamSource, chunk_frames: usize) -> Vec<f32> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut out = Vec::new();
    let mut chunk = vec![0.0f32; chunk_frames * 2];
    loop {
        assert!(Instant::now() < deadline, "stream stalled");
        if source.buffered_frames() < chunk_frames && !source.is_fully_buffered() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let n = source.read(&mut chunk);
        out.extend_from_slice(&chunk[..n * 2]);
        if n < chunk_frames {
            return out;
        }
    }
}

/// Read exactly `frames` frames of a looping source, never underrunning.
fn read_frames(source: &mut StreamSource, frames: usize, chunk_frames: usize) -> Vec<f32> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut out = Vec::with_capacity(frames * 2);
    let mut chunk = vec![0.0f32; chunk_frames * 2];
    while out.len() < frames * 2 {
        assert!(Instant::now() < deadline, "stream stalled");
        let want = chunk_frames.min(frames - out.len() / 2);
        if source.buffered_frames() < want {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let n = source.read(&mut chunk[..want * 2]);
        assert_eq!(n, want);
        out.extend_from_slice(&chunk[..n * 2]);
    }
    out
}

fn assert_same(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "different lengths");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!(
            (x - y).abs() <= 1e-6,
            "sample {i} differs: stream {x} vs decoded {y}"
        );
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn stream_gives_the_same_samples_as_full_decode() {
    let dir = TempDir::new("same");
    let path = wav(&dir, "a.wav", 48_000, 2, 96_000);
    let full = AudioDecoder::decode_file(&path).unwrap();

    // A ring far smaller than the file: backpressure and wrap-around.
    let mut source = StreamSource::open_with(
        &path,
        StreamOptions {
            ring_frames: 8_000,
            ..Default::default()
        },
    )
    .unwrap();
    let streamed = read_all(&mut source, 777);
    assert_same(&streamed, &full.samples);
    assert!(source.is_finished());
    assert_eq!(source.underruns(), 0);
}

#[test]
fn stream_matches_full_decode_after_resampling_and_downmix() {
    let dir = TempDir::new("resample");
    // Mono 44.1 kHz, stereo 22.05 kHz and a 96 kHz file: channel conversion
    // and the polyphase resampler must give the same samples chunk by chunk
    // as over the whole buffer.
    for (rate, channels, frames) in [
        (44_100u32, 1u16, 30_000usize),
        (22_050, 2, 20_000),
        (96_000, 2, 50_000),
    ] {
        let path = wav(&dir, &format!("r{rate}.wav"), rate, channels, frames);
        let full = AudioDecoder::decode_file(&path).unwrap();
        let mut source = StreamSource::open_with(
            &path,
            StreamOptions {
                ring_frames: 6_000,
                ..Default::default()
            },
        )
        .unwrap();
        let streamed = read_all(&mut source, 1_000);
        assert_eq!(
            streamed.len(),
            full.samples.len(),
            "{rate} Hz {channels} ch: frame count"
        );
        assert_same(&streamed, &full.samples);
    }
}

#[test]
fn duration_comes_from_the_header_before_any_decoding() {
    let dir = TempDir::new("duration");
    let path = wav(&dir, "d.wav", 44_100, 2, 44_100 * 3);
    let source = StreamSource::open_with(
        &path,
        StreamOptions {
            start_paused: true,
            ..Default::default()
        },
    )
    .unwrap();
    // The decoder has not produced a single frame, yet the length is known.
    assert_eq!(source.buffered_frames(), 0);
    let ms = source.duration_ms().expect("WAV header has a length");
    assert!((2_999..=3_001).contains(&ms), "duration {ms} ms");

    let probe = AudioDecoder::probe_file(&path).unwrap();
    assert!((probe.duration_secs.unwrap() - 3.0).abs() < 0.001);
}

#[test]
fn tags_are_read_from_the_file() {
    let dir = TempDir::new("tags");
    let path = dir.path("t.wav");
    write_wav(
        &path,
        48_000,
        2,
        &signal(4_800, 2),
        &[("INAM", "Tavern Theme"), ("IART", "The Bard")],
    );
    let probe = AudioDecoder::probe_file(&path).unwrap();
    assert_eq!(probe.meta.title.as_deref(), Some("Tavern Theme"));
    assert_eq!(probe.meta.artist.as_deref(), Some("The Bard"));

    let source = StreamSource::open(&path).unwrap();
    assert_eq!(source.meta().title.as_deref(), Some("Tavern Theme"));

    // A file without tags has an empty TrackMeta, not an error.
    let plain = wav(&dir, "plain.wav", 48_000, 2, 4_800);
    assert!(AudioDecoder::probe_file(&plain).unwrap().meta.is_empty());
}

#[test]
fn seek_repositions_and_discards_buffered_audio() {
    let dir = TempDir::new("seek");
    let path = wav(&dir, "s.wav", 48_000, 2, 48_000 * 3);
    let full = AudioDecoder::decode_file(&path).unwrap();
    let mut source = StreamSource::open(&path).unwrap();
    assert!(source.wait_ready(Duration::from_secs(5)));

    // Play a little, so the ring holds audio from the old position.
    let first = read_frames(&mut source, 500, 250);
    assert_same(&first, &full.samples[..1000]);
    assert_eq!(source.position_ms(), 10);

    // Forward, then backward: each time the first audible frame is exactly
    // the requested one, not leftovers from the old position.
    for target_ms in [2_000u64, 500, 0] {
        source.seek(target_ms);
        let target_frame = (target_ms * 48) as usize;
        let mut chunk = vec![0.0f32; 128];
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "seek never completed");
            assert_eq!(source.read(&mut chunk), 64);
            // Silence (never zero in the signal) means "not there yet".
            if chunk.iter().any(|s| *s != 0.0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_same(
            &chunk,
            &full.samples[target_frame * 2..target_frame * 2 + 128],
        );
        assert_eq!(source.position_frames(), target_frame as u64 + 64);
    }
    assert_eq!(
        source.underruns(),
        0,
        "refilling after a seek is not an underrun"
    );
}

#[test]
fn seeking_past_the_end_finishes_the_track() {
    let dir = TempDir::new("seek-end");
    let path = wav(&dir, "e.wav", 48_000, 2, 48_000);
    let (tx, rx) = bounded(4);
    let mut source = StreamSource::open(&path).unwrap();
    let id = Uuid::new_v4();
    source.set_event_sink(tx, id);
    source.seek(60_000);

    let mut chunk = vec![0.0f32; 256];
    wait_until("the track to finish", || source.read(&mut chunk) == 0);
    assert!(source.is_finished());
    assert_eq!(
        rx.try_recv(),
        Ok(StreamEvent::TrackFinished { track_id: id })
    );
}

#[test]
fn looping_is_seamless() {
    let dir = TempDir::new("loop");
    let path = wav(&dir, "l.wav", 48_000, 2, 9_600);
    let full = AudioDecoder::decode_file(&path).unwrap();
    // Held back until the loop flag is set, so the worker cannot reach the end
    // of the file first.
    let mut source = StreamSource::open_with(
        &path,
        StreamOptions {
            ring_frames: 5_000,
            start_paused: true,
        },
    )
    .unwrap();
    source.set_looped(true);
    source.set_decoder_paused(false);

    let frames = 9_600 * 5 + 321;
    let streamed = read_frames(&mut source, frames, 480);
    for (i, frame) in streamed.as_chunks::<2>().0.iter().enumerate() {
        let at = (i % 9_600) * 2;
        assert_eq!(frame, &full.samples[at..at + 2], "frame {i}");
    }
    assert!(!source.is_finished());
    assert_eq!(source.duration_frames(), Some(9_600));
    assert_eq!(
        source.position_frames(),
        321,
        "position wraps with the loop"
    );
    assert_eq!(source.underruns(), 0);
}

#[test]
fn underrun_gives_silence_and_does_not_panic() {
    let dir = TempDir::new("underrun");
    let path = wav(&dir, "u.wav", 48_000, 2, 48_000);
    let full = AudioDecoder::decode_file(&path).unwrap();
    let mut source = StreamSource::open_with(
        &path,
        StreamOptions {
            start_paused: true,
            ..Default::default()
        },
    )
    .unwrap();

    // The decoder produces nothing: every read is silence of the full size.
    let mut chunk = vec![1.0f32; 960];
    for expected in 1..=3u64 {
        assert_eq!(source.read(&mut chunk), 480);
        assert!(chunk.iter().all(|s| *s == 0.0));
        assert_eq!(source.underruns(), expected);
    }
    assert!(!source.is_finished());
    assert_eq!(source.position_frames(), 0, "silence does not advance time");

    // Once the decoder runs, the real audio starts from the very beginning.
    source.set_decoder_paused(false);
    assert!(source.wait_ready(Duration::from_secs(5)));
    assert_eq!(source.read(&mut chunk), 480);
    assert_same(&chunk, &full.samples[..960]);
}

#[test]
fn worker_thread_ends_when_the_source_is_dropped() {
    let dir = TempDir::new("drop");
    let path = wav(&dir, "x.wav", 48_000, 2, 48_000 * 10);

    let ended = |flag: &Arc<AtomicBool>| {
        wait_until("the worker thread to end", || flag.load(Ordering::Acquire))
    };

    // Busy (ring full, file not finished).
    let source = StreamSource::open(&path).unwrap();
    assert!(source.wait_ready(Duration::from_secs(5)));
    let flag = source.worker_exit_flag();
    assert!(!flag.load(Ordering::Acquire));
    drop(source);
    ended(&flag);

    // Held back by the diagnostic pause.
    let source = StreamSource::open_with(
        &path,
        StreamOptions {
            start_paused: true,
            ..Default::default()
        },
    )
    .unwrap();
    let flag = source.worker_exit_flag();
    drop(source);
    ended(&flag);

    // Idle after the end of the file.
    let short = wav(&dir, "short.wav", 48_000, 2, 4_800);
    let mut source = StreamSource::open(&short).unwrap();
    let flag = source.worker_exit_flag();
    read_all(&mut source, 480);
    drop(source);
    ended(&flag);
}

#[test]
fn track_finished_arrives_exactly_once() {
    let dir = TempDir::new("finished");
    let path = wav(&dir, "f.wav", 48_000, 2, 12_000);
    let (tx, rx) = bounded(8);
    let mut source = StreamSource::open(&path).unwrap();
    let id = Uuid::new_v4();
    source.set_event_sink(tx, id);

    let all = read_all(&mut source, 480);
    assert_eq!(all.len(), 24_000);
    assert!(source.is_finished());
    // Reading on after the end changes nothing and sends nothing more.
    let mut chunk = vec![0.0f32; 960];
    for _ in 0..5 {
        assert_eq!(source.read(&mut chunk), 0);
    }
    assert_eq!(
        rx.try_recv(),
        Ok(StreamEvent::TrackFinished { track_id: id })
    );
    assert!(rx.try_recv().is_err(), "a second event arrived");
}

#[test]
fn rewind_plays_a_finished_track_again() {
    let dir = TempDir::new("rewind");
    let path = wav(&dir, "w.wav", 48_000, 2, 4_800);
    let full = AudioDecoder::decode_file(&path).unwrap();
    let mut source = StreamSource::open(&path).unwrap();
    assert_same(&read_all(&mut source, 480), &full.samples);

    source.rewind();
    assert!(!source.is_finished());
    // Silence until the worker has repositioned (the signal is never zero),
    // then the whole file again.
    let mut head = vec![0.0f32; 128];
    wait_until("the refill", || {
        assert_eq!(source.read(&mut head), 64);
        head.iter().any(|s| *s != 0.0)
    });
    let mut again = head.clone();
    again.extend(read_all(&mut source, 480));
    assert_same(&again, &full.samples);
}

#[test]
fn mixer_plays_a_stream_track_and_reports_its_end() {
    let dir = TempDir::new("mixer");
    let path = wav(&dir, "m.wav", 48_000, 2, 48_000);
    let mixer = AudioMixer::new();
    let events = mixer.stream_events();

    let source = StreamSource::open(&path).unwrap();
    wait_until("the file to be buffered", || source.is_fully_buffered());
    let track = mixer.add_stream_track(source, false);
    assert_eq!(mixer.track_duration_ms(track).unwrap(), Some(1_000));
    mixer.play_track(track).unwrap();

    let mut out = vec![0.0f32; 480 * 2];
    let mut peak = 0.0f32;
    let mut blocks = 0;
    while mixer.is_track_playing(track).unwrap() {
        mixer.mix_into(&mut out);
        peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
        blocks += 1;
        assert!(blocks < 1_000, "the track never ended");
    }
    assert!(peak > 0.1, "the stream was silent");
    assert_eq!(mixer.track_underruns(track).unwrap(), 0);

    let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event, StreamEvent::TrackFinished { track_id: track });
    assert!(events.try_recv().is_err());

    // A stopped (finished) track plays again from the start.
    mixer.play_track(track).unwrap();
    assert_eq!(mixer.track_position_ms(track).unwrap(), 0);
    mixer.remove_track(track).unwrap();
}

#[test]
fn engine_streams_long_files_and_keeps_short_ones_in_memory() {
    let dir = TempDir::new("engine");
    let long = dir.path("long.wav");
    write_wav(
        &long,
        48_000,
        2,
        &signal(48_000 * 2, 2),
        &[("INAM", "Long One")],
    );
    let short = wav(&dir, "short.wav", 48_000, 2, 4_800);

    let engine = AudioEngine::new().unwrap();
    // Anything over one second is "long" for this test.
    engine.set_stream_threshold_secs(1);
    let long_id = engine.load_sound(&long).unwrap();
    let short_id = engine.load_sound(&short).unwrap();
    assert!(engine.is_sound_streamed(long_id));
    assert!(!engine.is_sound_streamed(short_id));
    assert_eq!(
        engine.sound_meta(long_id).unwrap().title.as_deref(),
        Some("Long One")
    );
    let (duration, rate, channels) = engine.sound_info(long_id).unwrap();
    assert!((duration - 2.0).abs() < 0.01 && rate == 48_000 && channels == 2);

    // Threshold 0 turns streaming off.
    engine.set_stream_threshold_secs(0);
    let in_memory = engine.load_sound(&long).unwrap();
    assert!(!engine.is_sound_streamed(in_memory));
    assert_eq!(engine.stats().loaded_sounds, 3);
    engine.unload_sound(in_memory).unwrap();

    // Play the streamed one through the headless output and wait for its end.
    let events = engine.stream_events();
    engine.start_headless_output().unwrap();
    let track = engine.play(long_id).unwrap();
    assert!(track.is_playing().unwrap());
    assert_eq!(track.duration_ms().unwrap(), Some(2_000));
    track.seek_ms(1_000).unwrap();
    let event = events
        .recv_timeout(Duration::from_secs(15))
        .expect("TrackFinished");
    assert_eq!(
        event,
        StreamEvent::TrackFinished {
            track_id: track.track_id
        }
    );
    engine.stop_output().unwrap();
    engine.unload_sound(long_id).unwrap();
    assert!(engine.sound_meta(long_id).is_none());
}

#[test]
fn stream_threshold_is_an_optional_config_field() {
    use soundshared::config::Config;

    // A v1.4.0 [audio] section has no such key: the default applies.
    let old: Config = toml::from_str("[audio]\nbuffer_size = 960\nmaster_volume = 0.65\n").unwrap();
    assert_eq!(old.audio.stream_threshold_secs, 60);
    assert_eq!(old.audio.master_volume, 0.65);

    let custom: Config = toml::from_str("[audio]\nstream_threshold_secs = 15\n").unwrap();
    assert_eq!(custom.audio.stream_threshold_secs, 15);
    let off: Config = toml::from_str("[audio]\nstream_threshold_secs = 0\n").unwrap();
    assert_eq!(off.audio.stream_threshold_secs, 0);
}
