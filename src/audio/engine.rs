//! Audio engine with CPAL output stream
//! High-level API for sound management and playback

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, FromSample, SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig,
    SupportedStreamConfigRange,
};
use crossbeam_channel::Receiver;
use parking_lot::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use self::device_adapter::{DeviceAdapter, StreamResampler};
use crate::audio::decoder::{
    AudioDecoder, DecodedAudio, DecoderError, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::audio::mixer::{AudioMixer, MixedOutput, MixerConfig};

/// Sample rate of the mix pipeline: what the mixer produces, what the network
/// forwarder publishes and what every web listener receives. The local device
/// capability never changes it — the device gets a converted copy instead.
const MIX_RATE: u32 = TARGET_SAMPLE_RATE;

/// Interleaved channel count of the mix pipeline (stereo).
const MIX_CHANNELS: usize = TARGET_CHANNELS as usize;

/// Unique identifier for a loaded sound
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SoundId(pub Uuid);

impl SoundId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SoundId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SoundId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Handle for controlling playback of a sound
#[derive(Debug, Clone)]
pub struct TrackHandle {
    pub sound_id: SoundId,
    pub track_id: Uuid,
    pub(crate) engine: Arc<AudioEngineInner>,
}

/// Errors that can occur in the audio engine
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("Decoder error: {0}")]
    Decoder(#[from] DecoderError),
    #[error("Mixer error: {0}")]
    Mixer(#[from] crate::audio::mixer::MixerError),
    #[error("CPAL error: {0}")]
    Cpal(#[from] cpal::BuildStreamError),
    #[error("CPAL play error: {0}")]
    CpalPlay(#[from] cpal::PlayStreamError),
    #[error("No audio device available")]
    NoDevice,
    #[error("Unsupported sample format: {0}")]
    UnsupportedFormat(String),
    #[error("Sound not found: {0}")]
    SoundNotFound(SoundId),
    #[error("Track not found: {0}")]
    TrackNotFound(Uuid),
    #[error("Channel error: {0}")]
    Channel(#[from] crossbeam_channel::SendError<MixedOutput>),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Where the *local* (desktop) audio output currently stands.
///
/// The network broadcast is independent of this: web listeners keep receiving
/// the same 48 kHz stereo f32 mix while this reports [`Headless`], and the
/// engine is never silently muted — a state is always reported.
///
/// [`Headless`]: LocalOutputState::Headless
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOutputState {
    /// PCM is going to a real output device.
    Running,
    /// No local device is open: the mixer publishes to the network only.
    Headless,
    /// The output stream stopped; the message says why.
    Failed(String),
    /// Output was never started, or has been stopped.
    Stopped,
}

impl LocalOutputState {
    /// True only while a device stream is open and playing.
    pub fn is_running(&self) -> bool {
        matches!(self, LocalOutputState::Running)
    }
}

impl std::fmt::Display for LocalOutputState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalOutputState::Running => write!(f, "running"),
            LocalOutputState::Headless => write!(f, "headless (network only)"),
            LocalOutputState::Failed(message) => write!(f, "failed: {message}"),
            LocalOutputState::Stopped => write!(f, "stopped"),
        }
    }
}

/// Snapshot of the local output state, for the UI status strip.
///
/// `sample_rate`, `channels` and `sample_format` describe the open *device*
/// stream and are `0` / `"none"` whenever no device stream is open (headless,
/// failed, stopped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutputStatus {
    /// Current state of the local output.
    pub state: LocalOutputState,
    /// Name of the output device, when one was opened (or last attempted).
    pub device: Option<String>,
    /// Device sample rate in Hz.
    pub sample_rate: u32,
    /// Device channel count.
    pub channels: u16,
    /// Device sample format, e.g. `f32` or `i16`.
    pub sample_format: String,
    /// Why the engine is in this state: a fallback was used, the stream failed…
    pub reason: Option<String>,
}

impl LocalOutputStatus {
    /// Status for "no local output started".
    pub fn stopped() -> Self {
        Self {
            state: LocalOutputState::Stopped,
            device: None,
            sample_rate: 0,
            channels: 0,
            sample_format: "none".to_string(),
            reason: None,
        }
    }

    /// True only while the desktop is actually being fed audio.
    pub fn is_local_audio_running(&self) -> bool {
        self.state.is_running()
    }

    /// One-line text for a status strip.
    pub fn summary(&self) -> String {
        let mut text = match &self.state {
            LocalOutputState::Running => format!(
                "local audio: {} · {} Hz · {} ch · {}",
                self.device.as_deref().unwrap_or("unknown device"),
                self.sample_rate,
                self.channels,
                self.sample_format
            ),
            LocalOutputState::Headless => "local audio: off (headless, network only)".to_string(),
            LocalOutputState::Failed(message) => format!("local audio: failed ({message})"),
            LocalOutputState::Stopped => "local audio: stopped".to_string(),
        };
        if let Some(reason) = &self.reason {
            text.push_str(" — ");
            text.push_str(reason);
        }
        text
    }
}

impl Default for LocalOutputStatus {
    fn default() -> Self {
        Self::stopped()
    }
}

/// Pure conversions that adapt the fixed 48 kHz stereo f32 mix to whatever the
/// local output device accepts.
///
/// They hold no CPAL state, so they are unit tested directly. The device
/// callback mixes into the 48 kHz stereo *staging* buffer, publishes that exact
/// buffer to the network forwarder, and only then converts it into the device
/// buffer: what a web listener receives never depends on the local device.
pub mod device_adapter {
    use cpal::{FromSample, Sample};

    /// Rate and channel count of one device buffer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DeviceAdapter {
        /// Device sample rate in Hz.
        pub sample_rate: u32,
        /// Device channel count (interleaved).
        pub channels: u16,
    }

    impl DeviceAdapter {
        /// Describe a device buffer layout.
        pub const fn new(sample_rate: u32, channels: u16) -> Self {
            Self {
                sample_rate,
                channels,
            }
        }

        /// True when the mix can be written into the device buffer unchanged.
        pub fn is_identity(&self) -> bool {
            self.sample_rate == super::MIX_RATE && self.channels as usize == super::MIX_CHANNELS
        }

        /// Frames of 48 kHz stereo needed to fill `out_frames` device frames.
        ///
        /// `carry` keeps the fractional remainder between callbacks, so the
        /// average ratio is exact instead of drifting by each callback's
        /// rounding error. A device rate of `0` is nonsense and is treated as
        /// "one device frame per mix frame" rather than dividing by zero.
        pub fn frames(&self, out_frames: usize, carry: &mut f64) -> usize {
            if self.sample_rate == super::MIX_RATE || self.sample_rate == 0 || out_frames == 0 {
                return out_frames;
            }

            let step = super::MIX_RATE as f64 / self.sample_rate as f64;
            *carry += out_frames as f64 * step;
            let whole = carry.floor();
            *carry -= whole;
            whole as usize
        }
    }

    /// 48 kHz stereo frames needed for `out_frames` device frames, rounded up.
    ///
    /// The pure variant of [`DeviceAdapter::frames`], without the carry: used to
    /// size a staging buffer for a single callback.
    pub fn staging_frames_ceil(out_frames: usize, device_rate: u32) -> usize {
        if device_rate == 0 || device_rate == super::MIX_RATE || out_frames == 0 {
            return out_frames;
        }

        let scaled = out_frames as u64 * super::MIX_RATE as u64;
        scaled.div_ceil(device_rate as u64) as usize
    }

    /// Convert `stereo` (48 kHz interleaved stereo f32) into `out`, the device
    /// buffer holding `out.len() / adapter.channels` frames at the device rate.
    ///
    /// Resampling is linear interpolation and is the identity transform when the
    /// rates match. Channel mapping writes L, R, then the mono downmix to any
    /// further channel; mono devices receive `(L + R) / 2`.
    pub fn convert_stereo_into<T>(stereo: &[f32], out: &mut [T], adapter: DeviceAdapter)
    where
        T: Sample + FromSample<f32>,
    {
        let channels = adapter.channels as usize;
        if channels == 0 || out.is_empty() {
            return;
        }

        let out_frames = out.len() / channels;
        let src_frames = stereo.len() / super::MIX_CHANNELS;
        if src_frames == 0 {
            for slot in out.iter_mut() {
                *slot = T::EQUILIBRIUM;
            }
            return;
        }

        let step = super::MIX_RATE as f64 / adapter.sample_rate.max(1) as f64;
        for frame in 0..out_frames {
            let (left, right) = if step == 1.0 && frame < src_frames {
                (stereo[frame * 2], stereo[frame * 2 + 1])
            } else {
                interpolate_stereo(stereo, src_frames, frame as f64 * step)
            };
            write_stereo_frame(
                &mut out[frame * channels..(frame + 1) * channels],
                left,
                right,
            );
        }

        // `out` is always a whole number of frames from CPAL, but never leave
        // unwritten samples behind if it is not.
        for slot in out[out_frames * channels..].iter_mut() {
            *slot = T::EQUILIBRIUM;
        }
    }

    /// Continuous 48 kHz stereo -> device-rate converter for the output callback.
    ///
    /// The old path mixed a fresh staging block for every callback and
    /// resampled it in isolation: each block restarted its read phase at 0 and
    /// clamped its last frames to the block's final sample, so the waveform
    /// tore at every callback boundary — about 100 times a second, which is
    /// heard as a buzzy, "farting" rasp on any device that is not 48 kHz.
    ///
    /// This keeps the read phase and the not-yet-consumed mix frames between
    /// callbacks, so interpolation always has the real next sample and the
    /// output is one continuous signal. Mix frames are pulled from `produce`
    /// only when needed; it mixes (and publishes) exactly the frames asked for.
    #[derive(Debug, Default)]
    pub struct StreamResampler {
        /// Interleaved stereo 48 kHz frames not fully consumed yet.
        pending: Vec<f32>,
        /// Read position in `pending`, in frames; always in `[0, 1)` between calls.
        pos: f64,
    }

    impl StreamResampler {
        pub fn new() -> Self {
            Self::default()
        }

        /// Fill `out` (interleaved, `adapter.channels` per frame, at the device
        /// rate). Returns what `produce` returned for the frames it mixed, or
        /// `false` when no new frames were needed.
        pub fn render<T, F>(&mut self, out: &mut [T], adapter: DeviceAdapter, produce: F) -> bool
        where
            T: Sample + FromSample<f32>,
            F: FnOnce(&mut [f32]) -> bool,
        {
            let channels = adapter.channels as usize;
            if channels == 0 || out.is_empty() {
                return false;
            }
            let out_frames = out.len() / channels;
            let step = if adapter.sample_rate == 0 {
                1.0
            } else {
                super::MIX_RATE as f64 / adapter.sample_rate as f64
            };

            // Frames needed so the last output frame can interpolate towards
            // its right-hand neighbour.
            let last_pos = self.pos + out_frames.saturating_sub(1) as f64 * step;
            let needed = last_pos.floor() as usize + 2;
            let have = self.pending.len() / super::MIX_CHANNELS;
            let mut playing = false;
            if needed > have {
                let start = self.pending.len();
                self.pending.resize(needed * super::MIX_CHANNELS, 0.0);
                playing = produce(&mut self.pending[start..]);
            }

            for frame in 0..out_frames {
                let p = self.pos + frame as f64 * step;
                let index = p.floor() as usize;
                let t = (p - index as f64) as f32;
                let a = index * super::MIX_CHANNELS;
                let b = a + super::MIX_CHANNELS;
                let left = self.pending[a] + (self.pending[b] - self.pending[a]) * t;
                let right = self.pending[a + 1] + (self.pending[b + 1] - self.pending[a + 1]) * t;
                write_stereo_frame(
                    &mut out[frame * channels..(frame + 1) * channels],
                    left,
                    right,
                );
            }
            for slot in out[out_frames * channels..].iter_mut() {
                *slot = T::EQUILIBRIUM;
            }

            // Drop the frames the read head has fully passed.
            let end = self.pos + out_frames as f64 * step;
            let consumed = (end.floor() as usize).min(self.pending.len() / super::MIX_CHANNELS);
            self.pending.drain(..consumed * super::MIX_CHANNELS);
            self.pos = end - consumed as f64;
            playing
        }
    }

    /// Linear interpolation between the two 48 kHz frames around `pos`.
    fn interpolate_stereo(stereo: &[f32], src_frames: usize, pos: f64) -> (f32, f32) {
        if pos <= 0.0 {
            return (stereo[0], stereo[1]);
        }

        let index = pos.floor() as usize;
        if index + 1 >= src_frames {
            let last = (src_frames - 1) * super::MIX_CHANNELS;
            return (stereo[last], stereo[last + 1]);
        }

        let fraction = (pos - index as f64) as f32;
        let a = index * super::MIX_CHANNELS;
        let b = a + super::MIX_CHANNELS;
        (
            stereo[a] + (stereo[b] - stereo[a]) * fraction,
            stereo[a + 1] + (stereo[b + 1] - stereo[a + 1]) * fraction,
        )
    }

    /// Write one stereo frame into a device frame of any channel count.
    fn write_stereo_frame<T>(frame: &mut [T], left: f32, right: f32)
    where
        T: Sample + FromSample<f32>,
    {
        match frame.len() {
            0 => {}
            1 => frame[0] = T::from_sample((left + right) * 0.5),
            2 => {
                frame[0] = T::from_sample(left);
                frame[1] = T::from_sample(right);
            }
            channels => {
                frame[0] = T::from_sample(left);
                frame[1] = T::from_sample(right);
                let middle = (left + right) * 0.5;
                for slot in frame[2..channels].iter_mut() {
                    *slot = T::from_sample(middle);
                }
            }
        }
    }
}

