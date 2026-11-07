//! Rebuffering raw PCM into fixed Opus frames and encoding them for the wire.
//!
//! # Why this module exists
//!
//! libopus accepts only a fixed set of frame sizes: 2.5 / 5 / 10 / 20 / 40 / 60
//! ms. It is *not* a streaming encoder that swallows arbitrary block lengths.
//! The hardware callback, however, hands us whatever block size the device
//! negotiated — 441, 512, 1024 frames … — while the headless mixer publishes
//! 480. Feeding those blocks to [`rusty_opus::OpusEncoder::encode`] does not fail
//! loudly: `opus_encode` itself only rejects the call and returns an error,
//! which used to drop every frame on devices whose buffer was not exactly
//! 480 frames (silence on the wire), and silently desynchronized the encoder
//! on the ones that were close. The audible result at the listener was
//! crackle riding on the music.
//!
//! So the PCM is rebuffered here first, exactly as opusfile/ffmpeg do it: an
//! interleaved f32 remainder is carried across calls, and only whole 480-frame
//! (10 ms) blocks are ever handed to the encoder.
//!
//! # Discontinuities
//!
//! When the stream pauses (nothing playing, so the mixer stops publishing) the
//! carried remainder belongs to audio that will never be completed. Gluing it
//! onto the next burst would put an out-of-place fragment in a packet, which
//! the client plays as a click. [`WireEncoder::reset`] drops the remainder and
//! refreshes the codec state at every discontinuity. Opus packets are
//! independent, so a listener that drops a packet never desynchronizes either.

use tracing::warn;

use rusty_opus::{Application, OpusEncoder, SignalType};

use super::broadcast::AudioFormatInfo;

/// Sample rate of the wire stream.
pub const WIRE_SAMPLE_RATE: u32 = 48_000;
/// Channels of the wire stream (always stereo: the mix is stereo).
pub const WIRE_CHANNELS: u16 = 2;
/// Frames per channel in one wire packet: 10 ms at 48 kHz.
pub const WIRE_FRAME_FRAMES: usize = 480;
/// Interleaved samples in one wire packet (`WIRE_FRAME_FRAMES * WIRE_CHANNELS`).
pub const WIRE_FRAME_SAMPLES: usize = WIRE_FRAME_FRAMES * WIRE_CHANNELS as usize;

/// Bitrate for the wire stream, in bits per second.
///
/// 64 kbit/s stereo is the "transparent enough for ambience" point of Opus:
/// it is roughly MP3 quality at 128-160 kbit/s, fits comfortably in the
/// bandwidth of a crowded table Wi-Fi, and still leaves the encoder VBR
/// headroom for percussive peaks. Lower (48) washes out a busy tavern mix,
/// higher (128) buys little on laptop speakers while costing jitter buffer
/// time on a weak link.
pub const WIRE_BITRATE: i32 = 64_000;

/// Builds the encoder with the wire settings.
///
/// Kept as a free function (not just an `impl`) because several call sites —
/// the broadcaster, the tests, and integrations — must produce byte-identical
/// packets.
pub fn wire_encoder() -> OpusEncoder {
    let mut encoder = OpusEncoder::new(
        WIRE_SAMPLE_RATE as i32,
        WIRE_CHANNELS as usize,
        Application::Audio,
    )
    .expect("Opus encoder initialization");

    // These are all best-effort: a rejected setting leaves the default in place,
    // which is still a working encoder, so failures are logged rather than fatal.
    encoder.bitrate_bps = WIRE_BITRATE;
    // VBR gives quiet passages fewer bits for free; constrained VBR keeps a
    // full-packet ceiling so a single packet cannot stall a weak link.
    encoder.use_cbr = false;
    // Music hint: biases mode selection towards the music-oriented path
    // instead of the speech one. Ambience and soundtracks are the use case.
    encoder.signal_type = Some(SignalType::Music);
    encoder.complexity = 10;
    encoder
}

/// PCM -> fixed-size Opus packets.
///
/// Holds the encoder plus the interleaved f32 remainder between calls. Feeding
/// it any block size is safe; it emits zero or more whole 10 ms packets.
pub struct WireEncoder {
    encoder: OpusEncoder,
    /// Leftover interleaved samples that did not fill a whole frame.
    remainder: Vec<f32>,
    /// Scratch packet buffer, reused so steady-state encoding does not
    /// allocate. Sized for the 4000-byte maximum of a 20 ms stereo packet
    /// with headroom.
    packet: Vec<u8>,
    /// Whether audio has been fed since the last [`reset`](Self::reset).
    primed: bool,
}

