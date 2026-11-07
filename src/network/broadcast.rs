//! Audio broadcasting to WebSocket clients
//!
//! The wire payload is Opus-encoded: raw f32 PCM at 48 kHz stereo is 3 Mbit/s,
//! which both floods weak Wi-Fi links (bursty delivery -> jitter-buffer
//! overruns on the client) and holds the CPU of every listener. The mixed
//! audio is encoded to 10 ms Opus packets (~100-300 bytes at 96 kbit/s) right
//! here, before it enters any per-client queue.
//!
//! The engine publishes whatever block size the local audio device asked for,
//! which is usually *not* 480 frames; [`WireEncoder`] reblocks it into fixed
//! 10 ms frames so every packet on the wire is a valid Opus frame (see
//! [`crate::network::opus_wire`] for why that matters).

use axum::extract::ws::Message;
use rusty_opus::OpusEncoder;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};
use uuid::Uuid;

pub use super::opus_wire::wire_encoder;
use super::opus_wire::{WireEncoder, WIRE_CHANNELS, WIRE_FRAME_FRAMES, WIRE_SAMPLE_RATE};

/// The single codec used on the wire. Declared as a constant so clients and
/// tests can agree on it; the format announcement carries it as `format`.
pub const WIRE_CODEC: &str = "opus";

/// A stream that has been silent for longer than this is treated as restarted:
/// the half-assembled frame from before the pause is discarded instead of being
/// glued onto the audio that comes next.
const STREAM_RESTART_GAP: std::time::Duration = std::time::Duration::from_millis(120);

/// How long the engine may stay silent before the partial frame it left in
/// the reblocker is padded and sent: a few device callbacks.
const STREAM_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(40);

/// `AudioFormatInfo` announced to clients on connect / first frame.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioFormatInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// "opus" — see `WIRE_CODEC`.
    pub format: String,
    /// Stream frames per channel per frame (10 ms at 48 kHz => 480).
    pub frame_size: usize,
}

impl AudioFormatInfo {
    pub fn wire() -> Self {
        Self {
            sample_rate: WIRE_SAMPLE_RATE,
            channels: WIRE_CHANNELS,
            format: WIRE_CODEC.to_string(),
            frame_size: WIRE_FRAME_FRAMES,
        }
    }
}

/// Configuration for the broadcaster.
///
/// Only the engine channel capacity lives here. The per-client send buffer and
/// the keepalive interval belong to the WebSocket session; the copies that used
/// to sit in this struct were never read by anything.
#[derive(Debug, Clone)]
pub struct BroadcasterConfig {
    /// Channel capacity for incoming audio from engine
    pub engine_channel_capacity: usize,
}

impl Default for BroadcasterConfig {
    fn default() -> Self {
        Self {
            engine_channel_capacity: 500,
        }
    }
}

/// Text frame that tells a listener which stream format it is receiving.
///
/// Shared by the broadcaster (announced with the first frame) and by the
/// WebSocket session (a listener that joined later, or asked with get_format),
/// so every producer spells the message the same way.
pub fn format_message(info: &AudioFormatInfo) -> String {
    serde_json::json!({
        "type": "format",
        "data": info
    })
    .to_string()
}

/// Internal messages for broadcaster control
enum BroadcasterCommand {
    RegisterClient(mpsc::Sender<Message>, oneshot::Sender<Uuid>),
    UnregisterClient(Uuid, oneshot::Sender<()>),
}

/// Handle to control the broadcaster
#[derive(Clone)]
pub struct BroadcasterHandle {
    /// Sender for audio frames from the audio engine
    audio_tx: mpsc::Sender<AudioFrame>,
    /// Current listener count
    listener_count: Arc<Mutex<usize>>,
    /// Broadcast channel for listener count updates
    count_tx: broadcast::Sender<usize>,
    /// Channel to send commands to broadcaster
    command_tx: mpsc::Sender<BroadcasterCommand>,
    /// Stream format established by the first mixed frame
    format_info: Arc<Mutex<Option<AudioFormatInfo>>>,
}

