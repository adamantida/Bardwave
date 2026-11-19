//! Audio decoding using Symphonia
//! Supports MP3, WAV, OGG, FLAC, OPUS formats

use std::fs::File;
use std::path::Path;

use anyhow::Result;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CodecParameters, Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use thiserror::Error;

/// Target sample rate for all decoded audio
pub const TARGET_SAMPLE_RATE: u32 = 48000;
/// Target channels for all decoded audio (stereo)
pub const TARGET_CHANNELS: u32 = 2;

/// Errors that can occur during audio decoding
#[derive(Error, Debug)]
pub enum DecoderError {
    #[error("Failed to open file: {0}")]
    FileOpen(#[from] std::io::Error),
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("No audio tracks found in file")]
    NoAudioTracks,
    #[error("Decoder error: {0}")]
    Decoder(#[from] symphonia::core::errors::Error),
    #[error("Resampling not implemented for this configuration")]
    ResampleUnsupported,
    #[error("Channel conversion not supported: {0}")]
    ChannelConversion(String),
}

/// Decoded audio data with metadata
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// PCM samples as f32, interleaved stereo (L, R, L, R, ...)
    pub samples: Vec<f32>,
    /// Sample rate (always TARGET_SAMPLE_RATE after decoding)
    pub sample_rate: u32,
    /// Number of channels (always TARGET_CHANNELS after decoding)
    pub channels: u32,
    /// Duration in seconds
    pub duration: f32,
    /// Number of frames (samples per channel)
    pub frames: usize,
}

impl DecodedAudio {
    /// Create a new DecodedAudio from raw samples
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: u32) -> Self {
        let frames = samples.len() / channels as usize;
        let duration = frames as f32 / sample_rate as f32;
        Self {
            samples,
            sample_rate,
            channels,
            duration,
            frames,
        }
    }

    /// Get the total number of samples (interleaved)
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Check if the audio is empty
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Get a slice of samples for a specific time range
    pub fn slice(&self, start_frame: usize, end_frame: usize) -> Vec<f32> {
        let start = start_frame * self.channels as usize;
        let end = end_frame * self.channels as usize;
        self.samples[start..end.min(self.samples.len())].to_vec()
    }
}

/// Tags of a file that a playlist or the console shows: what Symphonia read
/// from ID3 / Vorbis comments / MP4 atoms. Every field is optional: plenty of
/// files carry no tags at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl TrackMeta {
    /// True when no tag was found.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.artist.is_none() && self.album.is_none()
    }

    /// Take the tags of one metadata revision, keeping what is already set.
    fn absorb(&mut self, revision: &MetadataRevision) {
        fn clean(value: String) -> Option<String> {
            // RIFF INFO strings come NUL-terminated (and padded to even size).
            let value = value
                .trim_matches(|c: char| c.is_whitespace() || c == '\0')
                .to_string();
            (!value.is_empty()).then_some(value)
        }
        let mut album_artist = None;
        for tag in revision.tags() {
            let slot = match tag.std_key {
                Some(StandardTagKey::TrackTitle) => &mut self.title,
                Some(StandardTagKey::Artist) => &mut self.artist,
                Some(StandardTagKey::Album) => &mut self.album,
                Some(StandardTagKey::AlbumArtist) => {
                    album_artist = clean(tag.value.to_string());
                    continue;
                }
                _ => continue,
            };
            if slot.is_none() {
                *slot = clean(tag.value.to_string());
            }
        }
        if self.artist.is_none() {
            self.artist = album_artist;
        }
    }
}

/// What can be learned about a file without decoding it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileProbe {
    /// Tags (title / artist / album).
    pub meta: TrackMeta,
    /// Duration from the container header. `None` for formats that do not
    /// carry it (an MP3 without a Xing header): only a full decode knows.
    pub duration_secs: Option<f64>,
    /// Sample rate declared by the container.
    pub sample_rate: Option<u32>,
}

