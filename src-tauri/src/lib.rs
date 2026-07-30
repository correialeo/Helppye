mod audio;
mod conversation;
mod model_manager;
mod response_provider;
mod transcription;

use std::sync::Arc;

use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

use conversation::{emit_conversation_events, ConversationTimeline, ConversationTimelineState};
use response_provider::engine::process_conversation_events;
use transcription::provider::TranscriptionProvider;
use transcription::queue::TranscriptionQueue;
use transcription::types::TranscriptEvent;
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
            let timeline = Arc::new(ConversationTimeline::default());
            timeline.attach();
            let response_engine_state = response_provider::build(app.handle());
            let response_engine = response_engine_state.0.clone();
            // O motor e a timeline nascem separados, cada um com sua ideia de "sessão
            // atual". Alinha os dois já no boot para que o primeiro turno da primeira
            // sessão não seja recusado como pertencente a outra sessão.
            response_engine.begin_session(timeline.session_id());
            let app_handle = app.handle().clone();

            // O timer dedicado da utterance (`ConversationTimeline::reschedule_utterance_timer`)
            // finaliza uma utterance por silêncio sem que nenhum código externo chame de
            // volta o timeline — precisa de um jeito de emitir esses eventos por conta
            // própria. Registra aqui a mesma sequência emit+process usada manualmente logo
            // abaixo para o caminho síncrono (novo segmento chegando), para que os dois
            // caminhos cheguem ao frontend e ao `ResponseEngine` de forma idêntica.
            let app_handle_for_sink = app_handle.clone();
            let response_engine_for_sink = response_engine.clone();
            timeline.set_event_sink(Arc::new(move |events| {
                emit_conversation_events(&app_handle_for_sink, events.clone());
                process_conversation_events(
                    &app_handle_for_sink,
                    response_engine_for_sink.clone(),
                    &events,
                );
            }));

            let timeline_for_queue = timeline.clone();
            let response_engine_for_queue = response_engine.clone();
            let queue = Arc::new(TranscriptionQueue::spawn(provider.clone(), move |event| {
                // Uma transcrição que falha não pode sumir sem deixar rastro. `Failed` é
                // descartado silenciosamente por `ingest_transcript_event` (só `Ready`
                // vira segmento), então sem este log um provider sem modelo carregado
                // produz falha para *todo* segmento e o app fica em silêncio absoluto:
                // captura ativa, medidor de nível se mexendo, nada na timeline e nenhuma
                // linha no terminal explicando o porquê.
                if let TranscriptEvent::Failed {
                    segment_id,
                    source,
                    ref message,
                } = event
                {
                    tracing::warn!(
                        ?segment_id,
                        ?source,
                        %message,
                        "transcription failed for segment"
                    );
                }
                if let Err(e) = app_handle.emit(TRANSCRIPTION_EVENT, &event) {
                    tracing::warn!(%e, "failed to emit transcription event to frontend");
                }
                let conversation_events = timeline_for_queue.ingest_transcript_event(event);
                emit_conversation_events(&app_handle, conversation_events.clone());
                process_conversation_events(
                    &app_handle,
                    response_engine_for_queue.clone(),
                    &conversation_events,
                );
            }));
            let audio_state = audio::build(
                app.handle(),
                queue.clone(),
                timeline.clone(),
                response_engine.clone(),
            )?;
            app.manage(audio_state);
            app.manage(ConversationTimelineState(timeline));
            app.manage(response_engine_state);
            app.manage(TranscriptionState::new(provider.clone(), queue));

            let model_manager_state = model_manager::build(app.handle(), provider)?;
            // O arquivo do modelo sobrevive a um restart do app; o estado carregado dentro
            // do provider não. Restaurá-lo era efeito colateral de `model_status_command`,
            // invocado só pela tela de teste de áudio do onboarding — que deixa de ser
            // montada assim que o onboarding é concluído. A partir da segunda execução,
            // portanto, nada carregava o modelo e toda transcrição falhava com "no
            // transcription model configured". O ciclo de vida do modelo é do backend, não
            // de uma tela: a restauração acontece aqui, uma vez, no boot.
            let model_manager_for_restore = model_manager_state.0.clone();
            tauri::async_runtime::spawn(async move {
                match model_manager_for_restore.status_snapshot().await {
                    Ok(status) => tracing::info!(
                        state = ?status.state,
                        "transcription model state restored at startup"
                    ),
                    Err(e) => {
                        tracing::warn!(%e, "failed to restore transcription model at startup")
                    }
                }
            });
            app.manage(model_manager_state);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            audio::list_audio_devices_command,
            audio::list_system_audio_devices_command,
            audio::resolve_device_selection_command,
            audio::select_input_device_command,
            audio::select_output_device_command,
            audio::start_microphone_capture_command,
            audio::stop_microphone_capture_command,
            audio::start_system_audio_capture_command,
            audio::stop_system_audio_capture_command,
            conversation::conversation_timeline_snapshot_command,
            conversation::conversation_flush_turns_command,
            conversation::conversation_start_session_command,
            conversation::conversation_end_session_command,
            conversation::conversation_raw_segments_command,
            conversation::conversation_get_utterance_gap_ms_command,
            conversation::conversation_set_utterance_gap_ms_command,
            conversation::conversation_regenerate_suggestion_command,
            response_provider::response_provider_status_command,
            response_provider::response_set_provider_config_command,
            response_provider::response_set_api_key_command,
            response_provider::response_delete_api_key_command,
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