impl BroadcasterHandle {
    /// Send mixed audio frame from audio engine
    /// Returns error if channel is full (backpressure - engine should drop frame)
    pub fn send_audio(&self, frame: AudioFrame) -> Result<(), BroadcastError> {
        self.audio_tx
            .try_send(frame)
            .map_err(|_| BroadcastError::ChannelFull)
    }

    /// Get current listener count
    pub async fn listener_count(&self) -> usize {
        *self.listener_count.lock().await
    }

    /// Stream format, once the first frame has established it.
    ///
    /// Lets a session tell a late-joining listener what it is about to receive,
    /// instead of waiting for a format change that never comes.
    pub async fn format_info(&self) -> Option<AudioFormatInfo> {
        self.format_info.lock().await.clone()
    }

    /// Subscribe to listener count changes
    pub fn subscribe_count(&self) -> broadcast::Receiver<usize> {
        self.count_tx.subscribe()
    }

    /// Register a new WebSocket client
    pub async fn register_client(
        &self,
        ws_tx: mpsc::Sender<Message>,
    ) -> Result<Uuid, BroadcastError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.command_tx
            .send(BroadcasterCommand::RegisterClient(ws_tx, resp_tx))
            .await
            .map_err(|_| BroadcastError::NotRunning)?;
        resp_rx.await.map_err(|_| BroadcastError::NotRunning)
    }

    /// Unregister a WebSocket client
    pub async fn unregister_client(&self, id: Uuid) -> Result<(), BroadcastError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.command_tx
            .send(BroadcasterCommand::UnregisterClient(id, resp_tx))
            .await
            .map_err(|_| BroadcastError::NotRunning)?;
        // Wait until the broadcaster has actually removed the client so that
        // `listener_count` reflects the change as soon as this returns.
        resp_rx.await.map_err(|_| BroadcastError::NotRunning)
    }
}

/// Raw audio frame from the audio engine
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved f32 PCM samples (L, R, L, R, ...)
    pub samples: Vec<f32>,
    /// Sample rate (e.g., 48000)
    pub sample_rate: u32,
    /// Number of channels (e.g., 2 for stereo)
    pub channels: u16,
    /// Timestamp for synchronization
    pub timestamp: std::time::Instant,
    /// Monotonically increasing sequence number for gap detection
    pub seq_no: u64,
}

