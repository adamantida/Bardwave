//! Session recorder: the mix the players hear, saved as a WAV file.
//!
//! # Where the audio comes from
//!
//! The mixer publishes every mixed buffer to one bounded channel that the
//! network broadcaster drains. A crossbeam receiver is a *competing* consumer:
//! a second reader of that channel would steal frames from the players. So the
//! recorder does not read the mixer's channel; [`spawn_tee`] sits between the
//! mixer and the broadcaster, hands every buffer to the broadcaster untouched
//! and, only while a recording runs, a copy to the [`RecorderTap`].
//!
//! # Real-time safety
//!
//! Nothing here runs on the audio callback. The tee, the tap and the writer are
//! ordinary threads: the callback only does what it always did (a non-blocking
//! `try_send` into the mixer channel). When the writer falls behind (slow disk)
//! the tap drops whole buffers and counts the lost frames; it never blocks.
//!
//! # File format
//!
//! RIFF/WAVE, 48 kHz stereo, 16-bit PCM (default) or 32-bit float, written
//! without any extra dependency. The header is written with zero sizes when the
//! file is created and patched about once a second (and on finish), so a file
//! left behind by a crash or power loss is a valid WAV up to the last patch.
//! Long sessions are split into `<name>_part2.wav`, `_part3.wav`, ... before the
//! 4 GB limit of the format.
//!
//! # Gaps
//!
//! The mixer does not publish silence in headless mode. While the tap is idle
//! the writer pads silence from the wall clock, so the recording keeps the real
//! session time (see [`RecorderOptions::pad_silence`]).

use crate::audio::decoder::{TARGET_CHANNELS, TARGET_SAMPLE_RATE};
use crate::audio::mixer::MixedOutput;
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Sample rate of every recording (the mix rate).
pub const SAMPLE_RATE: u32 = TARGET_SAMPLE_RATE;
/// Channels of every recording (the mix is interleaved stereo).
pub const CHANNELS: usize = TARGET_CHANNELS as usize;

/// Default size (data bytes) at which a recording continues in a new file.
pub const DEFAULT_SPLIT_BYTES: u64 = 1_800_000_000;
/// Default size at which the operator is warned that the file is getting big.
pub const DEFAULT_WARN_BYTES: u64 = 1_500_000_000;
/// Refuse to start with less free space than this (when it can be determined).
pub const MIN_FREE_BYTES: u64 = 200 * 1024 * 1024;
/// How often the file header is made valid on disk.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Largest data chunk a WAV can hold (u32 sizes), with headroom for the header.
const MAX_WAV_DATA_BYTES: u64 = u32::MAX as u64 - 1024;
/// Buffers the tap keeps for the writer thread (~2.5 s of mix).
const TAP_CAPACITY: usize = 256;
/// Buffers the tee keeps for the broadcaster (same as the mixer channel).
const TEE_CAPACITY: usize = 64;
/// The writer wakes at least this often to pad silence and flush.
const IDLE_TICK: Duration = Duration::from_millis(100);
/// Frames of silence written per call while padding a gap.
const PAD_CHUNK_FRAMES: usize = 4800;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a recording could not start or had to stop.
#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    /// Another recording already owns the tap.
    #[error("a recording is already in progress")]
    AlreadyRecording,
    /// The recordings folder cannot be created.
    #[error("cannot create the recordings folder {}: {source}", path.display())]
    Folder {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The disk is (known to be) too full.
    #[error("not enough free disk space: {free_mb} MB free, at least {need_mb} MB needed")]
    DiskSpace { free_mb: u64, need_mb: u64 },
    /// The first file cannot be created.
    #[error("cannot create {}: {source}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A write failed while recording.
    #[error("write failed: {0}")]
    Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// Sample format and WAV writer
// ---------------------------------------------------------------------------

/// Sample format of the WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SampleFormat {
    /// 16-bit signed PCM (default; works everywhere).
    #[default]
    Pcm16,
    /// 32-bit IEEE float (the mix as it is, no rounding).
    Float32,
}

