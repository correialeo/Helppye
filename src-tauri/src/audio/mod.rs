pub mod config;
pub mod devices;
pub mod error;
pub mod level_meter;
pub mod pipeline;
pub mod platform;
pub mod provider;
pub mod resampler;
pub mod types;

use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use config::CaptureConfig;
use pipeline::MicrophoneCaptureProvider;
use provider::AudioCaptureProvider;
use types::AudioDevice;

const CAPTURE_EVENT: &str = "audio://capture-event";

#[derive(Default)]
pub struct AudioState {
    mic_cancel: tokio::sync::Mutex<Option<CancellationToken>>,
}

#[tauri::command]
pub async fn list_audio_devices_command() -> Result<Vec<AudioDevice>, String> {
    MicrophoneCaptureProvider
        .list_devices()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_microphone_capture_command(
    app: AppHandle,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    let mut guard = state.mic_cancel.lock().await;
    if guard.is_some() {
        return Err("microphone capture is already running".into());
    }

    let config = CaptureConfig::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel(config.channel_capacity);
    let cancel = CancellationToken::new();

    MicrophoneCaptureProvider
        .start(config, tx, cancel.clone())
        .await
        .map_err(|e| e.to_string())?;

    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Err(e) = app.emit(CAPTURE_EVENT, &event) {
                warn!(%e, "failed to emit audio capture event to frontend");
            }
        }
    });

    *guard = Some(cancel);
    Ok(())
}

#[tauri::command]
pub async fn stop_capture_command(state: State<'_, AudioState>) -> Result<(), String> {
    let mut guard = state.mic_cancel.lock().await;
    if let Some(cancel) = guard.take() {
        cancel.cancel();
    }
    Ok(())
}