/// A demuxer + codec pair that yields decoded packets as interleaved f32 in
/// the file's own channel layout and rate.
///
/// This is the one place that knows how to open a file and walk its packets:
/// [`AudioDecoder::decode_file`] (whole file into memory) and the streaming
/// source both sit on top of it, so they cannot disagree about what a file
/// contains.
pub(crate) struct PacketReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    codec_params: CodecParameters,
    /// Channel count of the decoded buffers (set by the first one).
    pub(crate) channels: Option<usize>,
    /// Sample rate of the decoded buffers (set by the first one).
    pub(crate) sample_rate: u32,
    pub(crate) meta: TrackMeta,
    /// Frames (at the file's rate) still to drop after an inexact seek.
    skip_frames: u64,
}

impl PacketReader {
    pub(crate) fn open(path: &Path) -> Result<Self, DecoderError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // The extension is a strong hint for the prober: without it, MP3 files
        // with a large ID3 tag or no Xing header are often not recognised.
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let meta_opts = MetadataOptions::default();
        let fmt_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };

        let mut probed = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &meta_opts)
            .map_err(|e| DecoderError::UnsupportedFormat(e.to_string()))?;

        let mut meta = TrackMeta::default();
        if let Some(md) = probed.metadata.get() {
            if let Some(rev) = md.current() {
                meta.absorb(rev);
            }
        }
        let mut format = probed.format;
        if let Some(rev) = format.metadata().current() {
            meta.absorb(rev);
        }

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(DecoderError::NoAudioTracks)?;
        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(DecoderError::Decoder)?;

        // The rate and channel count are taken from the *decoded* buffers, not
        // from the container header: MP3/AAC headers often omit the channel
        // layout (it used to default to stereo, so a mono file played twice
        // as fast with its samples split between the ears), and HE-AAC
        // declares half its real output rate.
        let sample_rate = codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);
        Ok(Self {
            format,
            decoder,
            track_id,
            codec_params,
            channels: None,
            sample_rate,
            meta,
            skip_frames: 0,
        })
    }

    /// Duration according to the container header, in seconds.
    pub(crate) fn duration_secs(&self) -> Option<f64> {
        let frames = self.codec_params.n_frames?;
        match (self.codec_params.time_base, self.codec_params.sample_rate) {
            (Some(tb), _) => {
                let t = tb.calc_time(frames);
                Some(t.seconds as f64 + t.frac)
            }
            (None, Some(rate)) if rate > 0 => Some(frames as f64 / rate as f64),
            _ => None,
        }
    }

    pub(crate) fn declared_sample_rate(&self) -> Option<u32> {
        self.codec_params.sample_rate
    }

    /// Decode the next packet: interleaved f32 in the file's channel layout.
    /// `Ok(None)` is the end of the file.
    pub(crate) fn next_chunk(&mut self) -> Result<Option<Vec<f32>>, DecoderError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                // A chained stream (e.g. concatenated Ogg) ends the track here.
                Err(symphonia::core::errors::Error::ResetRequired) => return Ok(None),
                Err(e) => return Err(DecoderError::Decoder(e)),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(audio_buf) => {
                    let spec = *audio_buf.spec();
                    let buf_channels = spec.channels.count();
                    if buf_channels == 0 || audio_buf.frames() == 0 {
                        continue;
                    }
                    match self.channels {
                        None => {
                            self.channels = Some(buf_channels);
                            self.sample_rate = spec.rate;
                        }
                        // A mid-stream layout change cannot be interleaved
                        // into one buffer; skip the odd packets.
                        Some(c) if c != buf_channels => continue,
                        Some(_) => {}
                    }
                    let mut converted =
                        AudioDecoder::convert_audio_buffer(audio_buf, spec.rate, 0)?;
                    if self.skip_frames > 0 {
                        let frames = (converted.len() / buf_channels) as u64;
                        if self.skip_frames >= frames {
                            self.skip_frames -= frames;
                            continue;
                        }
                        converted.drain(..self.skip_frames as usize * buf_channels);
                        self.skip_frames = 0;
                    }
                    return Ok(Some(converted));
                }
                // Skip corrupted frames
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(DecoderError::Decoder(e)),
            }
        }
    }

    /// Move to `frame` (counted at [`TARGET_SAMPLE_RATE`]). Lands exactly:
    /// whatever the container could not skip is dropped from the decoded output.
    pub(crate) fn seek(&mut self, frame: u64) -> Result<(), DecoderError> {
        let rate = TARGET_SAMPLE_RATE as u64;
        let time = Time::new(frame / rate, (frame % rate) as f64 / rate as f64);
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        let diff = seeked.required_ts.saturating_sub(seeked.actual_ts);
        self.skip_frames = match self.codec_params.time_base {
            Some(tb) => {
                let t = tb.calc_time(diff);
                ((t.seconds as f64 + t.frac) * self.sample_rate as f64).round() as u64
            }
            None => diff,
        };
        Ok(())
    }
}

