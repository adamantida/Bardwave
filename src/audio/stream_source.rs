//! Streaming source: long files are decoded on a worker thread and handed to
//! the audio callback through a lock-free ring buffer, instead of being held
//! in memory as one [`DecodedAudio`](crate::audio::decoder::DecodedAudio)
//! (an hour of stereo f32 at 48 kHz is about 1.4 GB).
//!
//! ```text
//! file -> Symphonia -> stereo -> 48 kHz -> [ SPSC ring ] -> audio callback
//!         (worker thread, may block and allocate)          (read only)
//! ```
//!
//! The callback side ([`StreamSource::read`]) never locks, allocates or does
//! I/O. When the worker falls behind it plays silence for the missing part and
//! counts an underrun; it never panics.
//!
//! Everything the worker produces is f32 interleaved stereo at
//! [`TARGET_SAMPLE_RATE`], converted by the same code as the in-memory path
//! (`AudioDecoder::decode_file`), so a streamed file sounds sample for sample
//! like a fully decoded one.
//!
//! # Seeking
//!
//! A ring buffer cannot be flushed from the consumer side while the producer
//! is writing. [`seek`](StreamSource::seek) therefore bumps a generation
//! counter and sends the target to the worker; the worker repositions the
//! file, then publishes `(generation, samples pushed so far)`. The reader
//! plays silence until it sees its generation acknowledged and then discards
//! exactly the samples that were pushed before the acknowledgement.
//!
//! # Lifetime
//!
//! Dropping the source stops the worker (it never outlives its track).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender, TryRecvError};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer, RingBuffer};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::audio::decoder::{
    AudioDecoder, DecoderError, PacketReader, Resampler, TrackMeta, TARGET_CHANNELS,
    TARGET_SAMPLE_RATE,
};

/// Files longer than this many seconds are streamed instead of decoded into
/// memory (unless `[audio] stream_threshold_secs` says otherwise). Short
/// effects stay in memory: they start instantly and cost little.
pub const DEFAULT_STREAM_THRESHOLD_SECS: u32 = 60;

/// Ring capacity in frames (3 s at 48 kHz, about 1.1 MB).
pub const DEFAULT_RING_FRAMES: usize = 144_000;

/// Frames [`StreamSource::wait_ready`] waits for before reporting "ready"
/// (100 ms): enough that the first callbacks cannot underrun.
const PREFILL_FRAMES: usize = 4_800;

/// Interleaved channel count of the ring (stereo).
const CH: usize = TARGET_CHANNELS as usize;

/// Whether a file with this header information should be streamed.
///
/// `duration_secs` comes from the container header. Formats without one (an
/// MP3 with no Xing header) fall back to the file size: more than about
/// 128 kbit/s * `threshold_secs` worth of bytes counts as long. A threshold of
/// zero disables streaming.
pub fn should_stream(duration_secs: Option<f64>, file_bytes: u64, threshold_secs: u32) -> bool {
    if threshold_secs == 0 {
        return false;
    }
    match duration_secs {
        Some(d) => d > f64::from(threshold_secs),
        None => file_bytes > u64::from(threshold_secs) * 16_000,
    }
}

/// Something that happened to a streamed track, sent to whoever listens
/// (playlists, the director).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    /// The track played to its end (never sent for a looping track, and sent
    /// once per run of the track).
    TrackFinished { track_id: Uuid },
}

/// Tuning knobs. The default is what the engine uses.
#[derive(Debug, Clone, Copy)]
pub struct StreamOptions {
    /// Ring capacity in frames.
    pub ring_frames: usize,
    /// Start with the decoder held back (diagnostics and tests: it makes an
    /// underrun reproducible). Release it with
    /// [`StreamSource::set_decoder_paused`].
    pub start_paused: bool,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            ring_frames: DEFAULT_RING_FRAMES,
            start_paused: false,
        }
    }
}

/// Messages to the worker.
enum Cmd {
    Seek { generation: u32, frame: u64 },
    Stop,
}

/// State shared by the reader and the worker.
struct Shared {
    stop: AtomicBool,
    paused: AtomicBool,
    looped: AtomicBool,
    /// The worker has pushed the last frame of the file (not looping).
    eof: AtomicBool,
    /// Last seek generation the worker has carried out ...
    ack_generation: AtomicU32,
    /// ... and how many samples it had pushed by then.
    ack_boundary: AtomicU64,
    /// Length of one pass of the file in frames, once a pass completed
    /// (0 = not known yet).
    total_frames: AtomicU64,
    /// Set by the worker thread as its very last act.
    exited: Arc<AtomicBool>,
}

