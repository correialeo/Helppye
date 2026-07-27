use async_trait::async_trait;

use crate::audio::segment::AudioSegment;
use crate::transcription::error::TranscriptionError;
use crate::transcription::types::Transcript;

/// A local speech-to-text backend. Mirrors `audio::provider::AudioCaptureProvider`'s shape
/// so adding a second backend later (a different whisper.cpp binding, a non-Whisper engine,
/// ...) is a new impl of this trait, not a change to callers.
///
/// Implementations must never block the audio capture thread, the segmenter, or the UI
/// thread: `transcribe` is expected to run on a dedicated blocking task (model inference is
/// CPU/GPU-bound, not async I/O), and callers are responsible for queuing so a slow or
/// backed-up transcription never applies backpressure to capture/segmentation.
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    /// Loads the model per `config`, if not already loaded. Called once; the model is then
    /// reused for every `transcribe` call. A provider that fails to load must return a
    /// specific `TranscriptionError` variant (`ModelNotFound`, `ModelLoadFailed`,
    /// `OutOfMemory`, `InvalidModelFormat`, ...), never fabricate a result.
    async fn load(
        &self,
        config: crate::transcription::types::ModelConfig,
    ) -> Result<(), TranscriptionError>;

    /// Transcribes one already-segmented span of speech. `segment.source` and
    /// `segment.samples` are never mixed across sources upstream — this only ever sees
    /// audio from one source at a time.
    async fn transcribe(&self, segment: AudioSegment) -> Result<Transcript, TranscriptionError>;

    /// Short identifier for logs/diagnostics (e.g. `"whisper-cpp"`).
    fn provider_name(&self) -> &'static str;
}
