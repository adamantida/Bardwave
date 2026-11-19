//! Audio mixer for combining multiple PCM streams
//! Thread-safe, non-blocking, lock-free where possible
//!
//! The mixer exposes a pull-based [`AudioMixer::mix_into`] API. The audio
//! device callback (CPAL) is the master clock: every time the device asks for
//! N frames, the mixer advances every active track by exactly N frames and
//! fills the callback buffer. This removes the old `thread::sleep(10ms)` loop
//! whose Windows timer granularity (~15.6 ms) caused constant underruns.
//!
//! When no output device is available, [`AudioMixer::start`] runs an
//! equivalent deadline-paced loop so network streaming still works.
//!
//! # Signal path
//!
//! ```text
//! source -> track gain (ramp / fade) -> pan -> bus gain (volume x duck)
//!        -> sum -> master volume -> limiter -> [publish_broadcast]
//!        -> + local-only tracks -> local limiter -> device
//! ```
//!
//! Tracks flagged `local_only` (previews on the master's machine) are rendered
//! in a separate pass, [`AudioMixer::mix_local_into`], that the caller runs
//! *after* [`AudioMixer::publish_broadcast`]: players never hear them.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::RwLock;
use uuid::Uuid;

use crate::audio::decoder::{DecodedAudio, TrackMeta, TARGET_SAMPLE_RATE};
use crate::audio::stream_source::{StreamEvent, StreamSource};

/// Number of interleaved channels produced by the mixer.
const CHANNELS: usize = 2;

/// Events of streamed tracks that may wait for a listener.
const STREAM_EVENT_CAPACITY: usize = 64;

/// Errors that can occur in the mixer
#[derive(Debug, thiserror::Error)]
pub enum MixerError {
    #[error("Track not found: {0}")]
    TrackNotFound(Uuid),
    #[error("Channel send error: {0}")]
    ChannelSend(#[from] crossbeam_channel::SendError<AudioBuffer>),
    #[error("Channel receive error: {0}")]
    ChannelRecv(#[from] crossbeam_channel::RecvError),
    #[error("Track already exists: {0}")]
    TrackExists(Uuid),
    #[error("Invalid volume value: {0} (must be 0.0-1.0)")]
    InvalidVolume(f32),
    #[error("Invalid pan value: {0} (must be -1.0..=1.0)")]
    InvalidPan(f32),
}

/// Audio buffer for mixing (interleaved stereo f32 at 48kHz)
pub type AudioBuffer = Vec<f32>;

/// Output from the mixer
#[derive(Debug, Clone)]
pub struct MixedOutput {
    /// Mixed audio samples (interleaved stereo f32)
    pub buffer: AudioBuffer,
    /// Monotonically increasing sequence number for this mix buffer
    pub seq_no: u64,
    /// Timestamp of when this buffer was mixed
    pub timestamp: u64,
}

/// Configuration for the mixer
#[derive(Debug, Clone)]
pub struct MixerConfig {
    /// Buffer size in frames (stereo pairs). 480 = 10 ms at 48 kHz.
    pub buffer_frames: usize,
    /// Number of buffers to keep in the broadcast output channel
    pub channel_capacity: usize,
    /// Master volume (0.0 - 1.0)
    pub master_volume: f32,
    /// Interval between mix iterations (ms) for the headless fallback loop.
    pub mix_interval_ms: u64,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            // ~10 ms at 48 kHz = 480 frames
            buffer_frames: 480,
            // ~640 ms of buffering for the network forwarder.
            channel_capacity: 64,
            master_volume: 1.0,
            mix_interval_ms: 10,
        }
    }
}

/// Length of the gain ramp used for volume changes, stop, pause and resume:
/// 10 ms at 48 kHz. A gain that jumps between two samples is an audible click
/// (and a dragged fader is a burst of them — "zipper" noise); spreading every
/// change over a few hundred samples makes it inaudible.
///
/// It is also the shortest fade the explicit fade API will produce.
const RAMP_FRAMES: f32 = 480.0;

/// Shortest explicit fade, in frames: the click-protection ramp.
const MIN_FADE_FRAMES: usize = RAMP_FRAMES as usize;

/// Frames processed per pass of the per-track loop. The scratch buffer that
/// holds the raw source samples lives on the stack, so this bounds its size
/// (4 KiB) and keeps the mix loop allocation-free for any callback length.
const CHUNK_FRAMES: usize = 512;

/// Ceiling of the master bus. The peak limiter keeps the summed mix below it,
/// so several loud tracks played together are turned down smoothly instead of
/// being hard-clipped into a buzzing square wave by the device or the codec.
const LIMIT_CEILING: f32 = 0.97;

/// Release time of the limiter: how fast the gain recovers after a peak.
const LIMIT_RELEASE_SECS: f32 = 0.2;

/// Convert milliseconds to frames at the mix rate.
fn ms_to_frames(ms: u32) -> usize {
    (u64::from(ms) * u64::from(TARGET_SAMPLE_RATE) / 1000) as usize
}

/// Shape of an explicit fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FadeCurve {
    /// Straight line. A linear crossfade of two uncorrelated sounds dips by
    /// 3 dB in the middle; use it for single fades or when that is wanted.
    Linear,
    /// Quarter sine / cosine. An incoming (0 -> 1) and an outgoing (1 -> 0)
    /// fade of the same length sum to constant power: a crossfade without a dip.
    #[default]
    EqualPower,
}

/// An explicit gain fade running on a track.
#[derive(Debug, Clone, Copy)]
struct Fade {
    start: f32,
    target: f32,
    len: usize,
    pos: usize,
    curve: FadeCurve,
}

impl Fade {
    /// A fade from `start` to `target` over `frames` (at least the click
    /// ramp), or `None` when there is nothing to fade.
    fn between(start: f32, target: f32, frames: usize, curve: FadeCurve) -> Option<Self> {
        if start == target {
            return None;
        }
        Some(Self {
            start,
            target,
            len: frames.max(MIN_FADE_FRAMES),
            pos: 0,
            curve,
        })
    }

    /// Advance one frame and return the gain for it. Reaches `target` exactly
    /// on the last frame.
    fn next_gain(&mut self) -> f32 {
        self.pos = (self.pos + 1).min(self.len);
        if self.pos >= self.len {
            return self.target;
        }
        let t = self.pos as f32 / self.len as f32;
        let progress = match self.curve {
            FadeCurve::Linear => t,
            FadeCurve::EqualPower if self.target >= self.start => (t * FRAC_PI_2).sin(),
            FadeCurve::EqualPower => 1.0 - (t * FRAC_PI_2).cos(),
        };
        self.start + (self.target - self.start) * progress
    }

    fn is_done(&self) -> bool {
        self.pos >= self.len
    }

    fn remaining(&self) -> usize {
        self.len - self.pos
    }
}

/// Mixing bus a track belongs to. Every bus has its own volume and ducking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bus {
    Music,
    /// Where tracks go unless told otherwise.
    #[default]
    Ambience,
    Sfx,
}

impl Bus {
    /// Every bus, in index order.
    pub const ALL: [Bus; BUS_COUNT] = [Bus::Music, Bus::Ambience, Bus::Sfx];

    fn index(self) -> usize {
        match self {
            Bus::Music => 0,
            Bus::Ambience => 1,
            Bus::Sfx => 2,
        }
    }
}

/// Number of mixing buses.
pub const BUS_COUNT: usize = 3;

/// How a bus is ducked (turned down under something else, e.g. a voice-over).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DuckSettings {
    /// Gain multiplier of the bus while ducked, 0.0 (silence) ..= 1.0 (none).
    pub target: f32,
    /// Time to go from no ducking down to `target`, in ms.
    pub attack_ms: u32,
    /// Time to come back from `target` to no ducking, in ms.
    pub release_ms: u32,
    /// `None`: hold until [`AudioMixer::release_duck`] (key held down).
    /// `Some(ms)`: release automatically that long after `target` was reached.
    pub hold_ms: Option<u32>,
}

impl Default for DuckSettings {
    fn default() -> Self {
        Self {
            target: 0.3,
            attack_ms: 150,
            release_ms: 600,
            hold_ms: None,
        }
    }
}

/// Gain of one bus over one block: linear from `g0` by `slope` per frame.
/// A piecewise-linear gain moves smoothly, so it makes no zipper noise.
#[derive(Debug, Clone, Copy)]
struct BusGain {
    g0: f32,
    slope: f32,
}

impl BusGain {
    const UNITY: Self = Self {
        g0: 1.0,
        slope: 0.0,
    };

    #[inline]
    fn at(&self, frame: usize) -> f32 {
        if self.slope == 0.0 {
            self.g0
        } else {
            self.g0 + self.slope * frame as f32
        }
    }
}

/// State of one bus.
#[derive(Debug, Clone, Copy)]
struct BusState {
    /// Bus volume the fader asks for.
    volume: f32,
    /// Volume actually applied (slewed towards `volume`).
    vol_gain: f32,
    /// Ducking multiplier actually applied (1.0 = not ducked).
    duck_gain: f32,
    /// A duck was requested and not yet released.
    duck_active: bool,
    /// Multiplier the duck heads for.
    duck_target: f32,
    /// Per-frame step while ducking down / coming back.
    step_down: f32,
    step_up: f32,
    /// Frames of full ducking left before the automatic release.
    hold_left: Option<u64>,
}

impl BusState {
    const fn new() -> Self {
        Self {
            volume: 1.0,
            vol_gain: 1.0,
            duck_gain: 1.0,
            duck_active: false,
            duck_target: 1.0,
            step_down: 0.0,
            step_up: 0.0,
            hold_left: None,
        }
    }

    fn applied_gain(&self) -> f32 {
        self.vol_gain * self.duck_gain
    }

    fn duck(&mut self, settings: DuckSettings) {
        let target = settings.target.clamp(0.0, 1.0);
        let depth = 1.0 - target;
        if depth <= 1e-6 {
            // Nothing to duck to: behave like a release.
            self.release();
            return;
        }
        let attack = ms_to_frames(settings.attack_ms).max(MIN_FADE_FRAMES) as f32;
        let release = ms_to_frames(settings.release_ms).max(MIN_FADE_FRAMES) as f32;
        self.duck_active = true;
        self.duck_target = target;
        self.step_down = depth / attack;
        self.step_up = depth / release;
        self.hold_left = settings.hold_ms.map(|ms| ms_to_frames(ms) as u64);
    }

    fn release(&mut self) {
        self.duck_active = false;
        self.hold_left = None;
    }

