use async_trait::async_trait;

use crate::audio::segment::AudioSegment;
use crate::transcription::error::TranscriptionError;
use crate::transcription::types::Transcript;

/// A *segment-in, text-out* speech-to-text backend: the low-level shape a batch engine like
/// whisper.cpp naturally has. This is deliberately **not** the pluggable extension point
/// exposed to the rest of the app — that is `transcription::provider::TranscriptionProvider`,
/// which is session-based and can also model streaming backends that never see a pre-cut
/// segment.
///
/// A provider that wraps a batch engine (today: `WhisperLocalTranscriptionProvider`) owns a
/// `SegmentTranscriber` and adapts it to the session contract. Keeping the two separate is
/// what lets a streaming backend (OpenAI Realtime, Gemini Live) exist without pretending it
/// transcribes fixed segments, and lets whisper.cpp exist without pretending it emits
/// partial results.
///
/// Implementations must never block the audio capture thread, the segmenter, or the UI
/// thread: `transcribe` is expected to run on a dedicated blocking task (model inference is
/// CPU/GPU-bound, not async I/O), and callers are responsible for queuing so a slow or
/// backed-up transcription never applies backpressure to capture/segmentation.
#[async_trait]
pub trait SegmentTranscriber: Send + Sync {
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