/// Internal engine state shared across handles.
///
/// Crate-visible because TrackHandle keeps an Arc of it and the handle is part
/// of the public API; the fields themselves stay private.
pub(crate) struct AudioEngineInner {
    mixer: Arc<AudioMixer>,
    /// Loaded sounds: SoundId -> Arc<DecodedAudio>
    sounds: RwLock<HashMap<SoundId, Arc<DecodedAudio>>>,
    /// Per-sound volume, applied to tracks of that sound as they are created
    /// and to the ones already playing
    sound_volumes: RwLock<HashMap<SoundId, f32>>,
    /// Active tracks: track_id -> SoundId
    active_tracks: RwLock<HashMap<Uuid, SoundId>>,
    /// CPAL output stream
    stream: Mutex<Option<cpal::Stream>>,
    /// Engine running state
    running: Arc<AtomicBool>,
    /// State of the *local* output. Shared with the device callback, so a
    /// stream error can be reported without keeping the engine alive.
    local_output: Arc<parking_lot::RwLock<LocalOutputStatus>>,
    /// Master volume (synced with mixer)
    master_volume: Arc<parking_lot::Mutex<f32>>,
    /// Next track ID
    next_track_id: AtomicU64,
    /// How `[audio]` wants the local device opened (read at `start_output`).
    device_prefs: parking_lot::Mutex<DevicePrefs>,
}

// Manual Debug implementation since cpal::Stream doesn't implement Debug
impl std::fmt::Debug for AudioEngineInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEngineInner")
            .field("mixer", &self.mixer)
            .field("sounds", &self.sounds)
            .field("sound_volumes", &self.sound_volumes)
            .field("active_tracks", &self.active_tracks)
            .field("stream", &"<cpal::Stream>")
            .field("running", &self.running)
            .field("local_output", &self.local_output)
            .field("master_volume", &self.master_volume)
            .field("next_track_id", &self.next_track_id)
            .finish()
    }
}

impl AudioEngineInner {
    /// Stop a specific track by its ID
    fn stop_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.mixer.stop_track(track_id)?;
        self.active_tracks.write().remove(&track_id);
        Ok(())
    }

    /// Pause a specific track
    fn pause_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.mixer.pause_track(track_id)?;
        Ok(())
    }

    /// Resume a paused track
    fn resume_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.mixer.resume_track(track_id)?;
        Ok(())
    }

    /// Set volume for a specific track
    fn set_track_volume(&self, track_id: Uuid, volume: f32) -> Result<(), EngineError> {
        self.mixer.set_track_volume(track_id, volume)?;
        Ok(())
    }

    /// Get volume for a specific track
    fn track_volume(&self, track_id: Uuid) -> Result<f32, EngineError> {
        Ok(self.mixer.track_volume(track_id)?)
    }

    /// Check if a track is playing
    fn is_track_playing(&self, track_id: Uuid) -> Result<bool, EngineError> {
        Ok(self.mixer.is_track_playing(track_id)?)
    }
}

/// High-level audio engine
#[derive(Clone)]
pub struct AudioEngine {
    inner: Arc<AudioEngineInner>,
}

impl AudioEngine {
    /// Create a new audio engine with default settings
    pub fn new() -> Result<Self, EngineError> {
        let config = MixerConfig::default();
        let mixer = Arc::new(AudioMixer::with_config(config));

        // cpal::Stream is !Send: the engine stays on the thread that built it.
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(AudioEngineInner {
            mixer,
            sounds: RwLock::new(HashMap::new()),
            sound_volumes: RwLock::new(HashMap::new()),
            active_tracks: RwLock::new(HashMap::new()),
            stream: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            local_output: Arc::new(parking_lot::RwLock::new(LocalOutputStatus::stopped())),
            master_volume: Arc::new(parking_lot::Mutex::new(1.0)),
            next_track_id: AtomicU64::new(0),
            device_prefs: parking_lot::Mutex::new(DevicePrefs::default()),
        });

        Ok(Self { inner })
    }

    /// Choose how the local device is opened by the next `start_output`.
    pub fn set_device_prefs(&self, prefs: DevicePrefs) {
        *self.inner.device_prefs.lock() = prefs;
    }

