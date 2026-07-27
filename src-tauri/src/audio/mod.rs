pub mod config;
pub mod devices;
pub mod error;
pub mod level_meter;
pub mod pipeline;
pub mod platform;
pub mod provider;
pub mod resampler;
pub mod sample_convert;
pub mod segment;
pub mod segmentation;
pub mod types;
pub mod vad;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use config::CaptureConfig;
use pipeline::MicrophoneCaptureProvider;
use platform::SystemAudioProvider;
use provider::AudioCaptureProvider;
use segmentation::{SegmentationConfig, Segmenter, SpeechEvent};
use types::{AudioCaptureEvent, AudioDevice, AudioSource};

use crate::transcription::queue::TranscriptionQueue;

const CAPTURE_EVENT: &str = "audio://capture-event";

pub struct AudioState {
    mic_cancel: tokio::sync::Mutex<Option<CancellationToken>>,
    system_cancel: tokio::sync::Mutex<Option<CancellationToken>>,
    transcription_queue: Arc<TranscriptionQueue>,
}

impl AudioState {
    pub fn new(transcription_queue: Arc<TranscriptionQueue>) -> Self {
        AudioState {
            mic_cancel: tokio::sync::Mutex::new(None),
            system_cancel: tokio::sync::Mutex::new(None),
            transcription_queue,
        }
    }
}

async fn start_capture(
    app: AppHandle,
    guard_cell: &tokio::sync::Mutex<Option<CancellationToken>>,
    provider: impl AudioCaptureProvider + 'static,
    source: AudioSource,
    transcription_queue: Arc<TranscriptionQueue>,
    already_running_msg: &str,
) -> Result<(), String> {
    let mut guard = guard_cell.lock().await;
    if guard.is_some() {
        return Err(already_running_msg.into());
    }

    let config = CaptureConfig::default();
    let sample_rate = config.target_sample_rate;
    let (tx, mut rx) = tokio::sync::mpsc::channel(config.channel_capacity);
    let cancel = CancellationToken::new();

    provider
        .start(config, tx, cancel.clone())
        .await
        .map_err(|e| e.to_string())?;

    tauri::async_runtime::spawn(async move {
        // One `Segmenter` per capture session: speech segments never mix across sources,
        // and never survive a stop/restart of this capture (in-flight speech at stop time
        // is discarded, not flushed — an accepted simplification for this first pass).
        let mut segmenter = Segmenter::new(source, sample_rate, SegmentationConfig::default());

        while let Some(event) = rx.recv().await {
            if let AudioCaptureEvent::Frame(ref frame) = event {
                for speech_event in segmenter.push_samples(&frame.samples) {
                    if let SpeechEvent::SegmentReady(segment) = speech_event {
                        transcription_queue.try_enqueue(segment);
                    }
                }
            }

            if let Err(e) = app.emit(CAPTURE_EVENT, &event) {
                warn!(%e, "failed to emit audio capture event to frontend");
            }
        }
    });

    *guard = Some(cancel);
    Ok(())
}

async fn stop_capture(
    guard_cell: &tokio::sync::Mutex<Option<CancellationToken>>,
) -> Result<(), String> {
    let mut guard = guard_cell.lock().await;
    if let Some(cancel) = guard.take() {
        cancel.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn list_audio_devices_command() -> Result<Vec<AudioDevice>, String> {
    MicrophoneCaptureProvider
        .list_devices()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_system_audio_devices_command() -> Result<Vec<AudioDevice>, String> {
    SystemAudioProvider
        .list_devices()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_microphone_capture_command(
    app: AppHandle,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    start_capture(
        app,
        &state.mic_cancel,
        MicrophoneCaptureProvider,
        AudioSource::Microphone,
        state.transcription_queue.clone(),
        "microphone capture is already running",
    )
    .await
}

#[tauri::command]
pub async fn stop_microphone_capture_command(state: State<'_, AudioState>) -> Result<(), String> {
    stop_capture(&state.mic_cancel).await
}

#[tauri::command]
pub async fn start_system_audio_capture_command(
    app: AppHandle,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    start_capture(
        app,
        &state.system_cancel,
        SystemAudioProvider,
        AudioSource::SystemOutput,
        state.transcription_queue.clone(),
        "system audio capture is already running",
    )
    .await
}

#[tauri::command]
pub async fn stop_system_audio_capture_command(state: State<'_, AudioState>) -> Result<(), String> {
    stop_capture(&state.system_cancel).await
}
