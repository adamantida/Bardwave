//! Audio engine module for SoundShared
//! Provides decoding, mixing, and playback functionality

pub mod decoder;
pub mod devices;
pub mod director;
pub mod director_audio;
pub mod emitter;
pub mod engine;
pub mod loudness;
pub mod mixer;
pub mod playlist;
pub mod stream_source;
pub mod youtube;

pub use decoder::{
    AudioDecoder, DecodedAudio, DecoderError, FileProbe, TrackMeta, TARGET_CHANNELS,
    TARGET_SAMPLE_RATE,
};
pub use devices::{
    list_output_devices, pick_output_device, plan_preview_route, DevicePick, OutputDeviceInfo,
    OutputSelection, PreviewPlan,
};
pub use director::{
    ActivationPlan, CatalogSound, Director, DirectorCommand, DirectorError, DirectorOutcome,
    DirectorState, FadeSpec, Playback, SceneReport,
};
pub use director_audio::{DuckMode, DuckTarget, GainUpdate, MixControl, NormalizeSettings};
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
pub use playlist::{
    MusicLayer, MusicPlayback, PlaylistCommand, PlaylistError, PlaylistPlayer, PlaylistState,
    PlaylistStatus,
};
pub use stream_source::{StreamEvent, StreamOptions, StreamSource, DEFAULT_STREAM_THRESHOLD_SECS};
pub use youtube::{display_name as youtube_display_name, extract_video_id};