/// High-level audio decoder using Symphonia
pub struct AudioDecoder;

impl AudioDecoder {
    /// Read the tags and the header duration of a file without decoding it.
    pub fn probe_file<P: AsRef<Path>>(path: P) -> Result<FileProbe, DecoderError> {
        let reader = PacketReader::open(path.as_ref())?;
        Ok(FileProbe {
            duration_secs: reader.duration_secs(),
            sample_rate: reader.declared_sample_rate(),
            meta: reader.meta,
        })
    }

    /// Decode an audio file to PCM samples at target sample rate and channels
    pub fn decode_file<P: AsRef<Path>>(path: P) -> Result<DecodedAudio, DecoderError> {
        let mut reader = PacketReader::open(path.as_ref())?;

        let mut all_samples = Vec::new();
        while let Some(chunk) = reader.next_chunk()? {
            all_samples.extend_from_slice(&chunk);
        }
        let sample_rate = reader.sample_rate;

        let channels = reader.channels.unwrap_or(TARGET_CHANNELS as usize);
        let channels_u32 = u32::try_from(channels).unwrap_or(TARGET_CHANNELS);

        // Downmix first: resampling two channels is cheaper than six.
        let final_samples = if channels_u32 != TARGET_CHANNELS {
            Self::convert_channels(&all_samples, channels_u32, TARGET_CHANNELS)?
        } else {
            all_samples
        };

        // Resample if needed
        let final_samples = if sample_rate != TARGET_SAMPLE_RATE && sample_rate > 0 {
            Self::resample(
                &final_samples,
                TARGET_CHANNELS,
                sample_rate,
                TARGET_SAMPLE_RATE,
            )?
        } else {
            final_samples
        };

        let frames = final_samples.len() / TARGET_CHANNELS as usize;
        let duration = frames as f32 / TARGET_SAMPLE_RATE as f32;

        Ok(DecodedAudio {
            samples: final_samples,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS,
            duration,
            frames,
        })
    }

