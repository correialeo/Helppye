mod audio;
mod model_manager;
mod transcription;

use std::sync::Arc;

use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

use transcription::provider::TranscriptionProvider;
use transcription::queue::TranscriptionQueue;
use transcription::whisper_provider::WhisperCppProvider;
use transcription::{TranscriptionState, TRANSCRIPTION_EVENT};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let provider: Arc<dyn TranscriptionProvider> = Arc::new(WhisperCppProvider::new());
            let app_handle = app.handle().clone();
            let queue = Arc::new(TranscriptionQueue::spawn(provider.clone(), move |event| {
                if let Err(e) = app_handle.emit(TRANSCRIPTION_EVENT, &event) {
                    tracing::warn!(%e, "failed to emit transcription event to frontend");
                }
            }));
            app.manage(audio::AudioState::new(queue.clone()));
            app.manage(TranscriptionState::new(provider.clone(), queue));

            let model_manager_state = model_manager::build(app.handle(), provider)?;
            app.manage(model_manager_state);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            audio::list_audio_devices_command,
            audio::list_system_audio_devices_command,
            audio::start_microphone_capture_command,
            audio::stop_microphone_capture_command,
            audio::start_system_audio_capture_command,
            audio::stop_system_audio_capture_command,
            transcription::configure_transcription_command,
            transcription::transcription_diagnostics_command,
            model_manager::model_status_command,
            model_manager::start_model_download_command,
            model_manager::cancel_model_download_command,
            model_manager::select_custom_model_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Helppye application");
}