    /// Advance by one block and return the gain ramp across it.
    fn advance(&mut self, frames: usize) -> BusGain {
        let n = frames as f32;
        let g0 = self.applied_gain();

        let vol_step = n / RAMP_FRAMES;
        self.vol_gain = approach(self.vol_gain, self.volume, vol_step, vol_step);

        let target = if self.duck_active {
            self.duck_target
        } else {
            1.0
        };
        self.duck_gain = approach(self.duck_gain, target, self.step_down * n, self.step_up * n);

        if self.duck_active && self.duck_gain <= self.duck_target {
            if let Some(left) = self.hold_left.as_mut() {
                *left = left.saturating_sub(frames as u64);
                if *left == 0 {
                    self.release();
                }
            }
        }

        let g1 = self.applied_gain();
        BusGain {
            g0,
            slope: (g1 - g0) / n,
        }
    }
}

/// Move `value` towards `target` by at most `down` (when above) or `up` (when
/// below), landing exactly on `target`.
fn approach(value: f32, target: f32, down: f32, up: f32) -> f32 {
    if value > target {
        (value - down).max(target)
    } else {
        (value + up).min(target)
    }
}

/// All buses plus the gains of the block that was mixed last.
#[derive(Debug)]
struct BusBank {
    states: [BusState; BUS_COUNT],
    /// Gains of the latest main pass; the local pass reuses them so the buses
    /// advance exactly once per callback.
    block: [BusGain; BUS_COUNT],
}

impl BusBank {
    const fn new() -> Self {
        Self {
            states: [BusState::new(); BUS_COUNT],
            block: [BusGain::UNITY; BUS_COUNT],
        }
    }

    fn advance(&mut self, frames: usize) -> [BusGain; BUS_COUNT] {
        for (state, gain) in self.states.iter_mut().zip(self.block.iter_mut()) {
            *gain = state.advance(frames);
        }
        self.block
    }
}

/// Per-channel gains of the pan law for `pan` in -1.0 (left) ..= 1.0 (right).
///
/// Sources are stereo, so this is a *balance*: the channel on the side the
/// sound moves to stays at unity and the opposite one is turned down with an
/// equal-power (cosine) law, reaching silence at the extreme. Centre is
/// exactly `(1.0, 1.0)`, so an unpanned track is untouched.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    let amount = pan.abs();
    let attenuated = if amount >= 1.0 {
        0.0
    } else {
        (amount * FRAC_PI_2).cos()
    };
    if pan < 0.0 {
        (1.0, attenuated)
    } else {
        (attenuated, 1.0)
    }
}

/// What happens to a track once its fade-out reaches silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingHalt {
    /// Keep the position: `resume_track` continues where it left off.
    Pause,
    /// Rewind to the start (a stopped track restarts from 0 when replayed).
    Stop,
}

/// Where a track's samples come from.
#[derive(Debug)]
enum TrackSource {
    /// A sound decoded into memory (short effects, loops): the track reads it
    /// at `TrackState::position`.
    Memory(Arc<DecodedAudio>),
    /// A long file decoded on a worker thread and read through a ring buffer.
    /// It keeps its own position.
    Stream(Box<StreamSource>),
}

/// Internal track state
#[derive(Debug)]
struct TrackState {
    _id: Uuid,
    /// Where the samples come from
    source: TrackSource,
    /// Current playback position in frames (memory sources only: a stream
    /// tracks its own)
    position: usize,
    /// Track volume (0.0 - 1.0): the gain the track ramps towards.
    volume: f32,
    /// Gain actually applied right now; follows `volume` (or 0 while fading
    /// out) through a short linear ramp, or the explicit `fade` when one runs.
    gain: f32,
    /// Explicit fade in progress. While `None`, `gain` slews towards the
    /// target with the 10 ms click-protection ramp.
    fade: Option<Fade>,
    /// Whether track is playing
    playing: bool,
    /// Set while the track fades out before pausing / stopping.
    halt: Option<PendingHalt>,
    /// Paused (not stopped or finished): kept by [`AudioMixer::prune_finished`].
    paused: bool,
    /// Whether track should loop
    looped: bool,
    /// Pan target, -1.0 ..= 1.0.
    pan: f32,
    /// Pan gains actually applied (slewed towards `pan_gains(pan)`).
    pan_l: f32,
    pan_r: f32,
    /// Bus the track is summed into.
    bus: Bus,
    /// Heard on the master's device only, never broadcast.
    local_only: bool,
}

impl TrackState {
    /// The gain this track is heading for.
    fn target_gain(&self) -> f32 {
        if self.halt.is_some() {
            0.0
        } else {
            self.volume
        }
    }

    /// Whether the track counts as playing for the outside world: a track that
    /// is fading out after `stop`/`pause` already reports as stopped.
    fn is_audible_playing(&self) -> bool {
        self.playing && self.halt.is_none()
    }

    /// The single place the mixer reads source samples from.
    ///
    /// Copies up to `dst.len() / 2` interleaved stereo frames into `dst`,
    /// wrapping when the track loops, and advances the source by the frames
    /// read. Returns the number of frames written; fewer than asked for means
    /// a non-looping source ended.
    ///
    /// A memory source copies from `self.position`; a stream source reads its
    /// ring buffer (silence on underrun, see [`StreamSource::read`]). It runs
    /// on the audio thread: no allocation, no blocking.
    fn fill_from_source(&mut self, dst: &mut [f32]) -> usize {
        let audio = match &mut self.source {
            TrackSource::Memory(audio) => audio,
            TrackSource::Stream(stream) => {
                stream.set_looped(self.looped);
                return stream.read(dst);
            }
        };
        let total = audio.frames;
        let want = dst.len() / CHANNELS;
        let samples = &audio.samples;
        let mut pos = self.position.min(total);
        let mut written = 0;
        while written < want {
            if pos >= total {
                if self.looped {
                    pos = 0;
                } else {
                    break;
                }
            }
            let n = (want - written).min(total - pos);
            dst[written * CHANNELS..(written + n) * CHANNELS]
                .copy_from_slice(&samples[pos * CHANNELS..(pos + n) * CHANNELS]);
            pos += n;
            written += n;
        }
        self.position = pos;
        written
    }

    /// Undo the last `frames` of [`fill_from_source`](Self::fill_from_source)
    /// that were read but not played (a fade-out ended in the middle of a chunk).
    ///
    /// A stream cannot take frames back out of its ring buffer: they are
    /// dropped (at most one chunk, about 10 ms of a track that is fading out).
    fn give_back(&mut self, frames: usize) {
        let TrackSource::Memory(audio) = &self.source else {
            return;
        };
        let total = audio.frames.max(1);
        let frames = frames % total;
        self.position = if self.position >= frames {
            self.position - frames
        } else {
            self.position + total - frames
        };
    }

    /// Whether there is anything to play at all.
    fn has_audio(&self) -> bool {
        match &self.source {
            TrackSource::Memory(audio) => audio.frames > 0,
            TrackSource::Stream(_) => true,
        }
    }

    /// The source is used up (a memory source at its end, a stream that played
    /// out), so a resume has nothing left to continue.
    fn at_end(&self) -> bool {
        match &self.source {
            TrackSource::Memory(audio) => self.position >= audio.frames,
            TrackSource::Stream(stream) => stream.is_finished(),
        }
    }

    /// A track starting from silence begins at its target pan instead of
    /// slewing there, so the attack of a hard-panned one-shot does not leak
    /// into the opposite channel. A track that is already sounding keeps its
    /// ramp.
    fn snap_pan_if_silent(&mut self) {
        if !self.playing {
            let (l, r) = pan_gains(self.pan);
            self.pan_l = l;
            self.pan_r = r;
        }
    }

    /// Back to the start of the source.
    fn rewind(&mut self) {
        self.position = 0;
        if let TrackSource::Stream(stream) = &mut self.source {
            stream.rewind();
        }
    }

    /// The source has played out.
    fn mark_ended(&mut self) {
        if let TrackSource::Memory(audio) = &self.source {
            self.position = audio.frames;
        }
    }

    /// Mix this track into `out` (adding), advancing it by `out.len() / 2`
    /// frames. `extra` is an additional gain, `bus` the bus gain ramp.
    fn render(
        &mut self,
        out: &mut [f32],
        extra: f32,
        bus: BusGain,
        scratch: &mut [f32; CHUNK_FRAMES * CHANNELS],
    ) {
        let frames = out.len() / CHANNELS;
        let ramp_step = 1.0 / RAMP_FRAMES;
        let target = self.target_gain();
        let halting = self.halt.is_some();
        let (pan_target_l, pan_target_r) = pan_gains(self.pan);
        let (mut pan_l, mut pan_r) = (self.pan_l, self.pan_r);
        let mut gain = self.gain;
        let mut fade = self.fade;
        let mut finished = false;
        let mut halted = false;
        let mut offset = 0;

        while offset < frames {
            let want = (frames - offset).min(CHUNK_FRAMES);
            let got = self.fill_from_source(&mut scratch[..want * CHANNELS]);
            let mut consumed = got;

            for i in 0..got {
                if let Some(f) = fade.as_mut() {
                    gain = f.next_gain();
                    if f.is_done() {
                        fade = None;
                    }
                } else if gain != target {
                    let delta = (target - gain).clamp(-ramp_step, ramp_step);
                    gain += delta;
                    if (target - gain).abs() < 1e-6 {
                        gain = target;
                    }
                }

                if pan_l != pan_target_l {
                    pan_l += (pan_target_l - pan_l).clamp(-ramp_step, ramp_step);
                    if (pan_target_l - pan_l).abs() < 1e-6 {
                        pan_l = pan_target_l;
                    }
                }
                if pan_r != pan_target_r {
                    pan_r += (pan_target_r - pan_r).clamp(-ramp_step, ramp_step);
                    if (pan_target_r - pan_r).abs() < 1e-6 {
                        pan_r = pan_target_r;
                    }
                }

                let g = gain * bus.at(offset + i) * extra;
                let dst = (offset + i) * CHANNELS;
                out[dst] += scratch[i * CHANNELS] * g * pan_l;
                out[dst + 1] += scratch[i * CHANNELS + 1] * g * pan_r;

                // The fade-out of a pause/stop has reached silence.
                if halting && gain <= 0.0 {
                    consumed = i + 1;
                    halted = true;
                    break;
                }
            }

            if consumed < got {
                self.give_back(got - consumed);
            }
            if halted {
                break;
            }
            if got < want {
                finished = true;
                break;
            }
            offset += want;
        }

        self.gain = gain;
        self.fade = fade;
        self.pan_l = pan_l;
        self.pan_r = pan_r;
        if finished {
            self.mark_ended();
            self.playing = false;
            self.halt = None;
            self.fade = None;
        } else if halted {
            self.playing = false;
            self.fade = None;
            match self.halt.take() {
                Some(PendingHalt::Stop) => self.rewind(),
                Some(PendingHalt::Pause) => self.paused = true,
                None => {}
            }
        }
    }
}