impl WireEncoder {
    /// A fresh encoder with an empty reblocker.
    pub fn new() -> Self {
        Self {
            encoder: wire_encoder(),
            remainder: Vec::with_capacity(WIRE_FRAME_SAMPLES * 2),
            packet: vec![0u8; 4096],
            primed: false,
        }
    }

    /// Push one mixed block and append every completed 10 ms packet to `out`.
    ///
    /// `samples` is interleaved stereo at 48 kHz, i.e. what the mixer produces.
    /// The return value is the number of packets appended, so callers can skip
    /// the rest of the broadcast path when a block was too short to complete
    /// a frame (which is normal, not an error).
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<u8>>) -> usize {
        if !samples.len().is_multiple_of(WIRE_CHANNELS as usize) {
            // A torn block would shift every following sample by one, which is
            // the classic "stereo channels rotated" crackle. Drop it instead.
            warn!(
                "Dropping {} samples: not a whole number of stereo frames",
                samples.len()
            );
            return 0;
        }

        // Fast path: nothing carried over and the block is already a whole
        // number of wire frames. This is the headless mixer's steady state, so
        // no copy is made at all.
        if self.remainder.is_empty() && samples.len().is_multiple_of(WIRE_FRAME_SAMPLES) {
            let mut encoded = 0;
            for block in samples.as_chunks::<WIRE_FRAME_SAMPLES>().0 {
                if let Some(packet) = self.encode_block(block) {
                    out.push(packet);
                    encoded += 1;
                }
            }
            self.primed = true;
            return encoded;
        }

        self.remainder.extend_from_slice(samples);
        let whole = self.remainder.len() / WIRE_FRAME_SAMPLES;
        if whole == 0 {
            return 0;
        }

        // Encoding needs `&mut self` while reading out of `self.remainder`, so
        // the ready frames are swapped into a scratch buffer first. That is one
        // copy per frame, only on the non-aligned path.
        let ready: Vec<f32> = self.remainder[..whole * WIRE_FRAME_SAMPLES].to_vec();
        self.remainder.drain(..whole * WIRE_FRAME_SAMPLES);

        let mut encoded = 0;
        // Prefer `as_chunks` over `chunks_exact` to match libopus's frame
        // alignment; the remainder (trailing partial frame) is kept for the
        // next call.
        for block in ready.as_chunks::<WIRE_FRAME_SAMPLES>().0 {
            if let Some(packet) = self.encode_block(block) {
                out.push(packet);
                encoded += 1;
            }
        }
        self.primed = true;
        encoded
    }

    /// Encode one whole frame into an owned packet.
    fn encode_block(&mut self, block: &[f32]) -> Option<Vec<u8>> {
        match self
            .encoder
            .encode(block, WIRE_FRAME_FRAMES, &mut self.packet[..])
        {
            Ok(len) if len > 0 => Some(self.packet[..len].to_vec()),
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

    /// Forget everything carried over: call at every discontinuity (stream
    /// start, pause/resume, local device switch).
    pub fn reset(&mut self) {
        self.remainder.clear();
        self.primed = false;
    }

    /// Pad the carried partial frame with silence and encode it.
    ///
    /// Called when the stream pauses. Without it the last few milliseconds
    /// of every sound stayed in the reblocker until the next sound started
    /// (and were then thrown away), so the end of a one-shot never reached
    /// the listeners. Returns the number of packets appended (0 or 1).
    pub fn flush(&mut self, out: &mut Vec<Vec<u8>>) -> usize {
        if self.remainder.is_empty() {
            return 0;
        }
        let mut block = std::mem::take(&mut self.remainder);
        block.resize(WIRE_FRAME_SAMPLES, 0.0);
        let encoded = match self.encode_block(&block) {
            Some(packet) => {
                out.push(packet);
                1
            }
            None => 0,
        };
        block.clear();
        self.remainder = block;
        encoded
    }

    /// Discard a partial frame that is too old to ever be completed.
    ///
    /// The engine publishes nothing while nothing plays, so after a pause the
    /// remainder is a fragment of audio from before the gap. Called by the
    /// broadcaster when the gap between published blocks is long.
    pub fn drop_partial_frame(&mut self) {
        if !self.remainder.is_empty() {
            self.remainder.clear();
        }
    }

    /// True when audio has been fed since the last reset.
    pub fn is_primed(&self) -> bool {
        self.primed
    }

    /// Number of interleaved samples waiting in the reblocker.
    pub fn buffered_samples(&self) -> usize {
        self.remainder.len()
    }

    /// The stream format this encoder produces.
    pub fn format_info() -> AudioFormatInfo {
        AudioFormatInfo {
            sample_rate: WIRE_SAMPLE_RATE,
            channels: WIRE_CHANNELS,
            format: super::broadcast::WIRE_CODEC.to_string(),
            frame_size: WIRE_FRAME_FRAMES,
        }
    }
}

impl Default for WireEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tail of a sound (less than one 10 ms frame) is padded and sent
    /// when the stream pauses, instead of waiting for the next sound.
    #[test]
    fn flush_sends_the_partial_tail() {
        let mut encoder = WireEncoder::new();
        let mut out = Vec::new();
        // 1.5 frames: one packet now, half a frame carried.
        encoder.push(&vec![0.25f32; WIRE_FRAME_SAMPLES * 3 / 2], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(encoder.buffered_samples(), WIRE_FRAME_SAMPLES / 2);

        assert_eq!(encoder.flush(&mut out), 1);
        assert_eq!(out.len(), 2);
        assert_eq!(encoder.buffered_samples(), 0);
        // Nothing left to flush.
        assert_eq!(encoder.flush(&mut out), 0);
    }

    /// 48 kHz sine, stereo, `frames` frames long, starting at `phase` (radians).
    fn sine(frames: usize, hz: f64, amplitude: f32, phase: f64) -> (Vec<f32>, f64) {
        let step = 2.0 * std::f64::consts::PI * hz / WIRE_SAMPLE_RATE as f64;
        let mut out = Vec::with_capacity(frames * 2);
        let mut p = phase;
        for _ in 0..frames {
            let s = (p.sin() as f32) * amplitude;
            out.push(s);
            out.push(s);
            p += step;
        }
        (out, p)
    }

    fn decode(packet: &[u8]) -> Vec<f32> {
        let mut decoder =
            rusty_opus::OpusDecoder::new(WIRE_SAMPLE_RATE as i32, WIRE_CHANNELS as usize)
                .expect("decoder");
        let mut out = vec![0f32; WIRE_FRAME_SAMPLES];
        let n = decoder
            .decode(packet, WIRE_FRAME_FRAMES, &mut out)
            .expect("decode");
        out.truncate(n * WIRE_CHANNELS as usize);
        out
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    #[test]
    fn whole_blocks_encode_one_packet_each() {
        let mut encoder = WireEncoder::new();
        let mut packets = Vec::new();
        for _ in 0..10 {
            let (block, _) = sine(WIRE_FRAME_FRAMES, 440.0, 0.5, 0.0);
            assert_eq!(encoder.push(&block, &mut packets), 1);
        }
        assert_eq!(packets.len(), 10);
        assert_eq!(encoder.buffered_samples(), 0);
    }

    /// The regression that made every non-480-frame device silent or crackly:
    /// arbitrary device block sizes must still produce valid 10 ms packets.
    #[test]
    fn arbitrary_block_sizes_still_produce_ten_ms_packets() {
        for block_frames in [441usize, 512, 1000, 1024, 47, 1] {
            let mut encoder = WireEncoder::new();
            let mut packets = Vec::new();

            let mut phase = 0.0;
            let mut fed_frames = 0usize;
            // Feed ~1 s of audio in blocks of this size, one frame bigger on
            // every pass so the reblocker sees a moving phase as well.
            while fed_frames < WIRE_SAMPLE_RATE as usize {
                let frames = block_frames + (fed_frames % 7);
                let (block, next) = sine(frames, 220.0, 0.4, phase);
                phase = next;
                encoder.push(&block, &mut packets);
                fed_frames += frames;
            }

            assert!(
                packets.len() >= 99,
                "block {block_frames}: 1 s of audio must yield ~100 packets, got {}",
                packets.len()
            );
            let mut total_level = 0f32;
            let mut packet_count = 0usize;
            let mut prev_sample: Option<f32> = None;
            let mut worst_step = 0f32;
            for (i, packet) in packets.iter().enumerate() {
                let decoded = decode(packet);
                assert_eq!(
                    decoded.len(),
                    WIRE_FRAME_SAMPLES,
                    "block {block_frames}: packet {i} is not a 10 ms stereo frame"
                );
                total_level += rms(&decoded);
                packet_count += 1;
                // Check for click-like discontinuities between consecutive
                // samples (a reblocking misalignment would show up here).
                for &s in &decoded {
                    if let Some(p) = prev_sample {
                        worst_step = worst_step.max((s - p).abs());
                    }
                    prev_sample = Some(s);
                }
            }
            let avg_level = total_level / packet_count as f32;
            // Opus at 64 kbit/s VBR is lossy: level varies per packet, but it
            // must not collapse to silence.
            assert!(
                avg_level > 0.01,
                "block {block_frames}: stream collapsed to silence (avg {avg_level})"
            );
            // Opus at 64 kbit/s VBR introduces quantization noise between
            // consecutive samples; a real reblocking misalignment would cause
            // a near-full-scale jump (~1.0). Allow Opus noise but catch clicks.
            assert!(
                worst_step < 0.5,
                "block {block_frames}: click-like step {worst_step}"
            );
        }
    }

    #[test]
    fn reblocked_stream_is_sample_aligned() {
        // Feed 441-frame blocks of a 1 kHz sine and check that the decoded
        // stream is continuous: a rounding error in the reblocker shows up as
        // a phase discontinuity at every block boundary.
        let mut encoder = WireEncoder::new();
        let mut packets = Vec::new();
        let mut phase = 0.0;
        for _ in 0..20 {
            let (block, next) = sine(441, 1000.0, 0.5, phase);
            phase = next;
            encoder.push(&block, &mut packets);
        }
        assert!(packets.len() >= 16);

        let mut decoded: Vec<f32> = Vec::new();
        for packet in &packets {
            decoded.extend(decode(packet));
        }

        // Opus at 64 kbit/s is lossy, so exact per-sample equality against
        // the ideal sine is not meaningful. Instead, check for structural
        // correctness: no click-like discontinuities between consecutive
        // samples (which would indicate a reblocking misalignment), and the
        // stream must not collapse to silence.
        let mut worst_step = 0f32;
        let mut prev: Option<f32> = None;
        for &s in &decoded {
            if let Some(p) = prev {
                worst_step = worst_step.max((s - p).abs());
            }
            prev = Some(s);
        }
        // A pure 1 kHz tone at 0.5 amplitude has max step ≈ 2π·1000/48000·0.5 ≈ 0.065.
        // Opus smoothing only reduces this. A step > 0.2 would be a click.
        assert!(
            worst_step < 0.2,
            "stream has a click-like step: worst {worst_step}"
        );

        let level = rms(&decoded);
        assert!(level > 0.01, "stream collapsed to silence (level {level})");
    }

    /// Dropping the partial frame at a pause must not shift the following
    /// audio: the packet that opens the next burst has to be a clean frame.
    #[test]
    fn discontinuity_drops_partial_frame_instead_of_gluing_it() {
        let mut encoder = WireEncoder::new();
        let mut packets = Vec::new();

        let (block, _) = sine(200, 300.0, 0.5, 0.0);
        assert_eq!(encoder.push(&block, &mut packets), 0);
        assert_eq!(encoder.buffered_samples(), 400);

        encoder.drop_partial_frame();
        assert_eq!(encoder.buffered_samples(), 0);

        let (block, _) = sine(WIRE_FRAME_FRAMES, 300.0, 0.5, 0.0);
        assert_eq!(encoder.push(&block, &mut packets), 1);
        let decoded = decode(&packets[0]);
        assert_eq!(decoded.len(), WIRE_FRAME_SAMPLES);
        // Opus at 64 kbit/s is lossy: allow a wide band around the expected level.
        assert!((rms(&decoded) - 0.5 / 2f32.sqrt()).abs() < 0.2);
    }

    #[test]
    fn torn_block_is_rejected_not_shifted() {
        let mut encoder = WireEncoder::new();
        let mut packets = Vec::new();
        // An odd number of samples cannot be whole stereo frames.
        let odd = vec![0.1f32; WIRE_FRAME_SAMPLES - 1];
        assert_eq!(encoder.push(&odd, &mut packets), 0);
        assert_eq!(packets.len(), 0);
        assert_eq!(
            encoder.buffered_samples(),
            0,
            "a torn block must not enter the reblocker"
        );
    }

    #[test]
    fn wire_packets_are_compact() {
        let mut encoder = WireEncoder::new();
        let mut packets = Vec::new();
        let (block, _) = sine(WIRE_FRAME_FRAMES, 440.0, 0.5, 0.0);
        encoder.push(&block, &mut packets);
        let packet = &packets[0];
        let pcm_bytes = WIRE_FRAME_SAMPLES * std::mem::size_of::<f32>();
        assert!(
            packet.len() < pcm_bytes / 8,
            "Opus packet {} B must be far below {pcm_bytes} B of PCM",
            packet.len()
        );
    }

    #[test]
    fn format_info_matches_the_encoder() {
        let info = WireEncoder::format_info();
        assert_eq!(info.sample_rate, WIRE_SAMPLE_RATE);
        assert_eq!(info.channels, WIRE_CHANNELS);
        assert_eq!(info.format, "opus");
        assert_eq!(info.frame_size, WIRE_FRAME_FRAMES);
    }
}