/// A file being decoded on a worker thread, read by the mixer.
pub struct StreamSource {
    // `Mutex` only makes the type `Sync` (the mixer keeps tracks behind a
    // lock shared between threads): `&mut self` paths use `get_mut`, which
    // does not lock.
    consumer: Mutex<Consumer<f32>>,
    shared: Arc<Shared>,
    cmd_tx: Sender<Cmd>,
    meta: TrackMeta,
    path: PathBuf,
    /// Duration from the container header, in frames at 48 kHz.
    header_frames: Option<u64>,

    /// Samples popped from the ring so far (played or discarded).
    popped: u64,
    /// Seek generation the reader asked for / is waiting on.
    want_generation: u32,
    /// A seek was requested and its acknowledgement was not seen yet.
    seek_pending: bool,
    /// After a seek the refill is expected, not an underrun.
    refilling: bool,
    /// Frame the current run started from (0, or the last seek target).
    base_frame: u64,
    /// Frames played since `base_frame`.
    played_frames: u64,
    underruns: u64,
    finished: bool,
    events: Option<(Sender<StreamEvent>, Uuid)>,
}

impl std::fmt::Debug for StreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSource")
            .field("path", &self.path)
            .field("position_frames", &self.position_frames())
            .field("underruns", &self.underruns)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl StreamSource {
    /// Open `path` for streaming. The container is probed here (so an
    /// unreadable file is an error now, not silence later); decoding runs on
    /// a worker thread.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecoderError> {
        Self::open_with(path, StreamOptions::default())
    }

    /// [`open`](Self::open) with explicit [`StreamOptions`].
    pub fn open_with(path: impl AsRef<Path>, options: StreamOptions) -> Result<Self, DecoderError> {
        let path = path.as_ref().to_path_buf();
        let reader = PacketReader::open(&path)?;
        let meta = reader.meta.clone();
        let header_frames = reader
            .duration_secs()
            .map(|s| (s * f64::from(TARGET_SAMPLE_RATE)).round() as u64);

        let (producer, consumer) = RingBuffer::new(options.ring_frames.max(PREFILL_FRAMES) * CH);
        let (cmd_tx, cmd_rx) = unbounded();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(options.start_paused),
            looped: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            ack_generation: AtomicU32::new(0),
            ack_boundary: AtomicU64::new(0),
            total_frames: AtomicU64::new(0),
            exited: Arc::new(AtomicBool::new(false)),
        });

        let worker = Worker {
            reader,
            path: path.clone(),
            producer,
            rx: cmd_rx,
            shared: Arc::clone(&shared),
            resampler: None,
            pending: Vec::new(),
            pending_pos: 0,
            pushed: 0,
            pass_frames: 0,
            ended: false,
        };
        thread::Builder::new()
            .name("bardwave-stream".into())
            .spawn(move || {
                let exited = Arc::clone(&worker.shared.exited);
                worker.run();
                exited.store(true, Ordering::Release);
            })?;

        Ok(Self {
            consumer: Mutex::new(consumer),
            shared,
            cmd_tx,
            meta,
            path,
            header_frames,
            popped: 0,
            want_generation: 0,
            seek_pending: false,
            refilling: false,
            base_frame: 0,
            played_frames: 0,
            underruns: 0,
            finished: false,
            events: None,
        })
    }

    /// Where [`StreamEvent::TrackFinished`] goes, and which id it carries.
    ///
    /// The sender must be from a *bounded* channel: the callback sends with
    /// `try_send`, which does not allocate on a bounded channel.
    pub fn set_event_sink(&mut self, tx: Sender<StreamEvent>, track_id: Uuid) {
        self.events = Some((tx, track_id));
    }

    /// Tags of the file (title, artist, album).
    pub fn meta(&self) -> &TrackMeta {
        &self.meta
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the track repeats at the end of the file. Gapless: the worker
    /// rewinds and keeps filling the same ring.
    pub fn set_looped(&self, looped: bool) {
        self.shared.looped.store(looped, Ordering::Relaxed);
    }

    /// Length of the file in frames at 48 kHz: from the container header
    /// before anything was decoded, exact after the first full pass.
    pub fn duration_frames(&self) -> Option<u64> {
        match self.shared.total_frames.load(Ordering::Acquire) {
            0 => self.header_frames,
            n => Some(n),
        }
    }

    /// Length of the file in milliseconds (see [`duration_frames`](Self::duration_frames)).
    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_frames().map(frames_to_ms)
    }

    /// Playback position in frames, counted from the start of the file.
    pub fn position_frames(&self) -> u64 {
        let pos = self.base_frame + self.played_frames;
        match self.duration_frames() {
            Some(total) if total > 0 && self.shared.looped.load(Ordering::Relaxed) => pos % total,
            _ => pos,
        }
    }

    /// Playback position in milliseconds.
    pub fn position_ms(&self) -> u64 {
        frames_to_ms(self.position_frames())
    }

    /// How many times the reader found the ring short of data.
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// The track played to its end.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether the worker thread has terminated.
    pub fn worker_exited(&self) -> bool {
        self.shared.exited.load(Ordering::Acquire)
    }

    /// A flag that turns true when the worker thread has terminated; it stays
    /// valid after the source is dropped (to watch a thread end).
    pub fn worker_exit_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shared.exited)
    }

    /// Frames waiting in the ring buffer.
    pub fn buffered_frames(&self) -> usize {
        self.consumer.lock().slots() / CH
    }

    /// The worker has decoded the whole file (not looping) and every frame of
    /// it is in the ring or already played.
    pub fn is_fully_buffered(&self) -> bool {
        self.shared.eof.load(Ordering::Acquire)
    }

    /// Hold the decoder back / let it run (diagnostics and tests).
    pub fn set_decoder_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
    }

    /// Wait until about 100 ms of audio (or the whole file, if shorter) is
    /// buffered, so playback starts without an underrun. Returns `false` on
    /// timeout. Never call it from the audio callback.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.consumer.lock().slots() >= PREFILL_FRAMES * CH
                || self.shared.eof.load(Ordering::Acquire)
            {
                return true;
            }
            if self.worker_exited() || Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Jump to `ms` milliseconds into the file. Audio already buffered is
    /// discarded; the reader plays silence until the worker has repositioned.
    /// Never blocks.
    pub fn seek(&mut self, ms: u64) {
        self.seek_frame(ms_to_frames(ms));
    }

    /// [`seek`](Self::seek) with the target in frames at 48 kHz.
    pub fn seek_frame(&mut self, frame: u64) {
        self.want_generation = self.want_generation.wrapping_add(1);
        self.seek_pending = true;
        self.refilling = true;
        self.finished = false;
        self.base_frame = frame;
        self.played_frames = 0;
        // Only fails when the worker is gone (it ended on an error): nothing
        // to seek then, the reader will play the track out as finished.
        let _ = self.cmd_tx.send(Cmd::Seek {
            generation: self.want_generation,
            frame,
        });
    }

    /// Back to the start, for a track that is played again. Does nothing (and
    /// keeps the buffered audio) when the source is untouched.
    pub fn rewind(&mut self) {
        if self.base_frame != 0 || self.played_frames != 0 || self.finished || self.seek_pending {
            self.seek_frame(0);
        }
    }

    /// Copy up to `dst.len() / 2` stereo frames into `dst`; real-time safe.
    ///
    /// Returns the number of frames written. Fewer than asked for means the
    /// track ended (and [`StreamEvent::TrackFinished`] was sent once). While
    /// the worker lags or a seek is being carried out the remainder is
    /// silence, the full count is returned, and a lag is counted as an
    /// underrun.
    pub fn read(&mut self, dst: &mut [f32]) -> usize {
        let want = dst.len() / CH;
        if want == 0 || self.finished {
            return 0;
        }
        let dst = &mut dst[..want * CH];

        if self.seek_pending && !self.sync_seek() {
            dst.fill(0.0);
            return want;
        }

        // Order matters: `eof` first, then the fill level. The worker sets it
        // after its last push, so if it reads true every frame is visible.
        let eof = self.shared.eof.load(Ordering::Acquire);
        let consumer = self.consumer.get_mut();
        let (got, _) = consumer.pop_partial_slice(dst);
        let samples = got.len();
        let n = samples / CH;
        self.popped += samples as u64;
        self.played_frames += n as u64;
        if n > 0 {
            self.refilling = false;
        }
        if n == want {
            return n;
        }

        if eof {
            if self.shared.looped.load(Ordering::Relaxed) {
                // Looping was switched on after the worker had finished the
                // file: restart it (one short gap, once).
                self.seek_frame(0);
                dst[n * CH..].fill(0.0);
                return want;
            }
            self.finished = true;
            if let Some((tx, track_id)) = &self.events {
                // Bounded channel: no allocation. A full channel drops the
                // event rather than blocking the audio thread.
                let _ = tx.try_send(StreamEvent::TrackFinished {
                    track_id: *track_id,
                });
            }
            return n;
        }

        dst[n * CH..].fill(0.0);
        if !self.refilling {
            self.underruns += 1;
        }
        want
    }

    /// Complete a pending seek: true once the worker acknowledged it and the
    /// audio from before it was discarded.
    fn sync_seek(&mut self) -> bool {
        if self.shared.ack_generation.load(Ordering::Acquire) != self.want_generation {
            return false;
        }
        let boundary = self.shared.ack_boundary.load(Ordering::Relaxed);
        let consumer = self.consumer.get_mut();
        // Everything pushed before the acknowledgement is in the ring by now.
        let stale = boundary.saturating_sub(self.popped) as usize;
        let stale = stale.min(consumer.slots());
        if stale > 0 {
            if let Ok(chunk) = consumer.read_chunk(stale) {
                chunk.commit_all();
            }
            self.popped += stale as u64;
        }
        if self.popped < boundary {
            return false;
        }
        self.seek_pending = false;
        true
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let _ = self.cmd_tx.send(Cmd::Stop);
        // The worker is not joined: this may run under the mixer's track lock
        // and must not wait for a decode in progress. It notices within a
        // packet and ends (see `worker_exit_flag`).
    }
}