impl AudioFrame {
    /// Create a new audio frame
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: u16, seq_no: u64) -> Self {
        Self {
            samples,
            sample_rate,
            channels,
            timestamp: std::time::Instant::now(),
            seq_no,
        }
    }

    /// Get number of frames (samples per channel)
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// Get duration of this frame in seconds
    pub fn duration(&self) -> f32 {
        self.frame_count() as f32 / self.sample_rate as f32
    }

    /// Convert to a binary WebSocket frame from an already-encoded payload.
    ///
    /// Format: [seq_no: u32 LE][server_ms: u32 LE][payload]
    ///
    /// The sequence number lets clients detect missing frames. `server_ms` is
    /// stream time on the server (milliseconds since the broadcaster started);
    /// clients use it both to drop stale backlog at ingress (a burst that was
    /// queued server-side must not flood the jitter buffer) and to track
    /// arrival jitter for an adaptive target, without any wall-clock sync.
    ///
    /// The payload is one Opus packet. The header stays a fixed 8 bytes: it
    /// was once 8 bytes of a u64 sequence while the player skipped only 4,
    /// shifting every stereo frame by one sample and crackling constantly.
    /// Both halves of this invariant are pinned by tests.
    pub fn to_binary_header(&self, stream_epoch: &std::time::Instant, payload: &[u8]) -> Vec<u8> {
        let server_ms = u32::try_from(stream_epoch.elapsed().as_millis()).unwrap_or(u32::MAX);
        let mut result = Vec::with_capacity(8 + payload.len());
        result.extend_from_slice(&(self.seq_no as u32).to_le_bytes());
        result.extend_from_slice(&server_ms.to_le_bytes());
        result.extend_from_slice(payload);
        result
    }

    /// Encode a *whole* 10 ms block of mixed PCM to one Opus packet.
    ///
    /// The engine's published block is not usually 480 frames long (it follows
    /// the local device's buffer size), so the broadcaster never feeds its own
    /// `samples` to the encoder through this; [`WireEncoder`] reblocks first and
    /// then calls this with an exact frame. Anything else is refused rather
    /// than passed to libopus, which rejects non-standard frame sizes anyway.
    pub fn opus_payload(encoder: &mut OpusEncoder, frame: &AudioFrame) -> Option<Vec<u8>> {
        if frame.channels != WIRE_CHANNELS
            || frame.sample_rate != WIRE_SAMPLE_RATE
            || frame.samples.len() != WIRE_FRAME_FRAMES * WIRE_CHANNELS as usize
        {
            warn!(
                "Dropping frame: expected {}Hz x{}ch, {} samples; got {}Hz x{}ch, {} samples",
                WIRE_SAMPLE_RATE,
                WIRE_CHANNELS,
                WIRE_FRAME_FRAMES * WIRE_CHANNELS as usize,
                frame.sample_rate,
                frame.channels,
                frame.samples.len()
            );
            return None;
        }
        // `max_size` is only the upper bound of the packet, not the frame size:
        // libopus reads the frame size from the input slice and the packet
        // capacity from the output slice. 4000 B covers any legal packet.
        let max_size = 4000;
        let mut packet = vec![0u8; max_size];
        match encoder.encode(&frame.samples, WIRE_FRAME_FRAMES, &mut packet) {
            Ok(len) if len > 0 => Some(packet[..len].to_vec()),
            Ok(_) => {
                warn!("Opus encoder produced an empty packet; frame dropped");
                None
            }
            Err(e) => {
                warn!("Opus encode failed: {:?}; frame dropped", e);
                None
            }
        }
    }

    /// Parse the sequence number from a binary frame (first 4 bytes).
    pub fn seq_no_from_binary(data: &[u8]) -> Option<u32> {
        if data.len() >= 4 {
            Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        } else {
            None
        }
    }

    /// Parse the server stream time from a binary frame (next 4 bytes).
    pub fn server_ms_from_binary(data: &[u8]) -> Option<u32> {
        if data.len() >= 8 {
            Some(u32::from_le_bytes([data[4], data[5], data[6], data[7]]))
        } else {
            None
        }
    }

    /// The Opus payload after the 8-byte header.
    pub fn payload_from_binary(data: &[u8]) -> Option<&[u8]> {
        if data.len() > 8 {
            Some(&data[8..])
        } else {
            None
        }
    }
}

/// Errors that can occur in the broadcaster
#[derive(Debug, thiserror::Error)]
pub enum BroadcastError {
    #[error("Audio channel full - backpressure, frame dropped")]
    ChannelFull,
    #[error("Broadcaster not running")]
    NotRunning,
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Internal client session for broadcasting
struct ClientSession {
    id: Uuid,
    ws_tx: mpsc::Sender<Message>,
}

/// The audio broadcaster - receives from engine, sends to all WebSocket clients
pub struct AudioBroadcaster {
    /// Receiver for audio frames from engine
    audio_rx: mpsc::Receiver<AudioFrame>,
    /// Receiver for control commands
    command_rx: mpsc::Receiver<BroadcasterCommand>,
    /// Connected clients
    clients: Arc<Mutex<Vec<ClientSession>>>,
    /// Listener count for external tracking
    listener_count: Arc<Mutex<usize>>,
    /// Broadcast channel for listener count updates
    count_tx: broadcast::Sender<usize>,
    /// Audio format info (set on first frame)
    format_info: Arc<Mutex<Option<AudioFormatInfo>>>,
    /// Monotonically increasing sequence number for audio frames
    seq_no: AtomicU64,
    /// Epoch for the per-frame `server_ms` stream timestamp
    stream_epoch: std::time::Instant,
    /// PCM -> fixed 10 ms Opus packets, behind a plain mutex: `rusty_opus::OpusEncoder` is
    /// `!Sync`, so holding it directly would make this whole struct `!Sync` and
    /// the `run()` future `!Send`. The lock is uncontended in single-task use.
    wire: std::sync::Mutex<WireEncoder>,
    /// Completed packets waiting for the next sequence number, filled by the
    /// reblocker (one input block can complete more than one frame).
    pending: std::collections::VecDeque<Vec<u8>>,
    /// When the last published block arrived, for gap detection.
    last_block_at: Option<std::time::Instant>,
}

impl AudioBroadcaster {
    /// Create a new broadcaster, returning handle for engine and the broadcaster to run
    pub fn new(config: BroadcasterConfig) -> (BroadcasterHandle, Self) {
        let (audio_tx, audio_rx) = mpsc::channel(config.engine_channel_capacity);
        let (command_tx, command_rx) = mpsc::channel(32);
        let (count_tx, _) = broadcast::channel(16);
        let listener_count = Arc::new(Mutex::new(0));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let format_info = Arc::new(Mutex::new(None));

        let handle = BroadcasterHandle {
            audio_tx,
            listener_count: listener_count.clone(),
            count_tx: count_tx.clone(),
            command_tx,
            format_info: format_info.clone(),
        };

        let broadcaster = Self {
            audio_rx,
            command_rx,
            clients,
            listener_count,
            count_tx,
            format_info,
            seq_no: AtomicU64::new(0),
            stream_epoch: std::time::Instant::now(),
            wire: std::sync::Mutex::new(WireEncoder::new()),
            pending: std::collections::VecDeque::new(),
            last_block_at: None,
        };

        (handle, broadcaster)
    }