/// Smooth peak limiter on the master bus.
///
/// Instant attack (the gain drops as soon as a sample would exceed the
/// ceiling) and an exponential release. Without it the sum of a few tracks
/// at full volume goes past full scale and is hard-clipped — on bass-heavy
/// ambience that is exactly the "farting" distortion.
#[derive(Debug, Clone, Copy)]
struct Limiter {
    gain: f32,
}

impl Limiter {
    const fn new() -> Self {
        Self { gain: 1.0 }
    }

    fn process(&mut self, out: &mut [f32]) {
        let release = 1.0 - (-1.0 / (LIMIT_RELEASE_SECS * TARGET_SAMPLE_RATE as f32)).exp();
        for frame in out.as_chunks_mut::<CHANNELS>().0 {
            let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            self.gain += (1.0 - self.gain) * release;
            if peak * self.gain > LIMIT_CEILING {
                self.gain = LIMIT_CEILING / peak;
            }
            for sample in frame.iter_mut() {
                // The clamp is only a safety net for NaN/inf from a broken file.
                *sample = (*sample * self.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

/// Everything the audio thread touches, shared with the headless loop.
#[derive(Debug)]
struct MixCore {
    tracks: RwLock<HashMap<Uuid, TrackState>>,
    master_volume: parking_lot::Mutex<f32>,
    /// Master-bus limiter state, carried across mix calls.
    limiter: parking_lot::Mutex<Limiter>,
    /// Limiter of the local pass (broadcast stays untouched by it).
    local_limiter: parking_lot::Mutex<Limiter>,
    buses: parking_lot::Mutex<BusBank>,
    /// Peak output level since the GUI last read it (f32 bits; non-negative
    /// floats order like their bit patterns, so `fetch_max` works).
    peak: AtomicU32,
}

impl MixCore {
    fn new(master_volume: f32) -> Self {
        Self {
            tracks: RwLock::new(HashMap::new()),
            master_volume: parking_lot::Mutex::new(master_volume),
            limiter: parking_lot::Mutex::new(Limiter::new()),
            local_limiter: parking_lot::Mutex::new(Limiter::new()),
            buses: parking_lot::Mutex::new(BusBank::new()),
            peak: AtomicU32::new(0),
        }
    }

    fn note_peak(&self, out: &[f32]) {
        let block_peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.peak.fetch_max(block_peak.to_bits(), Ordering::Relaxed);
    }

    /// Sum the tracks of one pass (`local` selects local-only or broadcast
    /// tracks) into `out`. Returns whether any of them was playing.
    fn render_tracks(
        &self,
        local: bool,
        extra: f32,
        gains: &[BusGain; BUS_COUNT],
        out: &mut [f32],
    ) -> bool {
        // Stack scratch for the raw source samples: nothing is allocated on
        // the audio thread.
        let mut scratch = [0.0f32; CHUNK_FRAMES * CHANNELS];
        let mut any_playing = false;

        // One write lock for the whole pass: positions, gains and fade
        // states are updated in place, so nothing is allocated on the
        // audio thread and the GUI never sees a half-updated track.
        let mut tracks = self.tracks.write();
        for track in tracks.values_mut() {
            if track.local_only != local || !track.playing || !track.has_audio() {
                continue;
            }
            any_playing = true;
            track.render(out, extra, gains[track.bus.index()], &mut scratch);
        }
        any_playing
    }

    /// The broadcast pass: all tracks that players may hear.
    fn mix_main(&self, out: &mut [f32]) -> bool {
        let frames = out.len() / CHANNELS;
        out.fill(0.0);
        if frames == 0 {
            return false;
        }

        let master = *self.master_volume.lock();
        let gains = self.buses.lock().advance(frames);
        let any_playing = self.render_tracks(false, 1.0, &gains, out);

        // Apply master volume once, after all tracks have been summed. The
        // CPAL callback used to multiply by master volume a second time.
        if (master - 1.0).abs() > f32::EPSILON {
            for sample in out.iter_mut() {
                *sample *= master;
            }
        }

        // Keep the bus below full scale. Hard clipping of an overloaded sum is
        // the harsh, buzzing distortion; the limiter turns it down instead.
        self.limiter.lock().process(out);

        if any_playing {
            self.note_peak(out);
        }

        any_playing
    }

    /// The local pass: adds local-only tracks on top of an already published
    /// mix.
    fn mix_local(&self, out: &mut [f32]) -> bool {
        if out.len() < CHANNELS {
            return false;
        }
        let master = *self.master_volume.lock();
        let gains = self.buses.lock().block;
        let any_playing = self.render_tracks(true, master, &gains, out);
        if any_playing {
            // The sum of the (already limited) mix and the previews may pass
            // full scale: keep it under the ceiling, for the device only.
            self.local_limiter.lock().process(out);
            self.note_peak(out);
        } else {
            self.local_limiter.lock().gain = 1.0;
        }
        any_playing
    }
}

/// High-performance audio mixer.
///
/// Tracks are mixed on demand by the audio callback (or by the headless loop
/// when no device is available). There is no separate feeder thread per track
/// and no fixed-size intermediate channel for local playback, so the mixer can
/// never fall behind the device clock.
#[derive(Debug)]
pub struct AudioMixer {
    config: MixerConfig,
    core: Arc<MixCore>,
    /// Broadcast channel for network streaming (WebSocket clients)
    broadcast_tx: Sender<MixedOutput>,
    broadcast_rx: Receiver<MixedOutput>,
    /// Headless mixer running state
    running: Arc<AtomicBool>,
    /// Timestamp counter
    timestamp: Arc<AtomicU64>,
    /// Monotonically increasing sequence number for each mix buffer
    seq_no: Arc<AtomicU64>,
    /// Next track ID generator
    next_track_id: Arc<AtomicU64>,
    /// Handle to the headless mix loop thread (None when the device drives us)
    mix_handle: parking_lot::Mutex<Option<thread::JoinHandle<()>>>,
    /// Events of streamed tracks. Bounded: the audio thread sends with
    /// `try_send`, which allocates nothing on a bounded channel.
    stream_events_tx: Sender<StreamEvent>,
    stream_events_rx: Receiver<StreamEvent>,
}

impl AudioMixer {
    /// Create a new audio mixer with default configuration
    pub fn new() -> Self {
        Self::with_config(MixerConfig::default())
    }

    /// Create a new audio mixer with custom configuration
    pub fn with_config(config: MixerConfig) -> Self {
        let (broadcast_tx, broadcast_rx) = bounded(config.channel_capacity);
        let (stream_events_tx, stream_events_rx) = bounded(STREAM_EVENT_CAPACITY);
        let master_volume = config.master_volume;

        Self {
            config,
            core: Arc::new(MixCore::new(master_volume)),
            broadcast_tx,
            broadcast_rx,
            running: Arc::new(AtomicBool::new(false)),
            timestamp: Arc::new(AtomicU64::new(0)),
            seq_no: Arc::new(AtomicU64::new(0)),
            next_track_id: Arc::new(AtomicU64::new(0)),
            mix_handle: parking_lot::Mutex::new(None),
            stream_events_tx,
            stream_events_rx,
        }
    }

    /// Add a new track to the mixer
    ///
    /// Returns the track ID. The track is created in a stopped state; call
    /// [`play_track`](Self::play_track) to begin playback. It starts centred
    /// (pan 0), on the [`Bus::Ambience`] bus and audible to everyone.
    pub fn add_track(&self, audio: Arc<DecodedAudio>, looped: bool) -> Uuid {
        self.insert_track(TrackSource::Memory(audio), looped, |_, _| {})
    }

    /// Add a track that streams from a file (see [`StreamSource`]).
    ///
    /// Like [`add_track`](Self::add_track) it starts stopped. When the track
    /// plays to its end a [`StreamEvent::TrackFinished`] with the returned id
    /// arrives on [`stream_events`](Self::stream_events).
    pub fn add_stream_track(&self, source: StreamSource, looped: bool) -> Uuid {
        let events = self.stream_events_tx.clone();
        self.insert_track(
            TrackSource::Stream(Box::new(source)),
            looped,
            move |id, source| {
                if let TrackSource::Stream(stream) = source {
                    stream.set_event_sink(events, id);
                }
            },
        )
    }

    fn insert_track(
        &self,
        mut source: TrackSource,
        looped: bool,
        attach: impl FnOnce(Uuid, &mut TrackSource),
    ) -> Uuid {
        let id = Uuid::from_u64_pair(0, self.next_track_id.fetch_add(1, Ordering::Relaxed));
        attach(id, &mut source);

        let track = TrackState {
            _id: id,
            source,
            position: 0,
            volume: 1.0,
            gain: 1.0,
            fade: None,
            playing: false,
            halt: None,
            paused: false,
            looped,
            pan: 0.0,
            pan_l: 1.0,
            pan_r: 1.0,
            bus: Bus::default(),
            local_only: false,
        };

        self.core.tracks.write().insert(id, track);
        id
    }

    /// Start the headless mix loop.
    ///
    /// This is only needed when there is no CPAL output device. When a device
    /// is present, the device callback drives [`mix_into`](Self::mix_into) and
    /// this method must not be called.
    pub fn start(&self) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }

        let config = self.config.clone();
        let core = Arc::clone(&self.core);
        let broadcast_tx = self.broadcast_tx.clone();
        let running = Arc::clone(&self.running);
        let timestamp = Arc::clone(&self.timestamp);
        let seq_no = Arc::clone(&self.seq_no);

        let handle = thread::spawn(move || {
            Self::headless_loop(config, core, broadcast_tx, running, timestamp, seq_no);
        });

        *self.mix_handle.lock() = Some(handle);
    }

    /// Stop the headless mixer and wait for the loop to finish.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.mix_handle.lock().take() {
            let _ = handle.join();
        }
    }

    /// Check if the headless mixer loop is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Mix all active broadcast tracks into `out`.
    ///
    /// `out` is interleaved stereo f32 and may be any length the device asks
    /// for (CPAL callbacks are not guaranteed to be 480 frames). The mixer
    /// advances every playing track by exactly `out.len() / 2` frames and
    /// applies the bus gains and the master volume exactly once.
    ///
    /// Tracks flagged local-only are **not** included: call
    /// [`mix_local_into`](Self::mix_local_into) on the same buffer after
    /// [`publish_broadcast`](Self::publish_broadcast) to add them.
    ///
    /// Returns `true` when at least one track was playing during this call.
    pub fn mix_into(&self, out: &mut [f32]) -> bool {
        self.core.mix_main(out)
    }

    /// Add the local-only tracks (previews) to `out`, which must hold the
    /// buffer just filled by [`mix_into`](Self::mix_into) and already handed
    /// to [`publish_broadcast`](Self::publish_broadcast).
    ///
    /// The buffer is then what the master's device should play; the broadcast
    /// copy never contained these tracks. Returns `true` when a local track was
    /// playing.
    pub fn mix_local_into(&self, out: &mut [f32]) -> bool {
        self.core.mix_local(out)
    }

    /// Peak output level (0.0..=1.0) since the previous call, for the meter.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.core.peak.swap(0, Ordering::Relaxed))
    }