    /// Create a new audio engine with custom mixer config and broadcast channel
    pub fn with_config(config: MixerConfig) -> Result<Self, EngineError> {
        let mixer = Arc::new(AudioMixer::with_config(config));

        // cpal::Stream is !Send: the engine stays on the thread that built it.
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(AudioEngineInner {
            mixer,
            sounds: RwLock::new(HashMap::new()),
            sound_volumes: RwLock::new(HashMap::new()),
            active_tracks: RwLock::new(HashMap::new()),
            stream: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            local_output: Arc::new(parking_lot::RwLock::new(LocalOutputStatus::stopped())),
            master_volume: Arc::new(parking_lot::Mutex::new(1.0)),
            next_track_id: AtomicU64::new(0),
            device_prefs: parking_lot::Mutex::new(DevicePrefs::default()),
        });

        Ok(Self { inner })
    }

    /// Initialize and start the CPAL output stream.
    ///
    /// Succeeds whenever the host has an output device at all. The fixed
    /// 48 kHz stereo f32 mix is adapted to whatever the device accepts, in this
    /// order:
    ///
    /// 1. 48 kHz, stereo, f32 — the mix format, written straight into the
    ///    device buffer;
    /// 2. stereo f32 at any rate — resampled inside the callback;
    /// 3. the device's own `default_output_config()` (any rate, channels and
    ///    sample format) — resampled, converted and channel-mapped inside the
    ///    callback.
    ///
    /// This deliberately returns `Err` only when there is no device or the
    /// stream cannot be built/played even with the device default config: a
    /// device that merely lacks a 48 kHz mode used to fail here, which silently
    /// degraded the app to headless mode — the browser kept playing while the
    /// desktop stayed silent.
    pub fn start_output(&self) -> Result<(), EngineError> {
        if self.inner.running.load(Ordering::Acquire) {
            warn!("Audio engine already running");
            return Ok(());
        }

        let host = cpal::default_host();
        let device = match host.default_output_device() {
            Some(device) => device,
            None => {
                self.record_local_output_failure(None, 0, 0, "none", "no default output device");
                return Err(EngineError::NoDevice);
            }
        };

        let device_name = device
            .name()
            .unwrap_or_else(|_| "<unknown device>".to_string());
        info!("Using audio device: {device_name}");

        // An enumeration failure is not fatal: the device default config below
        // is still usable, and the policy below handles an empty list.
        let supported = match device.supported_output_configs() {
            Ok(configs) => configs.collect::<Vec<_>>(),
            Err(e) => {
                warn!("supported_output_configs() failed: {e}");
                Vec::new()
            }
        };

        let prefs = *self.inner.device_prefs.lock();
        let (chosen, reason) = match Self::choose_output_config(&device, &supported, &prefs) {
            Ok(chosen) => chosen,
            Err(e) => {
                self.record_local_output_failure(Some(device_name), 0, 0, "none", e.to_string());
                return Err(e);
            }
        };

        let sample_format = chosen.sample_format();
        let adapter = DeviceAdapter::new(chosen.sample_rate().0, chosen.channels());

        // `[audio] buffer_size` (10 ms by default). Not every backend accepts
        // a fixed size, so `build_adapted_stream` retries with the device
        // default instead of failing outright.
        let stream_config = StreamConfig {
            buffer_size: prefs.buffer_size(),
            ..chosen.config()
        };

        info!(
            "Audio config: {}Hz, {}ch, {}, buffer: {:?}{}",
            stream_config.sample_rate.0,
            stream_config.channels,
            sample_format,
            stream_config.buffer_size,
            reason
                .as_deref()
                .map(|reason| format!(" [{reason}]"))
                .unwrap_or_default()
        );

        // The device callback is the master clock: it mixes into the 48 kHz
        // stereo staging buffer, publishes that exact buffer to the network
        // forwarder and then converts it for the device. No intermediate 10 ms
        // channel and no `thread::sleep` timing, both of which caused underruns.
        let mixer = Arc::clone(&self.inner.mixer);
        let running = Arc::clone(&self.inner.running);
        let status = Arc::clone(&self.inner.local_output);

        let stream = match sample_format {
            SampleFormat::F32 => build_adapted_stream::<f32>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::I16 => build_adapted_stream::<i16>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::U16 => build_adapted_stream::<u16>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::I32 => build_adapted_stream::<i32>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::I8 => {
                build_adapted_stream::<i8>(&device, &stream_config, adapter, mixer, running, status)
            }
            SampleFormat::U8 => {
                build_adapted_stream::<u8>(&device, &stream_config, adapter, mixer, running, status)
            }
            SampleFormat::I64 => build_adapted_stream::<i64>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::U64 => build_adapted_stream::<u64>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::U32 => build_adapted_stream::<u32>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            SampleFormat::F64 => build_adapted_stream::<f64>(
                &device,
                &stream_config,
                adapter,
                mixer,
                running,
                status,
            ),
            other => Err(EngineError::UnsupportedFormat(format!(
                "device sample format {other} is not supported by this build"
            ))),
        };

        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                self.record_local_output_failure(
                    Some(device_name),
                    adapter.sample_rate,
                    adapter.channels,
                    &sample_format.to_string(),
                    e.to_string(),
                );
                return Err(e);
            }
        };

        // Mark the engine running before starting the stream so the first
        // callback already mixes instead of emitting one silent buffer.
        self.inner.running.store(true, Ordering::Release);
        if let Err(e) = stream.play() {
            self.inner.running.store(false, Ordering::Release);
            self.record_local_output_failure(
                Some(device_name),
                adapter.sample_rate,
                adapter.channels,
                &sample_format.to_string(),
                format!("failed to start the output stream: {e}"),
            );
            return Err(e.into());
        }

        *self.inner.stream.lock().unwrap() = Some(stream);

        *self.inner.local_output.write() = LocalOutputStatus {
            state: LocalOutputState::Running,
            device: Some(device_name),
            sample_rate: adapter.sample_rate,
            channels: adapter.channels,
            sample_format: sample_format.to_string(),
            reason,
        };

        info!("Audio engine started successfully (device-driven mixer)");
        Ok(())
    }

    /// Ask the device what it offers, then apply the pure selection policy.
    fn choose_output_config(
        device: &cpal::Device,
        supported: &[SupportedStreamConfigRange],
        prefs: &DevicePrefs,
    ) -> Result<(SupportedStreamConfig, Option<String>), EngineError> {
        let device_default = match device.default_output_config() {
            Ok(config) => Some(config),
            Err(e) => {
                debug!("default_output_config() failed: {e}");
                None
            }
        };

        plan_output_config(supported, device_default.as_ref(), prefs)
    }

    /// Record why local audio is unavailable, so the UI can show it instead of
    /// the app going quietly mute.
    fn record_local_output_failure(
        &self,
        device: Option<String>,
        sample_rate: u32,
        channels: u16,
        sample_format: &str,
        reason: impl Into<String>,
    ) {
        let reason = reason.into();
        warn!("Local audio output unavailable: {reason}");
        *self.inner.local_output.write() = LocalOutputStatus {
            state: LocalOutputState::Failed(reason.clone()),
            device,
            sample_rate,
            channels,
            sample_format: sample_format.to_string(),
            reason: Some(reason),
        };
    }

    /// Start the audio engine without a local output device.
    ///
    /// Used when CPAL cannot open an output stream (headless server, no sound
    /// card, exclusive-mode conflict). The mixer then runs its own deadline-
    /// paced loop and still publishes frames to the network broadcaster.
    ///
    /// The local output status becomes [`LocalOutputState::Headless`], carrying
    /// the reason of the previous failed attempt when there was one.
    pub fn start_headless_output(&self) -> Result<(), EngineError> {
        let carried_reason = {
            let previous = self.inner.local_output.read();
            previous.reason.clone().or_else(|| match &previous.state {
                LocalOutputState::Failed(message) => Some(message.clone()),
                _ => None,
            })
        };

        self.start_headless_core(carried_reason)
    }

    /// [`start_headless_output`](Self::start_headless_output) with an explicit
    /// reason, for a caller that knows why the device could not be opened.
    pub fn start_headless_output_with_reason<R: Into<String>>(
        &self,
        reason: R,
    ) -> Result<(), EngineError> {
        self.start_headless_core(Some(reason.into()))
    }

    /// Start the mixer's own loop without a device, and record the state.
    fn start_headless_core(&self, reason: Option<String>) -> Result<(), EngineError> {
        if self.inner.running.load(Ordering::Acquire) {
            warn!("Audio engine already running");
            return Ok(());
        }

        self.inner.running.store(true, Ordering::Release);
        self.inner.mixer.start();

        match &reason {
            Some(reason) => {
                info!("Audio engine started in headless mode (network streaming only): {reason}")
            }
            None => info!("Audio engine started in headless mode (network streaming only)"),
        }

        *self.inner.local_output.write() = LocalOutputStatus {
            state: LocalOutputState::Headless,
            device: None,
            sample_rate: 0,
            channels: 0,
            sample_format: "none".to_string(),
            reason,
        };

        Ok(())
    }

    /// Stop the audio engine and output stream
    pub fn stop_output(&self) -> Result<(), EngineError> {
        if !self.inner.running.load(Ordering::Acquire) {
            return Ok(());
        }

        // Stop the clock first so the callback emits silence, then join the
        // headless loop if it was the one driving the mixer.
        self.inner.running.store(false, Ordering::Release);
        self.inner.mixer.stop();

        if let Some(stream) = self.inner.stream.lock().unwrap().take() {
            // Drop the stream to stop it (pause() returns PauseStreamError not in EngineError)
            drop(stream);
        }

        *self.inner.local_output.write() = LocalOutputStatus::stopped();

        info!("Audio engine stopped");
        Ok(())
    }

    /// Current state of the *local* (desktop) audio output.
    ///
    /// The web UI is unaffected by this: listeners always receive the 48 kHz
    /// stereo f32 mix, whether or not a local device is open.
    pub fn local_output_status(&self) -> LocalOutputStatus {
        self.inner.local_output.read().clone()
    }

    /// Check if the engine is running
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }

    /// Load a sound from a file path
    pub fn load_sound<P: AsRef<Path>>(&self, path: P) -> Result<SoundId, EngineError> {
        let decoded = AudioDecoder::decode_file(path.as_ref())?;
        let sound_id = SoundId::new();
        let audio = Arc::new(decoded);

        info!(
            "Loaded sound {}: {} frames, {:.2}s, {}Hz, {}ch",
            sound_id.0, audio.frames, audio.duration, audio.sample_rate, audio.channels
        );

        self.inner.sounds.write().insert(sound_id, audio);
        Ok(sound_id)
    }

    /// Load a sound from raw decoded audio data
    pub fn load_sound_from_data(&self, audio: DecodedAudio) -> SoundId {
        let sound_id = SoundId::new();
        self.inner.sounds.write().insert(sound_id, Arc::new(audio));
        sound_id
    }

    /// Play a loaded sound
    /// Returns a TrackHandle for controlling playback
    pub fn play(&self, sound_id: SoundId) -> Result<TrackHandle, EngineError> {
        self.play_with_loop(sound_id, false)
    }

    /// Play a loaded sound, optionally repeating it forever.
    ///
    /// This is the single playback entry point: `play` and `play_loop` are thin
    /// wrappers, so the "repeat" flag chosen in the UI travels with the track.
    pub fn play_with_loop(
        &self,
        sound_id: SoundId,
        looped: bool,
    ) -> Result<TrackHandle, EngineError> {
        let audio = {
            let sounds = self.inner.sounds.read();
            sounds
                .get(&sound_id)
                .cloned()
                .ok_or(EngineError::SoundNotFound(sound_id))?
        };

        // Forget tracks that finished or were stopped earlier, so neither the
        // mixer nor the registry grows with every press of play.
        if self.inner.mixer.prune_finished() > 0 {
            let mixer = &self.inner.mixer;
            self.inner
                .active_tracks
                .write()
                .retain(|track_id, _| mixer.has_track(*track_id));
        }

        let track_id = self.inner.mixer.add_track(audio, looped);

        // A sound's volume outlives its tracks: a track created later must pick
        // up the volume the user already chose for that sound.
        let sound_volume = self
            .inner
            .sound_volumes
            .read()
            .get(&sound_id)
            .copied()
            .unwrap_or(1.0);
        if (sound_volume - 1.0).abs() > f32::EPSILON {
            self.inner.mixer.set_track_volume(track_id, sound_volume)?;
        }

        // Start playback — the central mix loop will read audio directly from
        // the track's decoded samples, no feeder thread needed.
        self.inner.mixer.play_track(track_id)?;

        // Register active track
        self.inner.active_tracks.write().insert(track_id, sound_id);

        let handle = TrackHandle {
            sound_id,
            track_id,
            engine: self.inner.clone(),
        };

        debug!(
            "Started {}playback of sound {} on track {}",
            if looped { "looped " } else { "" },
            sound_id.0,
            track_id
        );
        Ok(handle)
    }

    /// Play a sound with looping enabled
    pub fn play_loop(&self, sound_id: SoundId) -> Result<TrackHandle, EngineError> {
        self.play_with_loop(sound_id, true)
    }

    /// Turn repeating on or off for every active track of a sound.
    ///
    /// The flag lives on the mixer track, so a sound that is already playing
    /// starts repeating the moment this is called, and a sound that is not
    /// playing picks the flag up on its next `play_with_loop`.
    pub fn set_sound_looped(&self, sound_id: SoundId, looped: bool) -> Result<(), EngineError> {
        for track_id in self.active_track_ids_of(sound_id) {
            self.inner.mixer.set_track_looped(track_id, looped)?;
        }
        Ok(())
    }

    /// Set the volume of a sound (0.0 - 1.0).
    ///
    /// Applies immediately to every track of that sound that is currently
    /// registered, and is remembered for tracks created later by
    /// [`play_with_loop`](Self::play_with_loop) — the same lifecycle as
    /// [`set_sound_looped`](Self::set_sound_looped).
    ///
    /// This is the per-sound control the UI needs: routing a sound's slider to
    /// [`set_master_volume`](Self::set_master_volume) would scale *every* sound
    /// and could silence the whole mix.
    pub fn set_sound_volume(&self, sound_id: SoundId, volume: f32) -> Result<(), EngineError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(EngineError::Mixer(
                crate::audio::mixer::MixerError::InvalidVolume(volume),
            ));
        }

        if !self.inner.sounds.read().contains_key(&sound_id) {
            return Err(EngineError::SoundNotFound(sound_id));
        }

        self.inner.sound_volumes.write().insert(sound_id, volume);

        for track_id in self.active_track_ids_of(sound_id) {
            self.inner.mixer.set_track_volume(track_id, volume)?;
        }

        debug!("Set volume of sound {} to {}", sound_id.0, volume);
        Ok(())
    }

    /// Volume most recently set for a sound, or 1.0 when none was ever set.
    pub fn sound_volume(&self, sound_id: SoundId) -> Result<f32, EngineError> {
        if !self.inner.sounds.read().contains_key(&sound_id) {
            return Err(EngineError::SoundNotFound(sound_id));
        }

        Ok(self
            .inner
            .sound_volumes
            .read()
            .get(&sound_id)
            .copied()
            .unwrap_or(1.0))
    }

    /// IDs of the registered tracks that belong to one sound.
    fn active_track_ids_of(&self, sound_id: SoundId) -> Vec<Uuid> {
        let tracks = self.inner.active_tracks.read();
        tracks
            .iter()
            .filter_map(|(track_id, candidate)| (*candidate == sound_id).then_some(*track_id))
            .collect()
    }

    /// Whether any active track of this sound is currently set to repeat.
    pub fn is_sound_looped(&self, sound_id: SoundId) -> Result<bool, EngineError> {
        let tracks = self.inner.active_tracks.read();
        for (track_id, sid) in tracks.iter() {
            if *sid == sound_id && self.inner.mixer.is_track_looped(*track_id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Stop a specific track by its handle
    pub fn stop_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.inner.mixer.stop_track(track_id)?;
        self.inner.active_tracks.write().remove(&track_id);
        Ok(())
    }

    /// Stop all tracks playing a specific sound
    pub fn stop_sound(&self, sound_id: SoundId) -> Result<(), EngineError> {
        let track_ids: Vec<Uuid> = {
            let tracks = self.inner.active_tracks.read();
            tracks
                .iter()
                .filter_map(|(tid, sid)| if *sid == sound_id { Some(*tid) } else { None })
                .collect()
        };

        for track_id in track_ids {
            self.stop_track(track_id)?;
        }

        Ok(())
    }

    /// Stop all playback
    pub fn stop_all(&self) -> Result<(), EngineError> {
        let track_ids: Vec<Uuid> = {
            let tracks = self.inner.active_tracks.read();
            tracks.keys().copied().collect()
        };

        for track_id in track_ids {
            self.stop_track(track_id)?;
        }

        Ok(())
    }

    /// Set master volume (0.0 - 1.0)
    pub fn set_master_volume(&self, volume: f32) -> Result<(), EngineError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(EngineError::Mixer(
                crate::audio::mixer::MixerError::InvalidVolume(volume),
            ));
        }
        *self.inner.master_volume.lock() = volume;
        self.inner.mixer.set_master_volume(volume)?;
        Ok(())
    }

    /// Get current master volume
    pub fn master_volume(&self) -> f32 {
        *self.inner.master_volume.lock()
    }

    /// Peak level of the mix since the previous call (0.0..=1.0).
    pub fn take_output_peak(&self) -> f32 {
        self.inner.mixer.take_peak()
    }

    /// Set volume for a specific track
    pub fn set_track_volume(&self, track_id: Uuid, volume: f32) -> Result<(), EngineError> {
        self.inner.mixer.set_track_volume(track_id, volume)?;
        Ok(())
    }

    /// Get volume for a specific track
    pub fn track_volume(&self, track_id: Uuid) -> Result<f32, EngineError> {
        Ok(self.inner.mixer.track_volume(track_id)?)
    }

    /// Pause a specific track
    pub fn pause_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.inner.mixer.pause_track(track_id)?;
        Ok(())
    }

    /// Resume a paused track from where it was paused
    pub fn resume_track(&self, track_id: Uuid) -> Result<(), EngineError> {
        self.inner.mixer.resume_track(track_id)?;
        Ok(())
    }

    /// Check if a track is playing
    pub fn is_track_playing(&self, track_id: Uuid) -> Result<bool, EngineError> {
        Ok(self.inner.mixer.is_track_playing(track_id)?)
    }

    /// Check if any track for a given sound is currently playing
    pub fn is_sound_playing(&self, sound_id: SoundId) -> Result<bool, EngineError> {
        let track_ids: Vec<Uuid> = {
            let tracks = self.inner.active_tracks.read();
            tracks
                .iter()
                .filter_map(|(tid, sid)| if *sid == sound_id { Some(*tid) } else { None })
                .collect()
        };
        for track_id in track_ids {
            if self.inner.mixer.is_track_playing(track_id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Get a loaded sound's metadata
    pub fn sound_info(&self, sound_id: SoundId) -> Result<(f32, u32, u32), EngineError> {
        let sounds = self.inner.sounds.read();
        let audio = sounds
            .get(&sound_id)
            .ok_or(EngineError::SoundNotFound(sound_id))?;
        Ok((audio.duration, audio.sample_rate, audio.channels))
    }

    /// List all loaded sound IDs
    pub fn list_sounds(&self) -> Vec<SoundId> {
        self.inner.sounds.read().keys().copied().collect()
    }

    /// Remove a sound from memory (stops any playing tracks first)
    pub fn unload_sound(&self, sound_id: SoundId) -> Result<(), EngineError> {
        self.stop_sound(sound_id)?;
        self.inner.sounds.write().remove(&sound_id);
        self.inner.sound_volumes.write().remove(&sound_id);
        Ok(())
    }

    /// Get the mixer's broadcast output receiver for network streaming (WebSocket clients)
    pub fn broadcast_output_receiver(&self) -> Receiver<MixedOutput> {
        self.inner.mixer.broadcast_output_receiver()
    }

    /// Get mixer statistics
    pub fn stats(&self) -> EngineStats {
        EngineStats {
            loaded_sounds: self.inner.sounds.read().len(),
            active_tracks: self.inner.active_tracks.read().len(),
            mixer_tracks: self.inner.mixer.track_count(),
            running: self.inner.running.load(Ordering::Acquire),
            master_volume: self.master_volume(),
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS,
            buffer_frames: self.inner.mixer.buffer_frames(),
        }
    }
}

/// Rate to try first for a stereo f32 device that cannot run at 48 kHz.
///
/// The device's own default rate is preferred when the range allows it; the
/// closest in-range rate to 48 kHz is the fallback.
/// What the `[audio]` section asks of the local output device.
///
/// The mix and the network stream are always 48 kHz stereo (that is the wire
/// format every listener decodes); these settings only choose how the desktop
/// sound card is opened. A device that cannot do what is asked falls back to
/// the next best mode, exactly as before, and the reason is logged and shown
/// in the status strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePrefs {
    /// Device sample rate to ask for, in Hz.
    pub sample_rate: u32,
    /// Device channel count to ask for (1 = mono downmix, 2 = stereo, more =
    /// stereo plus a centre mix on the extra channels).
    pub channels: u16,
    /// Device buffer in frames; 0 lets the driver choose.
    pub buffer_frames: u32,
}

impl Default for DevicePrefs {
    fn default() -> Self {
        Self {
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS as u16,
            buffer_frames: 480,
        }
    }
}

impl DevicePrefs {
    /// Build from `[audio]`, replacing nonsense with the defaults (and
    /// saying so in the log) instead of failing to open the device.
    pub fn from_audio_config(audio: &crate::config::AudioConfig) -> Self {
        let defaults = Self::default();
        let sample_rate = if (8_000..=192_000).contains(&audio.sample_rate) {
            audio.sample_rate
        } else {
            warn!(
                "[audio] sample_rate = {} is out of range (8000-192000); using {}",
                audio.sample_rate, defaults.sample_rate
            );
            defaults.sample_rate
        };
        let channels = if (1..=8).contains(&audio.channels) {
            audio.channels
        } else {
            warn!(
                "[audio] channels = {} is out of range (1-8); using {}",
                audio.channels, defaults.channels
            );
            defaults.channels
        };
        let buffer_frames = match audio.buffer_size {
            0 => 0,
            n @ 64..=8192 => n as u32,
            n => {
                warn!(
                    "[audio] buffer_size = {n} is out of range (0 or 64-8192); using {}",
                    defaults.buffer_frames
                );
                defaults.buffer_frames
            }
        };
        Self {
            sample_rate,
            channels,
            buffer_frames,
        }
    }

    fn buffer_size(&self) -> BufferSize {
        match self.buffer_frames {
            0 => BufferSize::Default,
            n => BufferSize::Fixed(n),
        }
    }
}

fn preferred_rate(
    range: &SupportedStreamConfigRange,
    device_default: Option<&SupportedStreamConfig>,
    wanted: u32,
) -> u32 {
    let min = range.min_sample_rate().0;
    let max = range.max_sample_rate().0;

    if (min..=max).contains(&wanted) {
        return wanted;
    }
    match device_default {
        Some(default) if default.sample_rate().0 >= min && default.sample_rate().0 <= max => {
            default.sample_rate().0
        }
        _ => TARGET_SAMPLE_RATE.clamp(min, max),
    }
}

/// Pure policy behind [`AudioEngine::choose_output_config`]: given what the
/// device offers, pick the configuration to open.
///
/// Priority: 48 kHz stereo f32 → stereo f32 at any rate → the device's own
/// `default_output_config()`. The chosen configuration is returned together with
/// the reason whenever the mix has to be resampled or converted on its way to
/// the device, so the UI can say why the device format differs from the mix.
///
/// `Err` is returned only when the device offers nothing at all that can be
/// opened — not merely because it lacks a 48 kHz mode.
fn plan_output_config(
    supported: &[SupportedStreamConfigRange],
    device_default: Option<&SupportedStreamConfig>,
    prefs: &DevicePrefs,
) -> Result<(SupportedStreamConfig, Option<String>), EngineError> {
    let configured =
        (prefs.sample_rate, prefs.channels) != (TARGET_SAMPLE_RATE, TARGET_CHANNELS as u16);

    // 0. Exactly what `[audio]` asks for (by default 48 kHz stereo f32, the
    //    mix format, which is written straight through).
    if let Some(range) = supported.iter().find(|candidate| {
        candidate.channels() == prefs.channels
            && candidate.sample_format() == SampleFormat::F32
            && candidate.min_sample_rate().0 <= prefs.sample_rate
            && candidate.max_sample_rate().0 >= prefs.sample_rate
    }) {
        return Ok((
            (*range).with_sample_rate(SampleRate(prefs.sample_rate)),
            None,
        ));
    }

    // 1. The mix format, when the configured one is not available.
    if let Some(range) = supported.iter().find(|candidate| {
        candidate.channels() == TARGET_CHANNELS as u16
            && candidate.sample_format() == SampleFormat::F32
            && candidate.min_sample_rate().0 <= TARGET_SAMPLE_RATE
            && candidate.max_sample_rate().0 >= TARGET_SAMPLE_RATE
    }) {
        return Ok((
            (*range).with_sample_rate(SampleRate(TARGET_SAMPLE_RATE)),
            configured.then(|| {
                format!(
                    "device has no {} Hz {} ch f32 mode ([audio]); using 48 kHz stereo",
                    prefs.sample_rate, prefs.channels
                )
            }),
        ));
    }

    // 2. Stereo f32 at a different rate: resample inside the callback.
    if let Some(range) = supported.iter().find(|candidate| {
        candidate.channels() == TARGET_CHANNELS as u16
            && candidate.sample_format() == SampleFormat::F32
    }) {
        let rate = preferred_rate(range, device_default, prefs.sample_rate);
        return Ok((
            (*range).with_sample_rate(SampleRate(rate)),
            Some(format!(
                "device has no 48 kHz stereo f32 mode; using {rate} Hz stereo f32 and resampling"
            )),
        ));
    }

    // 3. Whatever the device itself reports as its default format. CPAL
    //    guarantees this one is openable, which is what makes the engine work on
    //    devices that only expose, say, i16 at 44.1 kHz.
    if let Some(default) = device_default {
        return Ok((
            default.clone(),
            Some(format!(
                "device has no stereo f32 mode; using its default {} Hz, {} ch, {} with resampling and conversion",
                default.sample_rate().0,
                default.channels(),
                default.sample_format()
            )),
        ));
    }

    Err(EngineError::UnsupportedFormat(
        "device reports no usable output configuration".to_string(),
    ))
}

/// One device-buffer sample format, seen from the mix pipeline.
///
/// Every implementation mixes the 48 kHz stereo f32 staging buffer, publishes
/// that exact buffer for network listeners and only then writes it into the
/// device buffer.
trait MixWriter: cpal::SizedSample + FromSample<f32> {
    /// Mix one device buffer's worth of audio into `data`.
    ///
    /// `resampler` carries the rate conversion state between callbacks (and
    /// reuses its buffer, so the audio thread does not allocate per buffer).
    /// Returns `true` when at least one track was playing.
    fn mix_and_publish(
        mixer: &AudioMixer,
        resampler: &mut StreamResampler,
        data: &mut [Self],
        adapter: DeviceAdapter,
    ) -> bool;
}

impl MixWriter for f32 {
    fn mix_and_publish(
        mixer: &AudioMixer,
        resampler: &mut StreamResampler,
        data: &mut [Self],
        adapter: DeviceAdapter,
    ) -> bool {
        // Common case: the device takes exactly what the mixer produces, so the
        // device buffer *is* the staging buffer and nothing is copied.
        if adapter.is_identity() {
            let playing = mixer.mix_into(data);
            if playing {
                mixer.publish_broadcast(data);
            }
            return playing;
        }

        converted_mix(mixer, resampler, data, adapter)
    }
}

macro_rules! impl_mix_writer_converted {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl MixWriter for $ty {
                fn mix_and_publish(
                    mixer: &AudioMixer,
                    resampler: &mut StreamResampler,
                    data: &mut [Self],
                    adapter: DeviceAdapter,
                ) -> bool {
                    converted_mix(mixer, resampler, data, adapter)
                }
            }
        )+
    };
}