    /// Register a new WebSocket client
    async fn register_client(&self, ws_tx: mpsc::Sender<Message>) -> Uuid {
        let id = Uuid::new_v4();
        let session = ClientSession { id, ws_tx };

        let mut clients = self.clients.lock().await;
        clients.push(session);

        // Update listener count
        let count = clients.len();
        let mut listener_count = self.listener_count.lock().await;
        *listener_count = count;
        let _ = self.count_tx.send(count);

        info!("Client connected: {} (total listeners: {})", id, count);
        id
    }

    /// Unregister a WebSocket client
    async fn unregister_client(&self, id: Uuid) {
        let mut clients = self.clients.lock().await;
        clients.retain(|c| c.id != id);

        let count = clients.len();
        let mut listener_count = self.listener_count.lock().await;
        *listener_count = count;
        let _ = self.count_tx.send(count);

        info!("Client disconnected: {} (total listeners: {})", id, count);
    }

    /// Broadcast audio frame to all connected clients.
    ///
    /// The engine hands over mixed PCM in whatever block size the audio device
    /// produced, so the frame is first rebuffered into whole 10 ms frames and
    /// encoded. A block that does not complete a frame is not an error — it is
    /// carried over to the next one — and a block that completes two frames
    /// emits two numbered packets.
    async fn broadcast_frame(&mut self, frame: AudioFrame) {
        if frame.sample_rate != WIRE_SAMPLE_RATE || frame.channels != WIRE_CHANNELS {
            warn!(
                "Dropping block: wire stream is {}Hz x{}ch, got {}Hz x{}ch",
                WIRE_SAMPLE_RATE, WIRE_CHANNELS, frame.sample_rate, frame.channels
            );
            return;
        }

        let now = frame.timestamp;
        let mut packets = Vec::new();
        {
            let mut wire = self.wire.lock().expect("Opus encoder mutex poisoned");
            // A long silence means nothing was playing: the mixer stops
            // publishing, so the reblocker still holds a fragment of pre-pause
            // audio. Gluing it to the next burst would put it inside a packet —
            // an audible click at every start. Drop it and start on a fresh
            // frame instead.
            if let Some(last) = self.last_block_at {
                if now.saturating_duration_since(last) > STREAM_RESTART_GAP {
                    debug!(
                        "Stream resumed after {:?}; dropping the partial frame",
                        now.saturating_duration_since(last)
                    );
                    wire.drop_partial_frame();
                }
            }
            wire.push(&frame.samples, &mut packets);
        }
        self.last_block_at = Some(now);

        if packets.is_empty() {
            return;
        }
        self.send_packets(packets, frame.timestamp).await;

        // The first packet of the session is what establishes the stream
        // format. It is announced only once the stream really starts, so a
        // listener that connects before the first play still gets the format
        // from its session (see `format_info`) instead of a stale announcement.
        let announcement = {
            let mut format_info = self.format_info.lock().await;
            match format_info.as_ref() {
                Some(_) => None,
                None => {
                    let info = AudioFormatInfo::wire();
                    let message = format_message(&info);
                    *format_info = Some(info);
                    Some(message)
                }
            }
        };
        if let Some(text) = announcement {
            self.deliver(Message::Text(text)).await;
        }
    }

