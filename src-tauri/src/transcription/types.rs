use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::audio::types::AudioSource;

/// Language configuration for a transcription provider. `Automatic` lets the provider
/// detect the language per-segment; `Fixed` pins it (e.g. always Portuguese), which is
/// both faster and more accurate when the meeting language is known ahead of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptionLanguage {
    Automatic,
    Fixed(String),
}

impl Default for TranscriptionLanguage {
    /// Default: Portuguese — see `docs/local-transcription.md`.
    fn default() -> Self {
        TranscriptionLanguage::Fixed("pt".into())
    }
}

/// Compute backend for model inference. `Cpu` is the only backend verified to work in this
/// sandbox (no GPU passthrough here); `Gpu` is exposed so a provider can offer it where
/// available, but selecting it must fail loudly (`TranscriptionError`), never silently fall
/// back to CPU — see `docs/local-transcription.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceDevice {
    Cpu,
    Gpu,
}

/// Everything a `TranscriptionProvider` needs to load a model. No path defaults to a
/// bundled or auto-downloaded model — an unconfigured `model_path` is a `NotConfigured`
/// error, not a silent download.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model_path: std::path::PathBuf,
    pub model_name: String,
    pub language: TranscriptionLanguage,
    pub device: InferenceDevice,
}

/// The text output of transcribing one `AudioSegment`. Carries `segment_id`/`source` so the
/// frontend can correlate it back to the segment (and thus the right capture panel) even if
/// transcripts for microphone and system output arrive out of order.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub text: String,
    /// Language actually used/detected for this segment, if the provider reports one.
    pub language: Option<String>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub processing_time_ms: u64,
}