impl SampleFormat {
    /// Bytes per sample.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::Pcm16 => 2,
            Self::Float32 => 4,
        }
    }

    /// Bytes per frame (one sample per channel).
    pub fn bytes_per_frame(self) -> usize {
        self.bytes_per_sample() * CHANNELS
    }

    /// Size of the header that precedes the audio data.
    pub fn header_len(self) -> u64 {
        match self {
            Self::Pcm16 => 44,
            // `fmt ` chunk of 18 bytes (with the empty extension size).
            Self::Float32 => 46,
        }
    }
}

/// The RIFF/WAVE header for `data_len` bytes of audio.
pub fn wav_header(format: SampleFormat, data_len: u32) -> Vec<u8> {
    let header_len = format.header_len() as u32;
    let block_align = format.bytes_per_frame() as u16;
    let mut h = Vec::with_capacity(header_len as usize);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(header_len - 8).wrapping_add(data_len).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    match format {
        SampleFormat::Pcm16 => {
            h.extend_from_slice(&16u32.to_le_bytes());
            h.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
        }
        SampleFormat::Float32 => {
            h.extend_from_slice(&18u32.to_le_bytes());
            h.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
        }
    }
    h.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    h.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    h.extend_from_slice(&(SAMPLE_RATE * u32::from(block_align)).to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&((format.bytes_per_sample() * 8) as u16).to_le_bytes());
    if format == SampleFormat::Float32 {
        h.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    debug_assert_eq!(h.len() as u64, format.header_len());
    h
}

/// A buffered WAV writer whose header sizes can be refreshed while writing.
pub struct WavWriter<W: Write + Seek> {
    out: BufWriter<W>,
    format: SampleFormat,
    data_bytes: u64,
    scratch: Vec<u8>,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Write the header (sizes zero) at the start of `inner`.
    pub fn new(inner: W, format: SampleFormat) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(256 * 1024, inner);
        out.write_all(&wav_header(format, 0))?;
        Ok(Self {
            out,
            format,
            data_bytes: 0,
            scratch: Vec::new(),
        })
    }

    /// Audio bytes written so far.
    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    /// Append interleaved samples (whole frames only: a trailing odd sample is
    /// ignored). Returns the number of audio bytes written.
    pub fn write_samples(&mut self, samples: &[f32]) -> io::Result<u64> {
        let samples = &samples[..samples.len() - samples.len() % CHANNELS];
        self.scratch.clear();
        match self.format {
            SampleFormat::Pcm16 => {
                for &s in samples {
                    let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    self.scratch.extend_from_slice(&v.to_le_bytes());
                }
            }
            SampleFormat::Float32 => {
                for &s in samples {
                    let v = if s.is_nan() { 0.0 } else { s };
                    self.scratch.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        self.out.write_all(&self.scratch)?;
        let n = self.scratch.len() as u64;
        self.data_bytes += n;
        Ok(n)
    }

    /// Flush to the file and rewrite the RIFF and data sizes, so the file on
    /// disk is a valid WAV of everything written so far.
    pub fn patch_header(&mut self) -> io::Result<()> {
        let data = self.data_bytes.min(u64::from(u32::MAX)) as u32;
        let header_len = self.format.header_len();
        self.out.seek(SeekFrom::Start(4))?;
        self.out
            .write_all(&((header_len as u32 - 8).wrapping_add(data)).to_le_bytes())?;
        self.out.seek(SeekFrom::Start(header_len - 4))?;
        self.out.write_all(&data.to_le_bytes())?;
        self.out
            .seek(SeekFrom::Start(header_len + self.data_bytes))?;
        self.out.flush()
    }

    /// Final header patch and flush; returns the underlying writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.patch_header()?;
        self.out.into_inner().map_err(|e| e.into_error())
    }
}

// ---------------------------------------------------------------------------
// Tap and tee
// ---------------------------------------------------------------------------

/// What a recording gets from the tap: its id, the buffers, the loss counter.
type AttachedTap = (u64, Receiver<Vec<f32>>, Arc<AtomicU64>);

struct Attached {
    id: u64,
    tx: Sender<Vec<f32>>,
    dropped: Arc<AtomicU64>,
}

/// The point where the recorder takes buffers off the stream that goes to the
/// players. Idle (a mutex check) when nothing is recording.
#[derive(Default)]
pub struct RecorderTap {
    slot: Mutex<Option<Attached>>,
    next_id: AtomicU64,
}

impl RecorderTap {
    /// A new, idle tap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a recording is attached.
    pub fn is_active(&self) -> bool {
        self.slot.lock().is_some()
    }

    /// Offer one mixed buffer. Never blocks; when the writer is behind the
    /// buffer is dropped and its frames are added to the loss counter.
    pub fn offer(&self, buffer: &[f32]) {
        let slot = self.slot.lock();
        let Some(attached) = slot.as_ref() else {
            return;
        };
        match attached.tx.try_send(buffer.to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(lost)) => {
                attached
                    .dropped
                    .fetch_add((lost.len() / CHANNELS) as u64, Ordering::Relaxed);
            }
            // The writer is gone (it detaches itself); nothing to count.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn attach(&self) -> Result<AttachedTap, RecorderError> {
        let mut slot = self.slot.lock();
        if slot.is_some() {
            return Err(RecorderError::AlreadyRecording);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = bounded(TAP_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        *slot = Some(Attached {
            id,
            tx,
            dropped: dropped.clone(),
        });
        Ok((id, rx, dropped))
    }

    /// Detach recording `id` (a no-op when it is not the attached one). The
    /// writer drains what is queued, then finishes the file.
    fn detach(&self, id: u64) {
        let mut slot = self.slot.lock();
        if slot.as_ref().is_some_and(|a| a.id == id) {
            *slot = None;
        }
    }
}

impl std::fmt::Debug for RecorderTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecorderTap")
            .field("active", &self.is_active())
            .finish()
    }
}

/// The process-wide tap used by the console (the tee in `main` and the REC
/// button share it).
pub fn global_tap() -> Arc<RecorderTap> {
    static TAP: OnceLock<Arc<RecorderTap>> = OnceLock::new();
    TAP.get_or_init(|| Arc::new(RecorderTap::new())).clone()
}

/// Put the tap between the mixer and its consumer.
///
/// Returns the receiver the broadcaster must read instead of `source`. Every
/// buffer goes to it unchanged (dropped when it is full, exactly as the mixer
/// itself would drop it) and a copy goes to `tap` while a recording runs.
pub fn spawn_tee(source: Receiver<MixedOutput>, tap: Arc<RecorderTap>) -> Receiver<MixedOutput> {
    let (tx, rx) = bounded(TEE_CAPACITY);
    thread::Builder::new()
        .name("recorder-tee".into())
        .spawn(move || {
            for output in source {
                tap.offer(&output.buffer);
                let _ = tx.try_send(output);
            }
        })
        .expect("spawn recorder tee thread");
    rx
}

// ---------------------------------------------------------------------------
// Options, stats
// ---------------------------------------------------------------------------

/// How a recording is made.
#[derive(Debug, Clone)]
pub struct RecorderOptions {
    /// Folder the files go to (created when missing).
    pub dir: PathBuf,
    /// 16-bit PCM or 32-bit float.
    pub format: SampleFormat,
    /// Audio bytes per file; a longer recording continues in `_part2.wav`, ...
    pub split_bytes: u64,
    /// Audio bytes at which a size warning is raised.
    pub warn_bytes: u64,
    /// How often the header is made valid on disk.
    pub flush_interval: Duration,
    /// Refuse to start below this much free space (when known).
    pub min_free_bytes: u64,
    /// Fill idle time with silence so the file keeps the session's real time.
    pub pad_silence: bool,
    /// File name without extension; `bardwave-YYYYMMDD-HHMMSS` (UTC) when `None`.
    pub file_stem: Option<String>,
    /// Sleep after every written buffer: simulates a slow disk (tests and
    /// diagnostics of the loss counter). Zero in normal use.
    pub write_delay: Duration,
}

impl RecorderOptions {
    /// Defaults for a recording into `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            format: SampleFormat::Pcm16,
            split_bytes: DEFAULT_SPLIT_BYTES,
            warn_bytes: DEFAULT_WARN_BYTES,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            min_free_bytes: MIN_FREE_BYTES,
            pad_silence: true,
            file_stem: None,
            write_delay: Duration::ZERO,
        }
    }
}