    /// Publish an already-mixed buffer to the network broadcast channel.
    ///
    /// The send is non-blocking: if the forwarder is behind, the frame is
    /// dropped rather than stalling the audio callback.
    pub fn publish_broadcast(&self, buffer: &[f32]) {
        Self::publish_broadcast_parts(&self.broadcast_tx, &self.timestamp, &self.seq_no, buffer);
    }

    fn publish_broadcast_parts(
        broadcast_tx: &Sender<MixedOutput>,
        timestamp: &AtomicU64,
        seq_no: &AtomicU64,
        buffer: &[f32],
    ) {
        if buffer.is_empty() {
            return;
        }

        let ts = timestamp.fetch_add(1, Ordering::Relaxed);
        let seq = seq_no.fetch_add(1, Ordering::Relaxed);
        let output = MixedOutput {
            buffer: buffer.to_vec(),
            seq_no: seq,
            timestamp: ts,
        };

        // Drop frames when the forwarder is behind. Never block the audio
        // callback on a network consumer.
        if let Err(crossbeam_channel::TrySendError::Disconnected(_)) = broadcast_tx.try_send(output)
        {
            // Receiver dropped; the mixer can keep running harmlessly.
        }
    }

    /// Headless mix loop — only used when no CPAL device is available.
    ///
    /// Uses a fixed deadline instead of `sleep(interval)`, so scheduler
    /// oversleep does not accumulate into a permanent underrun.
    fn headless_loop(
        config: MixerConfig,
        core: Arc<MixCore>,
        broadcast_tx: Sender<MixedOutput>,
        running: Arc<AtomicBool>,
        timestamp: Arc<AtomicU64>,
        seq_no: Arc<AtomicU64>,
    ) {
        let frames = config.buffer_frames.max(1);
        let mut buffer = vec![0.0f32; frames * CHANNELS];

        let sample_rate = TARGET_SAMPLE_RATE.max(1) as u64;
        let period = Duration::from_nanos((frames as u64 * 1_000_000_000) / sample_rate);
        let mut next_deadline = Instant::now() + period;

        while running.load(Ordering::Acquire) {
            let playing = core.mix_main(&mut buffer);
            if playing {
                Self::publish_broadcast_parts(&broadcast_tx, &timestamp, &seq_no, &buffer);
            }
            // There is no local device here; local tracks still advance (and
            // finish) on the same clock, their output is discarded.
            core.mix_local(&mut buffer);

            let now = Instant::now();
            if next_deadline > now {
                thread::sleep(next_deadline - now);
            } else if now.duration_since(next_deadline) > period * 4 {
                // The process was descheduled or suspended for a long time.
                // Resync instead of trying to catch up with a burst.
                next_deadline = now;
            }
            next_deadline += period;
        }
    }