    /// Convert an audio buffer to f32 stereo at the given sample rate
    pub(crate) fn convert_audio_buffer(
        audio_buf: AudioBufferRef,
        _src_sample_rate: u32,
        _src_channels: u32,
    ) -> Result<Vec<f32>, DecoderError> {
        let frames = audio_buf.frames();
        let channels = audio_buf.spec().channels.count();

        // Convert to f32 - get all channels
        let mut channel_data: Vec<Vec<f32>> = Vec::with_capacity(channels);
        for ch in 0..channels {
            let samples: Vec<f32> = match &audio_buf {
                AudioBufferRef::U8(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v as f32 / 255.0 * 2.0 - 1.0)
                    .collect(),
                AudioBufferRef::U16(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v as f32 / 65535.0 * 2.0 - 1.0)
                    .collect(),
                AudioBufferRef::U24(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v.0 as f32 / 16777215.0 * 2.0 - 1.0)
                    .collect(),
                AudioBufferRef::U32(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v as f32 / 4294967295.0 * 2.0 - 1.0)
                    .collect(),
                AudioBufferRef::S8(ref buf) => {
                    buf.chan(ch).iter().map(|&v| v as f32 / 128.0).collect()
                }
                AudioBufferRef::S16(ref buf) => {
                    buf.chan(ch).iter().map(|&v| v as f32 / 32768.0).collect()
                }
                AudioBufferRef::S24(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v.0 as f32 / 8388608.0)
                    .collect(),
                AudioBufferRef::S32(ref buf) => buf
                    .chan(ch)
                    .iter()
                    .map(|&v| v as f32 / 2147483648.0)
                    .collect(),
                AudioBufferRef::F32(ref buf) => buf.chan(ch).to_vec(),
                AudioBufferRef::F64(ref buf) => buf.chan(ch).iter().map(|&v| v as f32).collect(),
            };
            channel_data.push(samples);
        }

        // Interleave channels
        let mut interleaved = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            for channel in &channel_data {
                interleaved.push(channel[frame]);
            }
        }
        Ok(interleaved)
    }

    /// Band-limited (windowed-sinc) sample-rate conversion of a whole buffer.
    ///
    /// See [`Resampler`]. The streaming source runs the same resampler chunk by
    /// chunk, so both paths produce the same samples.
    pub(crate) fn resample(
        samples: &[f32],
        channels: u32,
        from_rate: u32,
        to_rate: u32,
    ) -> Result<Vec<f32>, DecoderError> {
        if from_rate == to_rate {
            return Ok(samples.to_vec());
        }
        let channels = channels as usize;
        let mut resampler = Resampler::new(channels, from_rate, to_rate)?;
        let input_frames = samples.len() / channels;
        let mut output = Vec::new();
        resampler.emit(samples, 0, input_frames, true, &mut output);
        Ok(output)
    }

    /// Convert between channel configurations
    pub(crate) fn convert_channels(
        samples: &[f32],
        from_channels: u32,
        to_channels: u32,
    ) -> Result<Vec<f32>, DecoderError> {
        if from_channels == to_channels {
            return Ok(samples.to_vec());
        }

        let frames = samples.len() / from_channels as usize;
        let mut output = Vec::with_capacity(frames * to_channels as usize);

        match (from_channels, to_channels) {
            (1, 2) => {
                // Mono to stereo: duplicate
                for &sample in &samples[..frames] {
                    output.push(sample); // Left
                    output.push(sample); // Right
                }
            }
            (2, 1) => {
                // Stereo to mono: average
                for frame in 0..frames {
                    let left = samples[frame * 2];
                    let right = samples[frame * 2 + 1];
                    output.push((left + right) * 0.5);
                }
            }
            (_, 2) => {
                // Multi-channel to stereo. Symphonia orders channels by the
                // WAVE/SMPTE layout: FL, FR, FC, LFE, then surrounds. Keep the
                // fronts, fold the centre and the surrounds in at -3 dB, and
                // drop the LFE, instead of throwing away everything but the
                // first two channels (which lost all dialogue in a 5.1 file).
                let n = from_channels as usize;
                let k = std::f32::consts::FRAC_1_SQRT_2;
                let norm = if n >= 3 { 1.0 / (1.0 + k + k) } else { 1.0 };
                for frame in samples.chunks_exact(n) {
                    let mut left = frame[0];
                    let mut right = frame[1];
                    if n >= 3 {
                        left += frame[2] * k;
                        right += frame[2] * k;
                    }
                    for (i, s) in frame.iter().enumerate().skip(4) {
                        if i % 2 == 0 {
                            left += s * k;
                        } else {
                            right += s * k;
                        }
                    }
                    output.push(left * norm);
                    output.push(right * norm);
                }
            }
            _ => {
                return Err(DecoderError::ChannelConversion(format!(
                    "{} -> {} not implemented",
                    from_channels, to_channels
                )));
            }
        }

        Ok(output)
    }
}

/// Band-limited (windowed-sinc) sample-rate converter.
///
/// Linear interpolation, used before, aliases badly: every 44.1 -> 48 kHz
/// file got audible metallic images, and a 96 kHz source folded its
/// ultrasonic content down into the audible band. This is a polyphase
/// Blackman-windowed sinc, low-passed at the lower of the two Nyquist
/// frequencies.
///
/// It works on a stream: [`process`](Self::process) takes input in chunks of
/// any size and emits every output frame whose whole kernel is already
/// available; the last call passes `finish` to flush the tail. Output frame
/// `k` is always computed from the same input frames in the same order, so
/// chunking does not change a single sample.
pub(crate) struct Resampler {
    channels: usize,
    /// Source frames per output frame.
    step: f64,
    half_width: i64,
    table: Vec<f32>,
    /// Sliding window of input not yet released by the kernel.
    window: Vec<f32>,
    /// Absolute index of the first frame in `window`.
    base: usize,
    /// Input frames received so far.
    total_in: usize,
    /// Index of the next output frame.
    next_out: usize,
    acc: Vec<f32>,
}