impl_mix_writer_converted!(i8, u8, i16, u16, i32, u32, i64, u64, f64);

/// Mix what the device needs, publish it for the network, then convert it.
///
/// The published bytes are always the untouched 48 kHz stereo f32 mix: the
/// conversion happens *after* `publish_broadcast`, so what a web listener
/// receives never depends on the local device.
fn converted_mix<T>(
    mixer: &AudioMixer,
    resampler: &mut StreamResampler,
    data: &mut [T],
    adapter: DeviceAdapter,
) -> bool
where
    T: cpal::SizedSample + FromSample<f32>,
{
    resampler.render(data, adapter, |fresh| {
        let playing = mixer.mix_into(fresh);
        if playing {
            mixer.publish_broadcast(fresh);
        }
        playing
    })
}

/// Build the output stream for one device sample format.
///
/// Tries the requested buffer size first and retries with the device default,
/// because not every backend accepts a fixed size.
#[allow(clippy::too_many_arguments)]
fn build_adapted_stream<T>(
    device: &cpal::Device,
    stream_config: &StreamConfig,
    adapter: DeviceAdapter,
    mixer: Arc<AudioMixer>,
    running: Arc<AtomicBool>,
    status: Arc<parking_lot::RwLock<LocalOutputStatus>>,
) -> Result<cpal::Stream, EngineError>
where
    T: MixWriter,
{
    let build = |config: &StreamConfig| -> Result<cpal::Stream, cpal::BuildStreamError> {
        let mixer = Arc::clone(&mixer);
        let running = Arc::clone(&running);
        let status = Arc::clone(&status);

        // Sized on the first callback and reused afterwards, so the audio thread
        // does not allocate per buffer.
        let mut resampler = StreamResampler::new();

        device.build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if !running.load(Ordering::Acquire) {
                    for slot in data.iter_mut() {
                        *slot = T::EQUILIBRIUM;
                    }
                    return;
                }

                T::mix_and_publish(&mixer, &mut resampler, data, adapter);
            },
            move |err| {
                // A stream error ends local audio for good: record it, so the UI
                // can show it instead of the app going quietly mute.
                let message = err.to_string();
                error!("Audio stream error: {message}");
                let mut guard = status.write();
                guard.reason = Some(message.clone());
                guard.state = LocalOutputState::Failed(message);
            },
            None,
        )
    };

    match build(stream_config) {
        Ok(stream) => Ok(stream),
        Err(first_err) => {
            warn!(
                "Failed to build 10 ms output stream ({first_err}); retrying with the device default buffer size"
            );
            let fallback_config = StreamConfig {
                buffer_size: BufferSize::Default,
                ..stream_config.clone()
            };
            build(&fallback_config).map_err(EngineError::from)
        }
    }
}

