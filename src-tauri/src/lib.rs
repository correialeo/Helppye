mod model_manager;
mod response_provider;

// Públicos porque são os pontos de extensão da aplicação, não detalhes internos: um backend
// de transcrição novo implementa `transcription::provider::TranscriptionProvider`, uma
// estratégia de limpeza de texto implementa `normalization::TranscriptNormalizer`, e o
// harness de benchmark (`benchmark`, consumido pelo binário `src/bin/benchmark.rs`) usa
// `audio`, `conversation` e `telemetry`. Manter esses módulos privados obrigaria qualquer
// consumidor — inclusive um teste de integração em `tests/` — a passar por `run()`.
pub mod audio;
pub mod benchmark;
pub mod conversation;
pub mod integrity;
pub mod normalization;
pub mod telemetry;
pub mod transcription;

use std::sync::Arc;

use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tracing_subscriber::EnvFilter;

use audio::segment::SegmentId;
use conversation::{emit_conversation_events, ConversationTimeline, ConversationTimelineState};
use response_provider::engine::start_response_engine_event_loop;
use transcription::events::TranscriptionEvent;
use transcription::provider::TranscriptionProvider;
use transcription::queue::TranscriptionQueue;
use transcription::registry::TranscriptionProviderRegistry;
use transcription::runtime::{
    TranscriptionOutputSink, TranscriptionRuntime, TranscriptionRuntimeOutput,
};
use transcription::segment_transcriber::SegmentTranscriber;
use transcription::settings::TranscriptionSettings;
use transcription::types::{Transcript, TranscriptEvent};
use transcription::whisper_local::WhisperLocalTranscriptionProvider;
use transcription::whisper_provider::WhisperCppProvider;
use transcription::{TranscriptionState, TRANSCRIPTION_EVENT};

const GLOBAL_SESSION_TOGGLE_EVENT: &str = "helppye://global-session-toggle";