impl Resampler {
    const PHASES: usize = 256;
    /// Zero crossings on each side of the kernel.
    const ZERO_CROSSINGS: f64 = 16.0;

    pub(crate) fn new(channels: usize, from_rate: u32, to_rate: u32) -> Result<Self, DecoderError> {
        if channels == 0 || from_rate == 0 || to_rate == 0 {
            return Err(DecoderError::ResampleUnsupported);
        }
        // Source-sample step per output sample, and the low-pass cutoff as a
        // fraction of the source Nyquist (with a little transition band).
        let step = from_rate as f64 / to_rate as f64;
        let cutoff = (to_rate as f64 / from_rate as f64).min(1.0) * 0.94;
        let half_width = (Self::ZERO_CROSSINGS / cutoff).ceil() as i64;

        // Kernel table: PHASES entries per source sample, linearly interpolated.
        let table_len = half_width as usize * Self::PHASES + 2;
        let table: Vec<f32> = (0..table_len)
            .map(|i| {
                let x = i as f64 / Self::PHASES as f64; // distance in source samples
                if x >= half_width as f64 {
                    return 0.0;
                }
                let arg = std::f64::consts::PI * x * cutoff;
                let sinc = if x == 0.0 { 1.0 } else { arg.sin() / arg };
                let u = x / half_width as f64; // 0..1
                let window = 0.42
                    + 0.5 * (std::f64::consts::PI * u).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * u).cos();
                (cutoff * sinc * window) as f32
            })
            .collect();
        Ok(Self {
            channels,
            step,
            half_width,
            table,
            window: Vec::new(),
            base: 0,
            total_in: 0,
            next_out: 0,
            acc: vec![0.0; channels],
        })
    }

    /// Forget the input so far: the next sample starts a new signal (after a
    /// seek or a loop restart).
    pub(crate) fn reset(&mut self) {
        self.window.clear();
        self.base = 0;
        self.total_in = 0;
        self.next_out = 0;
    }

    /// Feed `input` (interleaved) and append the ready output to `out`.
    pub(crate) fn process(&mut self, input: &[f32], finish: bool, out: &mut Vec<f32>) {
        self.window.extend_from_slice(input);
        self.total_in += input.len() / self.channels;
        // Taken out of `self` so the window can be read while `emit` mutates
        // the rest of the state.
        let window = std::mem::take(&mut self.window);
        let (base, total_in) = (self.base, self.total_in);
        self.emit(&window, base, total_in, finish, out);
        self.window = window;

        // Release the input that no later output frame needs.
        let first_needed = self.first_source(self.next_out as f64 * self.step);
        let drop_frames = (first_needed.saturating_sub(self.base as i64).max(0) as usize)
            .min(self.window.len() / self.channels);
        if drop_frames > 0 {
            self.window.drain(..drop_frames * self.channels);
            self.base += drop_frames;
        }
    }

    fn first_source(&self, center: f64) -> i64 {
        (center.floor() as i64 - self.half_width + 1).max(0)
    }

    fn kernel(&self, distance: f64) -> f32 {
        let pos = distance.abs() * Self::PHASES as f64;
        let index = pos as usize;
        if index + 1 >= self.table.len() {
            return 0.0;
        }
        let frac = (pos - index as f64) as f32;
        self.table[index] + (self.table[index + 1] - self.table[index]) * frac
    }

    /// Produce output frames from `window` (input frames `base..total_in`).
    /// Without `finish` only frames whose whole kernel is present come out.
    pub(crate) fn emit(
        &mut self,
        window: &[f32],
        base: usize,
        total_in: usize,
        finish: bool,
        out: &mut Vec<f32>,
    ) {
        if total_in == 0 {
            return;
        }
        let channels = self.channels;
        let total_out = if finish {
            ((total_in as f64) / self.step).floor() as usize
        } else {
            usize::MAX
        };
        let mut acc = std::mem::take(&mut self.acc);
        while self.next_out < total_out {
            let center = self.next_out as f64 * self.step;
            let last_wanted = center.floor() as i64 + self.half_width;
            if !finish && last_wanted > total_in as i64 - 1 {
                break;
            }
            let first = self.first_source(center);
            let last = last_wanted.min(total_in as i64 - 1);
            acc.iter_mut().for_each(|a| *a = 0.0);
            let mut weight_sum = 0.0f32;
            for src in first..=last {
                let w = self.kernel(center - src as f64);
                weight_sum += w;
                let at = (src as usize - base) * channels;
                for (ch, a) in acc.iter_mut().enumerate() {
                    *a += window[at + ch] * w;
                }
            }
            // Normalising by the summed weights keeps DC exact, including at
            // the file edges where the kernel is truncated.
            let norm = if weight_sum.abs() > 1e-6 {
                1.0 / weight_sum
            } else {
                0.0
            };
            out.extend(acc.iter().map(|a| a * norm));
            self.next_out += 1;
        }
        self.acc = acc;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoded_audio_creation() {
        let samples = vec![0.0; 48000 * 2]; // 1 second of silence at 48kHz stereo
        let audio = DecodedAudio::new(samples, 48000, 2);
        assert_eq!(audio.frames, 48000);
        assert_eq!(audio.duration, 1.0);
        assert_eq!(audio.sample_rate, 48000);
        assert_eq!(audio.channels, 2);
    }

    #[test]
    fn resample_keeps_a_tone_clean() {
        // 1 kHz at 44.1 kHz -> 48 kHz: same frequency, same amplitude, no images.
        let from = 44_100u32;
        let input: Vec<f32> = (0..from as usize)
            .flat_map(|i| {
                let v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / from as f32).sin();
                [v, v]
            })
            .collect();
        let out = AudioDecoder::resample(&input, 2, from, 48_000).unwrap();
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() <= 2, "{frames} frames");

        // Compare against the ideal 48 kHz tone away from the edges.
        let mut err = 0.0f64;
        let mut sig = 0.0f64;
        for i in 1000..frames - 1000 {
            let ideal = 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin();
            let got = out[i * 2] as f64;
            err += (got - ideal).powi(2);
            sig += ideal.powi(2);
        }
        let snr_db = 10.0 * (sig / err).log10();
        assert!(snr_db > 60.0, "SNR only {snr_db:.1} dB");
    }

    #[test]
    fn streaming_resampler_matches_the_whole_buffer_for_any_chunking() {
        for (from, to) in [(44_100u32, 48_000u32), (22_050, 48_000), (96_000, 48_000)] {
            let input: Vec<f32> = (0..from as usize / 4)
                .flat_map(|i| {
                    let t = i as f32 / from as f32;
                    [
                        (2.0 * std::f32::consts::PI * 440.0 * t).sin(),
                        (2.0 * std::f32::consts::PI * 3_000.0 * t).cos() * 0.5,
                    ]
                })
                .collect();
            let whole = AudioDecoder::resample(&input, 2, from, to).unwrap();

            // Awkward chunk sizes, including ones smaller than the kernel.
            for chunk_frames in [1usize, 7, 100, 1_001, 5_000] {
                let mut resampler = Resampler::new(2, from, to).unwrap();
                let mut out = Vec::new();
                for chunk in input.chunks(chunk_frames * 2) {
                    resampler.process(chunk, false, &mut out);
                }
                resampler.process(&[], true, &mut out);
                assert_eq!(out.len(), whole.len(), "{from}->{to} chunk {chunk_frames}");
                assert!(
                    out.iter().zip(&whole).all(|(a, b)| a == b),
                    "{from}->{to} chunk {chunk_frames}: samples differ"
                );
            }
        }
    }

    #[test]
    fn surround_downmix_keeps_the_centre_channel() {
        // 6 channels: only the centre (index 2) carries signal.
        let frame = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let out = AudioDecoder::convert_channels(&frame, 6, 2).unwrap();
        assert!(out[0] > 0.2 && out[1] > 0.2);
    }

    #[test]
    fn test_slice() {
        let samples: Vec<f32> = (0..48000 * 2).map(|i| i as f32).collect();
        let audio = DecodedAudio::new(samples, 48000, 2);
        let slice = audio.slice(0, 1000);
        assert_eq!(slice.len(), 2000); // 1000 frames * 2 channels
    }
}