/// Engine statistics
#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineStats {
    pub loaded_sounds: usize,
    pub active_tracks: usize,
    pub mixer_tracks: usize,
    pub running: bool,
    pub master_volume: f32,
    pub sample_rate: u32,
    pub channels: u32,
    pub buffer_frames: usize,
}

impl TrackHandle {
    /// Stop this track
    pub fn stop(&self) -> Result<(), EngineError> {
        self.engine.stop_track(self.track_id)
    }

    /// Pause this track
    pub fn pause(&self) -> Result<(), EngineError> {
        self.engine.pause_track(self.track_id)
    }

    /// Resume this track
    pub fn resume(&self) -> Result<(), EngineError> {
        self.engine.resume_track(self.track_id)
    }

    /// Set volume for this track (0.0 - 1.0)
    pub fn set_volume(&self, volume: f32) -> Result<(), EngineError> {
        self.engine.set_track_volume(self.track_id, volume)
    }

    /// Get current volume for this track
    pub fn volume(&self) -> Result<f32, EngineError> {
        self.engine.track_volume(self.track_id)
    }

    /// Check if this track is playing
    pub fn is_playing(&self) -> Result<bool, EngineError> {
        self.engine.is_track_playing(self.track_id)
    }

    /// Get the sound ID for this track
    pub fn sound_id(&self) -> SoundId {
        self.sound_id
    }