/// Lifecycle of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderState {
    /// Taking audio.
    Recording,
    /// Stopped normally; files are final.
    Finished,
    /// Stopped by an error (files are finalized as far as possible).
    Failed,
}

/// A snapshot of a running (or finished) recording.
#[derive(Debug, Clone)]
pub struct RecorderStats {
    pub state: RecorderState,
    /// Frames written (including padded silence).
    pub frames_written: u64,
    /// Bytes on disk over all parts, headers included.
    pub bytes_written: u64,
    /// Frames lost because the writer fell behind.
    pub dropped_frames: u64,
    /// Number of files started so far.
    pub parts: u32,
    /// The file being written.
    pub current_path: PathBuf,
    /// Size or disk-space warning, if any.
    pub warning: Option<String>,
    /// The error that stopped the recording, if any.
    pub error: Option<String>,
}

impl RecorderStats {
    /// Recorded time in seconds.
    pub fn elapsed_secs(&self) -> f64 {
        self.frames_written as f64 / f64::from(SAMPLE_RATE)
    }
}

/// What a finished recording left behind.
#[derive(Debug, Clone)]
pub struct RecordingSummary {
    /// Every file written, in order.
    pub paths: Vec<PathBuf>,
    pub frames: u64,
    pub bytes: u64,
    pub dropped_frames: u64,
    pub duration_secs: f64,
    /// Set when the recording ended because of an error.
    pub error: Option<String>,
}