fn frames_to_ms(frames: u64) -> u64 {
    frames * 1000 / u64::from(TARGET_SAMPLE_RATE)
}

fn ms_to_frames(ms: u64) -> u64 {
    ms * u64::from(TARGET_SAMPLE_RATE) / 1000
}

/// The decoder thread.
struct Worker {
    reader: PacketReader,
    path: PathBuf,
    producer: Producer<f32>,
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    /// Built when the first packet tells the real sample rate.
    resampler: Option<Resampler>,
    /// Converted audio waiting for room in the ring.
    pending: Vec<f32>,
    pending_pos: usize,
    /// Samples pushed into the ring so far.
    pushed: u64,
    /// Frames produced in the current pass of the file.
    pass_frames: u64,
    /// The pass is over and the file is not repeating: nothing more to decode.
    ended: bool,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.shared.stop.load(Ordering::Acquire) {
                return;
            }
            // Commands first; only the newest seek matters.
            let mut seek = None;
            loop {
                match self.rx.try_recv() {
                    Ok(Cmd::Seek { generation, frame }) => seek = Some((generation, frame)),
                    Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => break,
                }
            }
            if let Some((generation, frame)) = seek {
                self.seek(generation, frame);
            }

            if self.shared.paused.load(Ordering::Acquire) {
                self.idle(Duration::from_millis(2));
                continue;
            }