    /// Get the track ID
    pub fn track_id(&self) -> Uuid {
        self.track_id
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        if self.inner.running.load(Ordering::Acquire) {
            let _ = self.stop_output();
        }
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new().expect("Failed to create default audio engine")
    }
}

#[cfg(test)]
mod tests {
    use super::device_adapter::{convert_stereo_into, staging_frames_ceil};
    use super::*;
    use cpal::Sample;

    #[test]
    fn test_engine_creation() {
        let engine = AudioEngine::new().unwrap();
        assert!(!engine.is_running());
        assert_eq!(engine.master_volume(), 1.0);
        assert_eq!(engine.list_sounds().len(), 0);
    }

    #[test]
    fn test_master_volume() {
        let engine = AudioEngine::new().unwrap();
        engine.set_master_volume(0.5).unwrap();
        assert_eq!(engine.master_volume(), 0.5);

        assert!(engine.set_master_volume(1.5).is_err());
    }

    #[test]
    fn test_sound_id_generation() {
        let id1 = SoundId::new();
        let id2 = SoundId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_load_sound_from_data() {
        let engine = AudioEngine::new().unwrap();
        let audio = DecodedAudio::new(vec![0.0; 48000 * 2], 48000, 2);
        let sound_id = engine.load_sound_from_data(audio);
        assert_eq!(engine.list_sounds().len(), 1);
        assert!(engine.list_sounds().contains(&sound_id));
    }

    // --- output-config policy ------------------------------------------------

    fn range(
        channels: u16,
        min_rate: u32,
        max_rate: u32,
        format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            SampleRate(min_rate),
            SampleRate(max_rate),
            cpal::SupportedBufferSize::Unknown,
            format,
        )
    }

    fn default_config(channels: u16, rate: u32, format: SampleFormat) -> SupportedStreamConfig {
        SupportedStreamConfig::new(
            channels,
            SampleRate(rate),
            cpal::SupportedBufferSize::Unknown,
            format,
        )
    }

    /// Regression for the reported bug: on a device whose only mode is 44.1 kHz
    /// (the usual Windows/WASAPI mix format), `start_output` used to find no
    /// 48 kHz stereo f32 config, return `UnsupportedFormat`, and drop the app to
    /// headless mode — the web UI kept playing while the desktop stayed silent.
    #[test]
    fn plan_output_config_honours_the_audio_section() {
        let supported = vec![
            range(2, 44_100, 96_000, SampleFormat::F32),
            range(1, 44_100, 96_000, SampleFormat::F32),
        ];
        let device_default = default_config(2, 48_000, SampleFormat::F32);
        let prefs = DevicePrefs {
            sample_rate: 44_100,
            channels: 1,
            buffer_frames: 256,
        };
        let (chosen, reason) =
            plan_output_config(&supported, Some(&device_default), &prefs).unwrap();
        assert_eq!(chosen.channels(), 1);
        assert_eq!(chosen.sample_rate().0, 44_100);
        assert!(reason.is_none());
        assert_eq!(prefs.buffer_size(), BufferSize::Fixed(256));
    }