const STATE_RECORDING: u8 = 0;
const STATE_FINISHED: u8 = 1;
const STATE_FAILED: u8 = 2;

struct Shared {
    state: AtomicU8,
    frames: AtomicU64,
    bytes: AtomicU64,
    dropped: Arc<AtomicU64>,
    info: Mutex<Info>,
}

struct Info {
    paths: Vec<PathBuf>,
    warning: Option<String>,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Recorder handle
// ---------------------------------------------------------------------------

/// A running recording. Dropping it stops the recording (the file is still
/// finalized by the writer thread).
pub struct Recorder {
    shared: Arc<Shared>,
    tap: Arc<RecorderTap>,
    id: u64,
    handle: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Start recording what `tap` receives.
    ///
    /// Creates the folder and the first file (so a bad folder or a full disk is
    /// reported here), then writes on its own thread.
    pub fn start(tap: &Arc<RecorderTap>, opts: RecorderOptions) -> Result<Self, RecorderError> {
        let (id, rx, dropped) = tap.attach()?;
        match Self::prepare(opts, dropped) {
            Ok((sink, shared, opts)) => {
                let thread_tap = tap.clone();
                let thread_shared = shared.clone();
                let spawned = thread::Builder::new()
                    .name("recorder".into())
                    .spawn(move || run(rx, sink, thread_shared, thread_tap, id, opts));
                match spawned {
                    Ok(handle) => Ok(Self {
                        shared,
                        tap: tap.clone(),
                        id,
                        handle: Some(handle),
                    }),
                    Err(e) => {
                        tap.detach(id);
                        Err(RecorderError::Io(e))
                    }
                }
            }
            Err(e) => {
                tap.detach(id);
                Err(e)
            }
        }
    }