    /// Set master volume (0.0 - 1.0)
    pub fn set_master_volume(&self, volume: f32) -> Result<(), MixerError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(MixerError::InvalidVolume(volume));
        }
        *self.core.master_volume.lock() = volume;
        Ok(())
    }

    /// Get current master volume
    pub fn master_volume(&self) -> f32 {
        *self.core.master_volume.lock()
    }

    /// Run `f` on a track, or fail with `TrackNotFound`.
    fn with_track<R>(
        &self,
        track_id: Uuid,
        f: impl FnOnce(&mut TrackState) -> R,
    ) -> Result<R, MixerError> {
        let mut tracks = self.core.tracks.write();
        tracks
            .get_mut(&track_id)
            .map(f)
            .ok_or(MixerError::TrackNotFound(track_id))
    }

    /// Set volume for a specific track
    ///
    /// The change is a 10 ms ramp. It replaces an explicit fade that is
    /// running (the fader was grabbed), except while the track is fading out
    /// to pause/stop: that fade keeps going and the new volume applies when
    /// the track is played again.
    pub fn set_track_volume(&self, track_id: Uuid, volume: f32) -> Result<(), MixerError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(MixerError::InvalidVolume(volume));
        }
        self.with_track(track_id, |track| {
            track.volume = volume;
            if track.halt.is_none() {
                track.fade = None;
            }
        })
    }

    /// Get volume for a specific track
    ///
    /// During an explicit fade this is the volume the fade heads for; see
    /// [`track_gain`](Self::track_gain) for the gain applied right now.
    pub fn track_volume(&self, track_id: Uuid) -> Result<f32, MixerError> {
        self.with_track(track_id, |t| t.volume)
    }

    /// Gain applied to the track as of the last mixed block (volume, fades and
    /// ramps included; bus, pan and master not). Mostly useful for tests and
    /// meters.
    pub fn track_gain(&self, track_id: Uuid) -> Result<f32, MixerError> {
        self.with_track(track_id, |t| t.gain)
    }

    /// Whether an explicit fade is still running on the track.
    pub fn is_track_fading(&self, track_id: Uuid) -> Result<bool, MixerError> {
        self.with_track(track_id, |t| t.fade.is_some())
    }

    /// Enable or disable looping for a specific track.
    ///
    /// Takes effect immediately: the mix loop checks the flag when it reaches
    /// the end of the buffer, so toggling it mid-playback either wraps around
    /// from the start or lets the track finish and stop.
    pub fn set_track_looped(&self, track_id: Uuid, looped: bool) -> Result<(), MixerError> {
        self.with_track(track_id, |t| t.looped = looped)
    }

    /// Whether a specific track is set to loop.
    pub fn is_track_looped(&self, track_id: Uuid) -> Result<bool, MixerError> {
        self.with_track(track_id, |t| t.looped)
    }

    /// Pan a track, -1.0 (hard left) ..= 1.0 (hard right); 0.0 is centre and
    /// leaves the signal untouched. See [`pan_gains`] for the law. Changes are
    /// ramped, so dragging the control is click-free.
    pub fn set_track_pan(&self, track_id: Uuid, pan: f32) -> Result<(), MixerError> {
        if !(-1.0..=1.0).contains(&pan) {
            return Err(MixerError::InvalidPan(pan));
        }
        self.with_track(track_id, |t| t.pan = pan)
    }

    /// Pan currently set on a track.
    pub fn track_pan(&self, track_id: Uuid) -> Result<f32, MixerError> {
        self.with_track(track_id, |t| t.pan)
    }

    /// Move a track to another bus. Applies from the next mixed block.
    pub fn set_track_bus(&self, track_id: Uuid, bus: Bus) -> Result<(), MixerError> {
        self.with_track(track_id, |t| t.bus = bus)
    }

    /// Bus a track is summed into.
    pub fn track_bus(&self, track_id: Uuid) -> Result<Bus, MixerError> {
        self.with_track(track_id, |t| t.bus)
    }

    /// Make a track local-only (a preview): it is heard on the master's device
    /// but never reaches the broadcast buffer. Set it before playing.
    pub fn set_track_local_only(&self, track_id: Uuid, local_only: bool) -> Result<(), MixerError> {
        self.with_track(track_id, |t| t.local_only = local_only)
    }

    /// Whether a track is local-only.
    pub fn is_track_local_only(&self, track_id: Uuid) -> Result<bool, MixerError> {
        self.with_track(track_id, |t| t.local_only)
    }

    /// Set the volume of a bus (0.0 - 1.0), applied to every track on it
    /// before the master limiter and ramped over 10 ms.
    pub fn set_bus_volume(&self, bus: Bus, volume: f32) -> Result<(), MixerError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(MixerError::InvalidVolume(volume));
        }
        self.core.buses.lock().states[bus.index()].volume = volume;
        Ok(())
    }

    /// Volume the bus fader is set to.
    pub fn bus_volume(&self, bus: Bus) -> f32 {
        self.core.buses.lock().states[bus.index()].volume
    }

    /// Gain applied to the bus as of the last mixed block: volume times the
    /// ducking multiplier, both smoothed.
    pub fn bus_gain(&self, bus: Bus) -> f32 {
        self.core.buses.lock().states[bus.index()].applied_gain()
    }

    /// Duck a bus: turn it down to `settings.target` over `attack_ms`. With
    /// `hold_ms: None` it stays down until [`release_duck`](Self::release_duck);
    /// otherwise it comes back by itself. Ducking again while one is running
    /// continues from the current level.
    pub fn duck_bus(&self, bus: Bus, settings: DuckSettings) {
        self.core.buses.lock().states[bus.index()].duck(settings);
    }

    /// Release the duck of a bus: it comes back to its full volume over the
    /// `release_ms` given to [`duck_bus`](Self::duck_bus). No-op when the bus
    /// is not ducked.
    pub fn release_duck(&self, bus: Bus) {
        self.core.buses.lock().states[bus.index()].release();
    }

    /// Whether a duck was requested on the bus and not yet released (it may
    /// still be on its way back up).
    pub fn is_bus_ducked(&self, bus: Bus) -> bool {
        let bank = self.core.buses.lock();
        let state = &bank.states[bus.index()];
        state.duck_active || state.duck_gain < 1.0
    }

    /// Start playback for a track (resets position to 0)
    ///
    /// Starts at full gain on purpose: a one-shot stinger (door slam, sword
    /// hit) must keep its attack.
    pub fn play_track(&self, track_id: Uuid) -> Result<(), MixerError> {
        self.with_track(track_id, |track| {
            track.snap_pan_if_silent();
            track.playing = true;
            track.rewind();
            track.halt = None;
            track.paused = false;
            track.fade = None;
            track.gain = track.volume;
        })
    }

    /// Start playback from the beginning, fading in to the track volume over
    /// `duration_ms` (at least the 10 ms click ramp).
    ///
    /// A track that is already audible keeps its current gain as the starting
    /// point of the fade instead of dropping to silence first.
    pub fn play_track_with_fade_in(
        &self,
        track_id: Uuid,
        duration_ms: u32,
        curve: FadeCurve,
    ) -> Result<(), MixerError> {
        self.with_track(track_id, |track| {
            let start = if track.playing { track.gain } else { 0.0 };
            track.snap_pan_if_silent();
            track.playing = true;
            track.rewind();
            track.halt = None;
            track.paused = false;
            track.gain = start;
            track.fade = Fade::between(start, track.volume, ms_to_frames(duration_ms), curve);
            if track.fade.is_none() {
                track.gain = track.volume;
            }
        })
    }

    /// Continue a paused track from where it was, fading back in.
    ///
    /// Unlike [`play_track`](Self::play_track) this keeps the position; the
    /// engine's `resume` used to call `play_track` and restart from the top.
    pub fn resume_track(&self, track_id: Uuid) -> Result<(), MixerError> {
        self.with_track(track_id, |track| {
            if !track.playing {
                track.snap_pan_if_silent();
                track.gain = 0.0;
            }
            track.playing = !track.at_end() || track.looped;
            track.halt = None;
            track.paused = false;
        })
    }

    /// Like [`resume_track`](Self::resume_track), but the fade-in lasts
    /// `duration_ms` along `curve`.
    pub fn resume_track_with_fade_in(
        &self,
        track_id: Uuid,
        duration_ms: u32,
        curve: FadeCurve,
    ) -> Result<(), MixerError> {
        self.with_track(track_id, |track| {
            if !track.playing {
                track.snap_pan_if_silent();
                track.gain = 0.0;
            }
            track.playing = !track.at_end() || track.looped;
            track.halt = None;
            track.paused = false;
            track.fade = Fade::between(track.gain, track.volume, ms_to_frames(duration_ms), curve);
        })
    }

    /// Fade a track's gain to `target_gain` (0.0 - 1.0) over `duration_ms`
    /// (at least the 10 ms click ramp) along `curve`.
    ///
    /// The fade starts from the gain the track has *right now*, so calling it
    /// in the middle of another fade continues smoothly instead of jumping.
    /// `target_gain` becomes the track volume. The track keeps playing (at
    /// silence for 0.0): follow up with a stop if it should end. A pending
    /// pause/stop fade-out is cancelled.
    pub fn fade_track_to(
        &self,
        track_id: Uuid,
        target_gain: f32,
        duration_ms: u32,
        curve: FadeCurve,
    ) -> Result<(), MixerError> {
        if !(0.0..=1.0).contains(&target_gain) {
            return Err(MixerError::InvalidVolume(target_gain));
        }
        self.with_track(track_id, |track| {
            track.volume = target_gain;
            track.halt = None;
            if track.playing {
                track.fade =
                    Fade::between(track.gain, target_gain, ms_to_frames(duration_ms), curve);
            } else {
                track.fade = None;
                track.gain = target_gain;
            }
        })
    }

    /// Stop playback for a track (does not remove it).
    ///
    /// The track fades out over a few milliseconds instead of being cut
    /// mid-waveform, which is an audible click; it reports as stopped at once.
    pub fn stop_track(&self, track_id: Uuid) -> Result<(), MixerError> {
        self.halt_track(track_id, PendingHalt::Stop, None)
    }

    /// Stop a track after a fade-out of `duration_ms`. It reports as stopped
    /// at once, keeps sounding while it fades, and rewinds when silent.
    ///
    /// Safe to repeat: a second stop during the fade-out does not restart it;
    /// only one that is *shorter* than what remains (e.g. the plain
    /// [`stop_track`](Self::stop_track)) shortens it.
    pub fn stop_track_with_fade_out(
        &self,
        track_id: Uuid,
        duration_ms: u32,
        curve: FadeCurve,
    ) -> Result<(), MixerError> {
        self.halt_track(track_id, PendingHalt::Stop, Some((duration_ms, curve)))
    }

    /// Pause playback for a track (fades out, keeps the position).
    pub fn pause_track(&self, track_id: Uuid) -> Result<(), MixerError> {
        self.halt_track(track_id, PendingHalt::Pause, None)
    }

    /// Pause a track after a fade-out of `duration_ms`; the position is kept.
    /// Same repeat rules as [`stop_track_with_fade_out`](Self::stop_track_with_fade_out).
    pub fn pause_track_with_fade_out(
        &self,
        track_id: Uuid,
        duration_ms: u32,
        curve: FadeCurve,
    ) -> Result<(), MixerError> {
        self.halt_track(track_id, PendingHalt::Pause, Some((duration_ms, curve)))
    }

    fn halt_track(
        &self,
        track_id: Uuid,
        halt: PendingHalt,
        fade: Option<(u32, FadeCurve)>,
    ) -> Result<(), MixerError> {
        self.with_track(track_id, |track| {
            if !track.playing {
                if halt == PendingHalt::Stop {
                    track.rewind();
                    track.paused = false;
                }
                return;
            }

            let fade = fade
                .and_then(|(ms, curve)| Fade::between(track.gain, 0.0, ms_to_frames(ms), curve));

            match track.halt {
                None => {
                    track.halt = Some(halt);
                    track.fade = fade;
                }
                Some(existing) => {
                    // Already fading out. Stop outranks pause, and the
                    // quicker of the two fades wins; never restart a fade
                    // that is already on its way.
                    if halt == PendingHalt::Stop {
                        track.halt = Some(PendingHalt::Stop);
                    } else {
                        track.halt = Some(existing);
                    }
                    let current_left = track.fade.map(|f| f.remaining());
                    match (current_left, fade) {
                        // The current fade-out is a short ramp already.
                        (None, _) => {}
                        // A short ramp replaces a long fade.
                        (Some(_), None) => track.fade = None,
                        (Some(left), Some(new)) => {
                            if new.len < left {
                                track.fade = Some(new);
                            }
                        }
                    }
                }
            }
        })
    }

    /// Receiver of [`StreamEvent`]s (a streamed track played to its end).
    pub fn stream_events(&self) -> Receiver<StreamEvent> {
        self.stream_events_rx.clone()
    }

    /// Playback position of a track in milliseconds. A track that is looping
    /// reports the position within the current pass.
    pub fn track_position_ms(&self, track_id: Uuid) -> Result<u64, MixerError> {
        self.with_track(track_id, |t| match &t.source {
            TrackSource::Memory(audio) => {
                let frames = if t.looped && audio.frames > 0 {
                    t.position % audio.frames
                } else {
                    t.position
                };
                frames as u64 * 1000 / u64::from(TARGET_SAMPLE_RATE)
            }
            TrackSource::Stream(stream) => stream.position_ms(),
        })
    }

    /// Jump to `ms` in a track (clamped to its length). A streamed track
    /// discards its buffered audio and refills from the new position.
    pub fn seek_track_ms(&self, track_id: Uuid, ms: u64) -> Result<(), MixerError> {
        self.with_track(track_id, |t| match &mut t.source {
            TrackSource::Memory(audio) => {
                let frame = ms * u64::from(TARGET_SAMPLE_RATE) / 1000;
                t.position = (frame as usize).min(audio.frames);
            }
            TrackSource::Stream(stream) => stream.seek(ms),
        })
    }

    /// Length of a track in milliseconds, `None` for a streamed file whose
    /// header does not say (before the first full pass).
    pub fn track_duration_ms(&self, track_id: Uuid) -> Result<Option<u64>, MixerError> {
        self.with_track(track_id, |t| match &t.source {
            TrackSource::Memory(audio) => {
                Some(audio.frames as u64 * 1000 / u64::from(TARGET_SAMPLE_RATE))
            }
            TrackSource::Stream(stream) => stream.duration_ms(),
        })
    }

    /// Tags of a streamed track (`None` for a sound held in memory, which
    /// does not keep them).
    pub fn track_meta(&self, track_id: Uuid) -> Result<Option<TrackMeta>, MixerError> {
        self.with_track(track_id, |t| match &t.source {
            TrackSource::Memory(_) => None,
            TrackSource::Stream(stream) => Some(stream.meta().clone()),
        })
    }

    /// Underruns of a streamed track (0 for memory sounds).
    pub fn track_underruns(&self, track_id: Uuid) -> Result<u64, MixerError> {
        self.with_track(track_id, |t| match &t.source {
            TrackSource::Memory(_) => 0,
            TrackSource::Stream(stream) => stream.underruns(),
        })
    }

    /// Check if a track is playing
    pub fn is_track_playing(&self, track_id: Uuid) -> Result<bool, MixerError> {
        self.with_track(track_id, |t| t.is_audible_playing())
    }

    /// Remove a track from the mixer
    pub fn remove_track(&self, track_id: Uuid) -> Result<(), MixerError> {
        let mut tracks = self.core.tracks.write();
        if tracks.remove(&track_id).is_some() {
            Ok(())
        } else {
            Err(MixerError::TrackNotFound(track_id))
        }
    }

    /// Drop every track that is neither playing nor fading out.
    ///
    /// Each `play` adds a track; without this a long session accumulates
    /// thousands of dead entries that the audio callback walks on every buffer.
    pub fn prune_finished(&self) -> usize {
        let mut tracks = self.core.tracks.write();
        let before = tracks.len();
        tracks.retain(|_, t| t.playing || t.paused);
        before - tracks.len()
    }

    /// Whether the mixer still holds this track.
    pub fn has_track(&self, track_id: Uuid) -> bool {
        self.core.tracks.read().contains_key(&track_id)
    }

    /// Get the broadcast output receiver for network streaming (WebSocket clients)
    pub fn broadcast_output_receiver(&self) -> Receiver<MixedOutput> {
        self.broadcast_rx.clone()
    }

    /// Get number of active tracks
    pub fn track_count(&self) -> usize {
        self.core.tracks.read().len()
    }

    /// Get buffer size in frames
    pub fn buffer_frames(&self) -> usize {
        self.config.buffer_frames
    }

    /// Get sample rate
    pub fn sample_rate(&self) -> u32 {
        TARGET_SAMPLE_RATE
    }

    /// Get channels
    pub fn channels(&self) -> u32 {
        CHANNELS as u32
    }
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decoder::DecodedAudio;

    #[test]
    fn test_mixer_creation() {
        let mixer = AudioMixer::new();
        assert!(!mixer.is_running());
        assert_eq!(mixer.master_volume(), 1.0);
        assert_eq!(mixer.track_count(), 0);
    }

    #[test]
    fn test_master_volume() {
        let mixer = AudioMixer::new();
        mixer.set_master_volume(0.5).unwrap();
        assert_eq!(mixer.master_volume(), 0.5);

        assert!(mixer.set_master_volume(1.5).is_err());
        assert!(mixer.set_master_volume(-0.1).is_err());
    }

    #[test]
    fn test_add_remove_track() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.0; 48000 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);
        assert_eq!(mixer.track_count(), 1);

        mixer.remove_track(id).unwrap();
        assert_eq!(mixer.track_count(), 0);
        assert!(mixer.remove_track(id).is_err());
    }

    #[test]
    fn test_track_volume() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.0; 48000 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);

        mixer.set_track_volume(id, 0.5).unwrap();
        assert_eq!(mixer.track_volume(id).unwrap(), 0.5);

        assert!(mixer.set_track_volume(id, 1.5).is_err());
    }

    #[test]
    fn test_play_stop_track() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.5; 4800], 48000, 2));
        let id = mixer.add_track(audio, false);

        assert!(!mixer.is_track_playing(id).unwrap());
        mixer.play_track(id).unwrap();
        assert!(mixer.is_track_playing(id).unwrap());
        mixer.stop_track(id).unwrap();
        assert!(!mixer.is_track_playing(id).unwrap());
    }

    #[test]
    fn test_mix_into_applies_master_volume_once() {
        let mixer = AudioMixer::new();
        mixer.set_master_volume(0.5).unwrap();

        let audio = Arc::new(DecodedAudio::new(vec![1.0; 480 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);
        mixer.play_track(id).unwrap();

        let mut out = vec![0.0f32; 480 * 2];
        assert!(mixer.mix_into(&mut out));
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[out.len() - 1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_mix_into_respects_arbitrary_buffer_size() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.25; 48000 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);
        mixer.play_track(id).unwrap();

        // A device callback may ask for more or fewer frames than the
        // nominal 10 ms mix size.
        for frames in [64usize, 480, 1024, 37] {
            let mut out = vec![0.0f32; frames * 2];
            assert!(mixer.mix_into(&mut out));
            assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        }
    }

    #[test]
    fn test_mix_into_loops_track() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.25; 100 * 2], 48000, 2));
        let id = mixer.add_track(audio, true);
        mixer.play_track(id).unwrap();

        let mut out = vec![0.0f32; 480 * 2];
        assert!(mixer.mix_into(&mut out));
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(mixer.is_track_playing(id).unwrap());
    }

    #[test]
    fn test_mix_into_marks_finished_track_stopped() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.5; 100 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);
        mixer.play_track(id).unwrap();

        let mut out = vec![0.0f32; 480 * 2];
        assert!(mixer.mix_into(&mut out));
        assert!(out[..200].iter().all(|s| (*s - 0.5).abs() < 1e-6));
        assert!(out[200..].iter().all(|s| *s == 0.0));
        assert!(!mixer.is_track_playing(id).unwrap());
    }

    /// Turning "repeat" on for a track that is already running keeps it going
    /// past the end of its buffer instead of letting it stop.
    #[test]
    fn test_set_track_looped_takes_effect_mid_playback() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.25; 100 * 2], 48000, 2));
        let id = mixer.add_track(audio, false);
        mixer.play_track(id).unwrap();

        assert!(!mixer.is_track_looped(id).unwrap());
        mixer.set_track_looped(id, true).unwrap();
        assert!(mixer.is_track_looped(id).unwrap());

        // One buffer longer than the sound: with repeat on it wraps and keeps
        // producing audio instead of falling silent.
        let mut out = vec![0.0f32; 480 * 2];
        assert!(mixer.mix_into(&mut out));
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(mixer.is_track_playing(id).unwrap());
    }

    /// Turning "repeat" off mid-playback lets the current pass finish and stop.
    #[test]
    fn test_clearing_track_loop_stops_at_end_of_buffer() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.5; 100 * 2], 48000, 2));
        let id = mixer.add_track(audio, true);
        mixer.play_track(id).unwrap();

        mixer.set_track_looped(id, false).unwrap();
        assert!(!mixer.is_track_looped(id).unwrap());

        let mut out = vec![0.0f32; 480 * 2];
        assert!(mixer.mix_into(&mut out));
        assert!(!mixer.is_track_playing(id).unwrap());
    }

    /// Stopping fades the track out over the ramp instead of cutting it
    /// mid-waveform (an audible click), and it reports as stopped at once.
    #[test]
    fn test_stop_fades_out_instead_of_cutting() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.5; 48_000 * 2], 48_000, 2));
        let id = mixer.add_track(audio, true);
        mixer.play_track(id).unwrap();

        let mut out = vec![0.0f32; 480 * 2];
        mixer.mix_into(&mut out);
        mixer.stop_track(id).unwrap();
        assert!(!mixer.is_track_playing(id).unwrap());

        let mut tail = vec![0.0f32; 960 * 2];
        mixer.mix_into(&mut tail);
        // Starts near the old level, decreases monotonically, ends silent.
        assert!(tail[0] > 0.49);
        assert!(tail
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[1] <= w[0] + 1e-6));
        assert_eq!(tail[tail.len() - 1], 0.0);
        // After the fade the track rewinds, so a new play starts from 0.
        assert_eq!(mixer.prune_finished(), 1);
    }

    /// A volume change ramps instead of stepping (no zipper noise).
    #[test]
    fn test_volume_change_is_ramped() {
        let mixer = AudioMixer::new();
        let audio = Arc::new(DecodedAudio::new(vec![0.5; 48_000 * 2], 48_000, 2));
        let id = mixer.add_track(audio, true);
        mixer.play_track(id).unwrap();
        mixer.set_track_volume(id, 0.0).unwrap();

        let mut out = vec![0.0f32; 480 * 2];
        mixer.mix_into(&mut out);
        let max_step = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_step < 0.01, "volume jumped by {max_step}");
    }

    /// Pause keeps the position; resume continues instead of restarting.
    #[test]
    fn test_pause_resume_keeps_position() {
        let mixer = AudioMixer::new();
        let ramp: Vec<f32> = (0..48_000).flat_map(|i| [i as f32 / 48_000.0; 2]).collect();
        let id = mixer.add_track(Arc::new(DecodedAudio::new(ramp, 48_000, 2)), false);
        mixer.play_track(id).unwrap();

        let mut out = vec![0.0f32; 4_800 * 2];
        mixer.mix_into(&mut out);
        mixer.pause_track(id).unwrap();
        mixer.mix_into(&mut out); // fade-out completes
        assert_eq!(mixer.prune_finished(), 0, "a paused track must be kept");

        mixer.resume_track(id).unwrap();
        mixer.mix_into(&mut out);
        // Well past the first 0.1 s, so it did not restart from the top.
        assert!(out[out.len() - 2] > 0.15);
    }

    /// Several loud tracks are limited below full scale instead of clipping.
    #[test]
    fn test_overloaded_mix_is_limited() {
        let mixer = AudioMixer::new();
        for _ in 0..4 {
            let audio = Arc::new(DecodedAudio::new(vec![0.8; 4_800 * 2], 48_000, 2));
            let id = mixer.add_track(audio, true);
            mixer.play_track(id).unwrap();
        }
        let mut out = vec![0.0f32; 4_800 * 2];
        mixer.mix_into(&mut out);
        assert!(out.iter().all(|s| s.abs() <= LIMIT_CEILING + 1e-6));
        assert!(out[out.len() - 1] > 0.9);
    }

    #[test]
    fn test_set_track_looped_rejects_unknown_track() {
        let mixer = AudioMixer::new();
        let unknown = uuid::Uuid::new_v4();
        assert!(mixer.set_track_looped(unknown, true).is_err());
        assert!(mixer.is_track_looped(unknown).is_err());
    }
}