            if self.pending_pos >= self.pending.len() && !self.ended {
                self.pending.clear();
                self.pending_pos = 0;
                self.decode_more();
                if self.pending.is_empty() && !self.ended {
                    continue; // the resampler wants more input first
                }
            }

            let progressed = self.push_pending();
            if self.ended
                && self.pending_pos >= self.pending.len()
                && !self.shared.eof.load(Ordering::Relaxed)
            {
                // Release: the reader that sees `eof` also sees every frame.
                self.shared.eof.store(true, Ordering::Release);
            }
            if !progressed {
                // Ring full, or the file is done: sleep until a command
                // arrives or the reader has made room.
                self.idle(if self.ended {
                    Duration::from_millis(100)
                } else {
                    Duration::from_millis(5)
                });
            }
        }
    }

    /// Sleep up to `timeout`, waking early for a command.
    fn idle(&mut self, timeout: Duration) {
        match self.rx.recv_timeout(timeout) {
            Ok(Cmd::Seek { generation, frame }) => self.seek(generation, frame),
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => {
                self.shared.stop.store(true, Ordering::Release)
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    /// Reposition the file, drop what was decoded for the old position and
    /// acknowledge to the reader.
    fn seek(&mut self, generation: u32, frame: u64) {
        self.pending.clear();
        self.pending_pos = 0;
        self.ended = false;
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
        self.pass_frames = frame;
        if let Err(e) = self.reader.seek(frame) {
            // Typically a position beyond the end: the track is over.
            debug!("stream seek to frame {frame} failed ({e}); treating as end");
            self.ended = true;
        }
        self.shared.eof.store(false, Ordering::Release);
        self.shared
            .ack_boundary
            .store(self.pushed, Ordering::Relaxed);
        self.shared
            .ack_generation
            .store(generation, Ordering::Release);
    }

    /// Decode one packet into `pending`, or handle the end of the pass.
    fn decode_more(&mut self) {
        match self.reader.next_chunk() {
            Ok(Some(native)) => self.convert(&native, false),
            Ok(None) => self.end_of_pass(),
            Err(e) => {
                warn!("stream decode of {} stopped: {e}", self.path.display());
                self.end_of_pass();
            }
        }
    }

    /// Native layout and rate -> stereo 48 kHz, appended to `pending`.
    fn convert(&mut self, native: &[f32], finish: bool) {
        let before = self.pending.len();
        let channels = self.reader.channels.unwrap_or(CH) as u32;
        let stereo;
        let samples: &[f32] = if channels != TARGET_CHANNELS && !native.is_empty() {
            match AudioDecoder::convert_channels(native, channels, TARGET_CHANNELS) {
                Ok(s) => {
                    stereo = s;
                    &stereo
                }
                Err(e) => {
                    warn!("stream: {e}");
                    &[]
                }
            }
        } else {
            native
        };

        let rate = self.reader.sample_rate;
        if rate != TARGET_SAMPLE_RATE && rate > 0 {
            if self.resampler.is_none() {
                self.resampler = Resampler::new(CH, rate, TARGET_SAMPLE_RATE).ok();
            }
            match self.resampler.as_mut() {
                Some(r) => r.process(samples, finish, &mut self.pending),
                None => self.pending.extend_from_slice(samples),
            }
        } else {
            self.pending.extend_from_slice(samples);
        }
        self.pass_frames += ((self.pending.len() - before) / CH) as u64;
    }

    fn end_of_pass(&mut self) {
        // Flush the resampler's tail (the last frames of the file).
        if self.resampler.is_some() {
            self.convert(&[], true);
        }
        let produced = self.pass_frames;
        if produced > 0 {
            self.shared.total_frames.store(produced, Ordering::Release);
        }
        if produced == 0 || !self.shared.looped.load(Ordering::Relaxed) {
            self.ended = true;
            return;
        }
        // Loop: rewind (reopen when the container cannot seek) and carry on
        // filling the same ring, so there is no gap at the join.
        if self.reader.seek(0).is_err() {
            match PacketReader::open(&self.path) {
                Ok(reader) => self.reader = reader,
                Err(e) => {
                    warn!("stream loop of {} failed: {e}", self.path.display());
                    self.ended = true;
                    return;
                }
            }
        }
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
        self.pass_frames = 0;
    }

    /// Move as much of `pending` into the ring as fits. Always whole frames.
    fn push_pending(&mut self) -> bool {
        let remaining = self.pending.len() - self.pending_pos;
        let room = self.producer.slots() & !1;
        let n = remaining.min(room) & !1;
        if n == 0 {
            return false;
        }
        let (pushed, _) = self
            .producer
            .push_partial_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
        let pushed = pushed.len();
        self.pending_pos += pushed;
        self.pushed += pushed as u64;
        pushed > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_decides_by_duration() {
        assert!(should_stream(Some(61.0), 0, 60));
        assert!(!should_stream(Some(60.0), 10_000_000, 60));
        assert!(!should_stream(Some(600.0), 10_000_000, 0));
    }

    #[test]
    fn unknown_duration_falls_back_to_file_size() {
        assert!(should_stream(None, 5_000_000, 60));
        assert!(!should_stream(None, 100_000, 60));
        assert!(!should_stream(None, 5_000_000, 0));
    }

    #[test]
    fn ms_and_frames_round_trip() {
        assert_eq!(ms_to_frames(1000), 48_000);
        assert_eq!(frames_to_ms(48_000 * 3 + 480), 3_010);
    }

    #[test]
    fn open_reports_a_missing_file() {
        let err = StreamSource::open("/definitely/not/here.wav").unwrap_err();
        assert!(matches!(err, DecoderError::FileOpen(_)));
    }
}