    #[test]
    fn plan_output_config_says_why_the_audio_section_was_not_met() {
        let supported = vec![range(2, 48_000, 48_000, SampleFormat::F32)];
        let device_default = default_config(2, 48_000, SampleFormat::F32);
        let prefs = DevicePrefs {
            sample_rate: 96_000,
            channels: 2,
            buffer_frames: 0,
        };
        let (chosen, reason) =
            plan_output_config(&supported, Some(&device_default), &prefs).unwrap();
        assert_eq!(chosen.sample_rate().0, 48_000);
        assert!(reason.unwrap().contains("96000 Hz"));
        assert_eq!(prefs.buffer_size(), BufferSize::Default);
    }

    #[test]
    fn device_prefs_replace_nonsense_with_defaults() {
        let audio = crate::config::AudioConfig {
            buffer_size: 3,
            sample_rate: 1,
            channels: 0,
            master_volume: 0.8,
        };
        assert_eq!(
            DevicePrefs::from_audio_config(&audio),
            DevicePrefs::default()
        );
    }

    #[test]
    fn plan_output_config_accepts_a_44100_only_device() {
        let supported = vec![
            range(2, 44_100, 44_100, SampleFormat::U8),
            range(2, 44_100, 44_100, SampleFormat::I16),
            range(2, 44_100, 44_100, SampleFormat::I32),
            range(2, 44_100, 44_100, SampleFormat::F32),
        ];
        let device_default = default_config(2, 44_100, SampleFormat::F32);

        let (chosen, reason) =
            plan_output_config(&supported, Some(&device_default), &DevicePrefs::default())
                .expect("a 44.1 kHz device must still be opened, not rejected");

        assert_eq!(chosen.sample_rate().0, 44_100);
        assert_eq!(chosen.channels(), 2);
        assert_eq!(chosen.sample_format(), SampleFormat::F32);
        assert!(
            reason.is_some(),
            "using a non-mix rate must be reported as a reason"
        );
        assert!(!DeviceAdapter::new(44_100, 2).is_identity());
    }

    #[test]
    fn plan_output_config_prefers_the_mix_format() {
        let supported = vec![
            range(2, 44_100, 44_100, SampleFormat::F32),
            range(2, 48_000, 48_000, SampleFormat::F32),
        ];
        let device_default = default_config(2, 44_100, SampleFormat::F32);

        let (chosen, reason) =
            plan_output_config(&supported, Some(&device_default), &DevicePrefs::default()).unwrap();

        assert_eq!(chosen.sample_rate().0, TARGET_SAMPLE_RATE);
        assert!(reason.is_none(), "the mix format needs no conversion");
        assert!(DeviceAdapter::new(48_000, 2).is_identity());
    }

    #[test]
    fn plan_output_config_accepts_a_rate_range_containing_48k() {
        let supported = vec![range(2, 44_100, 96_000, SampleFormat::F32)];

        let (chosen, reason) =
            plan_output_config(&supported, None, &DevicePrefs::default()).unwrap();

        assert_eq!(chosen.sample_rate().0, TARGET_SAMPLE_RATE);
        assert!(reason.is_none());
    }

    #[test]
    fn plan_output_config_falls_back_to_the_device_default_format() {
        // No stereo f32 anywhere: a 6-channel i16 device at 48 kHz.
        let supported = vec![range(6, 48_000, 48_000, SampleFormat::I16)];
        let device_default = default_config(6, 48_000, SampleFormat::I16);

        let (chosen, reason) =
            plan_output_config(&supported, Some(&device_default), &DevicePrefs::default()).unwrap();

        assert_eq!(chosen.channels(), 6);
        assert_eq!(chosen.sample_format(), SampleFormat::I16);
        assert!(reason.is_some());
    }

    #[test]
    fn plan_output_config_uses_the_default_when_nothing_is_listed() {
        let device_default = default_config(2, 44_100, SampleFormat::F32);

        let (chosen, reason) =
            plan_output_config(&[], Some(&device_default), &DevicePrefs::default()).unwrap();

        assert_eq!(chosen.sample_rate().0, 44_100);
        assert!(reason.is_some());
    }

    #[test]
    fn plan_output_config_fails_only_without_any_config() {
        assert!(plan_output_config(&[], None, &DevicePrefs::default()).is_err());
    }

    // --- mix → device conversions -------------------------------------------

    #[test]
    fn conversion_is_identity_at_48k_stereo_f32() {
        let stereo: Vec<f32> = (0..960).map(|i| i as f32 / 960.0).collect();
        let mut out = vec![f32::NAN; stereo.len()];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(48_000, 2));

