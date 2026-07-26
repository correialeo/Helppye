//! Windows system-audio capture via WASAPI loopback.
//!
//! Reimplemented from scratch against the `windows` crate rather than adapted from
//! Meetily: the audit (`docs/meetily-audio-audit.md` section 6, finding 4) found Meetily's
//! non-macOS system-audio path is an explicit `bail!("not yet implemented")` with nothing
//! functional to adapt — only its `devices/platform/windows.rs` device-listing pattern
//! served as a design reference (not copied), and it identifies the same fix this module
//! makes: select devices by stable endpoint ID, not name substring.
//!
//! See `docs/windows-wasapi-loopback.md` for the full design writeup.

mod capture;
mod com;
mod devices;
mod format;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::types::{AudioCaptureEvent, AudioDevice};

pub struct SystemAudioProvider;

#[async_trait]
impl AudioCaptureProvider for SystemAudioProvider {
    async fn list_devices(&self) -> Result<Vec<AudioDevice>, AudioCaptureError> {
        // COM calls are blocking and must not run on the async runtime's worker threads.
        tokio::task::spawn_blocking(devices::list_output_devices)
            .await
            .map_err(|e| {
                AudioCaptureError::Internal(format!("device enumeration task panicked: {e}"))
            })?
    }

    async fn start(
        &self,
        config: CaptureConfig,
        sender: mpsc::Sender<AudioCaptureEvent>,
        cancel: CancellationToken,
    ) -> Result<(), AudioCaptureError> {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), AudioCaptureError>>();

        std::thread::Builder::new()
            .name("helppye-loopback-capture".into())
            .spawn(move || capture::run_capture_thread(config, sender, cancel, ready_tx))
            .map_err(|e| {
                AudioCaptureError::Internal(format!("failed to spawn capture thread: {e}"))
            })?;

        ready_rx.await.map_err(|_| {
            AudioCaptureError::Internal("capture thread exited before signaling readiness".into())
        })?
    }
}
