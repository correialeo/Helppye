use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::types::{AudioCaptureEvent, AudioDevice};

/// Planned: native PipeWire capture. Not implemented yet. The Meetily reference has no
/// real implementation to adapt here either — see `docs/meetily-audio-audit.md` section 6,
/// finding 4 — so this needs to be designed from scratch against the PipeWire API.
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
            "system audio capture on Linux is not implemented yet (planned: native PipeWire)".into(),
        ))
    }
}