    /// Number and send encoded packets to every listener.
    async fn send_packets(&mut self, packets: Vec<Vec<u8>>, timestamp: std::time::Instant) {
        self.pending.extend(packets);
        while let Some(payload) = self.pending.pop_front() {
            let seq = self.seq_no.fetch_add(1, Ordering::Relaxed);
            let header = AudioFrame {
                samples: Vec::new(),
                sample_rate: WIRE_SAMPLE_RATE,
                channels: WIRE_CHANNELS,
                timestamp,
                seq_no: seq,
            };
            let binary_data = header.to_binary_header(&self.stream_epoch, &payload);
            self.deliver(Message::Binary(binary_data)).await;
        }
    }

    /// Whether the reblocker holds a partial frame.
    fn has_partial_frame(&self) -> bool {
        self.wire
            .lock()
            .map(|wire| wire.buffered_samples() > 0)
            .unwrap_or(false)
    }

    /// The stream paused: send the tail of the last sound padded to a frame.
    async fn flush_partial_frame(&mut self) {
        let mut packets = Vec::new();
        self.wire
            .lock()
            .expect("Opus encoder mutex poisoned")
            .flush(&mut packets);
        if !packets.is_empty() {
            self.send_packets(packets, std::time::Instant::now()).await;
        }
    }

    /// Queue one message for every connected client, pruning dead sessions.
    async fn deliver(&mut self, message: Message) {
        let mut clients = self.clients.lock().await;
        let mut disconnected = Vec::new();

        for client in clients.iter_mut() {
            // If the per-client buffer is full, skip this frame for that client
            // but keep the connection open. Only a closed channel means the
            // WebSocket task is really gone. Treating a full buffer as a
            // disconnect caused reconnect storms whenever the network
            // hiccuped; the client's sequence-gap handling covers the hole.
            match client.ws_tx.try_send(message.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("Client {} send buffer full; dropping a frame", client.id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    disconnected.push(client.id);
                }
            }
        }

        if !disconnected.is_empty() {
            for id in disconnected {
                clients.retain(|c| c.id != id);
                warn!("Dropped disconnected client: {}", id);
            }

            let count = clients.len();
            let mut listener_count = self.listener_count.lock().await;
            *listener_count = count;
            let _ = self.count_tx.send(count);
        }
    }

    /// Run the broadcaster main loop
    pub async fn run(mut self) {
        info!("Audio broadcaster started");

        loop {
            tokio::select! {
                // Handle audio frames from engine
                Some(frame) = self.audio_rx.recv() => {
                    self.broadcast_frame(frame).await;
                }
                // Nothing new for a while and a partial frame is waiting: the
                // sound ended, so send its tail instead of holding it.
                _ = tokio::time::sleep(STREAM_FLUSH_DELAY), if self.has_partial_frame() => {
                    self.flush_partial_frame().await;
                }
                // Handle control commands
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        BroadcasterCommand::RegisterClient(ws_tx, resp_tx) => {
                            let id = self.register_client(ws_tx).await;
                            let _ = resp_tx.send(id);
                        }
                        BroadcasterCommand::UnregisterClient(id, resp_tx) => {
                            self.unregister_client(id).await;
                            let _ = resp_tx.send(());
                        }
                    }
                }
                else => break, // Both channels closed
            }
        }

        info!("Audio broadcaster stopped (channels closed)");
    }
}