/// Tests for fades, pan, buses, ducking and local-only tracks.
#[cfg(test)]
mod fade_bus_tests {
    use super::*;

    const SR: usize = 48_000;

    fn constant(mixer: &AudioMixer, value: f32, frames: usize, looped: bool) -> Uuid {
        let audio = Arc::new(DecodedAudio::new(vec![value; frames * 2], 48_000, 2));
        mixer.add_track(audio, looped)
    }

    /// A looped 1 s sine with an integer number of cycles, so the loop is seamless.
    fn sine(mixer: &AudioMixer, hz: f32, amp: f32) -> Uuid {
        let mut samples = Vec::with_capacity(SR * 2);
        for i in 0..SR {
            let v = amp * (2.0 * std::f32::consts::PI * hz * i as f32 / SR as f32).sin();
            samples.push(v);
            samples.push(v);
        }
        mixer.add_track(Arc::new(DecodedAudio::new(samples, 48_000, 2)), true)
    }

    fn mix(mixer: &AudioMixer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        mixer.mix_into(&mut out);
        out
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
    }

    fn max_step(values: &[f32]) -> f32 {
        values
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    /// The track's own `playing` flag: unlike `is_track_playing` it stays true
    /// while a fade-out is still sounding.
    fn sounding(mixer: &AudioMixer, id: Uuid) -> bool {
        mixer.core.tracks.read()[&id].playing
    }

    fn position(mixer: &AudioMixer, id: Uuid) -> usize {
        mixer.core.tracks.read()[&id].position
    }

    fn power(window: &[f32]) -> f32 {
        window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32
    }

    fn db(ratio: f32) -> f32 {
        10.0 * ratio.log10()
    }

    // --- the default click-protection ramp is unchanged ----------------------

    #[test]
    fn default_stop_ramp_is_still_480_frames() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, SR, true);
        mixer.play_track(id).unwrap();
        mix(&mixer, 100);
        mixer.stop_track(id).unwrap();

        let out = left(&mix(&mixer, 479));
        assert!(sounding(&mixer, id), "still ramping after 479 frames");
        assert!((out[0] - 0.5 * (1.0 - 1.0 / 480.0)).abs() < 1e-5);