    fn prepare(
        opts: RecorderOptions,
        dropped: Arc<AtomicU64>,
    ) -> Result<(PartSink, Arc<Shared>, RecorderOptions), RecorderError> {
        std::fs::create_dir_all(&opts.dir).map_err(|source| RecorderError::Folder {
            path: opts.dir.clone(),
            source,
        })?;
        let mut warning = None;
        if let Some(free) = free_space(&opts.dir) {
            if free < opts.min_free_bytes {
                return Err(RecorderError::DiskSpace {
                    free_mb: free / (1024 * 1024),
                    need_mb: opts.min_free_bytes / (1024 * 1024),
                });
            }
            let per_hour = u64::from(SAMPLE_RATE) * opts.format.bytes_per_frame() as u64 * 3600;
            if free < per_hour {
                warning = Some(format!(
                    "Low disk space: {} MB free, about {} min of recording",
                    free / (1024 * 1024),
                    free * 60 / per_hour
                ));
            }
        }

        let stem = opts.file_stem.clone().unwrap_or_else(default_file_stem);
        let shared = Arc::new(Shared {
            state: AtomicU8::new(STATE_RECORDING),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            dropped,
            info: Mutex::new(Info {
                paths: Vec::new(),
                warning,
                error: None,
            }),
        });
        let sink = PartSink::open_first(&opts, &stem, shared.clone())?;
        Ok((sink, shared, opts))
    }

    /// A snapshot of the recording.
    pub fn stats(&self) -> RecorderStats {
        let info = self.shared.info.lock();
        RecorderStats {
            state: match self.shared.state.load(Ordering::Acquire) {
                STATE_RECORDING => RecorderState::Recording,
                STATE_FINISHED => RecorderState::Finished,
                _ => RecorderState::Failed,
            },
            frames_written: self.shared.frames.load(Ordering::Relaxed),
            bytes_written: self.shared.bytes.load(Ordering::Relaxed),
            dropped_frames: self.shared.dropped.load(Ordering::Relaxed),
            parts: info.paths.len() as u32,
            current_path: info.paths.last().cloned().unwrap_or_default(),
            warning: info.warning.clone(),
            error: info.error.clone(),
        }
    }

    /// Ask the writer to stop: it drains what is queued and finalizes the file.
    /// Does not wait; poll [`is_finished`](Self::is_finished).
    pub fn request_stop(&self) {
        self.tap.detach(self.id);
    }

    /// Whether the writer thread has ended (stopped or failed).
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    /// Stop, wait for the file to be finalized and report what was written.
    pub fn finish(mut self) -> RecordingSummary {
        self.request_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let stats = self.stats();
        let paths = self.shared.info.lock().paths.clone();
        RecordingSummary {
            paths,
            frames: stats.frames_written,
            bytes: stats.bytes_written,
            dropped_frames: stats.dropped_frames,
            duration_secs: stats.elapsed_secs(),
            error: stats.error,
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.request_stop();
    }
}

// ---------------------------------------------------------------------------
// Writer thread
// ---------------------------------------------------------------------------

/// The files of one recording: the open part and the rotation to the next.
struct PartSink {
    dir: PathBuf,
    stem: String,
    format: SampleFormat,
    split_bytes: u64,
    warn_bytes: u64,
    part: u32,
    writer: Option<WavWriter<File>>,
    shared: Arc<Shared>,
}

impl PartSink {
    fn open_first(
        opts: &RecorderOptions,
        stem: &str,
        shared: Arc<Shared>,
    ) -> Result<Self, RecorderError> {
        let (file, path, stem) = open_unique(&opts.dir, stem)?;
        let writer = WavWriter::new(file, opts.format).map_err(|source| RecorderError::Create {
            path: path.clone(),
            source,
        })?;
        let frame = opts.format.bytes_per_frame() as u64;
        let cap = opts.split_bytes.clamp(frame, MAX_WAV_DATA_BYTES);
        shared
            .bytes
            .fetch_add(opts.format.header_len(), Ordering::Relaxed);
        shared.info.lock().paths.push(path);
        Ok(Self {
            dir: opts.dir.clone(),
            stem,
            format: opts.format,
            split_bytes: cap - cap % frame,
            warn_bytes: opts.warn_bytes,
            part: 1,
            writer: Some(writer),
            shared,
        })
    }

