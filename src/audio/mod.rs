//! Audio engine module for SoundShared
//! Provides decoding, mixing, and playback functionality

pub mod decoder;
pub mod engine;
pub mod mixer;
pub mod youtube;

pub use decoder::{AudioDecoder, DecodedAudio, DecoderError, TARGET_CHANNELS, TARGET_SAMPLE_RATE};
pub use engine::{
    AudioEngine, EngineError, EngineStats, LocalOutputState, LocalOutputStatus, SoundId,
    TrackHandle,
};
pub use mixer::{AudioMixer, MixedOutput, MixerConfig, MixerError};
pub use youtube::{display_name as youtube_display_name, extract_video_id};