        let out = left(&mix(&mixer, 1));
        assert_eq!(out[0], 0.0);
        assert!(!sounding(&mixer, id), "silent exactly after 480 frames");
    }

    // --- crossfade ------------------------------------------------------------

    fn crossfade_window_levels(curve: FadeCurve) -> Vec<f32> {
        let mixer = AudioMixer::new();
        // Two uncorrelated sources of equal power (0.5 amplitude sines).
        let a = sine(&mixer, 480.0, 0.5);
        let b = sine(&mixer, 1000.0, 0.5);
        mixer.play_track(a).unwrap();
        mix(&mixer, 480);
        let reference = power(&left(&mix(&mixer, 4800)));

        mixer.fade_track_to(a, 0.0, 3000, curve).unwrap();
        mixer.play_track_with_fade_in(b, 3000, curve).unwrap();

        // 3 s in 100 ms windows, fed in device-sized blocks.
        (0..30)
            .map(|_| {
                let mut window = Vec::new();
                for _ in 0..10 {
                    window.extend(left(&mix(&mixer, 480)));
                }
                db(power(&window) / reference)
            })
            .collect()
    }

    #[test]
    fn equal_power_crossfade_keeps_total_energy_constant() {
        for level in crossfade_window_levels(FadeCurve::EqualPower) {
            assert!(level.abs() < 0.5, "energy off by {level} dB");
        }
    }

    #[test]
    fn linear_crossfade_dips_in_the_middle() {
        let levels = crossfade_window_levels(FadeCurve::Linear);
        let dip = levels.iter().cloned().fold(f32::MAX, f32::min);
        assert!(dip < -2.0, "linear should dip ~3 dB, got {dip}");
    }

    // --- fade shape / endpoints ----------------------------------------------

    #[test]
    fn fade_in_starts_silent_and_lands_exactly_on_volume() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 10 * SR, false);
        mixer.set_track_volume(id, 0.8).unwrap();
        mixer
            .play_track_with_fade_in(id, 1000, FadeCurve::Linear)
            .unwrap();

        let out = left(&mix(&mixer, SR));
        assert!(out[0] < 0.01, "starts from silence, got {}", out[0]);
        assert!((out[SR / 2 - 1] - 0.5 * 0.8 * 0.5).abs() < 1e-3);
        assert_eq!(out[SR - 1], 0.5 * 0.8, "last frame is exactly the volume");
        assert!(!mixer.is_track_fading(id).unwrap());
        assert_eq!(mixer.track_gain(id).unwrap(), 0.8);
        // Monotonic, no overshoot.
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn fade_shorter_than_the_ramp_is_clamped_to_the_ramp() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, SR, true);
        mixer.play_track(id).unwrap();
        mixer.fade_track_to(id, 0.0, 0, FadeCurve::Linear).unwrap();
        let out = left(&mix(&mixer, 480));
        assert!(
            max_step(&out) < 0.5 / 400.0,
            "a zero-length fade must not click"
        );
        assert_eq!(out[479], 0.0);
    }

    #[test]
    fn fade_rejects_invalid_target() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 100, false);
        assert!(mixer
            .fade_track_to(id, 1.5, 100, FadeCurve::Linear)
            .is_err());
        assert!(mixer
            .fade_track_to(id, f32::NAN, 100, FadeCurve::Linear)
            .is_err());
        assert!(mixer
            .fade_track_to(Uuid::new_v4(), 0.5, 100, FadeCurve::Linear)
            .is_err());
    }

    // --- interrupted fades ------------------------------------------------------

    #[test]
    fn interrupted_fade_continues_without_a_jump() {
        for curve in [FadeCurve::Linear, FadeCurve::EqualPower] {
            let mixer = AudioMixer::new();
            let id = constant(&mixer, 0.5, SR, true);
            mixer.play_track(id).unwrap();

            let mut all = Vec::new();
            mixer.fade_track_to(id, 0.0, 1000, curve).unwrap();
            all.extend(left(&mix(&mixer, 24_000)));
            let mid = mixer.track_gain(id).unwrap();
            assert!(mid > 0.05 && mid < 0.95, "mid-fade gain {mid}");

            // Turn around mid-fade, then again.
            mixer.fade_track_to(id, 1.0, 1000, curve).unwrap();
            all.extend(left(&mix(&mixer, 12_000)));
            mixer.fade_track_to(id, 0.2, 500, curve).unwrap();
            all.extend(left(&mix(&mixer, 24_000)));

            // The signal is 0.5 * gain; no fade here is steeper than a few
            // 1e-5 of gain per frame, a jump would be orders larger.
            let step = max_step(&all);
            assert!(step < 0.5 * 1e-4, "{curve:?}: jump of {step}");
            assert_eq!(mixer.track_gain(id).unwrap(), 0.2);
        }
    }

    #[test]
    fn set_volume_during_a_fade_takes_over_from_the_current_gain() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, SR, true);
        mixer.play_track(id).unwrap();
        mixer
            .fade_track_to(id, 0.0, 1000, FadeCurve::Linear)
            .unwrap();
        mix(&mixer, 24_000);
        let before = mixer.track_gain(id).unwrap();
        mixer.set_track_volume(id, 1.0).unwrap();
        let out = left(&mix(&mixer, 960));
        assert!(!mixer.is_track_fading(id).unwrap());
        assert!(
            (out[0] - 0.5 * before).abs() < 0.01,
            "no jump at the hand-over"
        );
        assert_eq!(*out.last().unwrap(), 0.5);
    }

    // --- stop / pause after a long fade --------------------------------------

    #[test]
    fn track_stops_exactly_when_the_fade_out_ends() {
        for curve in [FadeCurve::Linear, FadeCurve::EqualPower] {
            let mixer = AudioMixer::new();
            let id = constant(&mixer, 0.5, 10 * SR, true);
            mixer.play_track(id).unwrap();
            mix(&mixer, 1234);
            mixer.stop_track_with_fade_out(id, 100, curve).unwrap();
            assert!(
                !mixer.is_track_playing(id).unwrap(),
                "reports stopped at once"
            );

            // 100 ms = 4800 frames; the odd block sizes put the end mid-chunk.
            let mut tail = left(&mix(&mixer, 4799));
            assert!(sounding(&mixer, id), "{curve:?}: still fading after 4799");
            assert!(tail[0] > 0.45, "tail starts near the old level");
            tail.extend(left(&mix(&mixer, 1)));
            assert!(!sounding(&mixer, id), "{curve:?}: halted on frame 4800");
            assert_eq!(tail[4799], 0.0);
            assert!(tail.windows(2).all(|w| w[1] <= w[0] + 1e-6));
            assert_eq!(position(&mixer, id), 0, "stop rewinds");
            assert_eq!(mixer.prune_finished(), 1);
        }
    }

    #[test]
    fn pause_with_fade_waits_for_the_end_and_keeps_the_position() {
        let mixer = AudioMixer::new();
        // Short looped ramp: the fade-out runs across several loop wraps and
        // ends in the middle of a chunk.
        let ramp: Vec<f32> = (0..1000).flat_map(|i| [i as f32 / 2000.0; 2]).collect();
        let id = mixer.add_track(Arc::new(DecodedAudio::new(ramp, 48_000, 2)), true);
        mixer.play_track(id).unwrap();

        mixer
            .pause_track_with_fade_out(id, 100, FadeCurve::EqualPower)
            .unwrap();
        mix(&mixer, 4799);
        assert!(sounding(&mixer, id));
        mix(&mixer, 1);
        assert!(!sounding(&mixer, id));
        assert_eq!(
            position(&mixer, id),
            4800 % 1000,
            "position at the exact end"
        );
        assert_eq!(mixer.prune_finished(), 0, "paused tracks are kept");

        mixer
            .resume_track_with_fade_in(id, 50, FadeCurve::Linear)
            .unwrap();
        let out = left(&mix(&mixer, 2400));
        assert!(sounding(&mixer, id));
        assert!(out[0] < 0.01);
        assert_eq!(mixer.track_gain(id).unwrap(), 1.0);
    }

    #[test]
    fn repeated_stop_during_fade_out_is_safe() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 10 * SR, true);
        mixer.play_track(id).unwrap();
        mixer
            .stop_track_with_fade_out(id, 1000, FadeCurve::Linear)
            .unwrap();
        let mut all = left(&mix(&mixer, 24_000));

        // Same request again, then a pause: neither restarts nor downgrades.
        mixer
            .stop_track_with_fade_out(id, 1000, FadeCurve::Linear)
            .unwrap();
        mixer
            .pause_track_with_fade_out(id, 1000, FadeCurve::Linear)
            .unwrap();
        all.extend(left(&mix(&mixer, 23_999)));
        assert!(sounding(&mixer, id));
        all.extend(left(&mix(&mixer, 1)));
        assert!(!sounding(&mixer, id), "ends on the original schedule");
        assert_eq!(position(&mixer, id), 0, "stop outranks the later pause");
        assert!(max_step(&all) < 0.5 / 40_000.0, "no restart, no jump");

        // Stopping a stopped track is a no-op.
        mixer.stop_track(id).unwrap();
        mixer
            .stop_track_with_fade_out(id, 500, FadeCurve::Linear)
            .unwrap();
        assert!(!sounding(&mixer, id));
    }

    #[test]
    fn plain_stop_shortens_a_long_fade_out() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 10 * SR, true);
        mixer.play_track(id).unwrap();
        mixer
            .stop_track_with_fade_out(id, 5000, FadeCurve::EqualPower)
            .unwrap();
        mix(&mixer, 12_000);
        let level = mixer.track_gain(id).unwrap();
        mixer.stop_track(id).unwrap();
        let out = left(&mix(&mixer, 480));
        assert!(!sounding(&mixer, id), "the quick stop wins");
        assert!(max_step(&out) < 0.5 * (1.0 / 480.0) + 1e-5);
        assert!((out[0] - 0.5 * level).abs() < 0.01);
    }

    #[test]
    fn fade_to_cancels_a_pending_halt() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 10 * SR, true);
        mixer.play_track(id).unwrap();
        mixer
            .stop_track_with_fade_out(id, 1000, FadeCurve::Linear)
            .unwrap();
        mix(&mixer, 12_000);
        mixer
            .fade_track_to(id, 1.0, 500, FadeCurve::Linear)
            .unwrap();
        assert!(mixer.is_track_playing(id).unwrap());
        mix(&mixer, 24_000);
        assert_eq!(mixer.track_gain(id).unwrap(), 1.0);
        assert!(sounding(&mixer, id));
    }

    // --- pan ------------------------------------------------------------------

    #[test]
    fn centre_pan_leaves_the_signal_untouched() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, SR, true);
        mixer.play_track(id).unwrap();
        let out = mix(&mixer, 960);
        assert!(out.iter().all(|s| *s == 0.5));
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
    }

    #[test]
    fn hard_panned_one_shot_starts_at_its_target_pan() {
        let mixer = AudioMixer::new();
        for (pan, silent_left) in [(1.0f32, true), (-1.0, false)] {
            let id = constant(&mixer, 0.5, 100 * SR, true);
            mixer.set_track_pan(id, pan).unwrap();
            mixer.play_track(id).unwrap();
            let out = mix(&mixer, 4);
            let (silent, loud) = if silent_left { (0, 1) } else { (1, 0) };
            assert_eq!(out[silent], 0.0, "pan {pan}: leaked into the far channel");
            assert!((out[loud] - 0.5).abs() < 1e-6, "pan {pan}: near channel");
            mixer.stop_track(id).unwrap();
            mix(&mixer, 480);
            mixer.remove_track(id).unwrap();
        }
    }

    #[test]
    fn pan_follows_the_equal_power_balance_law() {
        // Law: the far channel is turned down by cos(|pan| * pi / 2).
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
        let (l, r) = pan_gains(0.5);
        assert_eq!(r, 1.0);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        // Symmetric.
        assert_eq!(pan_gains(-0.3).0, pan_gains(0.3).1);
        assert_eq!(pan_gains(-0.3).1, pan_gains(0.3).0);

        let mixer = AudioMixer::new();
        for pan in [-1.0f32, -0.5, 0.25, 0.5, 1.0] {
            let id = constant(&mixer, 0.5, 100 * SR, true);
            mixer.set_track_pan(id, pan).unwrap();
            mixer.play_track(id).unwrap();
            let (gl, gr) = pan_gains(pan);
            mix(&mixer, 1000); // let the pan ramp settle
            let out = mix(&mixer, 480);
            assert!((out[0] - 0.5 * gl).abs() < 1e-6, "pan {pan}: L {}", out[0]);
            assert!((out[1] - 0.5 * gr).abs() < 1e-6, "pan {pan}: R {}", out[1]);
            mixer.stop_track(id).unwrap();
            mix(&mixer, 480);
            mixer.remove_track(id).unwrap();
        }
    }

    #[test]
    fn pan_change_is_ramped() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, SR, true);
        mixer.play_track(id).unwrap();
        mix(&mixer, 480);
        mixer.set_track_pan(id, 1.0).unwrap();
        let out = mix(&mixer, 960);
        assert!(max_step(&left(&out)) < 0.01);
        assert_eq!(out[out.len() - 2], 0.0);
        assert_eq!(mixer.track_pan(id).unwrap(), 1.0);
    }

    #[test]
    fn pan_rejects_out_of_range_values() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 100, false);
        assert!(matches!(
            mixer.set_track_pan(id, 1.1),
            Err(MixerError::InvalidPan(_))
        ));
        assert!(mixer.set_track_pan(id, f32::NAN).is_err());
        assert!(mixer.set_track_pan(id, -1.0).is_ok());
    }

    // --- buses and ducking -------------------------------------------------------

    #[test]
    fn tracks_default_to_the_ambience_bus() {
        let mixer = AudioMixer::new();
        let id = constant(&mixer, 0.5, 100, false);
        assert_eq!(mixer.track_bus(id).unwrap(), Bus::Ambience);
        mixer.set_track_bus(id, Bus::Sfx).unwrap();
        assert_eq!(mixer.track_bus(id).unwrap(), Bus::Sfx);
    }

    fn duck_scenario() -> AudioMixer {
        let mixer = AudioMixer::new();
        let music = constant(&mixer, 0.4, SR, true);
        let amb = constant(&mixer, 0.2, SR, true);
        mixer.set_track_bus(music, Bus::Music).unwrap();
        mixer.play_track(music).unwrap();
        mixer.play_track(amb).unwrap();
        mixer
    }

    #[test]
    fn bus_volume_scales_only_its_tracks_and_is_smooth() {
        let mixer = duck_scenario();
        mixer.set_bus_volume(Bus::Music, 0.5).unwrap();
        assert_eq!(mixer.bus_volume(Bus::Music), 0.5);
        let out = left(&mix(&mixer, 960));
        assert!(max_step(&out) < 0.01, "bus fader must not zipper");
        assert!(out[0] > out[959], "moving down");
        // The next block runs at the settled level.
        let settled = left(&mix(&mixer, 480));
        assert!(settled
            .iter()
            .all(|s| (*s - (0.4 * 0.5 + 0.2)).abs() < 1e-6));
        assert!(mixer.set_bus_volume(Bus::Music, 2.0).is_err());
    }

    #[test]
    fn ducking_attacks_holds_and_returns_to_exactly_the_original_volume() {
        let mixer = duck_scenario();
        let before = mix(&mixer, 480);

        mixer.duck_bus(
            Bus::Music,
            DuckSettings {
                target: 0.25,
                attack_ms: 100,
                release_ms: 200,
                hold_ms: None,
            },
        );
        assert!(mixer.is_bus_ducked(Bus::Music));
        let attack = left(&mix(&mixer, 4800));
        assert!(max_step(&attack) < 1e-3, "attack is smooth");
        // Held ducked; the other bus is untouched.
        let held = left(&mix(&mixer, 24_000));
        assert!((held[held.len() - 1] - (0.4 * 0.25 + 0.2)).abs() < 1e-6);
        assert!((mixer.bus_gain(Bus::Music) - 0.25).abs() < 1e-6);
        assert_eq!(mixer.bus_gain(Bus::Sfx), 1.0);

        mixer.release_duck(Bus::Music);
        let release = left(&mix(&mixer, 9600));
        assert!(max_step(&release) < 1e-3, "release is smooth");
        assert!(!mixer.is_bus_ducked(Bus::Music));
        assert_eq!(mixer.bus_gain(Bus::Music), 1.0, "exactly back to unity");

        let after = mix(&mixer, 480);
        assert_eq!(after, before, "bit-exact original level");
    }

    #[test]
    fn timed_duck_releases_by_itself() {
        let mixer = duck_scenario();
        let before = mix(&mixer, 480);
        mixer.duck_bus(
            Bus::Music,
            DuckSettings {
                target: 0.0,
                attack_ms: 50,
                release_ms: 50,
                hold_ms: Some(200),
            },
        );
        mix(&mixer, 2400); // attack done
        assert_eq!(mixer.bus_gain(Bus::Music), 0.0);
        assert!(mixer.is_bus_ducked(Bus::Music));
        mix(&mixer, 9600 + 480); // hold (200 ms) over
        mix(&mixer, 2400 + 480); // release done
        assert!(!mixer.is_bus_ducked(Bus::Music));
        assert_eq!(mix(&mixer, 480), before);
    }

    #[test]
    fn release_mid_attack_returns_smoothly_from_where_it_was() {
        let mixer = duck_scenario();
        mixer.duck_bus(
            Bus::Music,
            DuckSettings {
                target: 0.0,
                attack_ms: 1000,
                release_ms: 1000,
                hold_ms: None,
            },
        );
        let mut all = left(&mix(&mixer, 12_000));
        let mid = mixer.bus_gain(Bus::Music);
        assert!(mid > 0.5 && mid < 0.9, "mid-attack {mid}");
        mixer.release_duck(Bus::Music);
        all.extend(left(&mix(&mixer, 48_000)));
        assert!(max_step(&all) < 1e-3);
        assert_eq!(mixer.bus_gain(Bus::Music), 1.0);
    }

    #[test]
    fn duck_target_is_clamped_and_unity_target_is_a_release() {
        let mixer = duck_scenario();
        mixer.duck_bus(
            Bus::Music,
            DuckSettings {
                target: 1.0,
                ..DuckSettings::default()
            },
        );
        assert!(!mixer.is_bus_ducked(Bus::Music));
        mixer.duck_bus(
            Bus::Music,
            DuckSettings {
                target: -3.0,
                ..DuckSettings::default()
            },
        );
        mix(&mixer, 48_000);
        assert_eq!(mixer.bus_gain(Bus::Music), 0.0);
    }

    #[test]
    fn buses_stay_under_the_limiter_ceiling() {
        let mixer = AudioMixer::new();
        for bus in Bus::ALL {
            for _ in 0..2 {
                let id = constant(&mixer, 0.9, SR, true);
                mixer.set_track_bus(id, bus).unwrap();
                mixer.play_track(id).unwrap();
            }
        }
        mixer.duck_bus(Bus::Sfx, DuckSettings::default());
        let out = mix(&mixer, 9600);
        assert!(out.iter().all(|s| s.abs() <= LIMIT_CEILING + 1e-6));
    }

    // --- local-only (preview) tracks -------------------------------------------

    #[test]
    fn local_only_track_never_reaches_the_broadcast_buffer() {
        let mixer = AudioMixer::new();
        let main = constant(&mixer, 0.25, SR, true);
        let preview = constant(&mixer, 0.5, SR, true);
        mixer.set_track_local_only(preview, true).unwrap();
        assert!(mixer.is_track_local_only(preview).unwrap());
        mixer.play_track(main).unwrap();
        mixer.play_track(preview).unwrap();

        // The sequence the device callback runs.
        let mut out = vec![0.0f32; 960];
        let playing = mixer.mix_into(&mut out);
        assert!(playing);
        mixer.publish_broadcast(&out);
        assert!(mixer.mix_local_into(&mut out));

        let published = mixer.broadcast_output_receiver().try_recv().unwrap();
        assert!(published.buffer.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(
            out.iter().all(|s| (*s - 0.75).abs() < 1e-6),
            "device hears both"
        );
    }

    #[test]
    fn only_a_preview_playing_publishes_nothing() {
        let mixer = AudioMixer::new();
        let preview = constant(&mixer, 0.5, SR, true);
        mixer.set_track_local_only(preview, true).unwrap();
        mixer.play_track(preview).unwrap();

        let mut out = vec![0.0f32; 960];
        assert!(!mixer.mix_into(&mut out), "nothing for the broadcast");
        assert!(out.iter().all(|s| *s == 0.0));
        assert!(mixer.mix_local_into(&mut out));
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        assert!(mixer.broadcast_output_receiver().try_recv().is_err());
    }

    #[test]
    fn preview_follows_master_volume_and_stays_limited() {
        let mixer = AudioMixer::new();
        let preview = constant(&mixer, 0.8, SR, true);
        mixer.set_track_local_only(preview, true).unwrap();
        mixer.play_track(preview).unwrap();
        mixer.set_master_volume(0.5).unwrap();
        let mut out = vec![0.0f32; 960];
        mixer.mix_into(&mut out);
        mixer.mix_local_into(&mut out);
        assert!(out.iter().all(|s| (*s - 0.4).abs() < 1e-6));

        // Previews on top of a loud mix are limited for the device only.
        mixer.set_master_volume(1.0).unwrap();
        for _ in 0..3 {
            let id = constant(&mixer, 0.8, SR, true);
            mixer.play_track(id).unwrap();
        }
        let mut out = vec![0.0f32; 4800];
        mixer.mix_into(&mut out);
        let main_peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        mixer.mix_local_into(&mut out);
        assert!(main_peak <= LIMIT_CEILING + 1e-6);
        assert!(out.iter().all(|s| s.abs() <= LIMIT_CEILING + 1e-6));
    }

    #[test]
    fn headless_loop_advances_previews_without_broadcasting_them() {
        let mixer = AudioMixer::new();
        // 50 ms preview that finishes by itself.
        let preview = constant(&mixer, 0.5, 2400, false);
        mixer.set_track_local_only(preview, true).unwrap();
        mixer.play_track(preview).unwrap();
        let rx = mixer.broadcast_output_receiver();
        mixer.start();

        let deadline = Instant::now() + Duration::from_secs(5);
        while sounding(&mixer, preview) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        mixer.stop();
        assert!(!sounding(&mixer, preview), "the preview ran to its end");
        assert!(rx.try_recv().is_err(), "and was never published");
    }

    // --- the source seam -----------------------------------------------------------

    #[test]
    fn fill_from_source_wraps_and_gives_back() {
        let mixer = AudioMixer::new();
        let ramp: Vec<f32> = (0..10).flat_map(|i| [i as f32; 2]).collect();
        let id = mixer.add_track(Arc::new(DecodedAudio::new(ramp, 48_000, 2)), true);
        let mut tracks = mixer.core.tracks.write();
        let track = tracks.get_mut(&id).unwrap();

        let mut dst = [0.0f32; 2 * 25];
        assert_eq!(track.fill_from_source(&mut dst), 25);
        assert_eq!(track.position, 5);
        assert_eq!(dst[2 * 12], 2.0, "wrapped twice");
        track.give_back(7);
        assert_eq!(track.position, 8);
        track.give_back(10);
        assert_eq!(track.position, 8, "a whole loop is a no-op");

        track.looped = false;
        track.position = 8;
        assert_eq!(track.fill_from_source(&mut dst), 2, "stops at the end");
    }
}
