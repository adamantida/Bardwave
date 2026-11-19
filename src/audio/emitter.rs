//! Emitters: random and periodic sounds inside a scene ("crows every 20-60
//! seconds", "dripping in the cave", "thunder in the distance").
//!
//! # Layers
//!
//! ```text
//!   Scheduler (pure core)   tick(now_ms) -> Vec<EmitterEvent>
//!        |                  no threads, no clock, no engine: fully testable
//!        +-- EmitterRunner  a thread that calls tick every 50 ms and hands the
//!        |                  events to a sink (for owners that are not the
//!        |                  Director's thread)
//!        +-- EmitterHost    what the Director embeds: the scheduler of the
//!                           active scene, rebuilt when the scene (or its
//!                           emitters, or the catalog behind them) changes
//! ```
//!
//! The Director is driven from the thread that owns the audio engine (see its
//! module docs), and it already has a timer, [`Director::tick`]. The emitters
//! ride on it: [`Director::next_deadline`] includes the next emitter instant,
//! so the console and the headless server wake exactly when a sound is due and
//! nothing else has to be spawned. The pure core is the same either way.
//!
//! # Rules
//!
//! * The random generator is a small xorshift64* ([`Rng`]) with an explicit
//!   seed: the same seed, specs and tick times give the same events.
//! * An emitter is idle for a random time in `min..=max` between two plays
//!   (clamped to at least [`MIN_INTERVAL_MS`]; `min > max` is swapped). Its
//!   first play comes after a short random shift (at most
//!   [`FIRST_SHIFT_MAX_MS`]) so a scene does not start with a burst of
//!   simultaneous sounds.
//! * `max_overlap` limits how many instances of one emitter sound at once. An
//!   instance lasts as long as its sound ([`EmitterSound::duration_ms`]; an
//!   unknown length counts as [`FALLBACK_DURATION_MS`]). When all slots are
//!   taken at the due time, that play is skipped and the next interval is
//!   drawn.
//! * Volume is `volume * (1 + volume_jitter * u)` and pan is
//!   `pan_jitter * u` with `u` uniform in `-1..=1`, both clamped to the legal
//!   range. Zero or nonsensical parameters never stall the scheduler: no
//!   sounds means a disabled emitter, a zero interval is lifted to the floor,
//!   `max_overlap = 0` counts as 1, NaN becomes the default.
//! * One emitter fires at most once per tick, and the next interval counts
//!   from the tick (not from the missed due time), so a stalled owner never
//!   causes a burst of catch-up sounds.
//!
//! # With the Director
//!
//! A scene's emitters start when the scene is activated and stop with it
//! (scene change, [`Director::stop_scene`], [`Director::stop_all`]). Starting
//! again the scene that is already active keeps the running timers. Instances
//! that already sound are faded out with the scene's fade-out when the scene
//! stops or the next scene has `stop_others`, and are spared when the same
//! scene is activated again.
//!
//! Engine pan and volume are per sound, not per instance: an instance started
//! while the same sound still plays keeps that sound's pan and level (the
//! engine's one-shot rule), and an emitter prefers a sound that is not
//! sounding when it has the choice.
//!
//! The GUI reads [`Director::emitter_snapshot`] for the "next sound in ..."
//! indicator.

use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use std::sync::Arc;

use crate::audio::director::{Director, Playback};
use crate::audio::engine::SoundId;
use crate::config::EmitterConfig;

/// How often a runner thread ticks the scheduler.
pub const TICK_STEP_MS: u64 = 50;

/// No emitter fires more often than this, whatever its config says.
pub const MIN_INTERVAL_MS: u64 = 100;

/// The first play of an emitter comes after at most this long (and never
/// later than its `max_interval_ms`).
pub const FIRST_SHIFT_MAX_MS: u64 = 5_000;

/// Assumed length of a sound whose duration is unknown.
pub const FALLBACK_DURATION_MS: u64 = 5_000;

