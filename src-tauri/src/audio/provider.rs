use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::types::{AudioCaptureEvent, AudioDevice};

/// A source of audio frames (microphone, system output, ...).
///
/// Implementations must never block the platform's real-time audio callback:
/// pushing to `sender` must be non-blocking (`try_send`), and a full channel
/// means the frame is dropped and counted, not awaited.
#[async_trait]
pub trait AudioCaptureProvider: Send + Sync {
    async fn list_devices(&self) -> Result<Vec<AudioDevice>, AudioCaptureError>;

    /// Starts capture. Returns once the stream is confirmed running (or failed to start) —
    /// it does not block for the lifetime of the capture. Capture stops when `cancel` is
    /// triggered; `sender` carries every event, including `Stopped` as the final event.
    async fn start(
        &self,
        config: CaptureConfig,
        sender: mpsc::Sender<AudioCaptureEvent>,
        cancel: CancellationToken,
    ) -> Result<(), AudioCaptureError>;

    /// Explicit stop hook for providers that need cleanup beyond cancellation
    /// (e.g. tearing down a platform-native tap). The default cancellation-token
    /// path already stops capture; this exists for providers where that isn't enough.
    async fn stop(&self) -> Result<(), AudioCaptureError> {
        Ok(())
    }
}
