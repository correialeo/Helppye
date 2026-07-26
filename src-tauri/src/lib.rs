mod audio;

use tracing_subscriber::EnvFilter;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    tauri::Builder::default()
        .manage(audio::AudioState::default())
        .invoke_handler(tauri::generate_handler![
            audio::list_audio_devices_command,
            audio::start_microphone_capture_command,
            audio::stop_capture_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Helppye application");
}
