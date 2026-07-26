use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::types::{AudioCaptureEvent, AudioDevice};

/// Planned: Core Audio Process Tap (macOS 14.4+), adapted from Meetily's
/// `capture/core_audio.rs` per `docs/third-party-components.md`. Not implemented yet —
/// requires real macOS 14.4+ hardware to validate, which this environment does not have.
pub struct SystemAudioProvider;

#[async_trait]
impl AudioCaptureProvider for SystemAudioProvider {
    async fn list_devices(&self) -> Result<Vec<AudioDevice>, AudioCaptureError> {
        Ok(Vec::new())
    }

    async fn start(
        &self,
        _config: CaptureConfig,
        _sender: mpsc::Sender<AudioCaptureEvent>,
        _cancel: CancellationToken,
    ) -> Result<(), AudioCaptureError> {
        Err(AudioCaptureError::Unsupported(
            "system audio capture on macOS is not implemented yet (planned: Core Audio Process Tap, requires macOS 14.4+)".into(),
        ))
    }
}