    fn writer(&mut self) -> &mut WavWriter<File> {
        self.writer.as_mut().expect("recorder part is open")
    }

    /// Finish the open part and start `_part<N+1>`.
    fn rotate(&mut self) -> io::Result<()> {
        if let Some(done) = self.writer.take() {
            done.finish()?;
        }
        self.part += 1;
        let path = self
            .dir
            .join(format!("{}_part{}.wav", self.stem, self.part));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.writer = Some(WavWriter::new(file, self.format)?);
        self.shared
            .bytes
            .fetch_add(self.format.header_len(), Ordering::Relaxed);
        self.shared.info.lock().paths.push(path);
        Ok(())
    }

    /// Append samples, continuing in a new file when the current one is full.
    fn write(&mut self, mut samples: &[f32]) -> io::Result<()> {
        let frame_bytes = self.format.bytes_per_frame() as u64;
        samples = &samples[..samples.len() - samples.len() % CHANNELS];
        while !samples.is_empty() {
            let used = self.writer().data_bytes();
            let room_frames = (self.split_bytes - used) / frame_bytes;
            if room_frames == 0 {
                self.rotate()?;
                continue;
            }
            let take = (room_frames as usize * CHANNELS).min(samples.len());
            let bytes = self.writer().write_samples(&samples[..take])?;
            self.shared.bytes.fetch_add(bytes, Ordering::Relaxed);
            self.shared
                .frames
                .fetch_add((take / CHANNELS) as u64, Ordering::Relaxed);
            samples = &samples[take..];
        }
        let used = self.writer().data_bytes();
        if used > self.warn_bytes {
            let mut info = self.shared.info.lock();
            if info
                .warning
                .as_deref()
                .is_none_or(|w| !w.starts_with("Large"))
            {
                info.warning = Some(format!(
                    "Large recording: {:.1} GB in this file; it continues in a new file at {:.1} GB",
                    used as f64 / 1e9,
                    self.split_bytes as f64 / 1e9
                ));
            }
        }
        Ok(())
    }

    fn patch_header(&mut self) -> io::Result<()> {
        self.writer().patch_header()
    }