fn session_toggle_shortcut() -> Shortcut {
    #[cfg(target_os = "macos")]
    let modifiers = Modifiers::SUPER;

    #[cfg(not(target_os = "macos"))]
    let modifiers = Modifiers::CONTROL;

    Shortcut::new(Some(modifiers), Code::KeyD)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed && *shortcut == session_toggle_shortcut() {
                        if let Err(e) = app.emit(GLOBAL_SESSION_TOGGLE_EVENT, ()) {
                            tracing::warn!(%e, "failed emit global session shortcut event frontend");
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            if let Err(e) = app.global_shortcut().register(session_toggle_shortcut()) {
                tracing::warn!(
                    %e,
                    "failed register global session shortcut; Ctrl+D/Cmd+D may only work while Helppye is focused"
                );
            }

            let transcriber: Arc<dyn SegmentTranscriber> = Arc::new(WhisperCppProvider::new());
            let timeline = Arc::new(ConversationTimeline::default());
            timeline.attach();
            let response_engine_state = response_provider::build(app.handle());
            let response_engine = response_engine_state.0.clone();
            // O motor e a timeline nascem separados, cada um com sua ideia de "sessão
            // atual". Alinha os dois já no boot para que o primeiro turno da primeira
            // sessão não seja recusado como pertencente a outra sessão.
            response_engine.begin_session(timeline.session_id());
            let app_handle = app.handle().clone();

            // O receiver interno nasce uma vez por aplicação e permanece vivo entre
            // sessões. A Timeline é o único publisher; o ResponseEngine é o consumer.
            let response_event_receiver = timeline.subscribe_internal_events();
            start_response_engine_event_loop(
                app_handle.clone(),
                response_engine.clone(),
                response_event_receiver,
            );

            // O timer dedicado da utterance (`ConversationTimeline::reschedule_utterance_timer`)
            // finaliza uma utterance por silêncio sem que nenhum código externo chame de
            // volta o timeline — precisa de um jeito de emitir esses eventos por conta
            // própria. Esta callback é exclusivamente o evento visual Tauri; o evento
            // interno do ResponseEngine é publicado pela própria Timeline no broadcast.
            let app_handle_for_sink = app_handle.clone();
            timeline.set_frontend_event_sink(Arc::new(move |events| {
                emit_conversation_events(&app_handle_for_sink, events);
            }))?;

            // Todo resultado que chega aqui já foi validado pelo `TranscriptionRuntime`:
            // pertence à sessão de conversa ativa, à sessão de transcrição viva daquela
            // fonte, e não é reentrega. O descarte acontece lá, antes da timeline — ver
            // `transcription/runtime.rs`.
            let timeline_for_queue = timeline.clone();
            let sink: TranscriptionOutputSink = Arc::new(move |output| match output {
                TranscriptionRuntimeOutput::Final(normalized) => {
                    let segment_id = normalized
                        .transcript
                        .segment_id
                        .unwrap_or_else(SegmentId::next);
                    let wire = TranscriptEvent::Ready(Transcript {
                        segment_id,
                        source: normalized.transcript.source,
                        text: normalized.normalization.normalized_text.clone(),
                        language: normalized.transcript.language.clone(),
                        started_at: normalized.transcript.started_at,
                        ended_at: normalized.transcript.ended_at,
                        processing_time_ms: normalized
                            .transcript
                            .processing_time_ms
                            .unwrap_or_default(),
                    });
                    if let Err(e) = app_handle.emit(TRANSCRIPTION_EVENT, &wire) {
                        tracing::warn!(%e, "failed to emit transcription event to frontend");
                    }

                    let conversation_events = timeline_for_queue.ingest_normalized_transcript(
                        &normalized.envelope,
                        &normalized.transcript,
                        &normalized.normalization,
                        normalized.speech_ended_at,
                    );
                    emit_conversation_events(&app_handle, conversation_events);
                }
                TranscriptionRuntimeOutput::Event(TranscriptionEvent::Error(error)) => {
                    // Uma transcrição que falha não pode sumir sem deixar rastro: sem este
                    // log, um provider sem modelo carregado produz falha para *todo*
                    // segmento e o app fica em silêncio absoluto — captura ativa, medidor
                    // de nível se mexendo, nada na timeline e nenhuma linha no terminal
                    // explicando o porquê.
                    tracing::warn!(
                        source = ?error.source,
                        provider = %error.provider,
                        message = %error.message,
                        "transcription failed"
                    );
                    let wire = TranscriptEvent::Failed {
                        segment_id: SegmentId::next(),
                        source: error.source,
                        message: error.message.clone(),
                    };
                    if let Err(e) = app_handle.emit(TRANSCRIPTION_EVENT, &wire) {
                        tracing::warn!(%e, "failed to emit transcription event to frontend");
                    }
                }
                TranscriptionRuntimeOutput::Event(_) => {}
                TranscriptionRuntimeOutput::Discarded { .. } => {}
            });

            let whisper_local: Arc<dyn TranscriptionProvider> =
                Arc::new(WhisperLocalTranscriptionProvider::new(transcriber.clone()));
            let mut registry = TranscriptionProviderRegistry::new();
            registry.register(whisper_local.clone());

            let runtime = Arc::new(TranscriptionRuntime::new(
                whisper_local,
                TranscriptionSettings::default(),
                sink,
            ));
            // Mesma razão do `response_engine.begin_session` acima: sem alinhar a fronteira
            // no boot, o primeiro segmento da primeira sessão seria descartado por não haver
            // sessão de transcrição ativa.
            let runtime_for_boot = runtime.clone();
            let boot_session_id = timeline.session_id();
            tauri::async_runtime::spawn(async move {
                runtime_for_boot.begin_session(boot_session_id).await;
            });

            let queue = Arc::new(TranscriptionQueue::spawn(runtime.clone()));
            let audio_state = audio::build(
                app.handle(),
                queue.clone(),
                timeline.clone(),
            )?;
            app.manage(audio_state);
            app.manage(ConversationTimelineState(timeline));
            app.manage(response_engine_state);
            app.manage(TranscriptionState::new(
                transcriber.clone(),
                queue,
                runtime,
                registry,
            ));

            let model_manager_state = model_manager::build(app.handle(), transcriber)?;
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
            integrity::origin_integrity_snapshot_command,
            response_provider::response_provider_status_command,
            response_provider::response_providers_command,
            response_provider::response_settings_command,
            response_provider::response_set_settings_command,
            response_provider::response_set_provider_config_command,
            response_provider::response_set_api_key_command,
            response_provider::response_delete_api_key_command,
            response_provider::response_last_rejection_command,
            transcription::configure_transcription_command,
            transcription::transcription_diagnostics_command,
            transcription::transcription_providers_command,
            transcription::transcription_settings_command,
            transcription::transcription_set_settings_command,
            transcription::transcription_correction_mode_command,
            transcription::transcription_set_correction_mode_command,
            transcription::transcription_vocabulary_command,
            transcription::transcription_add_vocabulary_entry_command,
            telemetry::telemetry_snapshot_command,
            telemetry::telemetry_set_content_policy_command,
            model_manager::model_status_command,
            model_manager::start_model_download_command,
            model_manager::cancel_model_download_command,
            model_manager::select_custom_model_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Helppye application");
}
