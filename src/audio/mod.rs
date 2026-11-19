//! Audio engine module for SoundShared
//! Provides decoding, mixing, and playback functionality

pub mod decoder;
pub mod director;
pub mod emitter;
pub mod engine;
pub mod loudness;
pub mod mixer;
pub mod stream_source;
pub mod youtube;

pub use decoder::{
    AudioDecoder, DecodedAudio, DecoderError, FileProbe, TrackMeta, TARGET_CHANNELS,
    TARGET_SAMPLE_RATE,
};
pub use director::{
    ActivationPlan, CatalogSound, Director, DirectorCommand, DirectorError, DirectorOutcome,
    DirectorState, FadeSpec, Playback, SceneReport,
};
pub use emitter::{
    EmitterEvent, EmitterHost, EmitterRunner, EmitterSnapshot, EmitterSound, EmitterSpec, Scheduler,
};
pub use engine::{
    AudioEngine, EngineError, EngineStats, LocalOutputState, LocalOutputStatus, SoundId,
    TrackHandle,
};
pub use mixer::{
    pan_gains, AudioMixer, Bus, DuckSettings, FadeCurve, MixedOutput, MixerConfig, MixerError,
};
pub use stream_source::{StreamEvent, StreamOptions, StreamSource, DEFAULT_STREAM_THRESHOLD_SECS};
pub use youtube::{display_name as youtube_display_name, extract_video_id};