    fn finish(&mut self) -> io::Result<()> {
        match self.writer.take() {
            Some(w) => w.finish().map(|_| ()),
            None => Ok(()),
        }
    }
}

fn run(
    rx: Receiver<Vec<f32>>,
    mut sink: PartSink,
    shared: Arc<Shared>,
    tap: Arc<RecorderTap>,
    id: u64,
    opts: RecorderOptions,
) {
    let silence = vec![0.0f32; PAD_CHUNK_FRAMES * CHANNELS];
    let mut last_flush = Instant::now();
    let mut pad_clock = Instant::now();

    let result: io::Result<()> = (|| loop {
        match rx.recv_timeout(IDLE_TICK) {
            Ok(buffer) => {
                pad_clock = Instant::now();
                sink.write(&buffer)?;
                if !opts.write_delay.is_zero() {
                    thread::sleep(opts.write_delay);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if opts.pad_silence {
                    let now = Instant::now();
                    let gap = now.duration_since(pad_clock);
                    let mut frames = (gap.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
                    pad_clock += Duration::from_secs_f64(frames as f64 / f64::from(SAMPLE_RATE));
                    while frames > 0 {
                        let n = frames.min(PAD_CHUNK_FRAMES);
                        sink.write(&silence[..n * CHANNELS])?;
                        frames -= n;
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if last_flush.elapsed() >= opts.flush_interval {
            sink.patch_header()?;
            last_flush = Instant::now();
        }
    })();

    let finish = sink.finish();
    let failure = result.err().or(finish.err());
    tap.detach(id);
    match failure {
        Some(e) => {
            shared.info.lock().error = Some(format!("Recording stopped: {e}"));
            shared.state.store(STATE_FAILED, Ordering::Release);
        }
        None => shared.state.store(STATE_FINISHED, Ordering::Release),
    }
}

// ---------------------------------------------------------------------------
// Names, folders, disk space
// ---------------------------------------------------------------------------

/// Where recordings go unless the operator picked a folder.
pub fn default_recordings_dir() -> PathBuf {
    crate::paths::data_dir().join("recordings")
}

/// `bardwave-YYYYMMDD-HHMMSS` for the current moment (UTC).
pub fn default_file_stem() -> String {
    file_stem_utc(SystemTime::now())
}

/// `bardwave-YYYYMMDD-HHMMSS` for `time`, in UTC. The standard library knows
/// no time zones, so recordings are named in UTC.
pub fn file_stem_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "bardwave-{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Create `<stem>.wav`, or `<stem>-2.wav`, ... if the name is taken (a
/// recording is never overwritten). Returns the file, its path and the stem
/// that was finally used (the part files are named after it).
fn open_unique(dir: &Path, stem: &str) -> Result<(File, PathBuf, String), RecorderError> {
    for n in 1u32..1000 {
        let candidate = if n == 1 {
            stem.to_string()
        } else {
            format!("{stem}-{n}")
        };
        let path = dir.join(format!("{candidate}.wav"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path, candidate)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(RecorderError::Create { path, source }),
        }
    }
    Err(RecorderError::Create {
        path: dir.join(format!("{stem}.wav")),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many files with this name",
        ),
    })
}

/// Free bytes on the volume holding `path`, when the platform can tell.
///
/// Known on Unix (`statvfs`); `None` elsewhere, where the check is skipped
/// (a full disk then ends the recording with a write error instead).
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a
    // plain-data out parameter that statvfs fully initializes on success.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

/// Free bytes on the volume holding `path` (unknown on this platform).
#[cfg(not(unix))]
pub fn free_space(_path: &Path) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// Display helpers (shared by the GUI and tests)
// ---------------------------------------------------------------------------

/// `HH:MM:SS` for a duration in seconds.
pub fn format_clock(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total % 3600 / 60,
        total % 60
    )
}

/// `123 KB` / `4.5 MB` / `1.20 GB`.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.2} GB", b / (KB * KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn utc_names_follow_the_calendar() {
        assert_eq!(file_stem_utc(UNIX_EPOCH), "bardwave-19700101-000000");
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(file_stem_utc(t), "bardwave-20231114-221320");
        // Leap day.
        let t = UNIX_EPOCH + Duration::from_secs(1_709_164_800 + 3661);
        assert_eq!(file_stem_utc(t), "bardwave-20240229-010101");
    }

    #[test]
    fn header_sizes_match_the_layout() {
        for format in [SampleFormat::Pcm16, SampleFormat::Float32] {
            assert_eq!(wav_header(format, 0).len() as u64, format.header_len());
        }
    }

    #[test]
    fn writer_patches_sizes_in_memory() {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), SampleFormat::Pcm16).unwrap();
        w.write_samples(&[0.5, -0.5, 1.0, -1.0]).unwrap();
        let bytes = w.finish().unwrap().into_inner();
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(i16::from_le_bytes(bytes[44..46].try_into().unwrap()), 16384);
        assert_eq!(
            i16::from_le_bytes(bytes[50..52].try_into().unwrap()),
            -32767
        );
    }

    #[test]
    fn size_labels() {
        assert_eq!(format_clock(3725.4), "01:02:05");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
    }
}
