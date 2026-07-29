pub mod config;
pub mod devices;
pub mod engine;
pub mod error;
pub mod level_meter;
pub mod pipeline;
pub mod platform;
pub mod provider;
pub mod resampler;
pub mod sample_convert;
pub mod segment;
pub mod segmentation;
pub mod selection;
pub mod types;
pub mod vad;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use engine::{CaptureEngine, DeviceSelectionSnapshot};
use pipeline::MicrophoneCaptureProvider;
use platform::SystemAudioProvider;
use types::{AudioDevice, AudioSource};

use crate::conversation::{emit_conversation_events, ConversationTimeline};
use crate::transcription::queue::TranscriptionQueue;

const CAPTURE_EVENT: &str = "audio://capture-event";
const DEVICE_SELECTION_FILENAME: &str = "device_selection.json";

pub struct CaptureEngineState(pub Arc<CaptureEngine>);

/// Constrói o `CaptureEngine` gerenciado — chamado uma vez em `.setup()`. Único ponto
/// deste módulo que toca o `AppHandle` diretamente (resolução do diretório de dados do
/// app e encaminhamento de eventos ao frontend), o mesmo padrão de `model_manager::build`.
pub fn build(
    app: &AppHandle,
    transcription_queue: Arc<TranscriptionQueue>,
    conversation_timeline: Arc<ConversationTimeline>,
) -> Result<CaptureEngineState, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir unavailable: {e}"))?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let config_path = data_dir.join(DEVICE_SELECTION_FILENAME);
    let initial_selection = selection::load(&config_path);

    let app_handle = app.clone();
    let app_handle_for_timeline = app.clone();
    let engine = CaptureEngine::new(
        Arc::new(MicrophoneCaptureProvider),
        Arc::new(SystemAudioProvider),
        transcription_queue,
        config_path,
        initial_selection,
        Arc::new(move |event| {
            if let Err(e) = app_handle.emit(CAPTURE_EVENT, &event) {
                tracing::warn!(%e, "failed to emit audio capture event to frontend");
            }
            emit_conversation_events(
                &app_handle_for_timeline,
                conversation_timeline.ingest_capture_event(&event),
            );
        }),
    );
    Ok(CaptureEngineState(Arc::new(engine)))
}

#[tauri::command]
pub async fn list_audio_devices_command(
    state: State<'_, CaptureEngineState>,
) -> Result<Vec<AudioDevice>, String> {
    state
        .0
        .list_devices(AudioSource::Microphone)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_system_audio_devices_command(
    state: State<'_, CaptureEngineState>,
) -> Result<Vec<AudioDevice>, String> {
    state
        .0
        .list_devices(AudioSource::SystemOutput)
        .await
        .map_err(|e| e.to_string())
}

/// Chamado uma vez na abertura do app: resolve e pré-seleciona o dispositivo de entrada e
/// de saída (seleção persistida → padrão do Windows → primeiro disponível), sem iniciar
/// captura. O frontend decide quando o usuário efetivamente inicia a captura.
#[tauri::command]
pub async fn resolve_device_selection_command(
    state: State<'_, CaptureEngineState>,
) -> Result<DeviceSelectionSnapshot, String> {
    state.0.resolve_selection().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn select_input_device_command(
    state: State<'_, CaptureEngineState>,
    device_id: String,
) -> Result<(), String> {
    state
        .0
        .clone()
        .select_device(AudioSource::Microphone, device_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn select_output_device_command(
    state: State<'_, CaptureEngineState>,
    device_id: String,
) -> Result<(), String> {
    state
        .0
        .clone()
        .select_device(AudioSource::SystemOutput, device_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_microphone_capture_command(
    state: State<'_, CaptureEngineState>,
) -> Result<(), String> {
    state
        .0
        .clone()
        .start_capture(AudioSource::Microphone)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_microphone_capture_command(
    state: State<'_, CaptureEngineState>,
) -> Result<(), String> {
    state.0.stop_capture(AudioSource::Microphone).await;
    Ok(())
}

#[tauri::command]
pub async fn start_system_audio_capture_command(
    state: State<'_, CaptureEngineState>,
) -> Result<(), String> {
    state
        .0
        .clone()
        .start_capture(AudioSource::SystemOutput)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_system_audio_capture_command(
    state: State<'_, CaptureEngineState>,
) -> Result<(), String> {
    state.0.stop_capture(AudioSource::SystemOutput).await;
    Ok(())
}