        assert_eq!(out, stereo);
    }

    #[test]
    fn staging_frames_cover_a_non_matching_rate() {
        // 480 frames of a 44.1 kHz device are 10.88 ms: slightly more than 522
        // frames at 48 kHz, so the staging buffer has to be rounded up.
        assert_eq!(staging_frames_ceil(480, 44_100), 523);
        assert_eq!(staging_frames_ceil(480, 48_000), 480);
        assert_eq!(staging_frames_ceil(480, 96_000), 240);
        assert_eq!(staging_frames_ceil(441, 44_100), 480);
    }

    /// The fractional carry must not drift: 1000 callbacks of 480 frames at
    /// 44.1 kHz have to consume exactly the 48 kHz frames their duration needs.
    #[test]
    fn staging_frames_do_not_drift_over_time() {
        let adapter = DeviceAdapter::new(44_100, 2);
        let mut carry = 0.0;
        let mut total = 0usize;

        for _ in 0..1000 {
            total += adapter.frames(480, &mut carry);
        }

        let expected = 1000.0 * 480.0 * 48_000.0 / 44_100.0;
        assert!(
            (total as f64 - expected).abs() <= 1.0,
            "drifted: consumed {total} 48 kHz frames, expected {expected}"
        );
    }

    /// A degenerate rate must not turn into a division by zero and a huge
    /// staging allocation in the audio callback.
    #[test]
    fn degenerate_device_rates_are_handled() {
        let mut carry = 0.0;
        assert_eq!(DeviceAdapter::new(0, 2).frames(480, &mut carry), 480);
        assert_eq!(carry, 0.0);
        assert_eq!(staging_frames_ceil(480, 0), 480);
        assert_eq!(DeviceAdapter::new(48_000, 0).frames(480, &mut carry), 480);
    }

    #[test]
    fn resampling_fills_the_whole_device_buffer() {
        let mut stereo = vec![0.0f32; 480 * 2];
        for frame in 0..480 {
            stereo[frame * 2] = 1.0;
            stereo[frame * 2 + 1] = -0.5;
        }
        let mut out = vec![f32::NAN; 441 * 2];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(44_100, 2));

        // 441 frames at 44.1 kHz is exactly 480 frames at 48 kHz, so every
        // device sample is written from the constant source.
        assert!(out.iter().all(|sample| *sample == 1.0 || *sample == -0.5));
        assert_eq!(out[0], 1.0);
        assert_eq!(out[out.len() - 1], -0.5);
    }

    #[test]
    fn resampling_maps_device_frames_back_to_source_time() {
        // Left channel is a ramp in steps of 2, so the last device frame shows
        // the interpolated source position.
        let stereo: Vec<f32> = (0..480 * 2).map(|i| i as f32).collect();
        let mut out = vec![f32::NAN; 441 * 2];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(44_100, 2));

        assert!(out.iter().all(|sample| sample.is_finite()));
        // Device frame 440 sits at source frame 440 * 48000/44100 = 478.91,
        // whose left sample is 957.8.
        assert!((out[880] - 957.8).abs() < 1.0, "got {}", out[880]);
    }

    #[test]
    fn i16_conversion_uses_the_full_scale_and_clamps() {
        assert_eq!(i16::from_sample(0.0f32), 0);
        assert_eq!(i16::from_sample(0.5f32), 16_384);
        assert_eq!(i16::from_sample(1.0f32), i16::MAX);
        assert_eq!(i16::from_sample(1.5f32), i16::MAX);
        assert_eq!(i16::from_sample(-1.0f32), i16::MIN);
        assert_eq!(i16::from_sample(-1.5f32), i16::MIN);
    }

    #[test]
    fn u16_conversion_uses_the_midpoint_convention_and_clamps() {
        assert_eq!(u16::from_sample(0.0f32), 32_768);
        assert_eq!(u16::from_sample(0.5f32), 49_152);
        assert_eq!(u16::from_sample(-1.0f32), 0);
        assert_eq!(u16::from_sample(1.0f32), u16::MAX);
        assert_eq!(u16::from_sample(1.5f32), u16::MAX);
        assert_eq!(u16::from_sample(-1.5f32), 0);
    }

    #[test]
    fn mono_devices_get_the_average_of_both_channels() {
        let stereo = vec![1.0f32, -0.5, 0.5, 0.5];
        let mut out = vec![0.0f32; 2];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(48_000, 1));

        assert!((out[0] - 0.25).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn extra_channels_get_the_mono_downmix() {
        let stereo = vec![1.0f32, -0.5];
        let mut out = vec![0.0f32; 4];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(48_000, 4));

        assert_eq!(out, vec![1.0, -0.5, 0.25, 0.25]);
    }

    #[test]
    fn channel_mapping_and_sample_format_apply_together() {
        let stereo = vec![1.0f32, -1.0];
        let mut out = vec![0u16; 2];

        convert_stereo_into(&stereo, &mut out, DeviceAdapter::new(48_000, 2));

        assert_eq!(out, vec![u16::MAX, 0]);
    }

    // --- the network format never follows the device -------------------------

    /// The web listener must always get the 48 kHz stereo f32 mix, whatever the
    /// local device is: the device conversion happens *after* the broadcast
    /// buffer was captured.
    #[test]
    fn device_format_never_changes_the_broadcast_format() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.25; 48_000 * 2], 48_000, 2));
        let track = mixer.add_track(audio, true);
        mixer.play_track(track).unwrap();

        // A 44.1 kHz u16 device: as far from the mix format as it gets.
        let adapter = DeviceAdapter::new(44_100, 2);
        let mut resampler = StreamResampler::new();
        let mut device_buffer = vec![0u16; 441 * 2];

        let playing = <u16 as MixWriter>::mix_and_publish(
            &mixer,
            &mut resampler,
            &mut device_buffer,
            adapter,
        );

        assert!(playing);

        let published = mixer
            .broadcast_output_receiver()
            .try_recv()
            .expect("the mix must be published for the network forwarder");
        // 441 frames at 44.1 kHz is exactly 480 frames at 48 kHz — stereo f32.
        assert_eq!(published.buffer.len(), 480 * 2);
        assert!(published.buffer.iter().all(|s| (*s - 0.25).abs() < 1e-6));

        // ...while the device buffer received the converted copy (0.25 * 32768
        // in the i16 domain, offset by the u16 midpoint).
        assert_eq!(device_buffer[0], u16::from_sample(0.25f32));
        assert_eq!(device_buffer[1], u16::from_sample(0.25f32));
    }

    /// A 44.1 kHz device fed through many callbacks must get one continuous
    /// waveform: the old per-block resampler restarted its phase and clamped
    /// the tail of every block, tearing the signal ~100 times a second.
    #[test]
    fn resampled_output_is_continuous_across_callbacks() {
        let mixer = AudioMixer::new();
        // 1 kHz sine at 48 kHz, half scale.
        let frames = 48_000;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin();
            samples.push(v);
            samples.push(v);
        }
        let track = mixer.add_track(Arc::new(DecodedAudio::new(samples, 48_000, 2)), true);
        mixer.play_track(track).unwrap();

        let adapter = DeviceAdapter::new(44_100, 2);
        let mut resampler = StreamResampler::new();
        let mut output = Vec::new();
        // Odd, varying callback sizes, like a real WASAPI device.
        for size in [441usize, 448, 437, 512, 441, 300, 441, 441, 1024, 441].repeat(4) {
            let mut buffer = vec![0f32; size * 2];
            <f32 as MixWriter>::mix_and_publish(&mixer, &mut resampler, &mut buffer, adapter);
            output.extend(buffer.as_chunks::<2>().0.iter().map(|f| f[0]));
        }

        // A 1 kHz, 0.5-amplitude sine at 44.1 kHz moves at most
        // 2*pi*1000/44100*0.5 ~= 0.0712 per sample; a seam would jump far more.
        let max_step = output
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_step < 0.075,
            "discontinuity of {max_step} in the resampled stream"
        );
    }

    #[test]
    fn identity_adapter_publishes_the_mix_unchanged() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.25; 48_000 * 2], 48_000, 2));
        let track = mixer.add_track(audio, true);
        mixer.play_track(track).unwrap();

        let adapter = DeviceAdapter::new(48_000, 2);
        let mut resampler = StreamResampler::new();
        let mut device_buffer = vec![f32::NAN; 480 * 2];

        let playing = <f32 as MixWriter>::mix_and_publish(
            &mixer,
            &mut resampler,
            &mut device_buffer,
            adapter,
        );

        assert!(playing);
        assert!(device_buffer.iter().all(|s| (*s - 0.25).abs() < 1e-6));

        let published = mixer.broadcast_output_receiver().try_recv().unwrap();
        assert_eq!(published.buffer.len(), 480 * 2);
        assert!(published.buffer.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    // --- per-sound volume ----------------------------------------------------

    #[test]
    fn sound_volume_applies_to_running_and_future_tracks() {
        let engine = AudioEngine::new().unwrap();
        let audio = DecodedAudio::new(vec![0.5; 4_800 * 2], 48_000, 2);
        let sound_id = engine.load_sound_from_data(audio);

        assert_eq!(engine.sound_volume(sound_id).unwrap(), 1.0);

        let first = engine.play(sound_id).unwrap();
        engine.set_sound_volume(sound_id, 0.25).unwrap();

        assert_eq!(engine.sound_volume(sound_id).unwrap(), 0.25);
        assert_eq!(engine.track_volume(first.track_id()).unwrap(), 0.25);

        // A track created after the slider moved picks the volume up.
        let second = engine.play(sound_id).unwrap();
        assert_eq!(engine.track_volume(second.track_id()).unwrap(), 0.25);

        // ...and so does a replayed one.
        engine.set_sound_volume(sound_id, 0.75).unwrap();
        let third = engine.play(sound_id).unwrap();
        assert_eq!(engine.track_volume(third.track_id()).unwrap(), 0.75);
    }

    #[test]
    fn sound_volume_rejects_out_of_range_and_unknown_sounds() {
        let engine = AudioEngine::new().unwrap();
        let audio = DecodedAudio::new(vec![0.0; 480 * 2], 48_000, 2);
        let sound_id = engine.load_sound_from_data(audio);

        assert!(engine.set_sound_volume(sound_id, 1.5).is_err());
        assert!(engine.set_sound_volume(sound_id, -0.1).is_err());
        assert!(engine.sound_volume(sound_id).is_ok());

        let unknown = SoundId::new();
        assert!(engine.set_sound_volume(unknown, 0.5).is_err());
        assert!(engine.sound_volume(unknown).is_err());

        // Unloading forgets the volume with the sound.
        engine.set_sound_volume(sound_id, 0.5).unwrap();
        engine.unload_sound(sound_id).unwrap();
        assert!(engine.sound_volume(sound_id).is_err());
    }

    // --- local output status -------------------------------------------------

    #[test]
    fn local_output_status_starts_stopped() {
        let engine = AudioEngine::new().unwrap();

        let status = engine.local_output_status();

        assert_eq!(status.state, LocalOutputState::Stopped);
        assert_eq!(status.state, LocalOutputStatus::default().state);
        assert!(!status.is_local_audio_running());
        assert_eq!(status.sample_rate, 0);
        assert!(status.summary().contains("stopped"));
    }

    #[test]
    fn headless_output_reports_headless_and_stop_clears_it() {
        let engine = AudioEngine::new().unwrap();

        engine
            .start_headless_output_with_reason("no output device")
            .unwrap();

        let status = engine.local_output_status();
        assert_eq!(status.state, LocalOutputState::Headless);
        assert!(!status.is_local_audio_running());
        assert_eq!(status.reason.as_deref(), Some("no output device"));
        assert!(status.summary().contains("headless"));

        engine.stop_output().unwrap();
        assert_eq!(
            engine.local_output_status().state,
            LocalOutputState::Stopped
        );
    }

    #[test]
    fn headless_output_carries_the_reason_of_a_failed_device() {
        let engine = AudioEngine::new().unwrap();

        // What a failed `start_output` leaves behind, then main.rs's fallback.
        engine.record_local_output_failure(
            Some("Broken Speakers".to_string()),
            44_100,
            2,
            "i16",
            "device has no usable output configuration",
        );
        engine.start_headless_output().unwrap();

        let status = engine.local_output_status();
        assert_eq!(status.state, LocalOutputState::Headless);
        assert_eq!(
            status.reason.as_deref(),
            Some("device has no usable output configuration")
        );
    }

    #[test]
    fn failed_status_keeps_the_device_that_was_tried() {
        let engine = AudioEngine::new().unwrap();

        engine.record_local_output_failure(
            Some("Broken Speakers".to_string()),
            44_100,
            2,
            "i16",
            "stream error",
        );

        let status = engine.local_output_status();
        assert_eq!(
            status.state,
            LocalOutputState::Failed("stream error".to_string())
        );
        assert_eq!(status.device.as_deref(), Some("Broken Speakers"));
        assert_eq!(status.sample_rate, 44_100);
        assert_eq!(status.channels, 2);
        assert_eq!(status.sample_format, "i16");
        assert!(status.summary().contains("failed"));
    }
}