/// Small deterministic generator (xorshift64*). Not for cryptography.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator for `seed` (any value, zero included).
    pub fn new(seed: u64) -> Self {
        // splitmix64 spreads similar seeds apart and keeps the state non-zero.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self {
            state: if z == 0 { 0x2545_F491_4F6C_DD1D } else { z },
        }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0.0..1.0`.
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u64 << 24) as f32)
    }

    /// Uniform in `-1.0..=1.0` (approximately; both ends reachable).
    pub fn next_signed(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }

    /// Uniform in `lo..=hi` (`lo` when `hi <= lo`).
    pub fn range_inclusive(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        let span = u128::from(hi - lo) + 1;
        lo + ((u128::from(self.next_u64()) * span) >> 64) as u64
    }

    /// Uniform index below `len` (`0` when `len <= 1`).
    pub fn index(&mut self, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        self.range_inclusive(0, len as u64 - 1) as usize
    }
}

/// One candidate sound of an emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitterSound {
    /// The engine sound.
    pub sound: SoundId,
    /// Its length; `None` or `0` = unknown ([`FALLBACK_DURATION_MS`]).
    pub duration_ms: Option<u32>,
}

impl EmitterSound {
    /// A candidate with a known length.
    pub fn new(sound: SoundId, duration_ms: u32) -> Self {
        Self {
            sound,
            duration_ms: Some(duration_ms),
        }
    }

    fn duration(&self) -> u64 {
        match self.duration_ms {
            Some(ms) if ms > 0 => u64::from(ms),
            _ => FALLBACK_DURATION_MS,
        }
    }
}

/// An emitter as the scheduler runs it (the engine-side twin of
/// [`EmitterConfig`]).
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterSpec {
    pub sounds: Vec<EmitterSound>,
    pub min_interval_ms: u32,
    pub max_interval_ms: u32,
    pub volume: f32,
    pub volume_jitter: f32,
    pub pan_jitter: f32,
    pub max_overlap: u32,
}

impl Default for EmitterSpec {
    fn default() -> Self {
        let d = EmitterConfig::default();
        Self {
            sounds: Vec::new(),
            min_interval_ms: d.min_interval_ms,
            max_interval_ms: d.max_interval_ms,
            volume: d.volume,
            volume_jitter: d.volume_jitter,
            pan_jitter: d.pan_jitter,
            max_overlap: d.max_overlap,
        }
    }
}

fn unit(value: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        default
    }
}

impl EmitterSpec {
    /// The spec behind a config; `resolve` maps a library sound id to the
    /// engine sound (and its length). Sounds that do not resolve are left out.
    pub fn from_config(
        config: &EmitterConfig,
        resolve: impl Fn(&str) -> Option<EmitterSound>,
    ) -> Self {
        Self {
            sounds: config.sounds.iter().filter_map(|id| resolve(id)).collect(),
            min_interval_ms: config.min_interval_ms,
            max_interval_ms: config.max_interval_ms,
            volume: config.volume,
            volume_jitter: config.volume_jitter,
            pan_jitter: config.pan_jitter,
            max_overlap: config.max_overlap,
        }
    }

    /// The effective interval bounds in ms: ordered, at least
    /// [`MIN_INTERVAL_MS`].
    pub fn interval_bounds(&self) -> (u64, u64) {
        let a = u64::from(self.min_interval_ms);
        let b = u64::from(self.max_interval_ms);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        (lo.max(MIN_INTERVAL_MS), hi.max(MIN_INTERVAL_MS))
    }

    /// The effective overlap limit (at least 1).
    pub fn overlap_limit(&self) -> usize {
        self.max_overlap.max(1) as usize
    }

    /// Whether the emitter can ever fire.
    pub fn is_enabled(&self) -> bool {
        !self.sounds.is_empty()
    }
}

/// One play the scheduler asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitterEvent {
    /// Index of the emitter in the scheduler (the scene's emitter list).
    pub emitter: usize,
    pub sound: SoundId,
    /// Level for this play, `0.0..=1.0`.
    pub volume: f32,
    /// Stereo position for this play, `-1.0..=1.0`.
    pub pan: f32,
}

/// What the GUI shows for one emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitterSnapshot {
    /// Index in the scene's emitter list.
    pub index: usize,
    /// `false` when the emitter has no playable sounds (it never fires).
    pub enabled: bool,
    /// Time until the next play; `None` for a disabled emitter.
    pub next_in_ms: Option<u64>,
    /// Instances of this emitter that still sound.
    pub active: usize,
}

#[derive(Debug, Clone)]
struct Instance {
    sound: SoundId,
    ends_ms: u64,
}

#[derive(Debug, Clone)]
struct EmitterState {
    spec: EmitterSpec,
    /// `None` for a disabled emitter.
    next_due_ms: Option<u64>,
    active: Vec<Instance>,
}

/// The deterministic core. See the module docs.
#[derive(Debug, Clone)]
pub struct Scheduler {
    emitters: Vec<EmitterState>,
    rng: Rng,
}

impl Scheduler {
    /// A scheduler over `specs`, started at `start_ms` (the clock the later
    /// `tick` calls use). The first play of each emitter is shifted by a
    /// random amount.
    pub fn new(specs: Vec<EmitterSpec>, seed: u64, start_ms: u64) -> Self {
        let mut rng = Rng::new(seed);
        let emitters = specs
            .into_iter()
            .map(|spec| {
                let next_due_ms = spec.is_enabled().then(|| {
                    let (_, hi) = spec.interval_bounds();
                    let cap = hi.min(FIRST_SHIFT_MAX_MS);
                    start_ms + rng.range_inclusive(0, cap)
                });
                EmitterState {
                    spec,
                    next_due_ms,
                    active: Vec::new(),
                }
            })
            .collect();
        Self { emitters, rng }
    }

    /// Number of emitters (disabled ones included).
    pub fn len(&self) -> usize {
        self.emitters.len()
    }

    /// No emitters at all.
    pub fn is_empty(&self) -> bool {
        self.emitters.is_empty()
    }

    /// Advance to `now_ms` (non-decreasing between calls) and return the plays
    /// that are due.
    pub fn tick(&mut self, now_ms: u64) -> Vec<EmitterEvent> {
        let mut events = Vec::new();
        for (index, emitter) in self.emitters.iter_mut().enumerate() {
            emitter.active.retain(|i| i.ends_ms > now_ms);
            let Some(due) = emitter.next_due_ms else {
                continue;
            };
            if due > now_ms {
                continue;
            }
            let (lo, hi) = emitter.spec.interval_bounds();
            let next = now_ms + self.rng.range_inclusive(lo, hi);
            emitter.next_due_ms = Some(next);
            if emitter.active.len() >= emitter.spec.overlap_limit() {
                continue; // all slots taken: this play is skipped
            }
            let spec = &emitter.spec;
            // Prefer a sound that is not sounding right now (the engine keeps
            // one pan/level per sound).
            let free: Vec<&EmitterSound> = spec
                .sounds
                .iter()
                .filter(|s| emitter.active.iter().all(|i| i.sound != s.sound))
                .collect();
            let pick = if free.is_empty() {
                spec.sounds[self.rng.index(spec.sounds.len())]
            } else {
                *free[self.rng.index(free.len())]
            };
            let volume = unit(spec.volume, 1.0);
            let jitter = unit(spec.volume_jitter, 0.0);
            let level = (volume * (1.0 + jitter * self.rng.next_signed())).clamp(0.0, 1.0);
            let pan = (unit(spec.pan_jitter, 0.0) * self.rng.next_signed()).clamp(-1.0, 1.0);
            emitter.active.push(Instance {
                sound: pick.sound,
                ends_ms: now_ms + pick.duration(),
            });
            events.push(EmitterEvent {
                emitter: index,
                sound: pick.sound,
                volume: level,
                pan,
            });
        }
        events
    }

    /// The earliest instant any emitter is due (`None` when none can fire).
    pub fn next_due_ms(&self) -> Option<u64> {
        self.emitters.iter().filter_map(|e| e.next_due_ms).min()
    }

    /// Per-emitter state for the GUI.
    pub fn snapshot(&self, now_ms: u64) -> Vec<EmitterSnapshot> {
        self.emitters
            .iter()
            .enumerate()
            .map(|(index, e)| EmitterSnapshot {
                index,
                enabled: e.next_due_ms.is_some(),
                next_in_ms: e.next_due_ms.map(|due| due.saturating_sub(now_ms)),
                active: e.active.iter().filter(|i| i.ends_ms > now_ms).count(),
            })
            .collect()
    }

    /// Sounds with an instance that still sounds at `now_ms` (no duplicates).
    pub fn active_sounds(&self, now_ms: u64) -> Vec<SoundId> {
        let mut out: Vec<SoundId> = Vec::new();
        for instance in self.emitters.iter().flat_map(|e| e.active.iter()) {
            if instance.ends_ms > now_ms && !out.contains(&instance.sound) {
                out.push(instance.sound);
            }
        }
        out
    }
}

// ------------------------------------------------------------- runner ----

enum RunnerMsg {
    Stop,
}

/// A thread that ticks a [`Scheduler`] every [`TICK_STEP_MS`] and passes the
/// events to a sink. For owners that cannot use the Director's own timer (the
/// Director embeds the scheduler itself and does not need this).
///
/// Dropping the runner stops and joins the thread.
pub struct EmitterRunner {
    tx: Sender<RunnerMsg>,
    handle: Option<std::thread::JoinHandle<()>>,
    snapshot: Arc<Mutex<Vec<EmitterSnapshot>>>,
}

impl EmitterRunner {
    /// Start the thread. `specs` and `seed` as for [`Scheduler::new`]; the
    /// scheduler clock starts at 0 now.
    pub fn spawn(
        specs: Vec<EmitterSpec>,
        seed: u64,
        mut sink: impl FnMut(EmitterEvent) + Send + 'static,
    ) -> Self {
        let (tx, rx): (Sender<RunnerMsg>, Receiver<RunnerMsg>) = bounded(4);
        let snapshot = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&snapshot);
        let handle = std::thread::Builder::new()
            .name("emitters".into())
            .spawn(move || {
                let mut scheduler = Scheduler::new(specs, seed, 0);
                let epoch = Instant::now();
                loop {
                    let now_ms = epoch.elapsed().as_millis() as u64;
                    for event in scheduler.tick(now_ms) {
                        sink(event);
                    }
                    *shared.lock() = scheduler.snapshot(now_ms);
                    match rx.recv_timeout(Duration::from_millis(TICK_STEP_MS)) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(RunnerMsg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn emitter thread");
        Self {
            tx,
            handle: Some(handle),
            snapshot,
        }
    }

    /// The state as of the last tick.
    pub fn snapshot(&self) -> Vec<EmitterSnapshot> {
        self.snapshot.lock().clone()
    }

    /// Stop the thread and wait for it. No event is delivered afterwards.
    pub fn stop(&mut self) {
        let _ = self.tx.try_send(RunnerMsg::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for EmitterRunner {
    fn drop(&mut self) {
        self.stop();
    }
}

// --------------------------------------------------------------- host ----

/// Config equality that treats a NaN as equal to itself (a hand-edited config
/// may hold one; plain `==` would make the host rebuild on every tick).
fn same_config(a: &EmitterConfig, b: &EmitterConfig) -> bool {
    a.sounds == b.sounds
        && a.min_interval_ms == b.min_interval_ms
        && a.max_interval_ms == b.max_interval_ms
        && a.max_overlap == b.max_overlap
        && a.volume.to_bits() == b.volume.to_bits()
        && a.volume_jitter.to_bits() == b.volume_jitter.to_bits()
        && a.pan_jitter.to_bits() == b.pan_jitter.to_bits()
}

/// What a built scheduler was made from; a change of any part rebuilds it.
#[derive(Debug, Clone)]
struct Signature {
    scene: String,
    configs: Vec<EmitterConfig>,
    /// Resolved engine sound per configured sound id (`None` = not loaded).
    resolved: Vec<Vec<Option<SoundId>>>,
}

/// The Director's emitter state. See the module docs.
pub struct EmitterHost {
    scheduler: Option<Scheduler>,
    built_for: Option<Signature>,
    epoch: Option<Instant>,
    /// Hands out a seed per (re)build; reproducible from [`set_seed`](Self::set_seed).
    seeds: Rng,
    last_ms: u64,
}

impl Default for EmitterHost {
    fn default() -> Self {
        Self::new()
    }
}

impl EmitterHost {
    /// A host with a random seed.
    pub fn new() -> Self {
        Self::with_seed(uuid::Uuid::new_v4().as_u128() as u64)
    }

    /// A host whose scheduler seeds all derive from `seed`.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            scheduler: None,
            built_for: None,
            epoch: None,
            seeds: Rng::new(seed),
            last_ms: 0,
        }
    }

    /// Reseed (affects schedulers built from now on).
    pub fn set_seed(&mut self, seed: u64) {
        self.seeds = Rng::new(seed);
    }

    /// Whether a scheduler is running.
    pub fn is_running(&self) -> bool {
        self.scheduler.is_some()
    }

    /// Stop all emitters.
    pub fn clear(&mut self) {
        self.scheduler = None;
        self.built_for = None;
    }

    fn now_ms(&mut self, now: Instant) -> u64 {
        let epoch = *self.epoch.get_or_insert(now);
        let ms = now.saturating_duration_since(epoch).as_millis() as u64;
        self.last_ms = self.last_ms.max(ms);
        self.last_ms
    }

    fn matches(
        &self,
        scene: &str,
        configs: &[EmitterConfig],
        resolve: &impl Fn(&str) -> Option<SoundId>,
    ) -> bool {
        let Some(sig) = &self.built_for else {
            return false;
        };
        sig.scene == scene
            && sig.configs.len() == configs.len()
            && sig
                .configs
                .iter()
                .zip(configs)
                .all(|(a, b)| same_config(a, b))
            && sig.resolved.len() == configs.len()
            && configs.iter().zip(&sig.resolved).all(|(config, stored)| {
                config.sounds.len() == stored.len()
                    && config
                        .sounds
                        .iter()
                        .zip(stored)
                        .all(|(id, was)| resolve(id) == *was)
            })
    }
}

// ----------------------------------------------------- Director glue ----

impl Director {
    /// Make the running emitters match the active scene: nothing when no
    /// scene is active or it has no emitters, a fresh scheduler (with the
    /// random first shift) when the scene, its emitters or the sounds behind
    /// them changed, and no change at all otherwise.
    pub(crate) fn sync_emitters<P: Playback + ?Sized>(&mut self, pb: &P, now: Instant) {
        let Some(scene) = self.active_scene().and_then(|id| self.scene(id)) else {
            self.emitters.clear();
            return;
        };
        if scene.emitters.is_empty() {
            self.emitters.clear();
            return;
        }
        let scene_id = scene.id.clone();
        let configs = scene.emitters.clone();
        let catalog = self.catalog();
        let resolve = |id: &str| catalog.iter().find(|e| e.id == id).map(|e| e.sound);
        if self.emitters.matches(&scene_id, &configs, &resolve) {
            return;
        }
        let resolved: Vec<Vec<Option<SoundId>>> = configs
            .iter()
            .map(|c| c.sounds.iter().map(|id| resolve(id)).collect())
            .collect();
        let specs: Vec<EmitterSpec> = configs
            .iter()
            .map(|config| {
                EmitterSpec::from_config(config, |id| {
                    let sound = resolve(id)?;
                    Some(EmitterSound {
                        sound,
                        duration_ms: pb.duration_ms(sound),
                    })
                })
            })
            .collect();
        let start_ms = self.emitters.now_ms(now);
        let seed = self.emitters.seeds.next_u64();
        self.emitters.scheduler = Some(Scheduler::new(specs, seed, start_ms));
        self.emitters.built_for = Some(Signature {
            scene: scene_id,
            configs,
            resolved,
        });
    }

    /// Sync, then start what is due. Returns whether a sound was started.
    pub(crate) fn run_emitters<P: Playback + ?Sized>(&mut self, pb: &P, now: Instant) -> bool {
        self.sync_emitters(pb, now);
        let now_ms = self.emitters.now_ms(now);
        let Some(scheduler) = self.emitters.scheduler.as_mut() else {
            return false;
        };
        let events = scheduler.tick(now_ms);
        let mut started = false;
        for event in events {
            let Some(entry) = self.catalog_sound(event.sound) else {
                continue;
            };
            let level = entry.level(event.volume);
            let result = pb
                .set_bus(event.sound, entry.bus)
                .and_then(|()| {
                    // Pan is per sound in the engine: never move a sound that
                    // still sounds from an earlier instance.
                    if pb.is_playing(event.sound) {
                        Ok(())
                    } else {
                        pb.set_pan(event.sound, event.pan)
                    }
                })
                .and_then(|()| pb.play_one_shot(event.sound, level));
            match result {
                Ok(()) => started = true,
                Err(e) => tracing::warn!("Emitter play of {} failed: {e}", event.sound),
            }
        }
        started
    }

    /// When an emitter is next due (for [`Director::next_deadline`]).
    pub(crate) fn emitter_deadline(&self) -> Option<Instant> {
        let epoch = self.emitters.epoch?;
        let due = self.emitters.scheduler.as_ref()?.next_due_ms()?;
        Some(epoch + Duration::from_millis(due))
    }

    /// Fade out the instances that still sound and stop the emitters (the
    /// scene is stopping).
    pub(crate) fn fade_out_emitters<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        fade: crate::audio::director::FadeSpec,
    ) {
        if let Some(scheduler) = &self.emitters.scheduler {
            for sound in scheduler.active_sounds(self.emitters.last_ms) {
                if pb.is_playing(sound) {
                    if let Err(e) = pb.stop(sound, fade) {
                        tracing::warn!("Emitter fade-out of {sound} failed: {e}");
                    }
                }
            }
        }
        self.emitters.clear();
    }

    /// Stop the emitters without touching what sounds.
    pub(crate) fn clear_emitters(&mut self) {
        self.emitters.clear();
    }

    /// Whether `sound` is an instance the emitters of `scene` started and that
    /// still sounds (such a sound is part of the scene, so re-activating it
    /// does not cut it off).
    pub(crate) fn emitter_instance_of(&self, scene: &str, sound: SoundId) -> bool {
        let host = &self.emitters;
        host.built_for
            .as_ref()
            .is_some_and(|sig| sig.scene == scene)
            && host
                .scheduler
                .as_ref()
                .is_some_and(|s| s.active_sounds(host.last_ms).contains(&sound))
    }

    /// Fix the seed of the emitters' random numbers (tests, reproducible
    /// sessions). Applies to schedulers built afterwards.
    pub fn set_emitter_seed(&mut self, seed: u64) {
        self.emitters.set_seed(seed);
    }

    /// The emitters of the active scene as the GUI shows them: per emitter the
    /// time to the next sound and the instances still sounding. Empty when
    /// the scene has no emitters or no scene is active.
    pub fn emitter_snapshot(&self, now: Instant) -> Vec<EmitterSnapshot> {
        let Some(epoch) = self.emitters.epoch else {
            return Vec::new();
        };
        let now_ms = now.saturating_duration_since(epoch).as_millis() as u64;
        let now_ms = now_ms.max(self.emitters.last_ms);
        self.emitters
            .scheduler
            .as_ref()
            .map(|s| s.snapshot(now_ms))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(sounds: Vec<EmitterSound>, min: u32, max: u32) -> EmitterSpec {
        EmitterSpec {
            sounds,
            min_interval_ms: min,
            max_interval_ms: max,
            ..Default::default()
        }
    }

    fn run(scheduler: &mut Scheduler, from_ms: u64, to_ms: u64) -> Vec<(u64, EmitterEvent)> {
        let mut out = Vec::new();
        let mut t = from_ms;
        while t <= to_ms {
            for e in scheduler.tick(t) {
                out.push((t, e));
            }
            t += TICK_STEP_MS;
        }
        out
    }

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut c = Rng::new(8);
        let xs: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        let zs: Vec<u64> = (0..16).map(|_| c.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(xs, zs);
        let mut z = Rng::new(0);
        assert_ne!(z.next_u64(), 0);
        for _ in 0..1000 {
            let f = a.next_f32();
            assert!((0.0..1.0).contains(&f));
            let v = a.range_inclusive(10, 12);
            assert!((10..=12).contains(&v));
            assert!(a.index(3) < 3);
        }
        assert_eq!(a.range_inclusive(5, 5), 5);
        assert_eq!(a.range_inclusive(9, 5), 9);
        assert_eq!(a.index(0), 0);
    }

    #[test]
    fn intervals_stay_within_bounds() {
        let s = SoundId::new();
        let mut sch = Scheduler::new(
            vec![spec(vec![EmitterSound::new(s, 10)], 1_000, 3_000)],
            1,
            0,
        );
        let events = run(&mut sch, 0, 600_000);
        assert!(events.len() > 100);
        // The first play is within the first shift.
        assert!(events[0].0 <= FIRST_SHIFT_MAX_MS + TICK_STEP_MS);
        for pair in events.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            // Ticks are 50 ms apart, so a gap may round up by one step.
            assert!(
                (1_000..=3_000 + TICK_STEP_MS).contains(&gap),
                "gap {gap} out of bounds"
            );
        }
    }

    #[test]
    fn same_seed_same_events_other_seed_differs() {
        let sounds = vec![
            EmitterSound::new(SoundId::new(), 500),
            EmitterSound::new(SoundId::new(), 800),
        ];
        let mut cfg = spec(sounds, 500, 2_000);
        cfg.volume = 0.6;
        cfg.volume_jitter = 0.5;
        cfg.pan_jitter = 0.8;
        let make = |seed| Scheduler::new(vec![cfg.clone()], seed, 0);
        let a = run(&mut make(42), 0, 120_000);
        let b = run(&mut make(42), 0, 120_000);
        let c = run(&mut make(43), 0, 120_000);
        assert!(!a.is_empty());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn max_overlap_is_never_exceeded() {
        let s = SoundId::new();
        for limit in [1u32, 2, 3] {
            let mut cfg = spec(vec![EmitterSound::new(s, 5_000)], 100, 400);
            cfg.max_overlap = limit;
            let mut sch = Scheduler::new(vec![cfg], 5, 0);
            let mut t = 0;
            let mut peak = 0;
            let mut fired = 0;
            while t < 120_000 {
                fired += sch.tick(t).len();
                peak = peak.max(sch.snapshot(t)[0].active);
                assert!(sch.snapshot(t)[0].active <= limit as usize);
                t += TICK_STEP_MS;
            }
            assert_eq!(peak, limit as usize, "the limit is actually reached");
            assert!(
                fired > limit as usize,
                "slots are reused after instances end"
            );
        }
    }

    #[test]
    fn prefers_sounds_that_are_not_sounding() {
        let (a, b) = (SoundId::new(), SoundId::new());
        let mut cfg = spec(
            vec![EmitterSound::new(a, 60_000), EmitterSound::new(b, 60_000)],
            100,
            100,
        );
        cfg.max_overlap = 2;
        for seed in 0..20 {
            let mut sch = Scheduler::new(vec![cfg.clone()], seed, 0);
            let events = run(&mut sch, 0, 20_000);
            assert_eq!(events.len(), 2);
            assert_ne!(events[0].1.sound, events[1].1.sound);
        }
    }

    #[test]
    fn volume_and_pan_stay_in_range() {
        let s = SoundId::new();
        let mut cfg = spec(vec![EmitterSound::new(s, 10)], 100, 100);
        cfg.volume = 0.9;
        cfg.volume_jitter = 1.0;
        cfg.pan_jitter = 1.0;
        let mut sch = Scheduler::new(vec![cfg], 3, 0);
        let events = run(&mut sch, 0, 300_000);
        assert!(events.len() > 1000);
        let (mut pan_min, mut pan_max) = (1.0f32, -1.0f32);
        for (_, e) in &events {
            assert!((0.0..=1.0).contains(&e.volume));
            assert!((-1.0..=1.0).contains(&e.pan));
            pan_min = pan_min.min(e.pan);
            pan_max = pan_max.max(e.pan);
        }
        assert!(pan_min < -0.8 && pan_max > 0.8, "pan uses the whole range");

        // Without jitter: centred, exact level.
        let mut cfg = spec(vec![EmitterSound::new(s, 10)], 100, 100);
        cfg.volume = 0.5;
        let mut sch = Scheduler::new(vec![cfg], 3, 0);
        for (_, e) in run(&mut sch, 0, 10_000) {
            assert_eq!(e.volume, 0.5);
            assert_eq!(e.pan, 0.0);
        }
    }

    #[test]
    fn degenerate_parameters_do_not_stall_or_flood() {
        let s = SoundId::new();

        // No sounds: disabled, never fires, no deadline.
        let mut sch = Scheduler::new(vec![spec(vec![], 100, 200)], 1, 0);
        assert!(run(&mut sch, 0, 60_000).is_empty());
        assert_eq!(sch.next_due_ms(), None);
        let snap = sch.snapshot(0);
        assert!(!snap[0].enabled && snap[0].next_in_ms.is_none());

        // min > max is swapped.
        let mut sch = Scheduler::new(
            vec![spec(vec![EmitterSound::new(s, 10)], 3_000, 1_000)],
            1,
            0,
        );
        let events = run(&mut sch, 0, 200_000);
        for pair in events.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!((1_000..=3_000 + TICK_STEP_MS).contains(&gap), "gap {gap}");
        }

        // Zero interval: lifted to the floor, one play per tick at most.
        let mut sch = Scheduler::new(vec![spec(vec![EmitterSound::new(s, 1)], 0, 0)], 1, 0);
        let events = run(&mut sch, 0, 10_000);
        assert!(!events.is_empty());
        assert!(events.len() <= (10_000 / MIN_INTERVAL_MS) as usize + 1);

        // max_overlap = 0 counts as 1; unknown duration uses the fallback.
        let mut cfg = spec(
            vec![EmitterSound {
                sound: s,
                duration_ms: None,
            }],
            100,
            100,
        );
        cfg.max_overlap = 0;
        let mut sch = Scheduler::new(vec![cfg], 1, 0);
        let events = run(&mut sch, 0, FALLBACK_DURATION_MS - 100);
        assert_eq!(events.len(), 1);

        // NaN and out-of-range numbers are made harmless.
        let mut cfg = spec(vec![EmitterSound::new(s, 10)], 100, 100);
        cfg.volume = f32::NAN;
        cfg.volume_jitter = f32::INFINITY;
        cfg.pan_jitter = -5.0;
        let mut sch = Scheduler::new(vec![cfg], 1, 0);
        for (_, e) in run(&mut sch, 0, 20_000) {
            assert!((0.0..=1.0).contains(&e.volume) && e.volume.is_finite());
            assert_eq!(e.pan, 0.0);
        }

        // No emitters at all.
        let mut sch = Scheduler::new(Vec::new(), 1, 0);
        assert!(sch.is_empty());
        assert!(sch.tick(1_000).is_empty());
        assert_eq!(sch.next_due_ms(), None);

        // Extreme intervals do not overflow.
        let mut sch = Scheduler::new(
            vec![spec(vec![EmitterSound::new(s, 10)], u32::MAX, u32::MAX)],
            1,
            u64::MAX / 4,
        );
        let _ = sch.tick(u64::MAX / 2);
    }

    #[test]
    fn first_play_is_shifted_and_next_due_reported() {
        let s = SoundId::new();
        let mut sch = Scheduler::new(
            vec![spec(vec![EmitterSound::new(s, 10)], 20_000, 60_000)],
            9,
            1_000,
        );
        let due = sch.next_due_ms().unwrap();
        assert!((1_000..=1_000 + FIRST_SHIFT_MAX_MS).contains(&due));
        assert!(sch.tick(due - 1).is_empty() || due == 0);
        let snap = sch.snapshot(due - 10);
        assert_eq!(snap[0].next_in_ms, Some(10));
        assert_eq!(sch.tick(due).len(), 1);
        // The next play is a full interval away.
        let next = sch.next_due_ms().unwrap();
        assert!((due + 20_000..=due + 60_000).contains(&next));
    }

    #[test]
    fn runner_delivers_events_and_stops() {
        let s = SoundId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut cfg = spec(vec![EmitterSound::new(s, 10)], 100, 100);
        cfg.max_overlap = 4;
        let mut runner = EmitterRunner::spawn(vec![cfg], 1, move |e| {
            let _ = tx.send(e);
        });
        let first = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert_eq!(first.sound, s);
        assert_eq!(runner.snapshot().len(), 1);
        runner.stop();
        while rx.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "nothing after stop");
        runner.stop(); // idempotent
    }
}
