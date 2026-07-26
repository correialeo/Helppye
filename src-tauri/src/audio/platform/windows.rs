use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::types::{AudioCaptureEvent, AudioDevice};

/// Planned: WASAPI loopback capture on an output device. Not implemented yet.
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
            "system audio capture on Windows is not implemented yet (planned: WASAPI loopback)".into(),
        ))
    }
}