/// Helper: create a test audio frame for testing
#[cfg(test)]
pub fn create_test_frame() -> AudioFrame {
    // 48000 Hz, stereo, 10ms frame = 480 frames = 960 samples
    let samples: Vec<f32> = (0..960).map(|i| (i as f32 * 0.01).sin() * 0.1).collect();
    AudioFrame::new(samples, 48000, 2, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_broadcaster_basic() {
        let config = BroadcasterConfig::default();
        let (handle, broadcaster) = AudioBroadcaster::new(config);

        // Spawn broadcaster
        let broadcaster_task = tokio::spawn(broadcaster.run());

        // Send a test frame
        let frame = create_test_frame();
        assert!(handle.send_audio(frame).is_ok());

        // Give it time to process
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Shutdown
        drop(handle);
        let _ = timeout(Duration::from_secs(1), broadcaster_task).await;
    }

    /// Encode a test frame the same way the broadcaster's loop does.
    #[cfg(test)]
    fn encode_test_frame(frame: &AudioFrame) -> Vec<u8> {
        let mut encoder = wire_encoder();
        let payload = AudioFrame::opus_payload(&mut encoder, frame)
            .expect("encoding a well-formed test frame");
        frame.to_binary_header(&std::time::Instant::now(), &payload)
    }

    #[test]
    fn test_audio_frame_properties() {
        let frame = create_test_frame();
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.frame_count(), 480);
        assert!((frame.duration() - 0.01).abs() < 0.001);
    }

    #[test]
    fn test_opus_binary_format() {
        let mut frame = create_test_frame();
        frame.seq_no = 0x1_0000_0005;
        let binary = encode_test_frame(&frame);

        // The header must stay 8 bytes: 4 for the sequence number (even when
        // the internal counter is u64) and 4 for the server stream time. The
        // Opus payload starts immediately after it.
        assert_eq!(AudioFrame::seq_no_from_binary(&binary), Some(5));
        assert!(AudioFrame::server_ms_from_binary(&binary).is_some());
        let payload = AudioFrame::payload_from_binary(&binary).expect("payload present");
        assert!(
            !payload.is_empty() && payload.len() < frame.samples.len(),
            "Opus payload must be non-empty and far smaller than PCM"
        );
    }

    #[test]
    fn test_opus_roundtrip_quality() {
        let frame = create_test_frame();
        let binary = encode_test_frame(&frame);
        let payload = AudioFrame::payload_from_binary(&binary).expect("payload present");

        let mut decoder =
            rusty_opus::OpusDecoder::new(WIRE_SAMPLE_RATE as i32, WIRE_CHANNELS as usize)
                .expect("Opus decoder initialization");
        let mut out = vec![0f32; frame.samples.len()];
        decoder
            .decode(payload, WIRE_FRAME_FRAMES, &mut out)
            .expect("decoding a freshly encoded packet");

        // Sine at the wire bitrate must survive: correlated, sane level.
        // Opus at 64 kbit/s is lossy, so allow a wider band than PCM.
        let rms_orig: f32 =
            (frame.samples.iter().map(|s| s * s).sum::<f32>() / frame.samples.len() as f32).sqrt();
        let rms_out: f32 = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(
            rms_out > rms_orig * 0.4 && rms_out < rms_orig * 1.6,
            "roundtrip RMS drifted: {rms_out} vs {rms_orig}"
        );
    }

    #[test]
    fn test_server_ms_is_monotonic_and_parsable() {
        let epoch = std::time::Instant::now();
        let frame = create_test_frame();
        let mut encoder = wire_encoder();
        let payload = AudioFrame::opus_payload(&mut encoder, &frame).expect("encode ok");
        let first = frame.to_binary_header(&epoch, &payload);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = frame.to_binary_header(&epoch, &payload);

        let ms_0 = AudioFrame::server_ms_from_binary(&first).expect("header present");
        let ms_1 = AudioFrame::server_ms_from_binary(&second).expect("header present");
        assert!(ms_0 <= ms_1, "server_ms must be monotonic");
        assert!(ms_0 < 10_000, "fresh epoch must give a small stream time");
    }

    #[tokio::test]
    async fn test_backpressure() {
        let config = BroadcasterConfig {
            engine_channel_capacity: 2,
        };
        let (handle, broadcaster) = AudioBroadcaster::new(config);
        let _task = tokio::spawn(broadcaster.run());

        // Fill the channel
        assert!(handle.send_audio(create_test_frame()).is_ok());
        assert!(handle.send_audio(create_test_frame()).is_ok());
        // Third should fail (backpressure)
        assert!(matches!(
            handle.send_audio(create_test_frame()),
            Err(BroadcastError::ChannelFull)
        ));

        drop(handle);
    }

    /// The engine publishes whatever the audio device asked for. A block that
    /// is not a multiple of 480 frames must still put whole 10 ms packets on
    /// the wire instead of being dropped (silence) or fed to libopus (which
    /// rejects the frame size).

    #[tokio::test]
    async fn odd_sized_engine_blocks_still_reach_listeners() {
        let (handle, broadcaster) = AudioBroadcaster::new(BroadcasterConfig::default());
        let _task = tokio::spawn(broadcaster.run());

        let (tx, mut rx) = mpsc::channel(64);
        handle.register_client(tx).await.expect("register");

        // 441 frames per block: a 44.1 kHz device buffer, a very common size
        // that is never a whole 10 ms frame at 48 kHz.
        for _ in 0..8 {
            let block = vec![0.25f32; 441 * 2];
            handle
                .send_audio(AudioFrame::new(block, WIRE_SAMPLE_RATE, WIRE_CHANNELS, 0))
                .expect("queue block");
        }
        // Well past STREAM_FLUSH_DELAY, so the padded tail has been sent on
        // every platform's timer resolution.
        tokio::time::sleep(STREAM_FLUSH_DELAY * 5).await;

        let mut packets = 0;
        while let Ok(message) = rx.try_recv() {
            if let Message::Binary(data) = message {
                assert!(data.len() > 8, "header plus payload");
                packets += 1;
            }
        }
        // 8 x 441 = 3528 frames is 7.35 wire frames: 7 whole packets, none
        // lost at a block boundary, plus the 0.35 tail padded and flushed once
        // the engine goes quiet.
        assert_eq!(
            packets, 8,
            "3528 frames must reblock into 7 whole packets plus the flushed tail"
        );

        drop(handle);
    }

    /// A pause must not glue a pre-pause fragment onto the next burst.

    #[tokio::test]
    async fn pause_flushes_the_partial_frame_instead_of_gluing_it() {
        let (handle, broadcaster) = AudioBroadcaster::new(BroadcasterConfig::default());
        let _task = tokio::spawn(broadcaster.run());

        let (tx, mut rx) = mpsc::channel(64);
        handle.register_client(tx).await.expect("register");

        let count_binary = |rx: &mut mpsc::Receiver<Message>| {
            let mut binary = 0;
            while let Ok(message) = rx.try_recv() {
                if let Message::Binary(_) = message {
                    binary += 1;
                }
            }
            binary
        };

        // Half a frame (the tail of a sound), then a pause.
        handle
            .send_audio(AudioFrame::new(
                vec![0.5f32; 240 * 2],
                WIRE_SAMPLE_RATE,
                WIRE_CHANNELS,
                0,
            ))
            .expect("queue partial block");
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The tail went out during the pause, padded to one packet; it used
        // to sit in the reblocker until the next sound (and then be dropped),
        // so the end of a one-shot never reached the listeners.
        assert_eq!(
            count_binary(&mut rx),
            1,
            "the tail must be sent during the pause"
        );

        // One whole frame after the pause is exactly one packet: nothing from
        // before the pause is glued onto it.
        handle
            .send_audio(AudioFrame::new(
                vec![0.5f32; WIRE_FRAME_FRAMES * 2],
                WIRE_SAMPLE_RATE,
                WIRE_CHANNELS,
                0,
            ))
            .expect("queue whole block");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(count_binary(&mut rx), 1);

        drop(handle);
    }

    #[tokio::test]
    async fn test_register_unregister_client() {
        let config = BroadcasterConfig::default();
        let (handle, broadcaster) = AudioBroadcaster::new(config);
        let _task = tokio::spawn(broadcaster.run());

        // Create a dummy message channel
        let (tx, _rx) = mpsc::channel(10);

        // Register client
        let id = handle.register_client(tx).await.unwrap();
        assert_eq!(handle.listener_count().await, 1);

        // Unregister client
        handle.unregister_client(id).await.unwrap();
        assert_eq!(handle.listener_count().await, 0);

        drop(handle);
    }
}
